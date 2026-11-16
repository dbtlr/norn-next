use std::time::Duration;

use norn_wire::{
    AnswerShape, Candidate, CandidateHead, ControlFile, CursorOrderChanged, DocumentPath, Forecast,
    Hint, InterruptionCause, MaintainerIdentity, MigrationRefusal, NameSet, NotReady, Operation,
    OperationKind, PagedRows, PlanFault, ReadFailure, ReasonCode, RefusedCheck, ReloadFailure,
    RequestBound, RequestPart, ResolutionTarget, ResolvedPlan, RootIdentity, Rung, Unsatisfied,
    UntrustedReason, VaultAddress, VaultName, WarmingPhase, WatcherLossCause,
};
use std::io::{Read, Write};

use super::*;

fn name() -> VaultName {
    VaultName::new("notes").unwrap()
}

fn names() -> NameSet {
    NameSet::new(["notes", "vault"].map(|n| VaultName::new(n).unwrap())).unwrap()
}

fn path() -> DocumentPath {
    DocumentPath::new("notes/a.md").unwrap()
}

fn target() -> ResolutionTarget {
    ResolutionTarget::new("glossary").unwrap()
}

fn plan() -> ResolvedPlan {
    ResolvedPlan::new(
        VaultAddress::name(name()),
        RootIdentity::from_device_and_inode(1, 2),
        vec![Operation::new(OperationKind::delete_document(path()))],
        Vec::new(),
        Vec::new(),
    )
}

/// One representative detail for each code whose class is its own, and one
/// for each variant of the detail enums a class depends on, beside the
/// status the fault-class table puts it at. Written out here rather than
/// derived through `fault_class`, so the table is checked, not echoed.
fn representatives() -> Vec<(ErrorDetail, StatusCode)> {
    use StatusCode as S;
    let not_run = |cause: ErrorDetail| {
        ErrorDetail::apply_not_run(ErrorEnvelope::new("cause", cause), Some(plan()))
    };
    vec![
        (ErrorDetail::duplicate_root(names()), S::CONFLICT),
        (ErrorDetail::shared_schema(names()), S::CONFLICT),
        (
            ErrorDetail::entry_untrusted(UntrustedReason::WatcherOverflow),
            S::SERVICE_UNAVAILABLE,
        ),
        (
            ErrorDetail::entry_untrusted(UntrustedReason::watcher_lost(
                WatcherLossCause::Backend,
                "gone",
            )),
            S::SERVICE_UNAVAILABLE,
        ),
        (
            ErrorDetail::entry_untrusted(UntrustedReason::store_damaged_rebuilding("torn")),
            S::SERVICE_UNAVAILABLE,
        ),
        (
            ErrorDetail::entry_untrusted(UntrustedReason::store_damaged_awaiting_demand("torn")),
            S::SERVICE_UNAVAILABLE,
        ),
        (
            ErrorDetail::entry_untrusted(UntrustedReason::environmental_refusal("denied")),
            S::INTERNAL_SERVER_ERROR,
        ),
        (
            ErrorDetail::entry_untrusted(UntrustedReason::schema_unreadable("not yaml")),
            S::INTERNAL_SERVER_ERROR,
        ),
        (
            ErrorDetail::entry_untrusted(UntrustedReason::leg_unwound("panicked")),
            S::INTERNAL_SERVER_ERROR,
        ),
        (
            ErrorDetail::maintainer_contended(MaintainerIdentity::named(41, "0.1.0", 1)),
            S::SERVICE_UNAVAILABLE,
        ),
        (ErrorDetail::unknown_vault(name()), S::NOT_FOUND),
        (ErrorDetail::already_served(name()), S::CONFLICT),
        (ErrorDetail::entry_held(name()), S::SERVICE_UNAVAILABLE),
        (
            ErrorDetail::entry_not_ready(NotReady::unattached()),
            S::SERVICE_UNAVAILABLE,
        ),
        (
            ErrorDetail::entry_not_ready(NotReady::warming(WarmingPhase::Healing, 1, None)),
            S::SERVICE_UNAVAILABLE,
        ),
        (
            ErrorDetail::reader_unavailable("busy"),
            S::SERVICE_UNAVAILABLE,
        ),
        (
            ErrorDetail::registry_unwritable("read-only"),
            S::INTERNAL_SERVER_ERROR,
        ),
        (
            ErrorDetail::read_failed(ReadFailure::count_cursor_expired(), "x"),
            S::CONFLICT,
        ),
        (
            ErrorDetail::read_failed(ReadFailure::count_result_capacity(None), "x"),
            S::SERVICE_UNAVAILABLE,
        ),
        (
            ErrorDetail::read_failed(ReadFailure::count_result_busy(), "x"),
            S::SERVICE_UNAVAILABLE,
        ),
        (
            ErrorDetail::read_failed(ReadFailure::count_result(), "x"),
            S::SERVICE_UNAVAILABLE,
        ),
        (
            ErrorDetail::read_failed(ReadFailure::statement(), "x"),
            S::SERVICE_UNAVAILABLE,
        ),
        (
            ErrorDetail::read_failed(ReadFailure::unreadable_document(), "x"),
            S::SERVICE_UNAVAILABLE,
        ),
        (
            ErrorDetail::read_failed(ReadFailure::count_result_too_large(1), "x"),
            S::INTERNAL_SERVER_ERROR,
        ),
        (
            ErrorDetail::read_failed(ReadFailure::declaration_not_pinned(None, None), "x"),
            S::INTERNAL_SERVER_ERROR,
        ),
        (
            not_run(ErrorDetail::entry_held(name())),
            S::SERVICE_UNAVAILABLE,
        ),
        (
            not_run(ErrorDetail::entry_untrusted(
                UntrustedReason::schema_unreadable("not yaml"),
            )),
            S::INTERNAL_SERVER_ERROR,
        ),
        (
            ErrorDetail::apply_outcome_unknown(plan()),
            S::SERVICE_UNAVAILABLE,
        ),
        (ErrorDetail::ambiguous_root(names()), S::CONFLICT),
        (
            ErrorDetail::ambiguous_target(
                target(),
                CandidateHead::new(
                    [Candidate::new(
                        DocumentPath::new("notes/glossary.md").unwrap(),
                        "notes/glossary",
                    )],
                    2,
                )
                .unwrap(),
                Hint::resolves(target()),
            ),
            S::CONFLICT,
        ),
        (ErrorDetail::unknown_target(target()), S::NOT_FOUND),
        (ErrorDetail::unknown_rule("tasks"), S::NOT_FOUND),
        (ErrorDetail::reload_busy(), S::SERVICE_UNAVAILABLE),
        (
            ErrorDetail::reload_failed(ReloadFailure::unsupported()),
            S::INTERNAL_SERVER_ERROR,
        ),
        (
            ErrorDetail::cursor_order_changed(CursorOrderChanged::new("a", None)),
            S::CONFLICT,
        ),
        (ErrorDetail::unreadable_bound("due", "x"), S::BAD_REQUEST),
        (
            ErrorDetail::plan_refused(
                plan(),
                Forecast::new(Vec::new(), Vec::new(), Vec::new()),
                vec![RefusedCheck::name_taken(path())],
                Vec::new(),
                Vec::new(),
                Vec::new(),
            ),
            S::CONFLICT,
        ),
        (
            ErrorDetail::root_changed(
                RootIdentity::from_device_and_inode(1, 2),
                RootIdentity::from_device_and_inode(1, 3),
            ),
            S::CONFLICT,
        ),
        (
            ErrorDetail::plan_interrupted(
                plan(),
                vec![path()],
                InterruptionCause::io_failure("full"),
                Vec::new(),
                Vec::new(),
            ),
            S::SERVICE_UNAVAILABLE,
        ),
        (
            ErrorDetail::write_failed(plan(), "full", Vec::new()),
            S::INTERNAL_SERVER_ERROR,
        ),
        (
            ErrorDetail::migration_refused(
                ControlFile::Schema,
                MigrationRefusal::version_ahead(2, 1),
            ),
            S::CONFLICT,
        ),
        (
            ErrorDetail::out_of_bound(RequestBound::page_rows(5, 1)),
            S::BAD_REQUEST,
        ),
        (
            ErrorDetail::part_not_taken(RequestPart::Cursor, Some(AnswerShape::Summary)),
            S::BAD_REQUEST,
        ),
        (
            ErrorDetail::cursor_not_taken(PagedRows::Tally, PagedRows::Document),
            S::BAD_REQUEST,
        ),
        (
            ErrorDetail::plan_invalid(PlanFault::content_cycle(vec![0, 1])),
            S::BAD_REQUEST,
        ),
        (
            ErrorDetail::unsatisfied(vec![Unsatisfied::unknown_predicate_key(
                "stauts",
                vec!["status".to_string()],
            )]),
            S::BAD_REQUEST,
        ),
        (
            ErrorDetail::engine_not_enabled(Rung::Vector, "off"),
            S::CONFLICT,
        ),
        (
            ErrorDetail::engine_unavailable(Rung::Vector, "empty"),
            S::SERVICE_UNAVAILABLE,
        ),
        (
            ErrorDetail::engine_failed(Rung::Vector, "failed"),
            S::INTERNAL_SERVER_ERROR,
        ),
        (ErrorDetail::bad_request(), S::BAD_REQUEST),
        (ErrorDetail::unauthorized(), S::UNAUTHORIZED),
        (ErrorDetail::hello_refused(), S::UNAUTHORIZED),
        (ErrorDetail::request_too_large(), S::PAYLOAD_TOO_LARGE),
        (
            ErrorDetail::token_file_unreadable(),
            S::INTERNAL_SERVER_ERROR,
        ),
        (ErrorDetail::unknown_verb(), S::NOT_FOUND),
        (ErrorDetail::busy(), S::SERVICE_UNAVAILABLE),
        (ErrorDetail::draining(), S::SERVICE_UNAVAILABLE),
        (ErrorDetail::outcome_unknown(), S::SERVICE_UNAVAILABLE),
        (ErrorDetail::drain_refused(), S::CONFLICT),
    ]
}

fn bearer() -> String {
    norn_wire::hex::encode(SECRET)
}

fn wire_post(verb: &str, body: &[u8]) -> Request<Full<Bytes>> {
    request(
        Method::POST,
        &format!("/v1/wire/{verb}"),
        Some(&bearer()),
        body,
    )
}

fn envelope(body: &Bytes) -> ErrorEnvelope {
    serde_json::from_slice(body).expect("an error envelope")
}

/// **Every class reaches its status on the wire route.** Every code in
/// `ReasonCode::ALL` has a representative, and so does every variant of the
/// detail enums a class depends on; each, answered by the host, reaches the
/// caller at the status its class maps to, carrying the same code and detail
/// — the code is never laundered into prose on the way out.
#[test]
fn every_code_reaches_its_class_status_on_the_wire_route() {
    // A warming entry is asked again until the deadline, then answered with
    // its last refusal: a short wait keeps that case quick.
    let case = case_waiting("norn-serve-wire-statuses", Duration::from_millis(200));
    let representatives = representatives();
    for code in ReasonCode::ALL {
        assert!(
            representatives
                .iter()
                .any(|(detail, _)| detail.code() == code),
            "{code:?} has no representative"
        );
    }
    for (detail, expected) in representatives {
        let sent = ErrorEnvelope::new("refused", detail);
        let retried = crate::request::is_warming(&sent);
        for _ in 0..if retried { 20 } else { 1 } {
            case.answers.push(Dispatched::Refused(sent.clone()));
        }
        let (status, kind, body) = send(&case, wire_post("find", b"{}"));
        assert_eq!(status, expected, "{:?}", sent.code());
        assert_eq!(status, status_of(sent.fault_class()), "{:?}", sent.code());
        assert_eq!(kind.as_deref(), Some("application/json"));
        assert_eq!(envelope(&body), sent);
        // What a retried answer left unanswered goes, so the next case is
        // answered from its own script.
        case.answers.next.lock().unwrap().clear();
    }
}

/// Every class has a status no other client-visible class shares, but for
/// `Operator` and `Defect`, which both answer 500: the caller cannot fix
/// either by changing or resending the request.
#[test]
fn each_class_maps_to_its_status() {
    let statuses: Vec<u16> = FaultClass::ALL
        .iter()
        .map(|class| status_of(*class).as_u16())
        .collect();
    assert_eq!(statuses, [400, 401, 404, 409, 413, 503, 500, 500]);
}

#[test]
fn a_report_is_answered_200_with_its_json() {
    let case = case("norn-serve-wire-report");
    case.answers
        .push(Dispatched::Report(br#"{"report":"here"}"#.to_vec()));
    let (status, kind, body) = send(&case, wire_post("find", b"{}"));
    assert_eq!(status, StatusCode::OK);
    assert_eq!(kind.as_deref(), Some("application/json"));
    assert_eq!(&body[..], br#"{"report":"here"}"#);
    let counters = &case.context.wire.counters;
    assert_eq!(
        counters.report_bytes.load(Ordering::SeqCst),
        body.len() as u64
    );
    assert_eq!(
        counters.body_bytes_encoded.load(Ordering::SeqCst),
        body.len() as u64
    );
}

/// A path that names no verb, a verb with no route, and the route under
/// another method are all 404 `serve/unknown-verb`; nothing is dispatched.
#[test]
fn a_path_naming_no_served_verb_is_404_unknown_verb() {
    let case = case("norn-serve-wire-unknown");
    for (method, path) in [
        (Method::POST, "/v1/wire/nope"),
        (Method::POST, "/v1/wire/"),
        (Method::POST, "/v1/wire/Find"),
        (Method::GET, "/v1/wire/find"),
        (Method::POST, "/v2/wire/find"),
        (Method::GET, "/"),
    ] {
        let (status, _, body) = send(&case, request(method.clone(), path, Some(&bearer()), b"{}"));
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
        assert_eq!(code(&body), "serve/unknown-verb", "{method} {path}");
    }
    assert_eq!(
        case.context.wire.counters.dispatches.load(Ordering::SeqCst),
        0
    );
}

/// A body that is not the verb's params is 400 `serve/bad-request`, whether
/// the route reads it as a mutation's vault or the route table parses it.
#[test]
fn a_body_that_does_not_parse_is_400_bad_request() {
    let case = case("norn-serve-wire-unparsed");
    let (status, _, body) = send(&case, wire_post("set", b"{}"));
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(code(&body), "serve/bad-request");
    case.answers
        .push(Dispatched::BadRequest("missing field `vault`".into()));
    let (status, _, body) = send(&case, wire_post("find", b"{}"));
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(code(&body), "serve/bad-request");
    assert!(envelope(&body).message().contains("missing field `vault`"));
}

/// A body declared over 256 MiB is refused before any of it is read, and
/// the connection is closed after the answer.
#[test]
fn a_body_declared_over_the_cap_is_413_before_it_is_read() {
    let case = case("norn-serve-wire-too-large");
    let mut over = wire_post("find", b"{}");
    over.headers_mut().insert(
        CONTENT_LENGTH,
        HeaderValue::from(WIRE_BODY_BYTES as u64 + 1),
    );
    let (status, _, body) = send(&case, over);
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(code(&body), "serve/request-too-large");
}

/// Past the in-flight bound a request is answered 503 `serve/busy` at once.
#[test]
fn past_the_in_flight_bound_a_request_is_busy_at_once() {
    let case = case("norn-serve-wire-busy");
    let held: Vec<_> = (0..crate::admission::IN_FLIGHT_BOUND)
        .map(|_| case.context.wire.admission.enter().unwrap())
        .collect();
    let (status, _, body) = send(&case, wire_post("find", b"{}"));
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(code(&body), "serve/busy");
    drop(held);
    let (status, _, _) = send(&case, wire_post("find", b"{}"));
    assert_eq!(status, StatusCode::OK);
}

/// Once the drain has begun, a new request is refused 503 `serve/draining`
/// and nothing is dispatched.
#[test]
fn once_the_drain_has_begun_a_new_request_is_draining() {
    let case = case("norn-serve-wire-draining");
    case.context.wire.admission.begin_drain();
    let (status, _, body) = send(&case, wire_post("find", b"{}"));
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(code(&body), "serve/draining");
    assert_eq!(
        case.context.wire.counters.dispatches.load(Ordering::SeqCst),
        0
    );
}

/// **A host-death error has a code, and stops the host**, apart from the
/// fault-class table: a stopped worker is answered 503
/// `serve/outcome-unknown`, and the first one triggers the process stop.
#[test]
fn a_stopped_worker_is_answered_outcome_unknown_and_stops_the_host() {
    let case = case("norn-serve-wire-host-stopped");
    assert!(!case.context.stopper.triggered());
    case.answers.push(Dispatched::HostStopped(
        "the host worker pool stopped".into(),
    ));
    let (status, _, body) = send(
        &case,
        wire_post("vault_reload", br#"{"vault":{"by":"name","name":"notes"}}"#),
    );
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(code(&body), "serve/outcome-unknown");
    assert!(case.context.stopper.triggered());
}

/// Under the counters feature every wire answer carries the host time.
#[cfg(feature = "counters")]
#[test]
fn every_wire_answer_carries_the_host_time_under_the_counters_feature() {
    let case = case("norn-serve-wire-host-time");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    for push in [
        Dispatched::Report(b"{}".to_vec()),
        Dispatched::Refused(ErrorEnvelope::new("held", ErrorDetail::entry_held(name()))),
    ] {
        case.answers.push(push);
        let answer = runtime.block_on(route(
            wire_post("find", b"{}"),
            &case.context,
            LOCAL.parse().unwrap(),
            PEER.parse().unwrap(),
        ));
        let micros = answer
            .headers()
            .get(HOST_TIME_HEADER)
            .expect("the host time header")
            .to_str()
            .unwrap();
        assert!(micros.parse::<u64>().is_ok(), "{micros}");
    }
}

/// A served connection over a real loopback socket, its bounds short, on a
/// runtime of its own thread.
struct Socket {
    port: u16,
}

const SHORT: Bounds = Bounds {
    keep_alive: Duration::from_millis(600),
    header_read: Duration::from_millis(300),
    body_arrival: Duration::from_millis(300),
};

fn serving(case: Case) -> Socket {
    let mut context = case.context;
    context.bounds = SHORT;
    let context = Arc::new(context);
    let scratch = case._scratch;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _scratch = scratch;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::spawn(answer(stream, Arc::clone(&context)));
            }
        });
    });
    Socket { port }
}

impl Socket {
    /// Write `bytes`, then read until the host closes the connection or
    /// `within` passes, answering what arrived, whether it closed, and when.
    /// The clock starts before the connect: the host's header bound runs from
    /// its accept, so a clock started after the write could read a bound that
    /// fired on time as early.
    fn exchange(&self, bytes: &[u8], within: Duration) -> (String, bool, Duration) {
        let started = std::time::Instant::now();
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream.write_all(bytes).unwrap();
        stream.set_read_timeout(Some(within)).unwrap();
        let mut read = Vec::new();
        let closed = stream.read_to_end(&mut read).is_ok();
        (
            String::from_utf8_lossy(&read).into_owned(),
            closed,
            started.elapsed(),
        )
    }
}

/// **A hello that declares a body and withholds it is bounded.** The hello
/// checks the credential before it reads the body, and an unauthenticated
/// caller must not hold the connection indefinitely: past the body-arrival
/// bound it is answered 400 `serve/bad-request` and closed.
#[test]
fn a_stalled_hello_body_is_answered_and_closed_at_the_arrival_bound() {
    let socket = serving(case("norn-serve-stalled-hello"));
    let (answer, closed, took) = socket.exchange(
        b"POST /v1/hello HTTP/1.1\r\nHost: x\r\nContent-Length: 64\r\n\r\n{\"label\"",
        Duration::from_secs(5),
    );
    assert!(closed, "the connection stayed open: {answer}");
    assert!(answer.starts_with("HTTP/1.1 400"), "{answer}");
    assert!(answer.contains("serve/bad-request"), "{answer}");
    assert!(took >= SHORT.body_arrival, "{took:?}");
}

/// A wire body that does not arrive in time is answered 400
/// `serve/bad-request`, nothing is dispatched, and the connection closes.
#[test]
fn a_stalled_wire_body_is_answered_and_closed_at_the_arrival_bound() {
    let case = case("norn-serve-stalled-wire");
    let counters = Arc::clone(&case.context.wire.counters);
    let socket = serving(case);
    let head = format!(
        "POST /v1/wire/find HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\nContent-Length: \
         100\r\n\r\n{{",
        bearer()
    );
    let (answer, closed, _) = socket.exchange(head.as_bytes(), Duration::from_secs(5));
    assert!(closed, "the connection stayed open: {answer}");
    assert!(answer.starts_with("HTTP/1.1 400"), "{answer}");
    assert!(answer.contains("serve/bad-request"), "{answer}");
    assert_eq!(counters.dispatches.load(Ordering::SeqCst), 0);
}

/// A request head that does not arrive within the header bound closes the
/// connection, unanswered.
#[test]
fn a_head_that_does_not_arrive_in_time_closes_the_connection() {
    let socket = serving(case("norn-serve-stalled-head"));
    let (answer, closed, took) =
        socket.exchange(b"POST /v1/hello HTTP/1.1\r\nHost:", Duration::from_secs(5));
    assert!(closed, "the connection stayed open");
    assert!(!answer.contains("HTTP/1.1 2"), "{answer}");
    assert!(
        took >= SHORT.header_read && took < SHORT.keep_alive,
        "{took:?}"
    );
}

/// A connection that sends nothing at all is held to the header bound for
/// its first request.
#[test]
fn a_connection_that_sends_nothing_closes_at_the_header_bound() {
    let socket = serving(case("norn-serve-silent"));
    let (_, closed, took) = socket.exchange(b"", Duration::from_secs(5));
    assert!(closed);
    assert!(
        took >= SHORT.header_read && took < SHORT.keep_alive,
        "{took:?}"
    );
}

/// After an answer, an idle connection is kept for the keep-alive bound —
/// longer than the header bound — and then closed.
#[test]
fn an_idle_connection_is_kept_alive_then_closed() {
    let socket = serving(case("norn-serve-keep-alive"));
    let hello = serde_json::json!({ "label": "default", "nonce": "ab".repeat(32) }).to_string();
    let request = format!(
        "POST /v1/hello HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{hello}",
        hello.len()
    );
    let (answer, closed, took) = socket.exchange(request.as_bytes(), Duration::from_secs(5));
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(closed);
    assert!(
        took >= SHORT.keep_alive,
        "closed after {took:?}, inside the keep-alive"
    );
}

/// A host that holds every call until released, and records how many ran at
/// once.
struct Gate {
    calls: std::sync::atomic::AtomicUsize,
    running: std::sync::atomic::AtomicUsize,
    widest: std::sync::atomic::AtomicUsize,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl crate::request::Dispatch for Gate {
    fn dispatch(&self, _: Verb, _: &[u8]) -> Dispatched {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.widest.fetch_max(now, Ordering::SeqCst);
        let _ = self.release.lock().unwrap().recv();
        self.running.fetch_sub(1, Ordering::SeqCst);
        Dispatched::Report(b"{}".to_vec())
    }
}

/// Wait until `condition` holds, or fail after five seconds.
fn wait_for(what: &str, condition: impl Fn() -> bool) {
    let by = std::time::Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(std::time::Instant::now() < by, "never: {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Send `verb` with `body` behind the bearer, and keep the connection.
fn post(socket: &Socket, verb: &str, body: &str) -> std::net::TcpStream {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", socket.port)).unwrap();
    let head = format!(
        "POST /v1/wire/{verb} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\nContent-Length: \
         {}\r\n\r\n{body}",
        bearer(),
        body.len()
    );
    stream.write_all(head.as_bytes()).unwrap();
    stream
}

/// **A client that disconnects while its mutation runs leaves its vault's
/// turn held until the host call returns.** hyper drops the request's future
/// when the peer closes; a second mutation of the vault then waits for the
/// turn, and is dispatched only once the first call has returned, so the two
/// never run at once.
#[test]
fn a_disconnect_while_a_mutation_runs_keeps_the_vaults_turn_until_its_call_returns() {
    let (release, held) = std::sync::mpsc::channel();
    let gate = Arc::new(Gate {
        calls: Default::default(),
        running: Default::default(),
        widest: Default::default(),
        release: Mutex::new(held),
    });
    let mut case = case("norn-serve-wire-disconnect");
    case.context.wire = Arc::new(Wire {
        admission: Arc::default(),
        semaphores: Arc::default(),
        reads: crate::pool::Pool::new("test-read", 2).unwrap(),
        mutations: crate::pool::Pool::new("test-mutation", 2).unwrap(),
        dispatcher: Arc::clone(&gate) as Arc<dyn crate::request::Dispatch>,
        ready_wait: Duration::from_secs(10),
        counters: Arc::default(),
    });
    let wire = Arc::clone(&case.context.wire);
    let socket = serving(case);
    let notes = r#"{"vault":{"by":"name","name":"notes"}}"#;

    let first = post(&socket, "init", notes);
    wait_for("the first init runs", || {
        gate.running.load(Ordering::SeqCst) == 1
    });
    drop(first);
    wait_for("hyper drops the gone request", || {
        wire.admission.unanswered() == 1
    });
    let mut second = post(&socket, "init", notes);
    // The first's claim stays registered while its call runs, so a second
    // registration is the second init, admitted and waiting for the turn.
    wait_for("the second init waits for the turn", || {
        wire.admission.registered() == 2
    });
    assert_eq!(gate.calls.load(Ordering::SeqCst), 1);
    release.send(()).unwrap();
    drop(release);
    second
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut answer = [0u8; 512];
    let read = second.read(&mut answer).unwrap();
    let answer = String::from_utf8_lossy(&answer[..read]);
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert_eq!(gate.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        gate.widest.load(Ordering::SeqCst),
        1,
        "two mutations of one vault ran at once"
    );
}
