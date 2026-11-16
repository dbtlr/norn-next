use std::time::Duration;

use norn_wire::DrainAnswer;

use super::wire::{SHORT, bearer, serving};
use super::*;

fn drain_post(bearer: Option<&str>, body: &[u8]) -> Request<Full<Bytes>> {
    request(Method::POST, DRAIN_PATH, bearer, body)
}

/// Send `request` while a stand-in serving loop closes the listener once a
/// drain begins, as the real one does.
fn send_serving(case: &Case, request: Request<Full<Bytes>>) -> (StatusCode, HeaderMapText, Bytes) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let lifecycle = Arc::clone(&case.context.lifecycle);
    let answer = runtime.block_on(async {
        let closer = tokio::spawn(async move {
            lifecycle.stopping().await;
            lifecycle.listener_closed();
        });
        let answer = route(
            request,
            &case.context,
            LOCAL.parse().unwrap(),
            PEER.parse().unwrap(),
        )
        .await;
        closer.abort();
        answer
    });
    let status = answer.status();
    let connection = answer
        .headers()
        .get(CONNECTION)
        .map(|value| value.to_str().unwrap().to_string());
    let body = runtime
        .block_on(answer.into_body().collect())
        .unwrap()
        .to_bytes();
    (status, HeaderMapText { connection }, body)
}

struct HeaderMapText {
    connection: Option<String>,
}

fn hello_state(case: &Case) -> HelloState {
    let (status, _, body) = send(
        case,
        request(
            Method::POST,
            HELLO_PATH,
            None,
            &hello_body("default", &nonce().to_hex()),
        ),
    );
    assert_eq!(status, StatusCode::OK);
    let answer: HelloAnswer = serde_json::from_slice(&body).expect("a hello answer");
    answer.facts().state()
}

/// **An auto-launched host drains on request**: it sets `draining`, closes
/// its listener, and only then answers 202 `{"version":1,"draining":true}`;
/// a hello now reports `draining`, and a second drain request is answered
/// 503 `serve/draining`.
#[test]
fn an_auto_launched_host_answers_202_once_its_listener_is_closed_then_503() {
    let mut case = case("norn-serve-drain-auto");
    case.context.launch = HelloLaunch::Auto;
    assert_eq!(hello_state(&case), HelloState::Serving);
    let (status, headers, body) = send_serving(&case, drain_post(Some(&bearer()), b"{}"));
    assert_eq!(status, StatusCode::ACCEPTED);
    let answer: DrainAnswer = serde_json::from_slice(&body).expect("a drain answer");
    assert_eq!(answer, DrainAnswer::draining());
    assert_eq!(headers.connection.as_deref(), Some("close"));
    assert_eq!(case.context.lifecycle.stage(), Stage::Draining);
    assert_eq!(
        case.context.lifecycle.reason(),
        Some(StopReason::DrainRequest)
    );
    assert!(case.context.wire.admission.draining());
    assert_eq!(hello_state(&case), HelloState::Draining);

    let (status, _, body) = send_serving(&case, drain_post(Some(&bearer()), b"{}"));
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(code(&body), "serve/draining");
}

/// **An explicit host refuses a drain request** 409 `serve/drain-refused`
/// and keeps serving.
#[test]
fn an_explicit_host_refuses_a_drain_request_and_keeps_serving() {
    let case = case("norn-serve-drain-explicit");
    let (status, _, body) = send_serving(&case, drain_post(Some(&bearer()), b"{}"));
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(code(&body), "serve/drain-refused");
    assert_eq!(case.context.lifecycle.stage(), Stage::Serving);
    assert_eq!(hello_state(&case), HelloState::Serving);
}

/// The drain request stands behind the bearer, and its body is `{}`.
#[test]
fn a_drain_request_needs_the_bearer_and_an_object_body() {
    let mut case = case("norn-serve-drain-refusals");
    case.context.launch = HelloLaunch::Auto;
    let (status, _, body) = send_serving(&case, drain_post(None, b"{}"));
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&body), "serve/unauthorized");
    let (status, _, body) = send_serving(&case, drain_post(Some(&bearer()), b"[]"));
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(code(&body), "serve/bad-request");
    let (status, _, body) = send_serving(&case, drain_post(Some(&bearer()), &[b' '; 5000]));
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(code(&body), "serve/request-too-large");
    assert_eq!(case.context.lifecycle.stage(), Stage::Serving);
}

/// **A wire body still arriving when the drain begins** is answered 503
/// `serve/draining` without the rest being read, and the connection closed;
/// nothing is dispatched.
#[test]
fn a_wire_body_still_arriving_when_the_drain_begins_is_answered_draining() {
    let case = case("norn-serve-drain-arriving");
    let lifecycle = Arc::clone(&case.context.lifecycle);
    let counters = Arc::clone(&case.context.wire.counters);
    let socket = serving(case);
    let head = format!(
        "POST /v1/wire/find HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\nContent-Length: \
         100\r\n\r\n{{",
        bearer()
    );
    let exchanged =
        std::thread::spawn(move || socket.exchange(head.as_bytes(), Duration::from_secs(5)));
    // The request is in flight once its head reached the route.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while lifecycle.drain.in_flight() == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the request never arrived"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    lifecycle.begin_drain(StopReason::Terminate);
    let (answer, closed, took) = exchanged.join().unwrap();
    assert!(closed, "the connection stayed open: {answer}");
    assert!(answer.starts_with("HTTP/1.1 503"), "{answer}");
    assert!(answer.contains("serve/draining"), "{answer}");
    assert!(
        took < SHORT.body_arrival,
        "answered at the arrival bound: {took:?}"
    );
    assert_eq!(counters.dispatches.load(Ordering::SeqCst), 0);
}

/// **An answer given during the drain is flushed, then the connection
/// half-closed**: an answer before the drain leaves its connection open, and
/// one given during it is read whole and then the end of the stream, and its
/// request is then no longer in flight.
#[test]
fn an_answer_during_the_drain_is_flushed_then_half_closed() {
    let case = case("norn-serve-drain-half-close");
    let lifecycle = Arc::clone(&case.context.lifecycle);
    let socket = serving(case);
    let hello = String::from_utf8(hello_body("default", &nonce().to_hex())).unwrap();
    let request = format!(
        "POST /v1/hello HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{hello}",
        hello.len()
    );
    // Before the drain the connection is kept alive past the read.
    let (answer, closed, _) = socket.exchange(request.as_bytes(), Duration::from_millis(200));
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(!closed, "a kept-alive connection closed before the drain");

    lifecycle.begin_drain(StopReason::Terminate);
    let (answer, closed, took) = socket.exchange(request.as_bytes(), Duration::from_secs(5));
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(answer.contains(r#""state":"draining""#), "{answer}");
    assert!(closed, "no end of stream after the drain's answer");
    assert!(
        took < SHORT.keep_alive,
        "closed by the keep-alive bound: {took:?}"
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while lifecycle.drain.in_flight() != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the flushed request stayed in flight"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// **An answer that went out kept-alive is not half-closed when the drain
/// begins during its flush.** A large answer, built before the drain and so
/// without `Connection: close`, is still being written when the drain
/// begins, because its client is not reading. Once the client has read it,
/// the connection still carries the next request, which is answered 503
/// `serve/draining` with the close, rather than an end of stream.
#[test]
fn a_kept_alive_answer_flushed_after_the_drain_began_leaves_its_connection_readable() {
    use std::io::{Read, Write};
    // Far past what loopback socket buffers hold, so the answer cannot
    // finish writing until the client reads it.
    const REPORT_BYTES: usize = 32 << 20;
    let case = case("norn-serve-drain-late-flush");
    case.answers
        .push(Dispatched::Report(vec![b' '; REPORT_BYTES]));
    let lifecycle = Arc::clone(&case.context.lifecycle);
    let socket = serving(case);
    let mut stream = socket.connect(Duration::from_secs(5));
    let wire = format!(
        "POST /v1/wire/find HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\nContent-Length: \
         2\r\n\r\n{{}}",
        bearer()
    );
    stream.write_all(wire.as_bytes()).unwrap();
    // The head is written only once the route has returned the answer, so
    // the answer was built, and its close decided, before the drain.
    let head = read_head(&mut stream);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(!head.to_ascii_lowercase().contains("connection:"), "{head}");
    assert_eq!(lifecycle.drain.in_flight(), 1, "the answer flushed already");

    lifecycle.begin_drain(StopReason::Terminate);
    let mut body = vec![0; REPORT_BYTES];
    stream.read_exact(&mut body).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while lifecycle.drain.in_flight() != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the flushed answer stayed in flight"
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    stream.write_all(wire.as_bytes()).unwrap();
    let mut answer = Vec::new();
    let closed = stream.read_to_end(&mut answer).is_ok();
    let answer = String::from_utf8_lossy(&answer);
    assert!(answer.starts_with("HTTP/1.1 503"), "{answer}");
    assert!(answer.contains("serve/draining"), "{answer}");
    assert!(
        answer.to_ascii_lowercase().contains("connection: close"),
        "{answer}"
    );
    assert!(closed, "the draining answer did not close: {answer}");
}

/// An answer's head, read up to its blank line and no further.
fn read_head(stream: &mut std::net::TcpStream) -> String {
    use std::io::Read;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        assert_eq!(stream.read(&mut byte).unwrap(), 1, "the head ended early");
        head.push(byte[0]);
    }
    String::from_utf8(head).unwrap()
}
