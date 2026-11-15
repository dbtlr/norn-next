//! The HTTP/1.1 connection handler: the hello, the bearer middleware in front
//! of every other path, and what stands behind it.
//!
//! `POST /v1/hello` is the one exchange answered without a bearer. Every
//! other request passes the bearer middleware first. Behind it, the counters
//! route (under the `counters` feature) is the only route yet: an
//! authenticated request to any other path is answered 404 with an empty
//! body. The wire route takes its path in a later task of this phase, through
//! `wire.rs`'s reserved `dispatch`.
//!
//! Every refusal is an `ErrorEnvelope` with `Content-Type: application/json`,
//! and none carries anything beyond its code: a caller nobody has
//! authenticated learns nothing about the stored tokens.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::{Body, Bytes};
use hyper::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use norn_wire::{
    BuildIdentity, ErrorDetail, ErrorEnvelope, HELLO_BODY_BYTES, HelloAnswer, HelloChannel,
    HelloFacts, HelloLaunch, HelloRequest, HelloState,
};
use tokio::net::TcpStream;

use crate::credentials::{Admission, Credentials, Keyed};

/// The path of the one exchange admitted without a bearer.
const HELLO_PATH: &str = "/v1/hello";

/// What every connection of one host answers from.
pub(crate) struct Context {
    pub(crate) credentials: Mutex<Credentials>,
    pub(crate) channel: HelloChannel,
    pub(crate) build: BuildIdentity,
    pub(crate) launch: HelloLaunch,
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

/// Serve one accepted connection until its client closes it.
pub(crate) async fn answer(stream: TcpStream, context: Arc<Context>) {
    // Both ends as this connection's socket reports them: the hello states
    // them. A socket that cannot report them is closed unanswered.
    let (Ok(local), Ok(peer)) = (stream.local_addr(), stream.peer_addr()) else {
        return;
    };
    let service = service_fn(move |request| {
        let context = Arc::clone(&context);
        async move { Ok::<_, Infallible>(route(request, &context, local, peer).await) }
    });
    // A connection that fails mid-exchange is the client's to retry; there
    // is no request whose outcome it could leave unknown.
    let _ = http1::Builder::new()
        .serve_connection(TokioIo::new(stream), service)
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
        Admission::Admitted => authenticated(&request, context),
        Admission::Unauthorized => refusal(
            StatusCode::UNAUTHORIZED,
            "the request carried no bearer this host accepts",
            ErrorDetail::unauthorized(),
        ),
        Admission::FailedClosed => token_file_unreadable(),
    }
}

/// What stands behind the bearer middleware.
fn authenticated<B>(request: &Request<B>, context: &Context) -> Answer {
    #[cfg(feature = "counters")]
    if request.method() == Method::GET && request.uri().path() == "/v1/debug/counters" {
        return counters(context);
    }
    #[cfg(not(feature = "counters"))]
    let _ = (request, context);
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = StatusCode::NOT_FOUND;
    response
}

/// The `serve.*` counters, as one JSON object of name to integer.
#[cfg(feature = "counters")]
fn counters(context: &Context) -> Answer {
    let tokens = context.credentials().counters();
    let body = serde_json::json!({
        "serve.token_fingerprints": tokens.fingerprints,
        "serve.token_file_reads": tokens.file_reads,
        "serve.token_lookups": tokens.lookups,
    });
    json(StatusCode::OK, body.to_string().into_bytes())
}

/// `POST /v1/hello`: one check of the token set, then the body read under
/// its cap, then one lookup by label.
///
/// The check comes before the body, so the fail-closed state is answered to
/// every hello, whatever its body: a caller nobody has verified learns of a
/// malformed or oversized body only from a host that is serving.
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
    let body = match capped_body(request).await {
        Ok(body) => body,
        Err(Unread::TooLarge) => {
            return refusal(
                StatusCode::PAYLOAD_TOO_LARGE,
                "the hello's body is over 4 KiB",
                ErrorDetail::request_too_large(),
            );
        }
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
        Keyed::Refused => refusal(
            StatusCode::UNAUTHORIZED,
            "the hello was refused",
            ErrorDetail::hello_refused(),
        ),
        Keyed::FailedClosed => token_file_unreadable(),
    }
}

/// Why a hello's body was not read.
enum Unread {
    /// Over [`HELLO_BODY_BYTES`], by its declared length or by what arrived.
    TooLarge,
    /// The body did not arrive whole.
    Failed,
}

async fn capped_body<B>(request: Request<B>) -> Result<Bytes, Unread>
where
    B: Body<Data = Bytes>,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let declared = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|length| length > HELLO_BODY_BYTES as u64) {
        return Err(Unread::TooLarge);
    }
    match Limited::new(request.into_body(), HELLO_BODY_BYTES)
        .collect()
        .await
    {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(error) if error.downcast_ref::<LengthLimitError>().is_some() => Err(Unread::TooLarge),
        Err(_) => Err(Unread::Failed),
    }
}

fn bad_hello() -> Answer {
    refusal(
        StatusCode::BAD_REQUEST,
        "the hello's body is not a JSON hello object",
        ErrorDetail::bad_request(),
    )
}

fn token_file_unreadable() -> Answer {
    refusal(
        StatusCode::INTERNAL_SERVER_ERROR,
        "this host's token file changed and no longer reads, so every request is refused until \
         it does; the host's log names the file and the fix",
        ErrorDetail::token_file_unreadable(),
    )
}

fn refusal(status: StatusCode, message: &str, detail: ErrorDetail) -> Answer {
    let envelope = ErrorEnvelope::new(message, detail);
    json(
        status,
        serde_json::to_vec(&envelope).expect("an error envelope serializes"),
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

    const SECRET: &[u8] = b"a stored secret";

    struct Case {
        _scratch: Scratch,
        dirs: ConfigDirs,
        context: Context,
    }

    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the bases a case's state lives under.
    fn case(name: &str) -> Case {
        let scratch = Scratch::new(name);
        std::fs::create_dir_all(scratch.join("config")).expect("a config base");
        let dirs = ConfigDirs::new(scratch.join("config"), scratch.join("data")).expect("bases");
        tokens::mutate(&dirs, |tokens| {
            tokens.add(&TokenLabel::default_label(), SECRET)
        })
        .expect("a token");
        let context = Context {
            credentials: Mutex::new(Credentials::new(&dirs, settled(&dirs))),
            channel: HelloChannel::Dev,
            build: BuildIdentity::new("0.0.0+test"),
            launch: HelloLaunch::Explicit,
        };
        Case {
            _scratch: scratch,
            dirs,
            context,
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
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(kind, None);
        assert!(body.is_empty());
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
