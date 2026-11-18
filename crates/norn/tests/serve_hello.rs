//! Host verification, end to end: the real `norn serve` beside test peers
//! that impersonate, relay to, or replace it, with both dialers —
//! `norn-client`'s and the serving crate's takeover dial — run through the
//! same cases.
//!
//! Every peer records whether it ever received an `Authorization` header, and
//! no case lets one. A dialer refusing is not enough on its own: the bearer
//! must never have left it.
//!
//! The case that relays through a second loopback address runs on Linux
//! only, where every `127.0.0.0/8` address is the loopback interface's;
//! macOS configures `127.0.0.1` alone unless an alias is added.
#![allow(clippy::disallowed_methods)] // Harness scaffolding: the token files a case arranges.

mod support;

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::{Method, Request};
use norn_config::Channel;
use norn_config::machine::tokens::{self, TokenLabel, Tokens};
use norn_wire::{
    BuildIdentity, HelloAnswer, HelloChannel, HelloFacts, HelloLaunch, HelloMismatch, HelloState,
};
use support::{Fixture, bind_unissued, exchange, free_port, get};

// The two dialers, behind one face.

#[derive(Clone, Copy, Debug)]
enum Dialer {
    Client,
    Takeover,
}

const DIALERS: [Dialer; 2] = [Dialer::Client, Dialer::Takeover];

#[derive(Debug, PartialEq)]
enum Failure {
    NotLoopback,
    NoToken,
    Connect,
    Exchange,
    HelloRefused,
    ListenerSaysTokenFileUnreadable,
    Verification(HelloMismatch),
}

/// A verified connection and the runtime that drives it: the connection's
/// task runs only while that runtime does.
struct Conn {
    runtime: tokio::runtime::Runtime,
    verified: Verified,
}

enum Verified {
    Client(norn_client::hello::Verified),
    Takeover(norn_serve::dial::Verified),
}

impl std::fmt::Debug for Conn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self.verified {
            Verified::Client(_) => "a verified client connection",
            Verified::Takeover(_) => "a verified takeover connection",
        })
    }
}

/// The channel this build is pinned to, as both dialers expect it.
fn channel_of_build() -> HelloChannel {
    Channel::COMPILED.into()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .unwrap()
}

impl Dialer {
    /// Dial and verify, answering the failure and its message on refusal.
    fn dial(self, address: SocketAddr, tokens: &Tokens) -> Result<Conn, (Failure, String)> {
        let channel = channel_of_build();
        let runtime = runtime();
        let verified = runtime
            .block_on(async {
                match self {
                    Dialer::Client => norn_client::hello::dial(address, channel, tokens)
                        .await
                        .map(Verified::Client),
                    Dialer::Takeover => norn_serve::dial::dial(address, channel, tokens)
                        .await
                        .map(Verified::Takeover),
                }
            })
            .map_err(|error| {
                use norn_wire::HelloFailure as E;
                let kind = match &error {
                    E::NotLoopback(_) => Failure::NotLoopback,
                    E::NoToken => Failure::NoToken,
                    E::Connect { .. } => Failure::Connect,
                    E::Exchange { .. } => Failure::Exchange,
                    E::HelloRefused { .. } => Failure::HelloRefused,
                    E::ListenerSaysTokenFileUnreadable { .. } => {
                        Failure::ListenerSaysTokenFileUnreadable
                    }
                    E::Verification { mismatch, .. } => Failure::Verification(*mismatch),
                };
                (kind, error.to_string())
            })?;
        Ok(Conn { runtime, verified })
    }
}

/// A dial whose connection must be driven while the dial runs: on a thread
/// of its own, answered on a channel.
fn dial_in_background(
    dialer: Dialer,
    address: SocketAddr,
    tokens: Tokens,
) -> Receiver<Result<(), (Failure, String)>> {
    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        let _ = sender.send(dialer.dial(address, &tokens).map(|_| ()));
    });
    receiver
}

impl Conn {
    /// A bearer `GET` on this connection, answering its status.
    fn get(&mut self, path: &str) -> Result<u16, String> {
        let request = Request::builder()
            .method(Method::GET)
            .uri(path)
            .body(Full::new(Bytes::new()))
            .unwrap();
        let Conn { runtime, verified } = self;
        runtime.block_on(async {
            match verified {
                Verified::Client(verified) => verified.send(request).await,
                Verified::Takeover(verified) => verified.send(request).await,
            }
            .map(|response| response.status().as_u16())
            .map_err(|error| error.to_string())
        })
    }
}

// The real host's token, written before it starts so the dialers and the
// peers know it.

fn with_token(fixture: &Fixture, label: &str, secret: &[u8]) -> Tokens {
    fixture.config_dir();
    tokens::mutate(&fixture.dirs(), |tokens| {
        tokens.add(&TokenLabel::new(label).unwrap(), secret)
    })
    .unwrap();
    tokens::read(&fixture.dirs()).unwrap()
}

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

// The test peers.

/// What a peer saw from the dialer.
#[derive(Clone, Default)]
struct Witness {
    authorization: Arc<AtomicBool>,
    first_lines: Arc<Mutex<Vec<String>>>,
}

impl Witness {
    /// A witness of one connection's stream from the dialer.
    fn stream(&self) -> StreamWitness {
        StreamWitness {
            seen: Vec::new(),
            authorization: Arc::clone(&self.authorization),
        }
    }

    fn saw_a_bearer(&self) -> bool {
        self.authorization.load(Ordering::SeqCst)
    }
}

/// What one connection's stream carried so far, lowercased: the header is
/// searched for in all of it, so one split across reads is still seen.
struct StreamWitness {
    seen: Vec<u8>,
    authorization: Arc<AtomicBool>,
}

impl StreamWitness {
    fn observe(&mut self, bytes: &[u8]) {
        self.seen.extend(bytes.iter().map(u8::to_ascii_lowercase));
        let needle = b"authorization:";
        if self
            .seen
            .windows(needle.len())
            .any(|window| window == needle)
        {
            self.authorization.store(true, Ordering::SeqCst);
        }
    }
}

/// One HTTP request's head and body off `stream`, every byte witnessed.
fn read_request(stream: &mut TcpStream, witness: &mut StreamWitness) -> Option<(String, Vec<u8>)> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    let head_end = loop {
        if let Some(at) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break at;
        }
        let read = stream.read(&mut buffer).ok().filter(|read| *read > 0)?;
        witness.observe(&buffer[..read]);
        bytes.extend_from_slice(&buffer[..read]);
    };
    let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
    let length: usize = head
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())?
        })
        .unwrap_or(0);
    let mut body = bytes[head_end + 4..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut buffer).ok().filter(|read| *read > 0)?;
        witness.observe(&buffer[..read]);
        body.extend_from_slice(&buffer[..read]);
    }
    Some((head, body))
}

/// Read everything else the dialer sends until it closes, witnessing it.
fn witness_the_rest(stream: &mut TcpStream, witness: &mut StreamWitness) {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut buffer = [0u8; 4096];
    while let Ok(read) = stream.read(&mut buffer) {
        if read == 0 {
            break;
        }
        witness.observe(&buffer[..read]);
    }
}

/// A peer without the token: it answers every hello with this connection's
/// true facts and a MAC it had to invent.
fn forger(listener: TcpListener) -> Witness {
    let witness = Witness::default();
    let seen = witness.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let seen = seen.clone();
            std::thread::spawn(move || {
                let mut witness = seen.stream();
                let Some((head, _)) = read_request(&mut stream, &mut witness) else {
                    return;
                };
                seen.first_lines
                    .lock()
                    .unwrap()
                    .push(head.lines().next().unwrap_or_default().to_string());
                let facts = HelloFacts::new(
                    channel_of_build(),
                    BuildIdentity::new("0.0.0+forged"),
                    HelloLaunch::Explicit,
                    HelloState::Serving,
                    stream.local_addr().unwrap(),
                    stream.peer_addr().unwrap(),
                );
                let body = serde_json::to_vec(&HelloAnswer::new(facts, [0x5a; 32])).unwrap();
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
                witness_the_rest(&mut stream, &mut witness);
            });
        }
    });
    witness
}

/// Relay one accepted connection to `upstream` in both directions,
/// witnessing what the dialer sends.
fn relay(mut downstream: TcpStream, mut upstream: TcpStream, witness: Witness) {
    let mut down_reader = downstream.try_clone().unwrap();
    let mut up_writer = upstream.try_clone().unwrap();
    let mut witness = witness.stream();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        while let Ok(read) = down_reader.read(&mut buffer) {
            if read == 0 {
                break;
            }
            witness.observe(&buffer[..read]);
            if up_writer.write_all(&buffer[..read]).is_err() {
                break;
            }
        }
        let _ = up_writer.shutdown(Shutdown::Write);
    });
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut upstream, &mut downstream);
        let _ = downstream.shutdown(Shutdown::Write);
    });
}

/// A connection to `target`, from `source` when one is given.
fn connect_from(source: Option<SocketAddr>, target: SocketAddr) -> TcpStream {
    let runtime = runtime();
    runtime.block_on(async {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        if let Some(source) = source {
            socket.bind(source).unwrap();
        }
        let stream = socket.connect(target).await.unwrap();
        let stream = stream.into_std().unwrap();
        stream.set_nonblocking(false).unwrap();
        stream
    })
}

// The cases.

/// The witness reads a stream, not each read: a header split across two
/// reads, in any case, is still seen.
#[test]
fn the_witness_sees_an_authorization_header_split_across_reads() {
    let witness = Witness::default();
    let mut stream = witness.stream();
    stream.observe(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthor");
    assert!(!witness.saw_a_bearer());
    stream.observe(b"IZATION: Bearer 00ff\r\n\r\n");
    assert!(witness.saw_a_bearer());
}

/// The positive control, and the one exemption: a verified dial sends its
/// bearer and is admitted, and the hello is the only exchange a host answers
/// without one.
#[test]
fn the_hello_is_the_only_exchange_admitted_without_a_bearer() {
    let fixture = Fixture::new("hello-exemption");
    let tokens = with_token(&fixture, "default", &[1; 32]);
    let port = free_port();
    let (served, bearer) = fixture.serve_on(port);

    for (method, path) in [
        ("GET", "/"),
        ("GET", "/v1/hello"),
        ("POST", "/v1/wire/find"),
        ("POST", "/v1/control/drain"),
        ("GET", "/v1/debug/counters"),
        ("POST", "/mcp"),
    ] {
        let reply = exchange(port, method, path, None, b"{}");
        assert_eq!(reply.status, 401, "{method} {path}: {reply:?}");
        assert_eq!(reply.code(), "serve/unauthorized");
        assert_eq!(
            reply.header("content-type").as_deref(),
            Some("application/json")
        );
    }
    assert_eq!(support::hello(port, "default").status, 200);
    assert_eq!(get(port, "/", Some(&bearer)).status, 404);

    for dialer in DIALERS {
        let mut conn = dialer
            .dial(loopback(port), &tokens)
            .unwrap_or_else(|failure| panic!("{dialer:?}: {failure:?}"));
        assert_eq!(
            conn.get("/"),
            Ok(404),
            "{dialer:?}: admitted, and nothing is routed"
        );
        assert_eq!(
            conn.get("/v1/wire/find"),
            Ok(404),
            "{dialer:?}: on the same connection"
        );
    }
    assert_eq!(served.terminate().0.code(), Some(0));
}

#[test]
fn a_peer_without_the_token_cannot_answer() {
    let fixture = Fixture::new("hello-forger");
    let tokens = with_token(&fixture, "default", &[2; 32]);
    let listener = bind_unissued();
    let address = listener.local_addr().unwrap();
    let witness = forger(listener);
    for dialer in DIALERS {
        let (failure, message) = dialer.dial(address, &tokens).expect_err("a refusal");
        assert_eq!(
            failure,
            Failure::Verification(HelloMismatch::Mac),
            "{dialer:?}"
        );
        assert!(message.contains("host verification failed"), "{message}");
        assert!(
            message.contains("the token changed since it was read"),
            "{message}"
        );
        assert!(
            message.contains("something other than a norn host holds the port"),
            "{message}"
        );
    }
    std::thread::sleep(Duration::from_millis(100));
    assert!(!witness.saw_a_bearer());
    let lines = witness.first_lines.lock().unwrap().clone();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines.iter().all(|line| line.starts_with("POST /v1/hello ")),
        "the hello is each connection's first exchange: {lines:?}"
    );
}

#[test]
fn a_relay_to_a_real_host_on_another_port_is_refused() {
    let fixture = Fixture::new("hello-relay-other-port");
    let tokens = with_token(&fixture, "default", &[3; 32]);
    let port = free_port();
    let (served, _) = fixture.serve_on(port);

    let listener = bind_unissued();
    let squatted = listener.local_addr().unwrap();
    let witness = Witness::default();
    let seen = witness.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            relay(
                stream,
                TcpStream::connect(loopback(port)).unwrap(),
                seen.clone(),
            );
        }
    });
    for dialer in DIALERS {
        let (failure, _) = dialer.dial(squatted, &tokens).expect_err("a refusal");
        assert_eq!(
            failure,
            Failure::Verification(HelloMismatch::Host),
            "{dialer:?}"
        );
    }
    std::thread::sleep(Duration::from_millis(100));
    assert!(!witness.saw_a_bearer());
    assert_eq!(served.terminate().0.code(), Some(0));
}

/// The squatter holds the dialer's accepted connection, frees the port, lets
/// the real host bind it, then relays over a connection of its own: from an
/// ephemeral port of `127.0.0.1`, or — on Linux — from the dialer's own source
/// port on `127.0.0.2`, so only the client address's IP differs.
fn a_relay_to_a_host_that_later_binds_the_same_port(label: &str, second_address: bool) {
    for dialer in DIALERS {
        let fixture = Fixture::new(label);
        let tokens = with_token(&fixture, "default", &[4; 32]);
        let listener = bind_unissued();
        let port = listener.local_addr().unwrap().port();
        let witness = Witness::default();
        let seen = witness.clone();
        let (freed, port_freed) = channel::<()>();
        let (host_up, host_listening): (Sender<()>, Receiver<()>) = channel();
        std::thread::spawn(move || {
            let (accepted, dialer_end) = listener.accept().unwrap();
            drop(listener);
            freed.send(()).unwrap();
            host_listening.recv().unwrap();
            let source =
                second_address.then(|| SocketAddr::from(([127, 0, 0, 2], dialer_end.port())));
            relay(accepted, connect_from(source, loopback(port)), seen);
        });
        let answer = dial_in_background(dialer, loopback(port), tokens);
        port_freed.recv().unwrap();
        let (served, _) = fixture.serve_on(port);
        host_up.send(()).unwrap();

        let (failure, message) = answer
            .recv_timeout(support::START_WITHIN)
            .expect("the dial ends")
            .expect_err("a refusal");
        assert_eq!(
            failure,
            Failure::Verification(HelloMismatch::Client),
            "{dialer:?}: {message}"
        );
        std::thread::sleep(Duration::from_millis(100));
        assert!(!witness.saw_a_bearer(), "{dialer:?}");
        assert_eq!(served.terminate().0.code(), Some(0));
    }
}

#[test]
fn a_relay_to_a_real_host_that_later_binds_the_same_port_is_refused() {
    a_relay_to_a_host_that_later_binds_the_same_port("hello-relay-same-port", false);
}

// macOS needs an administrator-added 127.0.0.2 alias; Linux routes all of 127/8.
#[cfg(target_os = "linux")]
#[test]
fn a_relay_through_a_second_loopback_address_with_the_clients_source_port_is_refused() {
    a_relay_to_a_host_that_later_binds_the_same_port("hello-relay-second-address", true);
}

/// A verified connection is never reopened, and the new listener behind the
/// port meets the hello on its own connection.
#[test]
fn a_listener_swapped_between_two_requests_never_receives_a_bearer() {
    for dialer in DIALERS {
        let fixture = Fixture::new("hello-swap");
        let tokens = with_token(&fixture, "default", &[5; 32]);
        let port = free_port();
        let (served, _) = fixture.serve_on(port);
        let mut conn = dialer
            .dial(loopback(port), &tokens)
            .expect("a verified dial");
        assert_eq!(conn.get("/"), Ok(404));

        assert_eq!(served.terminate().0.code(), Some(0));
        let witness = forger(TcpListener::bind(loopback(port)).unwrap());
        assert!(
            conn.get("/").is_err(),
            "{dialer:?}: the closed connection is not reopened"
        );
        let (failure, _) = dialer.dial(loopback(port), &tokens).expect_err("a refusal");
        assert_eq!(
            failure,
            Failure::Verification(HelloMismatch::Mac),
            "{dialer:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
        assert!(!witness.saw_a_bearer(), "{dialer:?}");
    }
}

#[test]
fn a_token_rotated_between_the_read_and_the_hello_fails_verification_and_names_both_causes() {
    let fixture = Fixture::new("hello-rotated");
    let held = with_token(&fixture, "default", &[6; 32]);
    let port = free_port();
    let (served, _) = fixture.serve_on(port);
    tokens::mutate(&fixture.dirs(), |tokens| {
        tokens.remove(&TokenLabel::default_label());
        tokens.add(&TokenLabel::default_label(), &[7; 32])
    })
    .unwrap();
    for dialer in DIALERS {
        let (failure, message) = dialer.dial(loopback(port), &held).expect_err("a refusal");
        assert_eq!(
            failure,
            Failure::Verification(HelloMismatch::Mac),
            "{dialer:?}"
        );
        assert!(message.contains("host verification failed"), "{message}");
        assert!(
            message.contains("the token changed since it was read"),
            "{message}"
        );
        assert!(
            message.contains("something other than a norn host holds the port"),
            "{message}"
        );
        assert!(message.contains("the bearer was not sent"), "{message}");
    }
    // The fresh read verifies.
    let fresh = tokens::read(&fixture.dirs()).unwrap();
    for dialer in DIALERS {
        assert!(dialer.dial(loopback(port), &fresh).is_ok(), "{dialer:?}");
    }
    assert_eq!(served.terminate().0.code(), Some(0));
}

#[test]
fn the_host_listens_only_on_the_exact_resolved_loopback_address() {
    let fixture = Fixture::new("hello-exact-address");
    let tokens = with_token(&fixture, "default", &[8; 32]);
    let port = free_port();
    let (served, _) = fixture.serve_on(port);
    assert!(TcpStream::connect(loopback(port)).is_ok());
    assert!(
        TcpStream::connect(SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port))).is_err(),
        "nothing listens on [::1]:{port}"
    );
    // macOS has no 127.0.0.2 alias by default; Linux routes all of 127/8.
    #[cfg(target_os = "linux")]
    assert!(
        TcpStream::connect(SocketAddr::from(([127, 0, 0, 2], port))).is_err(),
        "nothing listens on 127.0.0.2:{port}"
    );
    for dialer in DIALERS {
        let (failure, _) = dialer
            .dial(
                SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port)),
                &tokens,
            )
            .expect_err("no listener");
        assert_eq!(failure, Failure::Connect, "{dialer:?}");
    }
    assert_eq!(served.terminate().0.code(), Some(0));
}

/// Bar 7b: an auto-launched host binds only its endpoint from config.toml.
#[test]
fn an_auto_launched_host_listens_only_on_its_resolved_loopback_address() {
    let fixture = Fixture::new("hello-auto-exact-address");
    let tokens = with_token(&fixture, "default", &[8; 32]);
    let port = free_port();
    fixture.write_settings(&format!("[endpoint]\nport = {port}\n"));
    let mut served = fixture.start(&["serve", "--auto-launched"]);
    assert_eq!(served.listening_port(), port);
    assert!(TcpStream::connect(loopback(port)).is_ok());
    for dialer in DIALERS {
        let verified = dialer
            .dial(loopback(port), &tokens)
            .expect("resolved endpoint");
        let launch = match &verified.verified {
            Verified::Client(connection) => connection.answer().facts().launch(),
            Verified::Takeover(connection) => connection.answer().facts().launch(),
        };
        assert_eq!(launch, HelloLaunch::Auto);
    }
    assert!(
        TcpStream::connect(SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port))).is_err(),
        "nothing listens on [::1]:{port}"
    );
    // macOS has no 127.0.0.2 alias by default; Linux routes all of 127/8.
    #[cfg(target_os = "linux")]
    assert!(
        TcpStream::connect(SocketAddr::from(([127, 0, 0, 2], port))).is_err(),
        "nothing listens on 127.0.0.2:{port}"
    );
    assert_eq!(served.terminate().0.code(), Some(0));
}

/// Each failure that is not a verification failure is its own error, and an
/// unverified answer is reported as what a listener said.
#[test]
fn each_dial_failure_is_distinguished() {
    let fixture = Fixture::new("hello-failures");
    let tokens = with_token(&fixture, "default", &[9; 32]);
    let elsewhere = Fixture::new("hello-failures-other");
    let unknown_label = with_token(&elsewhere, "laptop", &[9; 32]);
    let none = Fixture::new("hello-failures-none");
    none.config_dir();
    let empty = tokens::read(&none.dirs()).unwrap();
    let port = free_port();
    let (served, _) = fixture.serve_on(port);

    for dialer in DIALERS {
        let failure = |address, tokens| dialer.dial(address, tokens).expect_err("a refusal");
        assert_eq!(
            failure(loopback(port), &empty).0,
            Failure::NoToken,
            "{dialer:?}"
        );
        assert_eq!(
            failure(SocketAddr::from(([192, 0, 2, 1], port)), &tokens).0,
            Failure::NotLoopback,
            "{dialer:?}"
        );
        assert_eq!(
            failure(loopback(free_port()), &tokens).0,
            Failure::Connect,
            "{dialer:?}"
        );
        // A refusal carries no MAC, so it is a failed verification, both
        // causes named.
        let (refused, message) = failure(loopback(port), &unknown_label);
        assert_eq!(refused, Failure::HelloRefused, "{dialer:?}");
        assert!(message.contains("host verification failed"), "{message}");
        assert!(
            message.contains("the token changed since it was read"),
            "{message}"
        );
        assert!(
            message.contains("something other than a norn host holds the port"),
            "{message}"
        );
        assert!(message.contains("the bearer was not sent"), "{message}");
    }

    use std::os::unix::fs::PermissionsExt;
    let file = fixture.tokens_file();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
    for dialer in DIALERS {
        let (failure, message) = dialer.dial(loopback(port), &tokens).expect_err("a refusal");
        assert_eq!(
            failure,
            Failure::ListenerSaysTokenFileUnreadable,
            "{dialer:?}"
        );
        assert!(
            message.contains("a listener at")
                && message.contains("said the token file is unreadable"),
            "{message}"
        );
        assert!(!message.contains("verification failed"), "{message}");
    }
    assert_eq!(served.terminate().0.code(), Some(0));
}

/// A listener that answers a hello with more than the cap is no hello
/// answer, and the dialer reads no more of it than the cap.
#[test]
fn an_answer_over_the_cap_is_refused_unread() {
    let fixture = Fixture::new("hello-oversized");
    let tokens = with_token(&fixture, "default", &[10; 32]);
    let listener = bind_unissued();
    let address = listener.local_addr().unwrap();
    let witness = Witness::default();
    let seen = witness.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let seen = seen.clone();
            std::thread::spawn(move || {
                let mut witness = seen.stream();
                if read_request(&mut stream, &mut witness).is_none() {
                    return;
                }
                let body = vec![b' '; 1 << 20];
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
                witness_the_rest(&mut stream, &mut witness);
            });
        }
    });
    for dialer in DIALERS {
        let (failure, _) = dialer.dial(address, &tokens).expect_err("a refusal");
        assert_eq!(
            failure,
            Failure::Verification(HelloMismatch::NotAnAnswer),
            "{dialer:?}"
        );
    }
    assert!(!witness.saw_a_bearer());
}

/// **Exit bars 1e, 1f and 1g.** A real host reports its compiled channel and
/// build identity, and the explicit or automatic launch kind it was given.
#[test]
fn the_hello_names_the_built_channel_identity_and_actual_launch_kind() {
    for (automatic, launch) in [(false, HelloLaunch::Explicit), (true, HelloLaunch::Auto)] {
        let fixture = Fixture::new("hello-build-and-launch");
        let port = free_port().to_string();
        fixture.write_settings(&format!("[endpoint]\nport = {port}\n"));
        let args = if automatic {
            vec!["serve", "--auto-launched"]
        } else {
            vec!["serve", "--port", &port]
        };
        let mut served = fixture.start(&args);
        let port = served.listening_port();
        let reply = support::hello(port, "default");
        assert_eq!(reply.status, 200, "{reply:?}");
        let answer: HelloAnswer = serde_json::from_slice(&reply.body).unwrap();
        assert_eq!(answer.facts().channel(), channel_of_build());
        assert_eq!(answer.facts().build(), env!("NORN_BUILD_IDENTITY"));
        assert_eq!(answer.facts().launch(), launch);
        assert_eq!(served.terminate().0.code(), Some(0));
    }
}

/// **Exit bar 6d (Linux and macOS).** A foreign loopback listener refuses a
/// real host start, including macOS's exact-bind fallback after the probe.
/// The macOS PR job runs this case by name.
#[test]
fn a_foreign_loopback_holder_refuses_a_real_start() {
    let fixture = Fixture::new("hello-loopback-holder");
    let holder = bind_unissued();
    let port = holder.local_addr().unwrap().port();
    let (status, stderr) = fixture.run(&["serve", "--port", &port.to_string()]);
    assert_eq!(
        status,
        norn_testkit::process::RunStatus::Exited(1),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "port {port} on 127.0.0.1 is held by another process"
        )),
        "{stderr}"
    );
    assert!(!stderr.contains("listening"), "{stderr}");
}
