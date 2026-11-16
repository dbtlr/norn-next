//! The HTTP/1.1 connection handler: the hello, the bearer middleware in front
//! of every other path, and the routes behind it.
//!
//! `POST /v1/hello` is the one exchange answered without a bearer. Every
//! other request passes the bearer middleware first. Behind it stand the wire
//! route, `POST /v1/wire/{verb}` (`wire.rs`, run by `request.rs`), and under
//! the `counters` feature `GET /v1/debug/counters`; any other path is 404
//! `serve/unknown-verb`.
//!
//! **Every refusal's status is its fault class's**, through [`status_of`], an
//! exhaustive match over `norn_wire::FaultClass`; every refusal is an
//! `ErrorEnvelope` with `Content-Type: application/json`, and a refusal made
//! before any verb runs carries nothing beyond its code, so a caller nobody
//! has authenticated learns nothing about the stored tokens.
//!
//! **Each connection's reads are paced** (`connection.rs`): its first
//! request's head must arrive within 5 s, an idle connection is closed after
//! 30 s, and a request body must arrive within 30 s of its head — the hello's
//! included — or the request is answered 400 `serve/bad-request` and the
//! connection closed. A wire body over 256 MiB is refused 413
//! `serve/request-too-large` before it is read whole.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::{Body, Bytes};
use hyper::header::{AUTHORIZATION, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use norn_wire::{
    BuildIdentity, ErrorDetail, ErrorEnvelope, FaultClass, HELLO_BODY_BYTES, HelloAnswer,
    HelloChannel, HelloFacts, HelloLaunch, HelloRequest, HelloState, Verb,
};
use tokio::net::TcpStream;

use crate::connection::{Bounds, Paced};
use crate::credentials::{Admission, Credentials, Keyed};
use crate::request::{Dispatched, Ended, Lane, Wire};
use crate::stop::Stopper;
use crate::wire;

/// The path of the one exchange admitted without a bearer.
const HELLO_PATH: &str = "/v1/hello";

/// The wire route's path, before the verb's name.
const WIRE_PREFIX: &str = "/v1/wire/";

/// The largest wire request body admitted: a declared limit set well above
/// any realistic plan, since plans carry document content. The largest plan
/// the realistic fixture profile produces, creating every document of its
/// 10,000-document scale, encodes to 50,181,398 bytes, about 19% of it.
pub(crate) const WIRE_BODY_BYTES: usize = 256 * 1024 * 1024;

/// The header that carries, under the `counters` feature, the microseconds
/// from a wire request's first dispatch to its answer being ready.
#[cfg(feature = "counters")]
pub(crate) const HOST_TIME_HEADER: &str = "x-norn-host-time-us";

/// What every connection of one host answers from.
pub(crate) struct Context {
    pub(crate) credentials: Mutex<Credentials>,
    pub(crate) channel: HelloChannel,
    pub(crate) build: BuildIdentity,
    pub(crate) launch: HelloLaunch,
    pub(crate) wire: Arc<Wire>,
    pub(crate) stopper: Arc<Stopper>,
    pub(crate) bounds: Bounds,
    /// The host's own cumulative readings, named as the counters route
    /// answers them.
    #[cfg(feature = "counters")]
    pub(crate) readings: Box<dyn Fn() -> Vec<(String, u64)> + Send + Sync>,
}

impl Context {
    /// The credential state. Held only between awaits, never across one, so
    /// on the current-thread runtime the lock is never contended; a panic
    /// that poisoned it left the state as consistent as any request does.
    fn credentials(&self) -> MutexGuard<'_, Credentials> {
        self.credentials
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

pub(crate) type Answer = Response<Full<Bytes>>;

/// Serve one accepted connection until its client closes it or one of its
/// bounds passes.
pub(crate) async fn answer(stream: TcpStream, context: Arc<Context>) {
    // Both ends as this connection's socket reports them: the hello states
    // them. A socket that cannot report them is closed unanswered.
    let (Ok(local), Ok(peer)) = (stream.local_addr(), stream.peer_addr()) else {
        return;
    };
    let (paced, phase) = Paced::new(stream, context.bounds);
    let service = service_fn(move |request| {
        let context = Arc::clone(&context);
        let phase = Arc::clone(&phase);
        async move {
            phase.busy();
            let answer = route(request, &context, local, peer).await;
            phase.idle();
            Ok::<_, Infallible>(answer)
        }
    });
    // A connection that fails mid-exchange is the client's to retry; a
    // request whose outcome it could leave unknown is answered before its
    // connection is read again.
    let _ = http1::Builder::new()
        .serve_connection(TokioIo::new(paced), service)
        .await;
}

/// One request's answer. Generic over the body so the routes are tested
/// with bodies of the suite's own making.
pub(crate) async fn route<B>(
    request: Request<B>,
    context: &Context,
    local: SocketAddr,
    peer: SocketAddr,
) -> Answer
where
    B: Body<Data = Bytes>,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    if request.method() == Method::POST && request.uri().path() == HELLO_PATH {
        return hello(request, context, local, peer).await;
    }
    let admission = {
        let mut credentials = context.credentials();
        credentials.check();
        credentials.admit(
            request
                .headers()
                .get_all(AUTHORIZATION)
                .iter()
                .map(HeaderValue::as_bytes),
        )
    };
    match admission {
        Admission::Admitted => authenticated(request, context).await,
        Admission::Unauthorized => refusal(
            "the request carried no bearer this host accepts",
            ErrorDetail::unauthorized(),
        ),
        Admission::FailedClosed => token_file_unreadable(),
    }
}

/// What stands behind the bearer middleware.
async fn authenticated<B>(request: Request<B>, context: &Context) -> Answer
where
    B: Body<Data = Bytes>,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    #[cfg(feature = "counters")]
    if request.method() == Method::GET && request.uri().path() == "/v1/debug/counters" {
        return counters(context);
    }
    if request.method() == Method::POST
        && let Some(name) = request.uri().path().strip_prefix(WIRE_PREFIX)
        && let Ok(verb) = Verb::try_from(name)
        && wire::lane(verb).is_some()
    {
        let answer = wire_request(verb, request, context).await;
        context.wire.counters.body_bytes_encoded.fetch_add(
            answer.body().size_hint().exact().unwrap_or(0),
            Ordering::Relaxed,
        );
        return answer;
    }
    // The connection stays open: a request that carried no body, the common
    // case, leaves nothing unread, and hyper does not reuse one whose body
    // went unread.
    refusal(
        "this host serves no such path: the wire route is POST /v1/wire/{verb}, for a verb \
         this host serves",
        ErrorDetail::unknown_verb(),
    )
}

/// `POST /v1/wire/{verb}` for a verb this host serves.
async fn wire_request<B>(verb: Verb, request: Request<B>, context: &Context) -> Answer
where
    B: Body<Data = Bytes>,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let wire_state = &context.wire;
    if wire_state.admission.draining() {
        return closing(draining());
    }
    if declared_length(&request).is_some_and(|length| length > WIRE_BODY_BYTES as u64) {
        return closing(too_large("the request's body is over 256 MiB"));
    }
    let Some(slot) = wire_state.admission.enter() else {
        // The body is not read, so the connection cannot carry another
        // request after this answer.
        return closing(refusal(
            "this host has as many requests in flight as it admits; send the request again",
            ErrorDetail::busy(),
        ));
    };
    let body = match arriving_body(request, WIRE_BODY_BYTES, context.bounds).await {
        Ok(body) => body,
        Err(Unread::TooLarge) => return closing(too_large("the request's body is over 256 MiB")),
        Err(Unread::Late) => return closing(late_body()),
        Err(Unread::Failed) => {
            return closing(bad_request("the request's body did not arrive whole"));
        }
    };
    let lane = match wire::lane(verb) {
        Some(wire::Runs::Mutation) => match wire::mutation_vault(verb, &body) {
            Ok(vault) => Lane::Mutation(vault),
            Err(error) => return unparsed(verb, &error),
        },
        _ => Lane::Read,
    };
    // The body crosses to the pool as it arrived, uncopied; the slot goes
    // with it, held until the request's last job lets go.
    let run = wire_state.serve(verb, lane, body, slot).await;
    #[cfg(feature = "counters")]
    let host_time = run.dispatched_at.elapsed();
    let answer = match run.ended {
        Ended::Dispatched(Dispatched::Report(report)) => {
            wire_state
                .counters
                .report_bytes
                .fetch_add(report.len() as u64, Ordering::Relaxed);
            json(StatusCode::OK, report)
        }
        Ended::Dispatched(Dispatched::Refused(envelope)) | Ended::StillWarming(envelope) => {
            envelope_answer(&envelope)
        }
        Ended::Dispatched(Dispatched::BadRequest(error)) => unparsed(verb, &error),
        Ended::Dispatched(Dispatched::HostStopped(detail)) => {
            context.stopper.worker_stopped(&detail);
            outcome_unknown()
        }
        Ended::Draining => draining(),
        Ended::Busy => refusal(
            "this request waited past its deadline to be taken up, and nothing ran; send it again",
            ErrorDetail::busy(),
        ),
        Ended::Lost => outcome_unknown(),
    };
    #[cfg(feature = "counters")]
    let answer = {
        let mut answer = answer;
        answer.headers_mut().insert(
            HOST_TIME_HEADER,
            HeaderValue::from(u64::try_from(host_time.as_micros()).unwrap_or(u64::MAX)),
        );
        answer
    };
    answer
}

/// The HTTP status a fault class answers with. The match carries no
/// wildcard, so a class added to the vocabulary does not compile here until
/// it has a status.
pub(crate) fn status_of(class: FaultClass) -> StatusCode {
    match class {
        FaultClass::Request => StatusCode::BAD_REQUEST,
        FaultClass::Unauthorized => StatusCode::UNAUTHORIZED,
        FaultClass::Missing => StatusCode::NOT_FOUND,
        FaultClass::Conflict => StatusCode::CONFLICT,
        FaultClass::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        FaultClass::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        FaultClass::Operator | FaultClass::Defect => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// A body that did not parse as `verb`'s params.
fn unparsed(verb: Verb, error: &str) -> Answer {
    bad_request(&format!(
        "the request's body is not JSON of `{verb}`'s params: {error}; nothing was dispatched"
    ))
}

fn bad_request(message: &str) -> Answer {
    refusal(message, ErrorDetail::bad_request())
}

fn late_body() -> Answer {
    bad_request("the request's body did not arrive in time, and nothing was dispatched")
}

fn too_large(message: &str) -> Answer {
    refusal(message, ErrorDetail::request_too_large())
}

fn draining() -> Answer {
    refusal(
        "this host is stopping and refused the request before running it; send it again once a \
         host serves",
        ErrorDetail::draining(),
    )
}

fn outcome_unknown() -> Answer {
    refusal(
        "the host could no longer answer this request while it was running, so whether it took \
         effect is not known; read the state back, or send the request again",
        ErrorDetail::outcome_unknown(),
    )
}

/// An answer after which the connection carries no further request: the
/// body was not read, or did not arrive in time.
fn closing(mut answer: Answer) -> Answer {
    answer
        .headers_mut()
        .insert(CONNECTION, HeaderValue::from_static("close"));
    answer
}

/// The `serve.*` counters and the host's readings, as one JSON object of name
/// to integer.
#[cfg(feature = "counters")]
fn counters(context: &Context) -> Answer {
    let tokens = context.credentials().counters();
    let wire = &context.wire.counters;
    let mut body = serde_json::Map::new();
    for (name, value) in [
        ("serve.token_fingerprints", tokens.fingerprints),
        ("serve.token_file_reads", tokens.file_reads),
        ("serve.token_lookups", tokens.lookups),
        ("serve.dispatches", wire.dispatches.load(Ordering::Relaxed)),
        (
            "serve.ready_retries",
            wire.ready_retries.load(Ordering::Relaxed),
        ),
        (
            "serve.connections_visited",
            wire.connections_visited.load(Ordering::Relaxed),
        ),
        (
            "serve.body_bytes_encoded",
            wire.body_bytes_encoded.load(Ordering::Relaxed),
        ),
        (
            "serve.report_bytes",
            wire.report_bytes.load(Ordering::Relaxed),
        ),
    ] {
        body.insert(name.to_string(), value.into());
    }
    for (name, value) in (context.readings)() {
        body.insert(name, value.into());
    }
    json(
        StatusCode::OK,
        serde_json::Value::Object(body).to_string().into_bytes(),
    )
}

/// `POST /v1/hello`: one check of the token set, then the body read under
/// its cap and the body-arrival bound, then one lookup by label.
///
/// The check comes before the body, so the fail-closed state is answered to
/// every hello, whatever its body: a caller nobody has verified learns of a
/// malformed or oversized body only from a host that is serving. The body's
/// arrival is bounded as every route's is, so a hello that declares a body
/// and withholds it holds its connection no longer than any request.
async fn hello<B>(
    request: Request<B>,
    context: &Context,
    local: SocketAddr,
    peer: SocketAddr,
) -> Answer
where
    B: Body<Data = Bytes>,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let failed_closed = {
        let mut credentials = context.credentials();
        credentials.check();
        credentials.fails_closed()
    };
    if failed_closed {
        return token_file_unreadable();
    }
    if declared_length(&request).is_some_and(|length| length > HELLO_BODY_BYTES as u64) {
        return closing(too_large("the hello's body is over 4 KiB"));
    }
    let body = match arriving_body(request, HELLO_BODY_BYTES, context.bounds).await {
        Ok(body) => body,
        Err(Unread::TooLarge) => return closing(too_large("the hello's body is over 4 KiB")),
        Err(Unread::Late) => return closing(late_body()),
        Err(Unread::Failed) => return bad_hello(),
    };
    let Ok(hello) = serde_json::from_slice::<HelloRequest>(&body) else {
        return bad_hello();
    };
    let facts = HelloFacts::new(
        context.channel,
        context.build,
        context.launch,
        // Draining arrives with the bounded drain, a later task.
        HelloState::Serving,
        local,
        peer,
    );
    // No second check: the held set is the one this request's check left,
    // unless another connection's check replaced it while the body arrived,
    // and `key` answers whatever is held now, a fail-closed state included.
    let keyed = context.credentials().key(hello.label(), || {
        hello.nonce().map(|nonce| facts.mac_input(&nonce))
    });
    match keyed {
        Keyed::Mac(mac) => {
            let answer = HelloAnswer::new(facts, mac);
            json(
                StatusCode::OK,
                serde_json::to_vec(&answer).expect("a hello answer serializes"),
            )
        }
        Keyed::Refused => refusal("the hello was refused", ErrorDetail::hello_refused()),
        Keyed::FailedClosed => token_file_unreadable(),
    }
}

/// Why a body was not read.
enum Unread {
    /// Over its route's cap, by what arrived.
    TooLarge,
    /// It did not arrive within the body-arrival bound.
    Late,
    /// The body did not arrive whole.
    Failed,
}

fn declared_length<B>(request: &Request<B>) -> Option<u64> {
    request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
}

/// The whole body, read under `cap` and within the body-arrival bound.
async fn arriving_body<B>(request: Request<B>, cap: usize, bounds: Bounds) -> Result<Bytes, Unread>
where
    B: Body<Data = Bytes>,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let collected = tokio::time::timeout(
        bounds.body_arrival,
        Limited::new(request.into_body(), cap).collect(),
    )
    .await
    .map_err(|_| Unread::Late)?;
    match collected {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(error) if error.downcast_ref::<LengthLimitError>().is_some() => Err(Unread::TooLarge),
        Err(_) => Err(Unread::Failed),
    }
}

fn bad_hello() -> Answer {
    bad_request("the hello's body is not a JSON hello object")
}

fn token_file_unreadable() -> Answer {
    refusal(
        "this host's token file changed and no longer reads, so every request is refused until \
         it does; the host's log names the file and the fix",
        ErrorDetail::token_file_unreadable(),
    )
}

/// A refusal with `detail`, under its class's status.
fn refusal(message: &str, detail: ErrorDetail) -> Answer {
    envelope_answer(&ErrorEnvelope::new(message, detail))
}

fn envelope_answer(envelope: &ErrorEnvelope) -> Answer {
    json(
        status_of(envelope.fault_class()),
        serde_json::to_vec(envelope).expect("an error envelope serializes"),
    )
}

fn json(status: StatusCode, body: Vec<u8>) -> Answer {
    let mut response = Response::new(Full::new(Bytes::from(body)));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

#[cfg(test)]
mod tests {
    use norn_config::ConfigDirs;
    use norn_config::machine::tokens::{self, TokenLabel};
    use norn_testkit::scratch::Scratch;
    use norn_wire::{HelloExpectation, Nonce};

    use super::*;
    use crate::credentials::mac_verifies;
    use crate::credentials::tests::settled;

    /// The wire route's cases: statuses, admission, bounds, and the
    /// stopped-worker answer.
    mod wire;

    const SECRET: &[u8] = b"a stored secret";

    struct Case {
        _scratch: Scratch,
        dirs: ConfigDirs,
        context: Context,
        answers: Arc<Answers>,
    }

    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the bases a case's state lives under.
    fn case(name: &str) -> Case {
        case_waiting(name, std::time::Duration::from_secs(5))
    }

    /// A case whose wire requests wait at most `ready_wait`.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the bases a case's state lives under.
    fn case_waiting(name: &str, ready_wait: std::time::Duration) -> Case {
        let scratch = Scratch::new(name);
        std::fs::create_dir_all(scratch.join("config")).expect("a config base");
        let dirs = ConfigDirs::new(scratch.join("config"), scratch.join("data")).expect("bases");
        tokens::mutate(&dirs, |tokens| {
            tokens.add(&TokenLabel::default_label(), SECRET)
        })
        .expect("a token");
        let answers = Arc::new(Answers::default());
        let context = Context {
            credentials: Mutex::new(Credentials::new(&dirs, settled(&dirs))),
            channel: HelloChannel::Dev,
            build: BuildIdentity::new("0.0.0+test"),
            launch: HelloLaunch::Explicit,
            wire: Arc::new(Wire {
                admission: Arc::default(),
                semaphores: Arc::default(),
                reads: crate::pool::Pool::new("test-read", 2).unwrap(),
                mutations: crate::pool::Pool::new("test-mutation", 2).unwrap(),
                dispatcher: Arc::clone(&answers) as Arc<dyn crate::request::Dispatch>,
                ready_wait,
                counters: Arc::default(),
            }),
            stopper: Arc::default(),
            bounds: Bounds::SERVING,
            #[cfg(feature = "counters")]
            readings: Box::new(|| vec![("host.request.requests".to_string(), 7)]),
        };
        Case {
            _scratch: scratch,
            dirs,
            context,
            answers,
        }
    }

    /// A host that answers each wire dispatch with the next scripted answer,
    /// or an empty report where none is left.
    #[derive(Default)]
    struct Answers {
        next: Mutex<std::collections::VecDeque<Dispatched>>,
    }

    impl Answers {
        fn push(&self, answer: Dispatched) {
            self.next.lock().unwrap().push_back(answer);
        }
    }

    impl crate::request::Dispatch for Answers {
        fn dispatch(&self, _: Verb, _: &[u8]) -> Dispatched {
            self.next
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Dispatched::Report(b"{}".to_vec()))
        }
    }

    /// Damage the token file so it no longer reads. The size changes, so the
    /// next fingerprint differs even within one coarse timestamp tick.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: bytes the API never writes.
    fn fail_closed(case: &Case) {
        let file = case.dirs.config_dir().join("tokens.toml");
        std::fs::write(file, b"[tokens").unwrap();
    }

    const LOCAL: &str = "127.0.0.1:28472";
    const PEER: &str = "127.0.0.1:54121";

    fn send(case: &Case, request: Request<Full<Bytes>>) -> (StatusCode, Option<String>, Bytes) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let answer = runtime.block_on(route(
            request,
            &case.context,
            LOCAL.parse().unwrap(),
            PEER.parse().unwrap(),
        ));
        let status = answer.status();
        let kind = answer
            .headers()
            .get(CONTENT_TYPE)
            .map(|value| value.to_str().unwrap().to_string());
        let body = runtime
            .block_on(answer.into_body().collect())
            .unwrap()
            .to_bytes();
        (status, kind, body)
    }

    fn request(
        method: Method,
        path: &str,
        bearer: Option<&str>,
        body: &[u8],
    ) -> Request<Full<Bytes>> {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(bearer) = bearer {
            builder = builder.header(AUTHORIZATION, format!("Bearer {bearer}"));
        }
        builder
            .body(Full::new(Bytes::copy_from_slice(body)))
            .unwrap()
    }

    fn hello_body(label: &str, nonce: &str) -> Vec<u8> {
        serde_json::json!({ "label": label, "nonce": nonce })
            .to_string()
            .into_bytes()
    }

    fn code(body: &Bytes) -> String {
        let envelope: ErrorEnvelope = serde_json::from_slice(body).expect("an error envelope");
        serde_json::to_value(envelope.code())
            .expect("a code serializes")
            .as_str()
            .expect("a code is a string")
            .to_string()
    }

    fn nonce() -> Nonce {
        Nonce::from_bytes([0xab; 32])
    }

    #[test]
    fn a_hello_is_answered_with_the_mac_over_this_connection() {
        let case = case("norn-serve-hello-ok");
        let (status, kind, body) = send(
            &case,
            request(
                Method::POST,
                HELLO_PATH,
                None,
                &hello_body("default", &nonce().to_hex()),
            ),
        );
        assert_eq!(status, StatusCode::OK);
        assert_eq!(kind.as_deref(), Some("application/json"));
        let answer: HelloAnswer = serde_json::from_slice(&body).expect("an answer");
        let facts = answer.facts();
        assert!(mac_verifies(
            SECRET,
            &facts.mac_input(&nonce()),
            &answer.mac().unwrap()
        ));
        assert_eq!(
            facts.check_connection(&HelloExpectation {
                channel: HelloChannel::Dev,
                dialed: LOCAL.parse().unwrap(),
                local: PEER.parse().unwrap(),
            }),
            Ok(())
        );
        assert_eq!(facts.launch(), HelloLaunch::Explicit);
        assert_eq!(facts.state(), HelloState::Serving);
        assert!(facts.is_build(BuildIdentity::new("0.0.0+test")));
        assert_eq!(case.context.credentials().counters().lookups, 1);
    }

    /// The MAC is keyed by the token the hello's label names, not by the
    /// one a dialer would choose first.
    #[test]
    fn a_hellos_mac_is_keyed_by_the_labelled_token() {
        let case = case("norn-serve-hello-labelled");
        const LAPTOP: &[u8] = b"the laptop's secret";
        tokens::mutate(&case.dirs, |tokens| {
            tokens.add(&TokenLabel::new("laptop").unwrap(), LAPTOP)
        })
        .expect("a second token");
        let (status, _, body) = send(
            &case,
            request(
                Method::POST,
                HELLO_PATH,
                None,
                &hello_body("laptop", &nonce().to_hex()),
            ),
        );
        assert_eq!(status, StatusCode::OK);
        let answer: HelloAnswer = serde_json::from_slice(&body).expect("an answer");
        let input = answer.facts().mac_input(&nonce());
        let mac = answer.mac().unwrap();
        assert!(mac_verifies(LAPTOP, &input, &mac), "keyed by `laptop`");
        assert!(!mac_verifies(SECRET, &input, &mac), "not by `default`");
    }

    #[test]
    fn every_hello_refusal_answers_its_status_and_code() {
        let case = case("norn-serve-hello-refusals");
        let good = nonce().to_hex();
        let refusals: [(&str, Vec<u8>, StatusCode, &str); 9] = [
            (
                "not json",
                b"label=default".to_vec(),
                StatusCode::BAD_REQUEST,
                "serve/bad-request",
            ),
            (
                "an array",
                serde_json::json!(["default", good])
                    .to_string()
                    .into_bytes(),
                StatusCode::BAD_REQUEST,
                "serve/bad-request",
            ),
            (
                "no nonce",
                br#"{"label":"default"}"#.to_vec(),
                StatusCode::BAD_REQUEST,
                "serve/bad-request",
            ),
            (
                "unknown label",
                hello_body("laptop", &good),
                StatusCode::UNAUTHORIZED,
                "serve/hello-refused",
            ),
            (
                "illegal label",
                hello_body("Not A Label", &good),
                StatusCode::UNAUTHORIZED,
                "serve/hello-refused",
            ),
            (
                "uppercase nonce",
                hello_body("default", &good.to_uppercase()),
                StatusCode::UNAUTHORIZED,
                "serve/hello-refused",
            ),
            (
                "short nonce",
                hello_body("default", &good[..62]),
                StatusCode::UNAUTHORIZED,
                "serve/hello-refused",
            ),
            (
                "over the cap",
                {
                    let mut body = hello_body("default", &good);
                    body.resize(HELLO_BODY_BYTES + 1, b' ');
                    body
                },
                StatusCode::PAYLOAD_TOO_LARGE,
                "serve/request-too-large",
            ),
            (
                "at the cap",
                {
                    let mut body = hello_body("default", &good);
                    body.resize(HELLO_BODY_BYTES, b' ');
                    body
                },
                StatusCode::OK,
                "",
            ),
        ];
        for (what, body, expected, expected_code) in refusals {
            let (status, kind, answer) =
                send(&case, request(Method::POST, HELLO_PATH, None, &body));
            assert_eq!(status, expected, "{what}");
            assert_eq!(kind.as_deref(), Some("application/json"), "{what}");
            if status != StatusCode::OK {
                assert_eq!(code(&answer), expected_code, "{what}");
                let envelope: serde_json::Value = serde_json::from_slice(&answer).unwrap();
                assert_eq!(
                    envelope["detail"],
                    serde_json::json!({ "code": expected_code }),
                    "{what}: the detail carries nothing beyond its code"
                );
            }
        }
    }

    /// A body larger than it declared, with no length to refuse it by, is
    /// refused as it arrives.
    #[test]
    fn a_hello_over_the_cap_by_its_declared_length_is_refused_before_it_is_read() {
        let case = case("norn-serve-hello-declared");
        let mut request = request(Method::POST, HELLO_PATH, None, b"{}");
        request
            .headers_mut()
            .insert(CONTENT_LENGTH, HeaderValue::from_static("4097"));
        let (status, _, body) = send(&case, request);
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(code(&body), "serve/request-too-large");
    }

    #[test]
    fn the_bearer_middleware_answers_each_refusal_and_admits_the_stored_secret() {
        let case = case("norn-serve-bearer");
        let stored = norn_wire::hex::encode(SECRET);
        for (what, bearer) in [
            ("no bearer", None),
            ("not hex", Some("not-hex")),
            ("uppercase hex", Some("0A")),
            ("unknown", Some("00ff")),
            ("empty", Some("")),
        ] {
            let (status, kind, body) =
                send(&case, request(Method::GET, "/v1/wire/find", bearer, b""));
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{what}");
            assert_eq!(kind.as_deref(), Some("application/json"), "{what}");
            assert_eq!(code(&body), "serve/unauthorized", "{what}");
        }
        // The hello's path is exempt only for its method.
        let (status, _, _) = send(&case, request(Method::GET, HELLO_PATH, None, b""));
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, kind, body) = send(
            &case,
            request(Method::GET, "/v1/wire/find", Some(&stored), b""),
        );
        assert_eq!(status, StatusCode::NOT_FOUND, "the wire route is POST only");
        assert_eq!(kind.as_deref(), Some("application/json"));
        assert_eq!(code(&body), "serve/unknown-verb");
    }

    /// The fail-closed state is answered before a hello's body is read, so
    /// a body that is malformed or over the cap meets it too.
    #[test]
    fn the_fail_closed_state_refuses_the_hello_and_every_bearer_with_500() {
        let case = case("norn-serve-fail-closed");
        let stored = norn_wire::hex::encode(SECRET);
        fail_closed(&case);
        let mut declared_over = request(Method::POST, HELLO_PATH, None, b"{}");
        declared_over
            .headers_mut()
            .insert(CONTENT_LENGTH, HeaderValue::from_static("4097"));
        for (what, request) in [
            (
                "a hello",
                request(
                    Method::POST,
                    HELLO_PATH,
                    None,
                    &hello_body("default", &nonce().to_hex()),
                ),
            ),
            ("an array", request(Method::POST, HELLO_PATH, None, b"[1]")),
            (
                "not json",
                request(Method::POST, HELLO_PATH, None, b"notjson"),
            ),
            (
                "no nonce",
                request(Method::POST, HELLO_PATH, None, br#"{"label":"default"}"#),
            ),
            (
                "over the cap",
                request(Method::POST, HELLO_PATH, None, &[b' '; 5000]),
            ),
            ("declared over the cap", declared_over),
            ("a bearer", request(Method::GET, "/", Some(&stored), b"")),
            ("no bearer", request(Method::GET, "/", None, b"")),
        ] {
            let (status, _, body) = send(&case, request);
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{what}");
            assert_eq!(code(&body), "serve/token-file-unreadable", "{what}");
            assert!(!String::from_utf8_lossy(&body).contains("mac"), "{what}");
        }
    }

    /// Every hello makes the one check, whatever its body, and only one: a
    /// hello refused for its body costs one fingerprint and no lookup.
    #[test]
    fn every_hello_checks_the_token_file_exactly_once() {
        let case = case("norn-serve-hello-one-check");
        let good = nonce().to_hex();
        let mut over = hello_body("default", &good);
        over.resize(HELLO_BODY_BYTES + 1, b' ');
        for (what, body, lookups) in [
            ("a hello", hello_body("default", &good), 1),
            ("not json", b"notjson".to_vec(), 0),
            ("over the cap", over, 0),
        ] {
            let before = case.context.credentials().counters();
            send(&case, request(Method::POST, HELLO_PATH, None, &body));
            let after = case.context.credentials().counters();
            assert_eq!(
                (
                    after.fingerprints - before.fingerprints,
                    after.file_reads - before.file_reads,
                    after.lookups - before.lookups,
                ),
                (1, 0, lookups),
                "{what}"
            );
        }
    }

    #[cfg(feature = "counters")]
    #[test]
    fn the_counters_route_answers_the_token_counters_behind_the_bearer() {
        let case = case("norn-serve-counters-route");
        let stored = norn_wire::hex::encode(SECRET);
        let (status, _, _) = send(&case, request(Method::GET, "/v1/debug/counters", None, b""));
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, kind, body) = send(
            &case,
            request(Method::GET, "/v1/debug/counters", Some(&stored), b""),
        );
        assert_eq!(status, StatusCode::OK);
        assert_eq!(kind.as_deref(), Some("application/json"));
        let counters: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(counters["serve.token_fingerprints"], 2);
        assert_eq!(counters["serve.token_file_reads"], 0);
        assert_eq!(
            counters["serve.token_lookups"], 1,
            "the unauthorized one presented no bearer"
        );
    }
}
