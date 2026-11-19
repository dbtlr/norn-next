//! Every message `/mcp` reads and every answer it composes, at the
//! translation layer: the envelope's structure, telling the two revisions
//! apart, the method table, the error and status table, and a call's result.
//! Every emitted message is validated against its revision's vendored schema.

mod fixtures;

use fixtures::assert_valid;
use norn_mcp::{
    Answer, Headers, LEGACY_VERSION, MODERN_VERSION, Message, PreparedToolLists, Revision,
    ToolList, handle,
};
use norn_wire::{ApplyMode, ErrorDetail, ErrorEnvelope, FaultClass, UntrustedReason, Verb};
use serde_json::{Value, json};

const LEGACY: &str = LEGACY_VERSION;
const MODERN: &str = MODERN_VERSION;

fn prepared() -> &'static PreparedToolLists {
    static PREPARED: std::sync::OnceLock<PreparedToolLists> = std::sync::OnceLock::new();
    PREPARED.get_or_init(|| ToolList::derive(Verb::ALL).unwrap().prepare("0.0.0+test"))
}

/// The header set a request is sent with.
#[derive(Clone, Copy, Default)]
struct Sent<'a> {
    version: Option<&'a str>,
    method: Option<&'a str>,
    name: Option<&'a str>,
}

impl<'a> Sent<'a> {
    fn headers(&self) -> Headers<'a> {
        Headers {
            protocol_version: self.version.map(str::as_bytes),
            method: self.method.map(str::as_bytes),
            name: self.name.map(str::as_bytes),
        }
    }
}

/// A 2025-era request's headers after `initialize`.
fn legacy_headers<'a>() -> Sent<'a> {
    Sent {
        version: Some(LEGACY),
        ..Sent::default()
    }
}

/// A conforming 2026-07-28 request's headers for `method`, naming `name`.
fn modern_headers<'a>(method: &'a str, name: Option<&'a str>) -> Sent<'a> {
    Sent {
        version: Some(MODERN),
        method: Some(method),
        name,
    }
}

/// The metadata every 2026-07-28 request carries.
fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": MODERN,
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "1"},
        "io.modelcontextprotocol/logLevel": "info",
    })
}

fn request(id: Value, method: &str, params: Value) -> Vec<u8> {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        .to_string()
        .into_bytes()
}

fn modern_request(method: &str, mut params: Value) -> Vec<u8> {
    params["_meta"] = meta();
    request(json!(1), method, params)
}

fn answered(sent: Sent<'_>, body: &[u8]) -> Answer {
    match handle(prepared(), sent.headers(), body) {
        Message::Answered(answer) => answer,
        Message::Call(call) => panic!("answered as a call to {}", call.verb),
    }
}

fn body(answer: &Answer) -> Value {
    serde_json::from_slice(&answer.body).unwrap()
}

fn revision_name(revision: Revision) -> &'static str {
    match revision {
        Revision::Legacy => LEGACY,
        Revision::Modern => MODERN,
    }
}

/// Assert `answer` is the JSON-RPC error `code` at `status`, valid in each
/// of `revisions`, with `id` echoed or omitted, never `null`.
fn assert_error(answer: &Answer, status: u16, code: i32, id: Option<Value>, revisions: &[&str]) {
    let value = body(answer);
    assert_eq!(answer.status, status, "{value}");
    assert_eq!(answer.error_code, Some(code), "{value}");
    assert_eq!(value["error"]["code"], code, "{value}");
    assert_eq!(value.get("id").cloned(), id, "{value}");
    assert!(value.get("id") != Some(&Value::Null), "{value}");
    // A JSON-RPC error never carries a wire code.
    let text = value.to_string();
    for namespace in ["serve/", "host/", "vault/", "engine/", "request/"] {
        assert!(!text.contains(namespace), "{value}");
    }
    for revision in revisions {
        assert_valid(revision, "JSONRPCErrorResponse", &value);
        assert_valid(revision, "JSONRPCMessage", &value);
    }
}

/// Assert `answer` is a 200 result valid as `definition` under `revision`,
/// and answer the result.
fn assert_result(answer: &Answer, revision: &str, definition: &str) -> Value {
    let value = body(answer);
    assert_eq!(answer.status, 200, "{value}");
    assert_eq!(answer.error_code, None, "{value}");
    assert_valid(revision, "JSONRPCResultResponse", &value);
    assert_valid(revision, "JSONRPCMessage", &value);
    assert_valid(revision, definition, &value["result"]);
    if revision == MODERN {
        assert_eq!(value["result"]["resultType"], "complete");
        assert_eq!(
            value["result"]["_meta"]["io.modelcontextprotocol/serverInfo"],
            json!({"name": "norn", "version": "0.0.0+test"})
        );
    }
    value["result"].clone()
}

// § Errors, row by row, in both revisions.

#[test]
fn a_body_that_is_not_json_is_a_parse_error_with_no_id() {
    for sent in [legacy_headers(), modern_headers("tools/list", None)] {
        for body in [b"{not json".as_slice(), b"", b"{\"id\":1,", b"{} trailing"] {
            assert_error(&answered(sent, body), 400, -32700, None, &[LEGACY, MODERN]);
        }
    }
}

#[test]
fn an_invalid_request_structure_is_refused_with_its_id_where_it_reads() {
    let cases: [(&[u8], Option<Value>); 10] = [
        (br#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#, None),
        (br#""a string""#, None),
        (
            br#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#,
            Some(json!(1)),
        ),
        (br#"{"id":"a","method":"ping"}"#, Some(json!("a"))),
        (br#"{"jsonrpc":"2.0","id":2}"#, Some(json!(2))),
        (br#"{"jsonrpc":"2.0","id":2,"method":7}"#, Some(json!(2))),
        (br#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#, None),
        (br#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#, None),
        (
            br#"{"jsonrpc":"2.0","id":3,"method":"ping","params":[]}"#,
            Some(json!(3)),
        ),
        (
            br#"{"jsonrpc":"2.0","id":3,"method":"ping","params":null}"#,
            Some(json!(3)),
        ),
    ];
    for sent in [legacy_headers(), modern_headers("ping", None)] {
        for (body, id) in &cases {
            assert_error(
                &answered(sent, body),
                400,
                -32600,
                id.clone(),
                &[LEGACY, MODERN],
            );
        }
    }
}

#[test]
fn a_2025_request_with_another_legacy_version_is_an_invalid_request() {
    for version in ["2025-06-18", "2025-03-26", "2024-11-05"] {
        let sent = Sent {
            version: Some(version),
            ..Sent::default()
        };
        let answer = answered(sent, &request(json!(4), "tools/list", json!({})));
        assert_error(&answer, 400, -32600, Some(json!(4)), &[LEGACY]);
    }
}

/// A headerless request is not assumed to be 2025-03-26: this server does not
/// speak it, so the header is a missing required header.
#[test]
fn a_2025_request_without_the_version_header_is_a_header_mismatch() {
    for method in ["tools/list", "tools/call", "ping", "resources/list"] {
        let answer = answered(Sent::default(), &request(json!(5), method, json!({})));
        assert_error(&answer, 400, -32020, Some(json!(5)), &[LEGACY]);
    }
}

#[test]
fn a_modern_request_missing_a_required_meta_field_is_invalid_params() {
    let without = |field: &str| {
        let mut meta = meta();
        meta.as_object_mut().unwrap().remove(field);
        meta
    };
    let bodies = [
        // Modern by its header alone, with no metadata.
        request(json!(6), "tools/list", json!({})),
        request(
            json!(6),
            "tools/list",
            json!({"_meta": without("io.modelcontextprotocol/clientCapabilities")}),
        ),
        request(
            json!(6),
            "tools/list",
            json!({"_meta": without("io.modelcontextprotocol/protocolVersion")}),
        ),
    ];
    for body in bodies {
        let answer = answered(modern_headers("tools/list", None), &body);
        assert_error(&answer, 400, -32602, Some(json!(6)), &[MODERN]);
    }
    // A header naming a version this server does not know is modern too.
    let sent = Sent {
        version: Some("2099-01-01"),
        method: Some("tools/list"),
        name: None,
    };
    let answer = answered(sent, &request(json!(6), "tools/list", json!({})));
    assert_error(&answer, 400, -32602, Some(json!(6)), &[MODERN]);
}

/// A `_meta` that is present but does not read is refused as a structure
/// fault before the revision is told, whatever header it is sent with: read
/// as absent, a 2026-07-28 request would be served on the 2025-era path past
/// its header checks.
#[test]
fn a_meta_that_does_not_read_is_an_invalid_request_in_either_revision() {
    const CAPABILITIES: &str = r#""io.modelcontextprotocol/clientCapabilities":{}"#;
    let twice = format!(
        r#"{{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{{"name":"vault_list","arguments":{{}},"_meta":{{"io.modelcontextprotocol/protocolVersion":"{MODERN}",{CAPABILITIES},{CAPABILITIES}}}}}}}"#
    );
    let version_twice = format!(
        r#"{{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{{"name":"vault_list","_meta":{{"io.modelcontextprotocol/protocolVersion":"{MODERN}","io.modelcontextprotocol/protocolVersion":"{MODERN}",{CAPABILITIES}}}}}}}"#
    );
    let mut bodies = vec![twice.clone().into_bytes(), version_twice.into_bytes()];
    for meta in [json!(5), json!(null), json!("x"), json!([MODERN, {}])] {
        bodies.push(request(
            json!(8),
            "tools/call",
            json!({"name": "vault_list", "_meta": meta}),
        ));
    }
    let header_sets = [
        // The reproduction: a 2025-11-25 header against 2026-07-28 metadata.
        legacy_headers(),
        modern_headers("tools/call", Some("vault_list")),
        Sent::default(),
    ];
    for sent in header_sets {
        for body in &bodies {
            let answer = answered(sent, body);
            assert_error(&answer, 400, -32600, Some(json!(8)), &[LEGACY, MODERN]);
        }
    }
    // With the duplicate removed, the same request is modern and refused for
    // its conflicting header.
    let once = twice.replacen(&format!(",{CAPABILITIES}"), "", 1);
    assert_ne!(once, twice);
    let answer = answered(legacy_headers(), once.as_bytes());
    assert_error(&answer, 400, -32020, Some(json!(8)), &[MODERN]);
}

/// A `_meta` protocol version that is present but not a string is still
/// carried: the request is 2026-07-28, refused for the version it lacks,
/// never served as 2025-era.
#[test]
fn a_meta_version_that_is_not_a_string_still_makes_a_request_modern() {
    for version in [json!(null), json!(20260728), json!({}), json!([MODERN])] {
        let mut meta = meta();
        meta["io.modelcontextprotocol/protocolVersion"] = version;
        let body = request(
            json!(9),
            "tools/call",
            json!({"name": "vault_list", "_meta": meta}),
        );
        for sent in [legacy_headers(), Sent::default()] {
            let answer = answered(sent, &body);
            assert_error(&answer, 400, -32602, Some(json!(9)), &[MODERN]);
        }
    }
}

#[test]
fn an_unsupported_meta_version_lists_every_supported_version() {
    for requested in ["2027-01-01", "2025-11-25"] {
        let mut meta = meta();
        meta["io.modelcontextprotocol/protocolVersion"] = json!(requested);
        let body = request(json!(7), "tools/list", json!({"_meta": meta}));
        let sent = Sent {
            version: Some(requested),
            method: Some("tools/list"),
            name: None,
        };
        let answer = answered(sent, &body);
        assert_error(&answer, 400, -32022, Some(json!(7)), &[MODERN]);
        assert_valid(MODERN, "UnsupportedProtocolVersionError", &body_of(&answer));
        assert_eq!(
            body_of(&answer)["error"]["data"],
            json!({"supported": [MODERN, LEGACY], "requested": requested})
        );
    }
}

fn body_of(answer: &Answer) -> Value {
    body(answer)
}

#[test]
fn a_header_that_disagrees_with_the_body_or_is_missing_is_a_header_mismatch() {
    let call = modern_request("tools/call", json!({"name": "find", "arguments": {}}));
    let list = modern_request("tools/list", json!({}));
    let cases: [(Sent<'_>, &Vec<u8>); 8] = [
        (
            Sent {
                version: None,
                ..modern_headers("tools/list", None)
            },
            &list,
        ),
        (
            Sent {
                version: Some("2099-01-01"),
                ..modern_headers("tools/list", None)
            },
            &list,
        ),
        (
            Sent {
                method: None,
                ..modern_headers("tools/list", None)
            },
            &list,
        ),
        (modern_headers("tools/call", None), &list),
        (modern_headers("tools/call", None), &call),
        (modern_headers("tools/call", Some("get")), &call),
        (modern_headers("tools/call", Some("=?base64?Z2V0?=")), &call),
        (
            modern_headers("tools/call", Some("=?base64?not base64?=")),
            &call,
        ),
    ];
    for (sent, body) in cases {
        let answer = answered(sent, body);
        assert_error(&answer, 400, -32020, Some(json!(1)), &[MODERN]);
        assert_valid(MODERN, "HeaderMismatchError", &body_of(&answer));
    }
    // The base64 form of the right name is the name.
    let Message::Call(call) = handle(
        prepared(),
        modern_headers("tools/call", Some("=?base64?ZmluZA==?=")).headers(),
        &call,
    ) else {
        panic!("a base64 name that decodes to the tool's is accepted");
    };
    assert_eq!(call.verb, Verb::Find);
}

#[test]
fn an_unknown_tool_bad_arguments_or_a_cursor_is_invalid_params_at_200() {
    let modern = |method: &str, params: Value| modern_request(method, params);
    for (name, params) in [
        ("nope", json!({"name": "nope"})),
        ("find", json!({"name": "find", "arguments": []})),
        ("find", json!({"name": "find", "arguments": "{}"})),
        ("find", json!({"name": "find", "arguments": null})),
        ("Find", json!({"name": "Find"})),
    ] {
        let answer = answered(
            legacy_headers(),
            &request(json!(8), "tools/call", params.clone()),
        );
        assert_error(&answer, 200, -32602, Some(json!(8)), &[LEGACY]);
        let answer = answered(
            modern_headers("tools/call", Some(name)),
            &modern("tools/call", params),
        );
        assert_error(&answer, 200, -32602, Some(json!(1)), &[MODERN]);
    }
    let answer = answered(
        legacy_headers(),
        &request(json!(8), "tools/list", json!({"cursor": "x"})),
    );
    assert_error(&answer, 200, -32602, Some(json!(8)), &[LEGACY]);
    let answer = answered(
        modern_headers("tools/list", None),
        &modern("tools/list", json!({"cursor": "x"})),
    );
    assert_error(&answer, 200, -32602, Some(json!(1)), &[MODERN]);
}

#[test]
fn an_unknown_method_is_method_not_found_200_legacy_and_404_modern() {
    for method in [
        "logging/setLevel",
        "subscriptions/listen",
        "resources/list",
        "resources/read",
        "prompts/list",
        "completion/complete",
        "server/discover",
    ] {
        let answer = answered(legacy_headers(), &request(json!(9), method, json!({})));
        assert_error(&answer, 200, -32601, Some(json!(9)), &[LEGACY]);
    }
    for method in [
        "initialize",
        "ping",
        "logging/setLevel",
        "resources/list",
        "prompts/list",
        "completion/complete",
    ] {
        let answer = answered(
            modern_headers(method, None),
            &modern_request(method, json!({})),
        );
        assert_error(&answer, 404, -32601, Some(json!(1)), &[MODERN]);
    }
}

// § Methods.

#[test]
fn initialize_negotiates_2025_11_25_whatever_it_requests_or_carries() {
    for (requested, header) in [
        (LEGACY, None),
        ("2024-11-05", None),
        ("2099-01-01", None),
        (LEGACY, Some("2025-06-18")),
        (LEGACY, Some(LEGACY)),
    ] {
        let params = json!({
            "protocolVersion": requested,
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "1"},
        });
        let sent = Sent {
            version: header,
            ..Sent::default()
        };
        let answer = answered(sent, &request(json!("init"), "initialize", params));
        let result = assert_result(&answer, LEGACY, "InitializeResult");
        assert_eq!(body(&answer)["id"], "init");
        assert_eq!(
            result,
            json!({
                "protocolVersion": LEGACY,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "norn", "version": "0.0.0+test"},
            })
        );
    }
}

#[test]
fn ping_server_discover_and_tools_list_are_answered_from_constants() {
    let result = assert_result(
        &answered(legacy_headers(), &request(json!(1), "ping", json!({}))),
        LEGACY,
        "EmptyResult",
    );
    assert_eq!(result, json!({}));
    let result = assert_result(
        &answered(
            modern_headers("server/discover", None),
            &modern_request("server/discover", json!({})),
        ),
        MODERN,
        "DiscoverResult",
    );
    assert_eq!(result["supportedVersions"], json!([MODERN, LEGACY]));
    assert_eq!(result["capabilities"], json!({"tools": {}}));
    assert_eq!(result["ttlMs"], 300_000);
    assert_eq!(result["cacheScope"], "public");

    let result = assert_result(
        &answered(
            legacy_headers(),
            &request(json!(1), "tools/list", json!({})),
        ),
        LEGACY,
        "ListToolsResult",
    );
    assert_eq!(result["tools"].as_array().unwrap().len(), 34);
    assert!(result.get("nextCursor").is_none());
    // `params` may be absent on a 2025-era request.
    let bare = br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    assert_result(&answered(legacy_headers(), bare), LEGACY, "ListToolsResult");
    let result = assert_result(
        &answered(
            modern_headers("tools/list", None),
            &modern_request("tools/list", json!({})),
        ),
        MODERN,
        "ListToolsResult",
    );
    assert_eq!(result["tools"].as_array().unwrap().len(), 34);
    assert_eq!(result["ttlMs"], 300_000);
    assert_eq!(result["cacheScope"], "public");
}

/// Supported notifications are accepted without a body.
#[test]
fn a_notification_is_accepted_with_202_and_no_body() {
    for method in [
        "notifications/initialized",
        "notifications/cancelled",
        "notifications/anything",
    ] {
        let body =
            json!({"jsonrpc": "2.0", "method": method, "params": {"requestId": 3}}).to_string();
        for sent in [
            Sent::default(),
            legacy_headers(),
            modern_headers(method, None),
        ] {
            let answer = answered(sent, body.as_bytes());
            assert_eq!(answer.status, 202);
            assert!(answer.body.is_empty());
            assert_eq!(answer.error_code, None);
        }
    }
}

#[test]
fn a_notification_with_an_unsupported_version_is_refused() {
    for version in ["2025-06-18", "2099-01-01"] {
        let answer = answered(
            Sent {
                version: Some(version),
                ..Sent::default()
            },
            br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        );
        assert_error(&answer, 400, -32600, None, &[LEGACY, MODERN]);
    }
}

// § Calls.

#[test]
fn every_served_tool_resolves_to_its_verb_and_mode_in_both_revisions() {
    let list = ToolList::derive(Verb::ALL).unwrap();
    for tool in &list.tools {
        let arguments = json!({"vault": {"by": "name", "name": "notes"}, "z": [1, {"a": 2}]});
        for revision in [Revision::Legacy, Revision::Modern] {
            let (sent, body) = match revision {
                Revision::Legacy => (
                    legacy_headers(),
                    request(
                        json!(10),
                        "tools/call",
                        json!({"name": tool.name, "arguments": arguments}),
                    ),
                ),
                Revision::Modern => (
                    modern_headers("tools/call", Some(&tool.name)),
                    modern_request(
                        "tools/call",
                        json!({"name": tool.name, "arguments": arguments}),
                    ),
                ),
            };
            let Message::Call(call) = handle(prepared(), sent.headers(), &body) else {
                panic!("{} is a call", tool.name);
            };
            assert_eq!(call.verb, tool.verb, "{}", tool.name);
            assert_eq!(call.mode, tool.mode, "{}", tool.name);
            assert_eq!(call.reply.revision(), revision);
            // The arguments are the bytes as they arrived, a slice of the body.
            let text = std::str::from_utf8(call.arguments).unwrap();
            assert_eq!(serde_json::from_str::<Value>(text).unwrap(), arguments);
            let at = call.arguments.as_ptr() as usize - body.as_ptr() as usize;
            assert_eq!(&body[at..at + call.arguments.len()], call.arguments);
        }
    }
    // Absent arguments read as `{}`.
    let body = request(json!(10), "tools/call", json!({"name": "vault_list"}));
    let Message::Call(call) = handle(prepared(), legacy_headers().headers(), &body) else {
        panic!("a call");
    };
    assert_eq!(call.arguments, b"{}");
}

fn call_reply(revision: Revision) -> norn_mcp::Reply {
    let (sent, body) = match revision {
        Revision::Legacy => (
            legacy_headers(),
            request(json!("c"), "tools/call", json!({"name": "set_preview"})),
        ),
        Revision::Modern => (modern_headers("tools/call", Some("set_preview")), {
            let mut params = json!({"name": "set_preview"});
            params["_meta"] = meta();
            request(json!("c"), "tools/call", params)
        }),
    };
    match handle(prepared(), sent.headers(), &body) {
        Message::Call(call) => {
            assert_eq!(call.mode, Some(ApplyMode::Preview));
            call.reply
        }
        Message::Answered(answer) => panic!("{}", String::from_utf8_lossy(&answer.body)),
    }
}

/// A report reaches both channels: `structuredContent` is the report
/// verbatim, and the text block is the same JSON as a string.
#[test]
fn a_report_is_composed_into_both_channels_from_its_bytes() {
    let report = br#"{"reading":{"trust":{"state":"ready"}},"unsatisfied":[{"unsatisfied":"unknown_sort_key","key":"t\u00e9","did_you_mean":["\"q\"\\"]}],"advisories":[],"report":{"outcome":"previewed","forecast":{"folders_made":[]}}}"#;
    for revision in [Revision::Legacy, Revision::Modern] {
        let reply = call_reply(revision);
        let answer = Answer {
            status: 200,
            body: reply.report(report),
            error_code: None,
        };
        let result = assert_result(&answer, revision_name(revision), "CallToolResult");
        let expected: Value = serde_json::from_slice(report).unwrap();
        assert_eq!(result["structuredContent"], expected);
        assert_eq!(result["content"].as_array().unwrap().len(), 1);
        assert_eq!(result["content"][0]["type"], "text");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert_eq!(
            text.as_bytes(),
            report,
            "the text is the report's own bytes"
        );
        assert!(result.get("isError").is_none());
        // structuredContent is the report verbatim, byte for byte.
        let needle = br#""structuredContent":"#;
        let at = answer
            .body
            .windows(needle.len())
            .position(|window| window == needle)
            .unwrap()
            + needle.len();
        assert_eq!(&answer.body[at..at + report.len()], report);
    }
}

fn one_envelope_per_class() -> Vec<(FaultClass, ErrorEnvelope)> {
    let details = [
        ErrorDetail::bad_request(),
        ErrorDetail::unauthorized(),
        ErrorDetail::unknown_verb(),
        ErrorDetail::drain_refused(),
        ErrorDetail::request_too_large(),
        ErrorDetail::busy(),
        ErrorDetail::token_file_unreadable(),
        ErrorDetail::entry_untrusted(UntrustedReason::leg_unwound("panicked")),
    ];
    let envelopes: Vec<_> = details
        .into_iter()
        .map(|detail| {
            let envelope = ErrorEnvelope::new("refused", detail);
            (envelope.fault_class(), envelope)
        })
        .collect();
    let classes: Vec<_> = envelopes.iter().map(|(class, _)| *class).collect();
    assert_eq!(classes, FaultClass::ALL, "one envelope per class, in order");
    envelopes
}

/// **A wire failure is an `isError` result carrying its class**, never a
/// JSON-RPC error: the envelope whole, its code unlaundered, and the class
/// beside it in band.
#[test]
fn every_fault_class_reaches_an_is_error_result_carrying_it() {
    for revision in [Revision::Legacy, Revision::Modern] {
        let reply = call_reply(revision);
        for (class, envelope) in one_envelope_per_class() {
            let answer = Answer {
                status: 200,
                body: reply.failure(&envelope),
                error_code: None,
            };
            let result = assert_result(&answer, revision_name(revision), "CallToolResult");
            assert_eq!(result["isError"], true);
            let structured = &result["structuredContent"];
            assert_eq!(
                structured["error"],
                serde_json::to_value(&envelope).unwrap()
            );
            assert_eq!(structured["class"], serde_json::to_value(class).unwrap());
            let text: Value =
                serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(&text, structured);
            assert!(body(&answer).get("error").is_none(), "not a JSON-RPC error");
        }
        let result = assert_result(
            &Answer {
                status: 200,
                body: reply.bad_request("no vault"),
                error_code: None,
            },
            revision_name(revision),
            "CallToolResult",
        );
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"]["class"], "request");
        assert_eq!(
            result["structuredContent"]["error"]["detail"]["code"],
            "serve/bad-request"
        );
    }
}

/// The mode is the tool's: a caller-supplied `mode` is refused for every mode
/// tool, in either direction, and a tool that takes no mode passes its
/// arguments through untouched.
#[test]
fn a_caller_supplied_mode_is_refused_for_every_mode_tool() {
    let list = ToolList::derive(Verb::ALL).unwrap();
    let mode_tools: Vec<_> = list
        .tools
        .iter()
        .filter(|tool| tool.mode.is_some())
        .collect();
    assert_eq!(mode_tools.len(), 20, "ten verbs, each with its preview");
    for tool in mode_tools {
        for supplied in [r#""apply""#, r#""preview""#, "null", "{}"] {
            let arguments = format!(r#"{{"vault":{{"by":"name","name":"n"}},"mode":{supplied}}}"#);
            assert!(
                norn_mcp::wire_body(arguments.as_bytes(), tool.mode).is_err(),
                "{} {supplied}",
                tool.name
            );
        }
        let spliced = norn_mcp::wire_body(br#"{ }"#, tool.mode).unwrap();
        let spliced: Value = serde_json::from_slice(&spliced).unwrap();
        assert_eq!(spliced["mode"], serde_json::to_value(tool.mode).unwrap());
    }
    let body = br#"{"vault":{"by":"name","name":"n"},"mode":"x"}"#;
    assert_eq!(&*norn_mcp::wire_body(body, None).unwrap(), body);
}
