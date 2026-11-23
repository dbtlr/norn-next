use std::time::Duration;

use norn_wire::ReasonCode;
use serde_json::{Value, json};

use super::wire::{bearer, representatives};
use super::*;

/// A case serving every tool.
fn mcp_case(name: &str) -> Case {
    mcp_case_waiting(name, Duration::from_secs(5))
}

fn mcp_case_waiting(name: &str, ready_wait: Duration) -> Case {
    let mut case = case_waiting(name, ready_wait);
    case.context.mcp_tools =
        crate::mcp::prepare_tools(&case.context.mcp_counters, Verb::ALL, "0.0.0+test").unwrap();
    case
}

const LEGACY: &str = "2025-11-25";
const MODERN: &str = "2026-07-28";

/// A `/mcp` POST behind the bearer with `headers`.
fn mcp_post(headers: &[(&str, &str)], body: &[u8]) -> Request<Full<Bytes>> {
    let mut request = request(Method::POST, MCP_PATH, Some(&bearer()), body);
    for (name, value) in headers {
        request.headers_mut().insert(
            hyper::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
    }
    request
}

/// A 2025-era `tools/call` of `tool` with `arguments`.
fn legacy_call(tool: &str, arguments: Value) -> Request<Full<Bytes>> {
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": tool, "arguments": arguments},
    });
    mcp_post(
        &[("mcp-protocol-version", LEGACY)],
        body.to_string().as_bytes(),
    )
}

/// A 2026-07-28 request for `method` with `params`, its headers conforming.
fn modern(method: &str, name: Option<&str>, mut params: Value) -> Request<Full<Bytes>> {
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": MODERN,
        "io.modelcontextprotocol/clientCapabilities": {},
    });
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let mut headers = vec![("mcp-protocol-version", MODERN), ("mcp-method", method)];
    if let Some(name) = name {
        headers.push(("mcp-name", name));
    }
    mcp_post(&headers, body.to_string().as_bytes())
}

fn modern_call(tool: &str, arguments: Value) -> Request<Full<Bytes>> {
    modern(
        "tools/call",
        Some(tool),
        json!({"name": tool, "arguments": arguments}),
    )
}

fn legacy(method: &str) -> Request<Full<Bytes>> {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": {}});
    mcp_post(
        &[("mcp-protocol-version", LEGACY)],
        body.to_string().as_bytes(),
    )
}

/// An answer's status, headers and body.
fn answer_of(case: &Case, request: Request<Full<Bytes>>) -> (StatusCode, HeaderMap, Bytes) {
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
    let headers = answer.headers().clone();
    let body = runtime
        .block_on(answer.into_body().collect())
        .unwrap()
        .to_bytes();
    assert!(
        !headers.contains_key("mcp-session-id"),
        "no answer issues a session"
    );
    (status, headers, body)
}

/// A tool outcome: status 200 and its result.
fn tool_result(case: &Case, request: Request<Full<Bytes>>) -> Value {
    let (status, headers, body) = answer_of(case, request);
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(headers[CONTENT_TYPE], "application/json");
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert!(value.get("error").is_none(), "{value}");
    value["result"].clone()
}

fn dispatches(case: &Case) -> u64 {
    case.context.wire.counters.dispatches.load(Ordering::SeqCst)
}

fn vault() -> Value {
    json!({"by": "name", "name": "notes"})
}

// § HTTP, in its order.

#[test]
fn every_method_but_post_is_405_with_allow_post_and_no_body() {
    let case = mcp_case("norn-serve-mcp-405");
    for method in [Method::GET, Method::DELETE, Method::PUT, Method::PATCH] {
        let (status, headers, body) = answer_of(
            &case,
            request(method.clone(), MCP_PATH, Some(&bearer()), b""),
        );
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
        assert_eq!(headers[ALLOW], "POST", "{method}");
        assert!(body.is_empty(), "{method}");
        assert!(!headers.contains_key(CONTENT_TYPE), "{method}");
    }
}

/// The bearer runs before path matching, on `/mcp` as everywhere.
#[test]
fn a_request_without_a_valid_bearer_is_401_before_anything_else() {
    let case = mcp_case("norn-serve-mcp-401");
    for method in [Method::POST, Method::GET] {
        let mut request = request(method.clone(), MCP_PATH, Some("00ff"), b"{}");
        request
            .headers_mut()
            .insert(ORIGIN, HeaderValue::from_static("http://evil"));
        let (status, _, body) = answer_of(&case, request);
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method}");
        assert_eq!(code(&body), "serve/unauthorized");
    }
    assert_eq!(case.context.mcp_counters.messages.load(Ordering::SeqCst), 0);
}

#[test]
fn any_origin_header_is_403_with_no_body() {
    let case = mcp_case("norn-serve-mcp-403");
    for origin in ["http://localhost", "null", "http://127.0.0.1:28472"] {
        let mut request = legacy("tools/list");
        request
            .headers_mut()
            .insert(ORIGIN, HeaderValue::from_str(origin).unwrap());
        let (status, _, body) = answer_of(&case, request);
        assert_eq!(status, StatusCode::FORBIDDEN, "{origin}");
        assert!(body.is_empty());
    }
    assert_eq!(case.context.mcp_counters.messages.load(Ordering::SeqCst), 0);
}

#[test]
fn draining_and_busy_are_503_envelopes_with_retry_after_before_the_body() {
    let case = mcp_case("norn-serve-mcp-busy");
    let held: Vec<_> = (0..crate::admission::IN_FLIGHT_BOUND)
        .map(|_| case.context.wire.admission.enter().unwrap())
        .collect();
    let (status, headers, body) = answer_of(&case, legacy("tools/list"));
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(code(&body), "serve/busy");
    assert_eq!(headers[RETRY_AFTER], "1");
    assert_eq!(headers[CONNECTION], "close");
    drop(held);

    let mut over = legacy("tools/list");
    over.headers_mut().insert(
        CONTENT_LENGTH,
        HeaderValue::from(WIRE_BODY_BYTES as u64 + 1),
    );
    let (status, _, body) = answer_of(&case, over);
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(code(&body), "serve/request-too-large");

    case.context.wire.admission.begin_drain();
    let (status, headers, body) = answer_of(&case, legacy("tools/list"));
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(code(&body), "serve/draining");
    assert_eq!(headers[RETRY_AFTER], "1");
    assert_eq!(case.context.mcp_counters.messages.load(Ordering::SeqCst), 0);
    assert_eq!(dispatches(&case), 0);
}

// § Methods: constants cost no host work and no pool thread.

#[test]
fn constant_answers_give_their_slot_back_and_dispatch_nothing() {
    let case = mcp_case("norn-serve-mcp-constants");
    // One slot left: every constant answer must give it back to leave it.
    let held: Vec<_> = (0..crate::admission::IN_FLIGHT_BOUND - 1)
        .map(|_| case.context.wire.admission.enter().unwrap())
        .collect();
    let initialize = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": LEGACY, "capabilities": {},
                   "clientInfo": {"name": "t", "version": "1"}},
    });
    for _ in 0..3 {
        for (request, expected) in [
            (
                mcp_post(&[], initialize.to_string().as_bytes()),
                StatusCode::OK,
            ),
            (legacy("ping"), StatusCode::OK),
            (legacy("tools/list"), StatusCode::OK),
            (modern("server/discover", None, json!({})), StatusCode::OK),
            (modern("tools/list", None, json!({})), StatusCode::OK),
            (
                mcp_post(
                    &[],
                    br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                ),
                StatusCode::ACCEPTED,
            ),
            (mcp_post(&[], b"not json"), StatusCode::BAD_REQUEST),
        ] {
            let (status, _, _) = answer_of(&case, request);
            assert_eq!(status, expected);
            assert_eq!(
                case.context.wire.admission.in_flight(),
                crate::admission::IN_FLIGHT_BOUND - 1,
                "the slot is given back"
            );
        }
    }
    drop(held);
    assert_eq!(dispatches(&case), 0, "no pool work");
    assert!(case.answers.seen.lock().unwrap().is_empty(), "no host call");
    assert_eq!(
        case.context.mcp_counters.messages.load(Ordering::SeqCst),
        21
    );
    assert_eq!(case.context.mcp_counters.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        case.context
            .mcp_counters
            .tool_list_derivations
            .load(Ordering::SeqCst),
        1,
        "listing derives nothing again"
    );
}

/// A notification is 202 with no body and no content type.
#[test]
fn a_notification_is_202_with_no_body() {
    let case = mcp_case("norn-serve-mcp-notification");
    for body in [
        br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.as_slice(),
        br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}"#,
    ] {
        let (status, headers, answer) = answer_of(&case, mcp_post(&[], body));
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(answer.is_empty());
        assert!(!headers.contains_key(CONTENT_TYPE));
    }
}

/// `norn-mcp`'s status and bytes are written as it composed them.
#[test]
fn a_json_rpc_error_keeps_the_status_norn_mcp_gave_it() {
    let case = mcp_case("norn-serve-mcp-errors");
    let modern_initialize = modern("initialize", None, json!({}));
    for (request, status, code) in [
        (mcp_post(&[], b"{nope"), 400, -32700),
        (legacy("resources/list"), 200, -32601),
        (modern_initialize, 404, -32601),
        (
            mcp_post(&[], br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#),
            400,
            -32020,
        ),
        (legacy_call("nope", json!({})), 200, -32602),
    ] {
        let (answered, headers, body) = answer_of(&case, request);
        assert_eq!(answered.as_u16(), status);
        assert_eq!(headers[CONTENT_TYPE], "application/json");
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["code"], code, "{value}");
    }
    assert_eq!(dispatches(&case), 0);
}

/// 2026-07-28 metadata naming a field twice, sent with a 2025-11-25 header,
/// is refused as a structure fault and dispatches nothing: it is never read
/// as absent and served on the 2025-era path. With the duplicate removed, the
/// same request is refused for its conflicting header.
#[test]
fn a_meta_naming_a_field_twice_is_refused_never_served_as_2025_era() {
    let case = mcp_case("norn-serve-mcp-meta-twice");
    const CAPABILITIES: &str = r#""io.modelcontextprotocol/clientCapabilities":{}"#;
    let twice = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"vault_list","arguments":{{}},"_meta":{{"io.modelcontextprotocol/protocolVersion":"{MODERN}",{CAPABILITIES},{CAPABILITIES}}}}}}}"#
    );
    let once = twice.replacen(&format!(",{CAPABILITIES}"), "", 1);
    for (body, code) in [(&twice, -32600), (&once, -32020)] {
        let request = mcp_post(&[("mcp-protocol-version", LEGACY)], body.as_bytes());
        let (status, headers, answer) = answer_of(&case, request);
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(headers[CONTENT_TYPE], "application/json");
        let value: Value = serde_json::from_slice(&answer).unwrap();
        assert_eq!(value["error"]["code"], code, "{value}");
        assert_eq!(value["id"], 1, "{value}");
    }
    assert_eq!(dispatches(&case), 0);
}

// § Calls.

#[test]
fn both_routes_dispatch_only_verbs_with_wire_lanes() {
    // The fixture prepares tools from the whole vocabulary, including any
    // verbs the wire table excludes.
    let case = mcp_case("norn-serve-mcp-route-parity");
    let arguments = json!({
        "vault": vault(),
        "plan": {"vault": vault()},
        "registration": {"name": "notes"},
        "name": "notes",
    });
    for tool in &case.context.mcp_tools.tools {
        let before = dispatches(&case);
        let (native, _, body) = answer_of(
            &case,
            request(
                Method::POST,
                &format!("/v1/wire/{}", tool.verb),
                Some(&bearer()),
                arguments.to_string().as_bytes(),
            ),
        );
        let result = tool_result(&case, legacy_call(&tool.name, arguments.clone()));
        if native == StatusCode::NOT_FOUND {
            assert_eq!(code(&body), "serve/unknown-verb");
            assert_eq!(result["isError"], true, "{}", tool.name);
            assert_eq!(
                result["structuredContent"]["error"]["detail"]["code"],
                code(&body)
            );
            assert_eq!(dispatches(&case), before, "{}", tool.name);
        } else {
            assert_eq!(native, StatusCode::OK, "{}", tool.name);
            assert_eq!(result["structuredContent"], json!({}), "{}", tool.name);
            assert_eq!(dispatches(&case), before + 2, "{}", tool.name);
        }
    }
}

/// A call that meets a ready entry is exactly one dispatch, its report
/// composed into both channels; a warming entry is asked again by the
/// readiness retry, as on the wire route.
#[test]
fn a_ready_call_is_one_dispatch_and_a_warming_one_is_retried() {
    let case = mcp_case("norn-serve-mcp-one-dispatch");
    case.answers
        .push(Dispatched::Report(br#"{"report":"here"}"#.to_vec()));
    let result = tool_result(&case, legacy_call("find", json!({"vault": vault()})));
    assert_eq!(result["structuredContent"], json!({"report": "here"}));
    assert_eq!(result["content"][0]["text"], r#"{"report":"here"}"#);
    assert_eq!(dispatches(&case), 1);
    assert_eq!(
        case.context
            .wire
            .counters
            .ready_retries
            .load(Ordering::SeqCst),
        0
    );
    assert_eq!(
        case.context
            .wire
            .counters
            .report_bytes
            .load(Ordering::SeqCst),
        br#"{"report":"here"}"#.len() as u64,
        "the report's own length, not the result's"
    );
    case.answers.push(Dispatched::Refused(ErrorEnvelope::new(
        "warming",
        ErrorDetail::entry_not_ready(norn_wire::NotReady::warming(
            norn_wire::WarmingPhase::Healing,
            0,
            None,
        )),
    )));
    case.answers.push(Dispatched::Report(b"{}".to_vec()));
    let result = tool_result(&case, modern_call("find", json!({"vault": vault()})));
    assert_eq!(result["structuredContent"], json!({}));
    assert_eq!(result["resultType"], "complete");
    assert_eq!(dispatches(&case), 3);
    assert_eq!(
        case.context
            .wire
            .counters
            .ready_retries
            .load(Ordering::SeqCst),
        1
    );
    assert_eq!(case.context.mcp_counters.calls.load(Ordering::SeqCst), 2);
}

/// **Every class reaches an `isError` result carrying it.** Every code's
/// representative, answered by the host, comes back as status 200 with the
/// envelope whole and its class beside it, never as a JSON-RPC error; so do
/// the route's own refusals after admission.
#[test]
fn every_code_reaches_an_is_error_result_carrying_its_class() {
    let case = mcp_case_waiting("norn-serve-mcp-statuses", Duration::from_millis(200));
    let representatives = representatives();
    for code in ReasonCode::ALL {
        assert!(
            representatives
                .iter()
                .any(|(detail, _)| detail.code() == code)
        );
    }
    for (detail, _) in representatives {
        let sent = ErrorEnvelope::new("refused", detail);
        let retried = crate::request::is_warming(&sent);
        for request in [
            legacy_call("find", json!({"vault": vault()})),
            modern_call("find", json!({"vault": vault()})),
        ] {
            if retried {
                for _ in 0..20 {
                    case.answers.push(Dispatched::Refused(sent.clone()));
                }
            } else {
                case.answers.push(Dispatched::Refused(sent.clone()));
            }
            let result = tool_result(&case, request);
            assert_eq!(result["isError"], true, "{:?}", sent.code());
            assert_eq!(
                result["structuredContent"]["error"],
                serde_json::to_value(&sent).unwrap()
            );
            assert_eq!(
                result["structuredContent"]["class"],
                serde_json::to_value(sent.fault_class()).unwrap()
            );
            let text: Value =
                serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(text, result["structuredContent"]);
            case.answers.next.lock().unwrap().clear();
        }
    }

    // The route's own: a stopped worker, and a body the route table refused.
    case.answers
        .push(Dispatched::HostStopped("the worker stopped".into()));
    let result = tool_result(
        &case,
        legacy_call("vault_reload", json!({"vault": vault()})),
    );
    assert_eq!(result["isError"], true);
    assert_eq!(
        result["structuredContent"]["error"]["detail"]["code"],
        "serve/outcome-unknown"
    );
    assert_eq!(result["structuredContent"]["class"], "unavailable");
    assert!(
        case.context.stopper.triggered(),
        "a dead worker still stops"
    );
    case.answers
        .push(Dispatched::BadRequest("missing field `x`".into()));
    let result = tool_result(&case, legacy_call("find", json!({"vault": vault()})));
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["class"], "request");
    assert_eq!(
        result["structuredContent"]["error"]["detail"]["code"],
        "serve/bad-request"
    );
    assert!(
        result["structuredContent"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("missing field `x`")
    );
}

/// The drain's sweep refuses a waiting call in band.
#[test]
fn a_call_after_the_drain_began_is_refused_before_its_body() {
    let case = mcp_case("norn-serve-mcp-swept");
    case.context.wire.admission.begin_drain();
    let (status, _, body) = answer_of(&case, legacy_call("find", json!({})));
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(code(&body), "serve/draining");
}

/// Arguments the pre-dispatch reads cannot use are an `isError`
/// `serve/bad-request` at 200, and nothing is dispatched.
#[test]
fn a_mutation_without_a_readable_vault_is_a_bad_request_result() {
    let case = mcp_case("norn-serve-mcp-no-vault");
    for (tool, arguments) in [
        ("set", json!({})),
        ("set_preview", json!({"vault": "notes"})),
        (
            "vault_reload",
            json!({"vault": {"by": "name", "name": "Not A Name"}}),
        ),
        ("apply", json!({"plan": {}})),
    ] {
        let result = tool_result(&case, legacy_call(tool, arguments));
        assert_eq!(result["isError"], true, "{tool}");
        assert_eq!(result["structuredContent"]["class"], "request", "{tool}");
        assert_eq!(
            result["structuredContent"]["error"]["detail"]["code"], "serve/bad-request",
            "{tool}"
        );
    }
    assert_eq!(dispatches(&case), 0);
    assert_eq!(case.context.wire.admission.in_flight(), 0);
}

/// **The mode is the tool's.** A caller-supplied `mode` is refused for all
/// ten mode tools and their previews, in either direction, and never reaches
/// the host; without one, the host sees the tool's mode.
#[test]
fn a_caller_supplied_mode_is_refused_for_every_mode_tool() {
    let case = mcp_case("norn-serve-mcp-mode");
    let mode_tools: Vec<_> = case
        .context
        .mcp_tools
        .tools
        .iter()
        .filter(|tool| tool.mode.is_some())
        .cloned()
        .collect();
    assert_eq!(mode_tools.len(), 20);
    let mut verbs: Vec<Verb> = mode_tools.iter().map(|tool| tool.verb).collect();
    verbs.dedup();
    assert_eq!(verbs.len(), 10, "every mode verb");
    for tool in &mode_tools {
        let arguments = |mode: Option<&str>| {
            let mut arguments = match tool.verb {
                Verb::Apply => json!({"plan": {"plan": "operations", "vault": vault()}}),
                _ => json!({"vault": vault()}),
            };
            if let Some(mode) = mode {
                arguments["mode"] = json!(mode);
            }
            arguments
        };
        let before = dispatches(&case);
        for supplied in ["apply", "preview"] {
            let result = tool_result(&case, legacy_call(&tool.name, arguments(Some(supplied))));
            assert_eq!(result["isError"], true, "{} {supplied}", tool.name);
            assert_eq!(
                result["structuredContent"]["error"]["detail"]["code"],
                "serve/bad-request"
            );
        }
        assert_eq!(dispatches(&case), before, "{}: no dispatch", tool.name);
        assert!(
            case.answers.seen.lock().unwrap().is_empty(),
            "{}: a refused mode reaches no host",
            tool.name
        );
        tool_result(&case, modern_call(&tool.name, arguments(None)));
        let (verb, body) = case.answers.seen.lock().unwrap().pop().unwrap();
        assert_eq!(verb, tool.verb);
        let mut body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["mode"], serde_json::to_value(tool.mode).unwrap());
        body.as_object_mut().unwrap().remove("mode");
        assert_eq!(body, arguments(None), "{}: the rest as sent", tool.name);
    }
}

/// **The drain-bound class of every tool is its wire call's**, read with the
/// tool's mode standing in: `init_preview` and `vault_migrate_preview` close
/// with the process, their apply tools are answered unknown, and
/// `vault_reload` reads its own `dry_run` either way.
#[test]
fn every_tool_takes_its_wire_calls_drain_bound_class() {
    use crate::request::AtBound;
    let case = mcp_case("norn-serve-mcp-at-bound");
    let address = r#"{"by":"name","name":"notes"}"#;
    for tool in &case.context.mcp_tools.tools {
        let arguments = match tool.verb {
            Verb::Apply => format!(r#"{{"plan":{{"plan":"operations","vault":{address}}}}}"#),
            Verb::VaultRegister => r#"{"registration":{"name":"notes","root":"/v"}}"#.into(),
            Verb::VaultUnregister | Verb::VaultSet => r#"{"name":"notes"}"#.into(),
            _ => format!(r#"{{"vault":{address}}}"#),
        };
        let runs = crate::wire::lane(tool.verb).unwrap();
        let (_, at_bound) =
            crate::mcp::pre_dispatch(tool.verb, runs, tool.mode, arguments.as_bytes()).unwrap();
        let wire_body = norn_mcp::wire_body(arguments.as_bytes(), tool.mode).unwrap();
        assert_eq!(
            at_bound,
            crate::wire::at_bound(tool.verb, &wire_body),
            "{}",
            tool.name
        );
        let expected = match tool.name.as_str() {
            "init_preview" | "vault_migrate_preview" => Some(AtBound::CloseWithProcess),
            "init" | "vault_migrate" | "vault_reload" => Some(AtBound::OutcomeUnknown),
            _ => None,
        };
        if let Some(expected) = expected {
            assert_eq!(at_bound, expected, "{}", tool.name);
        }
    }
    for (dry_run, expected) in [
        ("true", AtBound::CloseWithProcess),
        ("false", AtBound::OutcomeUnknown),
    ] {
        let arguments = format!(r#"{{"vault":{address},"dry_run":{dry_run}}}"#);
        let runs = crate::wire::lane(Verb::VaultReload).unwrap();
        let (_, at_bound) =
            crate::mcp::pre_dispatch(Verb::VaultReload, runs, None, arguments.as_bytes()).unwrap();
        assert_eq!(at_bound, expected, "dry_run {dry_run}");
    }
}

/// Under the counters feature a call's answer carries the wire route's host
/// time and its translation time, and the counters route serves the MCP
/// counters. A mode tool's translation also counts its mode check, which runs
/// before the host-time window opens, so only a tool without a mode keeps its
/// translation inside the host time; `find` has none.
#[cfg(feature = "counters")]
#[test]
fn a_call_carries_its_host_and_translation_time_and_the_counters_serve_mcp() {
    let case = mcp_case("norn-serve-mcp-host-time");
    let (_, headers, _) = answer_of(&case, legacy_call("find", json!({"vault": vault()})));
    let micros = |name: &str| -> u64 { headers[name].to_str().unwrap().parse().unwrap() };
    assert!(micros(TRANSLATE_HEADER) <= micros(HOST_TIME_HEADER));
    let (_, headers, _) = answer_of(&case, legacy("tools/list"));
    assert!(
        !headers.contains_key(HOST_TIME_HEADER),
        "nothing was dispatched"
    );
    let (status, _, body) = answer_of(
        &case,
        request(Method::GET, "/v1/debug/counters", Some(&bearer()), b""),
    );
    assert_eq!(status, StatusCode::OK);
    let counters: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(counters["mcp.tool_list_derivations"], 1);
    assert_eq!(counters["mcp.messages"], 2);
    assert_eq!(counters["mcp.calls"], 1);
    assert!(counters["mcp.translate_us"].is_u64());
}

/// A call refused before the wire run still counts its translation: the mode
/// check and the refusal it composes. Its answer carries no timing header,
/// since it never entered the run.
#[cfg(feature = "counters")]
#[test]
fn a_call_refused_before_the_run_counts_its_translation() {
    let case = mcp_case("norn-serve-mcp-early-translation");
    let translated = || {
        case.context
            .mcp_counters
            .translate_ns
            .load(Ordering::SeqCst)
    };
    for arguments in [
        json!({"vault": vault(), "mode": "apply"}),
        json!({"vault": 5}),
    ] {
        let before = translated();
        let (_, headers, _) = answer_of(&case, legacy_call("set_preview", arguments.clone()));
        assert!(!headers.contains_key(TRANSLATE_HEADER), "{arguments}");
        assert!(
            translated() > before,
            "{arguments}: translation was counted"
        );
    }
}

/// The request log names `mcp` for an answer from constants and
/// `mcp/<verb>` for a call, spelled from the resolved verb.
#[test]
fn the_request_log_labels_mcp_and_its_calls() {
    let mut case = mcp_case("norn-serve-mcp-log");
    case.context.request_log = true;
    let line = |case: &Case, request| {
        let ((), lines) = crate::log::capture::lines_of(|| {
            answer_of(case, request);
        });
        assert_eq!(lines.len(), 1, "{lines:?}");
        lines[0].clone()
    };
    let logged = line(&case, legacy("tools/list"));
    assert!(logged.starts_with("request: mcp 200 ok "), "{logged}");
    let logged = line(&case, mcp_post(&[], b"{nope"));
    assert!(
        logged.starts_with("request: mcp 400 jsonrpc/-32700 "),
        "{logged}"
    );
    let logged = line(&case, legacy_call("find", json!({"vault": vault()})));
    assert!(logged.starts_with("request: mcp/find 200 ok "), "{logged}");
    let logged = line(
        &case,
        legacy_call("set_preview", json!({"vault": vault(), "mode": "x"})),
    );
    assert!(
        logged.starts_with("request: mcp/set 200 serve/bad-request "),
        "{logged}"
    );
    let logged = line(&case, legacy_call("set_preview", json!({"vault": 5})));
    assert!(
        logged.starts_with("request: mcp/set 200 serve/bad-request "),
        "{logged}"
    );
    case.answers.push(Dispatched::Refused(ErrorEnvelope::new(
        "unknown",
        ErrorDetail::unknown_vault(norn_wire::VaultName::new("notes").unwrap()),
    )));
    let logged = line(&case, legacy_call("find", json!({"vault": vault()})));
    assert!(
        logged.starts_with("request: mcp/find 200 host/unknown-vault "),
        "{logged}"
    );
    let logged = line(&case, request(Method::GET, MCP_PATH, Some(&bearer()), b""));
    assert!(logged.starts_with("request: mcp 405 ok "), "{logged}");
}
