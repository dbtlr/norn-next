//! The takeover dial's hello: this crate's own dialer half.
//!
//! **The takeover's dialer.** A start that finds its channel's lock held
//! dials the endpoint, verifies whatever answers by the hello, and drains a
//! verified auto-launched host over the same connection (`start.rs`); the
//! port check dials once the same way. The takeover verifies a host exactly
//! as every other dialer does, and the `norn` package's process suite runs
//! one relay matrix against this dialer and `norn-client`'s.
//!
//! **This crate has no edge to `norn-client`, by design**, so the dialer is
//! written twice. What the two must agree on is `norn-wire`'s: the request,
//! the reply's reading, the body cap, the loopback test, the comparison of
//! the answer's facts with the connection, and the failure a dial reports
//! (`HelloFailure`). Only the transport and the HMAC check are repeated here.
//!
//! **No bearer without its own verified hello.** [`dial`] opens one
//! connection, sends the hello as its first exchange, and returns a
//! [`Verified`] connection only when the MAC verifies under the chosen token
//! and the facts name this connection; [`Verified::send`] is then the one way
//! to send that token as a bearer, and only on that connection. Every failure
//! closes the connection with the bearer unsent. There is no timeout here:
//! the caller bounds the dial.

use std::fmt;
use std::io;
use std::net::SocketAddr;

use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Bytes;
use hyper::client::conn::http1::{self, SendRequest};
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HOST, HeaderValue};
use hyper::{Method, Request, Response};
use hyper_util::rt::TokioIo;
use norn_config::machine::tokens::Tokens;
use norn_wire::{
    HELLO_BODY_BYTES, HelloAnswer, HelloChannel, HelloExpectation, HelloFailure, HelloMismatch,
    HelloReply, HelloRequest, Nonce, is_dialable,
};
use tokio::net::{TcpSocket, TcpStream};
use tokio::task::JoinHandle;

use crate::credentials::mac_verifies;

/// A connection whose hello verified: the only way this dialer sends a
/// bearer, and only on the connection the hello verified.
pub struct Verified {
    sender: SendRequest<Full<Bytes>>,
    _connection: Connection,
    bearer: HeaderValue,
    host: HeaderValue,
    answer: HelloAnswer,
}

impl fmt::Debug for Verified {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Verified")
            .field("answer", &self.answer)
            .finish_non_exhaustive()
    }
}

impl Verified {
    /// The verified answer: the host's channel, build, launch kind and state.
    pub fn answer(&self) -> &HelloAnswer {
        &self.answer
    }

    /// Send `request` on this connection with the bearer, and read its
    /// answer whole. Any `Authorization` the request carried is replaced.
    /// A connection that has closed fails; it is never reopened, because a
    /// new connection needs its own hello.
    pub async fn send(
        &mut self,
        mut request: Request<Full<Bytes>>,
    ) -> Result<Response<Bytes>, hyper::Error> {
        let headers = request.headers_mut();
        headers.insert(AUTHORIZATION, self.bearer.clone());
        headers.entry(HOST).or_insert_with(|| self.host.clone());
        self.sender.ready().await?;
        let response = self.sender.send_request(request).await?;
        let (parts, body) = response.into_parts();
        let body = body.collect().await?.to_bytes();
        Ok(Response::from_parts(parts, body))
    }
}

/// Dial `address`, run the hello with the token [`Tokens::dialer_token`]
/// chooses, and verify the answer against `channel` and this connection.
pub async fn dial(
    address: SocketAddr,
    channel: HelloChannel,
    tokens: &Tokens,
) -> Result<Verified, HelloFailure> {
    dial_over(address, channel, tokens, new_socket).await
}

/// [`dial`], over the socket `open` makes: the dial path's one seam, so a
/// suite sees what the path itself does to each connection's socket.
async fn dial_over<S: DialerSocket>(
    address: SocketAddr,
    channel: HelloChannel,
    tokens: &Tokens,
    open: impl FnOnce(SocketAddr) -> io::Result<S>,
) -> Result<Verified, HelloFailure> {
    if !is_dialable(address) {
        return Err(HelloFailure::NotLoopback(address));
    }
    let token = tokens.dialer_token().ok_or(HelloFailure::NoToken)?;
    let connect = |error| HelloFailure::Connect { address, error };
    let socket = open(address).map_err(connect)?;
    set_options(&socket).map_err(connect)?;
    let stream = socket.connect(address).await.map_err(connect)?;
    let local = stream.local_addr().map_err(connect)?;
    let exchange = |error: hyper::Error| HelloFailure::Exchange {
        address,
        error: error.to_string(),
    };
    let (mut sender, connection) = http1::handshake::<_, Full<Bytes>>(TokioIo::new(stream))
        .await
        .map_err(exchange)?;
    let connection = Connection(tokio::spawn(async move {
        let _ = connection.await;
    }));

    let nonce = fresh_nonce().map_err(|error| HelloFailure::Exchange {
        address,
        error: format!("drawing the nonce failed: {error}"),
    })?;
    let host = HeaderValue::from_str(&address.to_string()).expect("a socket address is a header");
    let body = serde_json::to_vec(&HelloRequest::new(token.label().as_str(), &nonce))
        .expect("a hello request serializes");
    let request = Request::builder()
        .method(Method::POST)
        .uri("/v1/hello")
        .header(HOST, host.clone())
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .expect("the hello request is well formed");
    let response = sender.send_request(request).await.map_err(exchange)?;
    let status = response.status().as_u16();
    let verification = |mismatch| HelloFailure::Verification { address, mismatch };
    // An answer over the cap is no hello answer; it is not read past it.
    let body = Limited::new(response.into_body(), HELLO_BODY_BYTES)
        .collect()
        .await
        .map_err(|_| verification(HelloMismatch::NotAnAnswer))?
        .to_bytes();
    let answer = match HelloReply::read(status, &body) {
        HelloReply::Answer(answer) => answer,
        HelloReply::Refused => return Err(HelloFailure::HelloRefused { address }),
        HelloReply::TokenFileUnreadable => {
            return Err(HelloFailure::ListenerSaysTokenFileUnreadable { address });
        }
        HelloReply::Unrecognized => return Err(verification(HelloMismatch::NotAnAnswer)),
    };
    // The MAC first: until it verifies, the facts are whoever answered's.
    let mac = answer.mac().ok_or(verification(HelloMismatch::Mac))?;
    if !mac_verifies(token.secret(), &answer.facts().mac_input(&nonce), &mac) {
        return Err(verification(HelloMismatch::Mac));
    }
    answer
        .facts()
        .check_connection(&HelloExpectation {
            channel,
            dialed: address,
            local,
        })
        .map_err(verification)?;

    let mut bearer = HeaderValue::from_str(&format!(
        "Bearer {}",
        norn_wire::hex::encode(token.secret())
    ))
    .expect("lowercase hex is a header value");
    bearer.set_sensitive(true);
    Ok(Verified {
        sender,
        _connection: connection,
        bearer,
        host,
        answer,
    })
}

/// The task driving one connection, aborted when its owner goes: a dial that
/// fails closes its connection at once, bearer unsent.
struct Connection(JoinHandle<()>);

impl Drop for Connection {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The socket a dial connects with. Its one option is set once, on a socket
/// nothing else holds a descriptor of, before it connects; the dial path
/// sets it through this trait, so a suite can watch it do so.
trait DialerSocket {
    fn set_reuseaddr(&self, reuseaddr: bool) -> io::Result<()>;
    fn connect(self, address: SocketAddr) -> impl Future<Output = io::Result<TcpStream>> + Send;
}

impl DialerSocket for TcpSocket {
    fn set_reuseaddr(&self, reuseaddr: bool) -> io::Result<()> {
        TcpSocket::set_reuseaddr(self, reuseaddr)
    }

    fn connect(self, address: SocketAddr) -> impl Future<Output = io::Result<TcpStream>> + Send {
        TcpSocket::connect(self, address)
    }
}

/// `SO_REUSEADDR`, so a dialer's socket left in TIME_WAIT on the endpoint
/// cannot make a later start's probe or bind fail.
fn set_options(socket: &impl DialerSocket) -> io::Result<()> {
    socket.set_reuseaddr(true)
}

/// A fresh socket of `address`'s family, no option set yet.
fn new_socket(address: SocketAddr) -> io::Result<TcpSocket> {
    if address.is_ipv4() {
        TcpSocket::new_v4()
    } else {
        TcpSocket::new_v6()
    }
}

fn fresh_nonce() -> Result<Nonce, getrandom::Error> {
    let mut bytes = [0u8; Nonce::BYTES];
    getrandom::fill(&mut bytes)?;
    Ok(Nonce::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use norn_config::ConfigDirs;
    use norn_config::machine::tokens::{self, TokenLabel};
    use norn_testkit::scratch::Scratch;

    use super::*;

    /// What the dial path did to one socket, in order.
    #[derive(Debug, PartialEq)]
    enum Event {
        SetReuseaddr(bool),
        /// The connect, with the option as the socket held it then.
        Connect {
            reuseaddr: bool,
        },
    }

    /// A real socket that records what the dial path does to it.
    struct Recorded {
        socket: TcpSocket,
        events: Arc<Mutex<Vec<Event>>>,
    }

    impl DialerSocket for Recorded {
        fn set_reuseaddr(&self, reuseaddr: bool) -> io::Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(Event::SetReuseaddr(reuseaddr));
            self.socket.set_reuseaddr(reuseaddr)
        }

        async fn connect(self, address: SocketAddr) -> io::Result<TcpStream> {
            let reuseaddr = self.socket.reuseaddr()?;
            self.events
                .lock()
                .unwrap()
                .push(Event::Connect { reuseaddr });
            self.socket.connect(address).await
        }
    }

    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the base the token file lives under.
    fn one_token(scratch: &Scratch) -> Tokens {
        std::fs::create_dir_all(scratch.join("config")).expect("a config base");
        let dirs = ConfigDirs::new(scratch.join("config"), scratch.join("data")).expect("bases");
        tokens::mutate(&dirs, |tokens| {
            tokens.add(&TokenLabel::default_label(), b"a secret")
        })
        .expect("a token");
        tokens::read(&dirs).expect("the token file")
    }

    /// The dial path itself, through the socket it opens: `SO_REUSEADDR` set
    /// once on each connection's socket, and on before it connects. A dial
    /// refused before it connects opens no socket.
    #[test]
    fn the_dialer_sets_reuseaddr_once_before_connecting() {
        let scratch = Scratch::new("norn-dialer-reuseaddr");
        let tokens = one_token(&scratch);
        // A listener that closes each connection unanswered, so each dial
        // connects and then fails its hello.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                drop(stream);
            }
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        let dial = |address: SocketAddr| {
            let events = Arc::new(Mutex::new(Vec::new()));
            let recorded = Arc::clone(&events);
            let outcome = runtime.block_on(dial_over(address, HelloChannel::Dev, &tokens, |at| {
                Ok(Recorded {
                    socket: new_socket(at)?,
                    events: recorded,
                })
            }));
            let events = std::mem::take(&mut *events.lock().unwrap());
            (outcome, events)
        };
        for _ in 0..2 {
            let (outcome, events) = dial(address);
            assert!(
                matches!(outcome, Err(HelloFailure::Exchange { .. })),
                "{outcome:?}"
            );
            assert_eq!(
                events,
                [
                    Event::SetReuseaddr(true),
                    Event::Connect { reuseaddr: true }
                ]
            );
        }
        let (outcome, events) = dial("192.0.2.1:9".parse().unwrap());
        assert!(matches!(outcome, Err(HelloFailure::NotLoopback(_))));
        assert_eq!(events, []);
    }

    /// The seam leaves the public dial a future a multi-threaded runtime can
    /// move between threads.
    #[test]
    fn a_dial_is_send() {
        fn is_send<T: Send>(_: T) {}
        let scratch = Scratch::new("norn-dialer-send");
        let tokens = one_token(&scratch);
        is_send(dial(
            "127.0.0.1:9".parse().unwrap(),
            HelloChannel::Dev,
            &tokens,
        ));
    }
}
