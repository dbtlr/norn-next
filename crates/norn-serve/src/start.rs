//! The start, takeover and launch-race sequence: how one start comes to hold
//! the channel, or ends with a true report of why it does not.
//!
//! A start is **explicit** (`norn serve`) or **launched** (with the
//! auto-launch marker). "Verified" means the hello passed the dialer's check
//! on that connection (`dial.rs`).
//!
//! 1. **Settings** are read before this module runs; nothing is locked yet.
//! 2. **Try `serve.lock`.** Taken: the start goes on to step 5. Refused (a
//!    link at the name, a permission refusal): exit 1 naming the path.
//! 3. **The lock is held** ([`wait_on_held_lock`]). The deadline is now + the
//!    release bound, and the takeover cap starts. Each poll, then the poll
//!    interval, until the deadline or the cap:
//!    1. Try `serve.lock`; taken, go on to step 5.
//!    2. Take the token file's fingerprint, re-reading the set where it
//!       changed, and dial the endpoint within the dial bound with the chosen
//!       token. No token, an unreadable token file, a refused connect, a
//!       timeout or a failed verification are all *no verified answer*, and no
//!       bearer is sent on that connection.
//!    3. Verified `draining`: the deadline becomes now + the release bound,
//!       never past the cap — that host set `draining` before closing its
//!       listener, so it frees the lock within the release bound of now.
//!    4. Verified `serving`: a launched start exits 10 at once. An explicit
//!       start meeting an explicit host exits 1 at once. An explicit start
//!       meeting an auto-launched host sends no drain it could not wait out
//!       (now + the drain's dial bound + the release bound past the cap: exit
//!       1); otherwise it sends the drain request on the same connection,
//!       within the drain's own dial bound, and counts it sent. A 202, a 503
//!       `serve/draining` or no answer in time is *draining*: the deadline
//!       resets, never past the cap. A 409 `serve/drain-refused` is an
//!       explicit host. Anything else is no verified answer. It keeps dialing
//!       every poll, so a launched host that takes the freed lock first is
//!       seen, verified and drained in turn; and while a drain it sent may
//!       still be outstanding — until it takes the lock or a later hello
//!       verifies a host — it tries `serve.lock` alone every 10 ms, so it
//!       usually takes the freed lock ahead of a launched start's 100 ms poll.
//! 4. **The deadline or the cap passed** with the lock held: the facts
//!    ([`Facts`]) are logged, and the start exits 11 (launched) or 1.
//! 5. **`serve.lock` is taken**: the serving entry arms the start-bound
//!    watchdog, mints, reads the registry and builds the host, then checks
//!    the port ([`probe`]) and binds.
//!
//! **Exit 10 and 11 are a launched start's alone.** 10 says a verified host
//! serves this channel; 11 that the channel lock is held and no verified host
//! answers at this endpoint. The `norn-client` of the same build reads them,
//! so they need no cross-release stability.
//!
//! **Accepted windows.** A launched start can win the lock a drained host
//! frees; the explicit start then verifies and drains it in turn, and the
//! takeover cap bounds the whole wait, so losing outright ends with a true
//! report and exit 1, and running the start again succeeds. Two holders in a
//! row — a stopping holder, then a second start slow to listen — can reach a
//! waiter's deadline inside the second's start bound; the report is still
//! true. A port taken between the probe and the bind is refused at the bind.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::{Duration, Instant};

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::header::CONTENT_TYPE;
use hyper::{Method, Request};
use norn_config::machine::tokens::WatchedTokens;
use norn_config::{ChannelLock, ConfigDirs};
use norn_wire::{DrainReply, DrainRequest, HelloChannel, HelloFailure, HelloLaunch, HelloState};
use tokio::net::TcpSocket;

use crate::Launch;
use crate::bounds::{
    DIAL_BOUND, DRAIN_DIAL_BOUND, LOCK_TRY_INTERVAL, POLL_INTERVAL, RELEASE_BOUND, TAKEOVER_CAP,
};
use crate::dial::{Verified, dial};
use crate::lifecycle::REFUSED;
use crate::log::log;

/// A launched start's status: a verified host serves this channel.
pub(crate) const HOST_SERVES: u8 = 10;

/// A launched start's status: the channel lock is held and no verified host
/// answers at this endpoint.
pub(crate) const CHANNEL_HELD: u8 = 11;

/// The drain request's path.
pub(crate) const DRAIN_PATH: &str = "/v1/control/drain";

/// A start that ends without holding the channel: its status and the one
/// line it logs.
#[derive(Debug)]
pub(crate) struct Ended {
    pub(crate) status: u8,
    pub(crate) line: String,
}

impl Ended {
    fn refused(line: String) -> Ended {
        Ended {
            status: REFUSED,
            line,
        }
    }
}

/// Who is starting, and where it dials.
pub(crate) struct Start<'a> {
    pub(crate) dirs: &'a ConfigDirs,
    pub(crate) endpoint: SocketAddr,
    pub(crate) channel: HelloChannel,
    pub(crate) launch: Launch,
}

/// The bounds the held-lock wait keeps: the channel's constants in every
/// host, and the same shape scaled down in this crate's cases, so the cap and
/// the resets are exercised in milliseconds rather than minutes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pace {
    pub(crate) release: Duration,
    pub(crate) cap: Duration,
    pub(crate) dial: Duration,
    pub(crate) drain_dial: Duration,
    pub(crate) poll: Duration,
    pub(crate) lock_try: Duration,
}

impl Pace {
    /// The channel's own bounds.
    pub(crate) const CHANNEL: Pace = Pace {
        release: RELEASE_BOUND,
        cap: TAKEOVER_CAP,
        dial: DIAL_BOUND,
        drain_dial: DRAIN_DIAL_BOUND,
        poll: POLL_INTERVAL,
        lock_try: LOCK_TRY_INTERVAL,
    };
}

/// Steps 2 to 4: the channel lock, waited for where it is held.
pub(crate) async fn take_channel(start: &Start<'_>) -> Result<ChannelLock, Ended> {
    match try_lock(start.dirs)? {
        Some(lock) => Ok(lock),
        None => {
            log(format_args!(
                "the channel lock in {} is held; waiting for it, and dialing {} for the host \
                 that holds it",
                start.dirs.config_dir().display(),
                start.endpoint
            ));
            wait_on_held_lock(start, Pace::CHANNEL).await
        }
    }
}

/// One try of `serve.lock`: held is `None`; a refusal ends the start.
fn try_lock(dirs: &ConfigDirs) -> Result<Option<ChannelLock>, Ended> {
    ChannelLock::try_take(dirs).map_err(|error| {
        Ended::refused(format!(
            "the channel lock {} could not be taken: {error}",
            dirs.config_dir().join("serve.lock").display()
        ))
    })
}

/// What a waiter observed, for the report it gives if the wait ends with the
/// lock still held. Each fact is something it saw, and nothing more.
#[derive(Debug, Default)]
pub(crate) struct Facts {
    /// A listener answered and failed verification.
    verification_failed: bool,
    /// The token file this start reads did not read, with why.
    token_file_unreadable: Option<String>,
    /// A listener said its token file is unreadable: an unverified answer.
    listener_said_unreadable: bool,
    /// Drain requests sent.
    drains_sent: u32,
    /// The last verified answer said `draining`.
    last_verified_draining: bool,
}

impl Facts {
    /// The report of step 4: the channel lock is held and nothing verified
    /// answers, with every fact the wait observed.
    pub(crate) fn report(&self, endpoint: SocketAddr, at_cap: bool) -> String {
        let mut report = format!(
            "the channel lock is held and nothing verified answers at {endpoint}, so a host may \
             be starting, stopping or serving another endpoint; deleting serve.lock does not \
             stop that host and is not a fix"
        );
        if at_cap {
            report.push_str("; the wait ended at the takeover cap");
        }
        if self.verification_failed {
            report.push_str(
                "; a listener at the endpoint failed host verification: the credential may have \
                 changed since it was read, or something other than a norn host holds the port",
            );
        }
        if let Some(error) = &self.token_file_unreadable {
            report.push_str(&format!(
                "; this start could not read the token file: {error}"
            ));
        }
        if self.listener_said_unreadable {
            report.push_str(
                "; a listener at the endpoint said its token file is unreadable (an unverified \
                 answer)",
            );
        }
        if self.drains_sent > 0 {
            report.push_str(&format!(
                "; {} drain request{} sent, and the channel lock was still held when the wait \
                 ended, by a holder this start cannot name",
                self.drains_sent,
                if self.drains_sent == 1 {
                    " was"
                } else {
                    "s were"
                }
            ));
        }
        if self.last_verified_draining {
            report.push_str("; a verified host was draining when the wait ended");
        }
        report
    }
}

/// What one poll's dial found.
enum Dialed {
    /// No verified answer, for the reason the facts keep.
    Nothing,
    Verified(Box<Verified>),
}

/// Step 3: wait on a held lock until it comes free, a verified host decides
/// the start, or the deadline or the cap passes.
async fn wait_on_held_lock(start: &Start<'_>, pace: Pace) -> Result<ChannelLock, Ended> {
    let began = Instant::now();
    let cap = began + pace.cap;
    let mut deadline = began + pace.release;
    let mut tokens = WatchedTokens::read(start.dirs);
    let mut facts = Facts::default();
    // A drain this start sent that may still be outstanding.
    let mut outstanding = false;
    loop {
        let now = Instant::now();
        if now >= deadline || now >= cap {
            return Err(Ended {
                status: match start.launch {
                    Launch::Auto => CHANNEL_HELD,
                    Launch::Explicit => REFUSED,
                },
                line: facts.report(start.endpoint, now >= cap),
            });
        }
        // 3.1
        if let Some(lock) = try_lock(start.dirs)? {
            return Ok(lock);
        }
        // 3.2
        if let Dialed::Verified(mut verified) =
            dial_once(start, pace, &mut tokens, &mut facts).await
        {
            outstanding = false;
            let answer = verified.answer().facts();
            if answer.state() == HelloState::Draining {
                // 3.3
                facts.last_verified_draining = true;
                deadline = (Instant::now() + pace.release).min(cap);
            } else {
                // 3.4
                facts.last_verified_draining = false;
                let launch = answer.launch();
                if start.launch == Launch::Auto {
                    return Err(Ended {
                        status: HOST_SERVES,
                        line: "a verified norn host already serves this channel".into(),
                    });
                }
                if launch == HelloLaunch::Explicit {
                    return Err(host_serves());
                }
                if Instant::now() + pace.drain_dial + pace.release > cap {
                    return Err(Ended::refused(format!(
                        "a verified auto-launched norn host serves this channel, and the \
                         takeover ended at its cap after {} drain request{}",
                        facts.drains_sent,
                        if facts.drains_sent == 1 { "" } else { "s" }
                    )));
                }
                facts.drains_sent += 1;
                match drain(&mut verified, pace).await {
                    DrainReply::Draining | DrainReply::AlreadyDraining => {
                        if facts.drains_sent == 1 {
                            log(format_args!(
                                "a verified auto-launched norn host serves this channel; it was \
                                 asked to drain, and this start takes over once it has"
                            ));
                        }
                        outstanding = true;
                        deadline = (Instant::now() + pace.release).min(cap);
                    }
                    DrainReply::Refused => return Err(host_serves()),
                    DrainReply::Unrecognized => {}
                }
            }
        }
        // The poll interval, with the 10 ms tries while a drain is out.
        let rest = Instant::now() + pace.poll;
        if outstanding {
            while Instant::now() < rest {
                tokio::time::sleep(pace.lock_try).await;
                if let Some(lock) = try_lock(start.dirs)? {
                    return Ok(lock);
                }
            }
        } else {
            tokio::time::sleep_until(tokio::time::Instant::from_std(rest)).await;
        }
    }
}

fn host_serves() -> Ended {
    Ended::refused(
        "a norn host already serves this channel; stop that host before starting another".into(),
    )
}

/// One dial and hello within the dial bound, with the token set brought up
/// to date first; every way of getting no verified answer is kept in `facts`.
async fn dial_once(
    start: &Start<'_>,
    pace: Pace,
    tokens: &mut WatchedTokens,
    facts: &mut Facts,
) -> Dialed {
    tokens.check();
    let tokens = match tokens.current() {
        Ok(tokens) => tokens,
        Err(error) => {
            facts.token_file_unreadable = Some(error.to_string());
            return Dialed::Nothing;
        }
    };
    facts.token_file_unreadable = None;
    match tokio::time::timeout(pace.dial, dial(start.endpoint, start.channel, tokens)).await {
        Ok(Ok(verified)) => Dialed::Verified(Box::new(verified)),
        Ok(Err(failure)) => {
            match failure {
                HelloFailure::Verification { .. } | HelloFailure::HelloRefused { .. } => {
                    facts.verification_failed = true;
                }
                HelloFailure::ListenerSaysTokenFileUnreadable { .. } => {
                    facts.listener_said_unreadable = true;
                }
                _ => {}
            }
            Dialed::Nothing
        }
        Err(_) => Dialed::Nothing,
    }
}

/// The drain request on a verified connection, within the drain's own dial
/// bound. No answer in time is read as draining: a drain that took effect may
/// answer late.
async fn drain(verified: &mut Verified, pace: Pace) -> DrainReply {
    let body = serde_json::to_vec(&DrainRequest::new()).expect("a drain request serializes");
    let request = Request::builder()
        .method(Method::POST)
        .uri(DRAIN_PATH)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .expect("the drain request is well formed");
    match tokio::time::timeout(pace.drain_dial, verified.send(request)).await {
        Ok(Ok(reply)) => DrainReply::read(reply.status().as_u16(), reply.body()),
        // A closed connection or no answer in time: the drain may have
        // taken effect.
        Ok(Err(_)) | Err(_) => DrainReply::Draining,
    }
}

/// **Step 5.3, the port check.** Bind a probe socket to the wildcard address
/// of the endpoint's family at its port, **with** `SO_REUSEADDR` and without
/// `SO_REUSEPORT`, and close it. `SO_REUSEADDR` lets the probe pass sockets in
/// TIME_WAIT — this channel's previous host's, a dialer's — and never a
/// listener, on the wildcard or on loopback: a listener at `127.0.0.1` holds
/// the wildcard's port on Linux, and a wildcard listener beside which BSD
/// rules would let a loopback bind succeed holds it everywhere.
///
/// **The IPv4 wildcard alone.** The endpoint is an IPv4 loopback address, and
/// every dialer of it dials that address. A listener bound only to the IPv6
/// wildcard (`[::]` with `IPV6_V6ONLY`) can receive none of those connections,
/// so it neither intercepts a dialer nor contends for the endpoint, and a
/// start that refused over it would refuse over a socket no client reaches —
/// as no mainstream server refuses its IPv4 port over another program's IPv6
/// one. A dual-stack `[::]` listener does receive IPv4 connections, and the
/// IPv4 wildcard bind meets it already.
pub(crate) fn probe(endpoint: SocketAddr) -> std::io::Result<()> {
    let (socket, wildcard) = match endpoint {
        SocketAddr::V4(_) => (TcpSocket::new_v4()?, Ipv4Addr::UNSPECIFIED.into()),
        SocketAddr::V6(_) => (TcpSocket::new_v6()?, Ipv6Addr::UNSPECIFIED.into()),
    };
    socket.set_reuseaddr(true)?;
    socket.bind(SocketAddr::new(wildcard, endpoint.port()))
}

/// Step 5.3's dial once a process holds the port: whether a verified host of
/// this channel answers there — one whose lock file was removed.
pub(crate) async fn verified_host_at(
    endpoint: SocketAddr,
    channel: HelloChannel,
    tokens: &WatchedTokens,
) -> bool {
    let Ok(tokens) = tokens.current() else {
        return false;
    };
    matches!(
        tokio::time::timeout(DIAL_BOUND, dial(endpoint, channel, tokens)).await,
        Ok(Ok(_))
    )
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    fn endpoint_of(listener: &TcpListener) -> SocketAddr {
        SocketAddr::new(
            Ipv4Addr::LOCALHOST.into(),
            listener.local_addr().unwrap().port(),
        )
    }

    /// Whether the start's step 5.3 probe, or failing it the step 5.4 bind on
    /// the exact address, refuses `endpoint` as a port another process holds.
    /// The bind needs a reactor to listen on, so it runs inside one.
    fn refused_at_probe_or_bind(endpoint: SocketAddr) -> bool {
        match probe(endpoint) {
            Err(error) => error.kind() == std::io::ErrorKind::AddrInUse,
            Ok(()) => {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .build()
                    .unwrap();
                let _entered = runtime.enter();
                matches!(crate::bind(endpoint), Err(error) if error.kind() == std::io::ErrorKind::AddrInUse)
            }
        }
    }

    /// A listener on loopback, and one on the wildcard, hold the port; a free
    /// port passes, and the probe holds nothing after it. Linux refuses a
    /// loopback listener at the wildcard probe itself. BSD (macOS) lets a
    /// wildcard bind pass beside a listener on one specific address, so there
    /// the loopback holder is refused by the exact-address bind that follows
    /// the probe, which is why the start binds with `SO_REUSEADDR` and never
    /// `SO_REUSEPORT`: the two steps together refuse it on every platform.
    #[test]
    fn the_probe_refuses_a_listener_on_loopback_or_the_wildcard() {
        let loopback = TcpListener::bind("127.0.0.1:0").unwrap();
        #[cfg(target_os = "linux")]
        {
            let error = probe(endpoint_of(&loopback)).expect_err("a loopback listener passed");
            assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
        }
        assert!(
            refused_at_probe_or_bind(endpoint_of(&loopback)),
            "a loopback listener passed both the probe and the bind"
        );

        let wildcard = TcpListener::bind("0.0.0.0:0").unwrap();
        let error = probe(endpoint_of(&wildcard)).expect_err("a wildcard listener passed");
        assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);

        let free = endpoint_of(&loopback);
        drop(loopback);
        probe(free).expect("a free port");
        probe(free).expect("the probe holds nothing after it");
    }

    /// **A socket in TIME_WAIT on the port passes the probe**: a connection
    /// whose server end closed first, as a stopping host's does, leaves the
    /// server's end in TIME_WAIT on the endpoint's port.
    #[test]
    fn the_probe_passes_a_socket_in_time_wait() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = endpoint_of(&listener);
        let client = std::net::TcpStream::connect(endpoint).unwrap();
        let (server, _) = listener.accept().unwrap();
        drop(listener);
        // The server's end closes first, so its socket enters TIME_WAIT once
        // the client's FIN arrives.
        drop(server);
        let mut client = client;
        let mut byte = [0u8; 1];
        use std::io::Read;
        assert_eq!(client.read(&mut byte).unwrap(), 0, "the server's FIN");
        drop(client);
        // The case holds a socket in TIME_WAIT: a wildcard bind without
        // `SO_REUSEADDR` is refused over it.
        let plain = TcpSocket::new_v4().unwrap();
        let error = plain
            .bind(SocketAddr::new(
                Ipv4Addr::UNSPECIFIED.into(),
                endpoint.port(),
            ))
            .expect_err("no socket in TIME_WAIT");
        assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
        drop(plain);
        probe(endpoint).expect("a socket in TIME_WAIT refused the probe");
    }

    /// The report says what was observed and nothing more.
    #[test]
    fn the_facts_report_names_only_what_was_observed() {
        let endpoint: SocketAddr = "127.0.0.1:28472".parse().unwrap();
        let bare = Facts::default().report(endpoint, false);
        assert!(bare.contains("the channel lock is held and nothing verified answers"));
        assert!(bare.contains("deleting serve.lock does not stop that host"));
        for absent in [
            "verification",
            "token file",
            "drain request",
            "draining",
            "cap",
        ] {
            assert!(!bare.contains(absent), "{absent}: {bare}");
        }
        let facts = Facts {
            verification_failed: true,
            token_file_unreadable: Some("tokens.toml: not a regular file".into()),
            listener_said_unreadable: true,
            drains_sent: 2,
            last_verified_draining: true,
        };
        let full = facts.report(endpoint, true);
        for present in [
            "takeover cap",
            "failed host verification",
            "credential may have changed",
            "something other than a norn host",
            "could not read the token file: tokens.toml: not a regular file",
            "said its token file is unreadable (an unverified answer)",
            "2 drain requests were sent",
            "a holder this start cannot name",
            "a verified host was draining",
        ] {
            assert!(full.contains(present), "{present}: {full}");
        }
    }

    // ---- The held-lock wait, at a small scale ----

    use std::sync::{Arc, Mutex};

    use norn_config::machine::tokens::{self, TokenLabel};
    use norn_testkit::scratch::Scratch;

    use crate::drain::Drain;
    use crate::http;
    use crate::lifecycle::serving_for_tests;

    /// The channel's bounds scaled down a hundredfold, their proportions
    /// kept where the wait's decisions turn on them.
    const SMALL: Pace = Pace {
        release: Duration::from_millis(400),
        cap: Duration::from_millis(800),
        dial: Duration::from_millis(100),
        drain_dial: Duration::from_millis(100),
        poll: Duration::from_millis(20),
        lock_try: Duration::from_millis(2),
    };

    /// A config directory holding one `default` token.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the base the token file lives under.
    fn channel_dirs(scratch: &Scratch, secret: &[u8]) -> ConfigDirs {
        std::fs::create_dir_all(scratch.join("config")).expect("a config base");
        let dirs = ConfigDirs::new(scratch.join("config"), scratch.join("data")).expect("bases");
        tokens::mutate(&dirs, |tokens| {
            tokens.add(&TokenLabel::default_label(), secret)
        })
        .expect("a token");
        dirs
    }

    /// How a stand-in host answers each connection.
    #[derive(Clone, Copy, PartialEq)]
    enum Holder {
        /// One host for every connection, which drains for real: its stage
        /// moves to `draining` and it answers a later hello so.
        One,
        /// A fresh host for each connection, as though each drained host were
        /// replaced by a launched one that took the freed lock.
        FreshEachConnection,
    }

    /// A stand-in host started as `launch` on a loopback port, answering the
    /// hello and the drain request from `dirs`'s token file. Each drain it
    /// begins calls `on_drain` once its listener would close.
    fn stand_in(
        dirs: &ConfigDirs,
        launch: HelloLaunch,
        holder: Holder,
        on_drain: impl Fn() + Send + Sync + 'static,
    ) -> SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let dirs = dirs.clone();
        let on_drain = Arc::new(on_drain);
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let fresh = || {
                    let lifecycle = serving_for_tests(
                        Arc::new(Drain::new(Duration::from_secs(1))),
                        Arc::default(),
                    );
                    let context = Arc::new(http::context_for_tests(
                        &dirs,
                        launch,
                        Arc::clone(&lifecycle),
                    ));
                    let on_drain = Arc::clone(&on_drain);
                    let closing = Arc::clone(&lifecycle);
                    tokio::spawn(async move {
                        closing.stopping().await;
                        closing.listener_closed();
                        on_drain();
                    });
                    context
                };
                let mut shared: Option<Arc<http::Context>> = None;
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    let context = match holder {
                        Holder::FreshEachConnection => fresh(),
                        Holder::One => Arc::clone(shared.get_or_insert_with(fresh)),
                    };
                    tokio::spawn(http::answer(stream, context));
                }
            });
        });
        address
    }

    fn wait(
        dirs: &ConfigDirs,
        endpoint: SocketAddr,
        launch: Launch,
    ) -> (Result<ChannelLock, Ended>, Duration) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let start = Start {
            dirs,
            endpoint,
            channel: HelloChannel::Dev,
            launch,
        };
        let began = Instant::now();
        let outcome = runtime.block_on(wait_on_held_lock(&start, SMALL));
        (outcome, began.elapsed())
    }

    fn ended(outcome: Result<ChannelLock, Ended>) -> Ended {
        match outcome {
            Ok(_) => panic!("the wait took the lock"),
            Err(ended) => ended,
        }
    }

    /// A port nothing listens on.
    fn nothing_listens() -> SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    }

    /// **An explicit start meeting a verified explicit host exits 1 at
    /// once**; an explicit host is never displaced.
    #[test]
    fn an_explicit_start_meeting_an_explicit_host_exits_1_at_once() {
        let scratch = Scratch::new("norn-serve-wait-explicit");
        let dirs = channel_dirs(&scratch, b"the secret");
        let _held = ChannelLock::try_take(&dirs).unwrap().unwrap();
        let drained = Arc::new(Mutex::new(0));
        let counted = Arc::clone(&drained);
        let host = stand_in(&dirs, HelloLaunch::Explicit, Holder::One, move || {
            *counted.lock().unwrap() += 1;
        });
        let (outcome, took) = wait(&dirs, host, Launch::Explicit);
        let ended = ended(outcome);
        assert_eq!(ended.status, REFUSED);
        assert!(
            ended.line.contains("already serves this channel"),
            "{}",
            ended.line
        );
        assert!(took < SMALL.release, "{took:?}");
        assert_eq!(
            *drained.lock().unwrap(),
            0,
            "a drain was sent to an explicit host"
        );
    }

    /// **A launched start that meets a verified serving host exits 10 at
    /// once**, whatever launched that host, and sends no drain.
    #[test]
    fn a_launched_start_meeting_a_verified_host_exits_10_at_once() {
        for launch in [HelloLaunch::Explicit, HelloLaunch::Auto] {
            let scratch = Scratch::new("norn-serve-wait-launched");
            let dirs = channel_dirs(&scratch, b"the secret");
            let _held = ChannelLock::try_take(&dirs).unwrap().unwrap();
            let host = stand_in(&dirs, launch, Holder::One, || {});
            let (outcome, took) = wait(&dirs, host, Launch::Auto);
            let ended = ended(outcome);
            assert_eq!(ended.status, HOST_SERVES);
            assert!(took < SMALL.release, "{took:?}");
        }
    }

    /// **A held lock with nothing verified answering**: past the release
    /// bound a launched start exits 11 and an explicit one 1, with the facts.
    #[test]
    fn a_held_lock_with_no_verified_answer_ends_at_the_release_bound_with_the_facts() {
        for (launch, status) in [(Launch::Auto, CHANNEL_HELD), (Launch::Explicit, REFUSED)] {
            let scratch = Scratch::new("norn-serve-wait-nothing");
            let dirs = channel_dirs(&scratch, b"the secret");
            let _held = ChannelLock::try_take(&dirs).unwrap().unwrap();
            let (outcome, took) = wait(&dirs, nothing_listens(), launch);
            let ended = ended(outcome);
            assert_eq!(ended.status, status);
            assert!(took >= SMALL.release && took < SMALL.cap, "{took:?}");
            assert!(
                ended.line.contains("nothing verified answers"),
                "{}",
                ended.line
            );
            assert!(!ended.line.contains("verification"), "{}", ended.line);
        }
    }

    /// A listener that fails verification is no verified answer, and the
    /// facts name both possible causes.
    #[test]
    fn a_listener_that_fails_verification_is_named_in_the_facts() {
        let scratch = Scratch::new("norn-serve-wait-unverified");
        let dirs = channel_dirs(&scratch, b"the secret");
        let other = Scratch::new("norn-serve-wait-unverified-other");
        let other_dirs = channel_dirs(&other, b"another channel's secret");
        let _held = ChannelLock::try_take(&dirs).unwrap().unwrap();
        let impostor = stand_in(&other_dirs, HelloLaunch::Auto, Holder::One, || {});
        let ended = ended(wait(&dirs, impostor, Launch::Explicit).0);
        assert_eq!(ended.status, REFUSED);
        assert!(
            ended.line.contains("failed host verification"),
            "{}",
            ended.line
        );
        assert!(!ended.line.contains("drain request"), "{}", ended.line);
    }

    /// **An explicit start takes over a verified auto-launched host**: it
    /// sends the drain, keeps polling, and takes the lock the drained host
    /// frees.
    #[test]
    fn an_explicit_start_drains_an_auto_host_and_takes_the_lock_it_frees() {
        let scratch = Scratch::new("norn-serve-wait-takeover");
        let dirs = channel_dirs(&scratch, b"the secret");
        let held = Arc::new(Mutex::new(Some(
            ChannelLock::try_take(&dirs).unwrap().unwrap(),
        )));
        let freed = Arc::clone(&held);
        let host = stand_in(&dirs, HelloLaunch::Auto, Holder::One, move || {
            drop(freed.lock().unwrap().take());
        });
        let (outcome, took) = wait(&dirs, host, Launch::Explicit);
        assert!(outcome.is_ok(), "{:?}", outcome.err());
        assert!(took < SMALL.release, "{took:?}");
        assert!(
            held.lock().unwrap().is_none(),
            "the lock was taken before the drain"
        );
    }

    /// **The takeover cap ends the wait with a true report.** Each drained
    /// host is replaced by another launched one that holds the lock, so the
    /// explicit start drains each in turn, sends no drain it could not wait
    /// out, and ends at its cap saying how many drains it sent.
    #[test]
    fn the_takeover_cap_ends_a_wait_that_keeps_losing_the_freed_lock() {
        let scratch = Scratch::new("norn-serve-wait-cap");
        let dirs = channel_dirs(&scratch, b"the secret");
        let _held = ChannelLock::try_take(&dirs).unwrap().unwrap();
        let drains = Arc::new(Mutex::new(Vec::new()));
        let counted = Arc::clone(&drains);
        let host = stand_in(
            &dirs,
            HelloLaunch::Auto,
            Holder::FreshEachConnection,
            move || {
                counted.lock().unwrap().push(Instant::now());
            },
        );
        let began = Instant::now();
        let (outcome, took) = wait(&dirs, host, Launch::Explicit);
        let ended = ended(outcome);
        assert_eq!(ended.status, REFUSED);
        assert!(took <= SMALL.cap, "the wait ran past its cap: {took:?}");
        let drains = drains.lock().unwrap();
        assert!(!drains.is_empty());
        assert!(
            ended.line.contains(&format!(
                "takeover ended at its cap after {} drain request",
                drains.len()
            )),
            "{}",
            ended.line
        );
        for sent in drains.iter() {
            assert!(
                *sent + SMALL.release <= began + SMALL.cap,
                "a drain was sent that the cap could not wait out"
            );
        }
    }

    /// A verified `draining` answer resets the deadline, never past the cap:
    /// a host that keeps answering `draining` holds the wait to the cap, and
    /// the facts say a verified host was draining.
    #[test]
    fn a_verified_draining_host_resets_the_deadline_up_to_the_cap() {
        let scratch = Scratch::new("norn-serve-wait-draining");
        let dirs = channel_dirs(&scratch, b"the secret");
        let _held = ChannelLock::try_take(&dirs).unwrap().unwrap();
        let host = stand_in(&dirs, HelloLaunch::Auto, Holder::One, || {});
        let (outcome, took) = wait(&dirs, host, Launch::Explicit);
        let ended = ended(outcome);
        assert_eq!(ended.status, REFUSED);
        assert!(took >= SMALL.cap, "the wait ended before its cap: {took:?}");
        for fact in [
            "takeover cap",
            "1 drain request was sent",
            "a verified host was draining",
        ] {
            assert!(ended.line.contains(fact), "{fact}: {}", ended.line);
        }
    }
}
