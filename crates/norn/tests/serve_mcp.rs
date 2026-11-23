//! MCP over a real `norn serve` at `/mcp`: every served tool enrolled and
//! called over loopback behind the bearer in both revisions, a forecast and
//! every warning carried in both channels of a result, and — under the
//! `counters` feature — the one-dispatch, no-pool-work and derive-once bars.
//! Every answer is validated against its revision's vendored schema, and none
//! carries a session id.
#![allow(clippy::disallowed_methods)] // Harness scaffolding: the vault a case serves.

mod support;

#[path = "../../norn-mcp/tests/fixtures/mod.rs"]
mod mcp_schema;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use mcp_schema::{assert_valid, validator};
use norn_wire::{VaultAddress, VaultName, Verb};
use serde_json::{Value, json};
use support::{Fixture, Reply, Served, exchange_with, free_port};

const LEGACY: &str = "2025-11-25";
const MODERN: &str = "2026-07-28";

/// A vault of three documents, two linked, under the fixture's work
/// directory.
fn a_vault(fixture: &Fixture, name: &str) -> PathBuf {
    let root = fixture.sandbox.work_dir().join(name);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("alpha.md"),
        "---\nstatus: draft\n---\n# Alpha\n\nalpha body, see [[beta]]\n",
    )
    .unwrap();
    std::fs::write(root.join("beta.md"), "# Beta\n\nbeta body\n").unwrap();
    std::fs::write(
        root.join("delta.md"),
        "# Delta\n\ndelta body, see [[beta]]\n",
    )
    .unwrap();
    root
}

fn host(label: &str) -> (Fixture, Served, u16, String) {
    let fixture = Fixture::new(label);
    fixture.write_settings("");
    let port = free_port();
    let (served, bearer) = fixture.serve_on(port);
    (fixture, served, port, bearer)
}

/// An MCP client of one revision, as a conforming SDK client speaks it.
struct Client {
    port: u16,
    bearer: String,
    revision: &'static str,
    next_id: u64,
}

impl Client {
    /// A 2025-11-25 client, which opens with `initialize`.
    fn legacy(port: u16, bearer: &str) -> Client {
        let mut client = Client {
            port,
            bearer: bearer.to_string(),
            revision: LEGACY,
            next_id: 0,
        };
        let result = client.request(
            "initialize",
            json!({"protocolVersion": LEGACY, "capabilities": {},
                   "clientInfo": {"name": "serve-mcp", "version": "1"}}),
        );
        assert_valid(LEGACY, "InitializeResult", &result);
        assert_eq!(result["protocolVersion"], LEGACY);
        let reply = client.post(
            &[],
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        );
        assert_eq!(reply.status, 202);
        assert!(reply.body.is_empty());
        client
    }

    /// A 2026-07-28 client, which opens with `server/discover`.
    fn modern(port: u16, bearer: &str) -> Client {
        let mut client = Client {
            port,
            bearer: bearer.to_string(),
            revision: MODERN,
            next_id: 0,
        };
        let result = client.request("server/discover", json!({}));
        assert_valid(MODERN, "DiscoverResult", &result);
        assert_eq!(result["supportedVersions"], json!([MODERN, LEGACY]));
        client
    }

    fn post(&self, headers: &[(&str, &str)], body: &Value) -> Reply {
        let reply = exchange_with(
            self.port,
            "POST",
            "/mcp",
            Some(&self.bearer),
            headers,
            body.to_string().as_bytes(),
            Duration::from_secs(60),
        );
        assert!(
            reply.header("mcp-session-id").is_none(),
            "no answer issues a session: {}",
            reply.head
        );
        reply
    }

    /// One request, answered 200 with a result valid in this revision.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let (reply, value) = self.exchange(method, None, params);
        assert_eq!(reply.status, 200, "{method}: {value}");
        assert!(value.get("error").is_none(), "{method}: {value}");
        assert_valid(self.revision, "JSONRPCResultResponse", &value);
        if self.revision == MODERN {
            assert_eq!(value["result"]["resultType"], "complete", "{value}");
        }
        value["result"].clone()
    }

    /// One request and its whole answer, which is a valid message.
    fn exchange(&mut self, method: &str, name: Option<&str>, mut params: Value) -> (Reply, Value) {
        self.next_id += 1;
        let mut headers = Vec::new();
        if self.revision == MODERN {
            params["_meta"] = json!({
                "io.modelcontextprotocol/protocolVersion": MODERN,
                "io.modelcontextprotocol/clientCapabilities": {},
            });
            headers.push(("MCP-Protocol-Version", MODERN));
            headers.push(("Mcp-Method", method));
            if let Some(name) = name {
                headers.push(("Mcp-Name", name));
            }
        } else if method != "initialize" {
            headers.push(("MCP-Protocol-Version", LEGACY));
        }
        let body =
            json!({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params});
        let reply = self.post(&headers, &body);
        assert_eq!(
            reply.header("content-type").as_deref(),
            Some("application/json")
        );
        let value: Value = serde_json::from_slice(&reply.body)
            .unwrap_or_else(|_| panic!("not JSON: {}", reply.body_text()));
        assert_valid(self.revision, "JSONRPCMessage", &value);
        assert_eq!(value["id"], self.next_id);
        (reply, value)
    }

    /// `tools/call` of `tool`: status 200 and a valid `CallToolResult`.
    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        let (reply, value) = self.exchange(
            "tools/call",
            Some(tool),
            json!({"name": tool, "arguments": arguments}),
        );
        assert_eq!(reply.status, 200, "{tool}: {value}");
        assert!(value.get("error").is_none(), "{tool}: {value}");
        let result = value["result"].clone();
        assert_valid(self.revision, "CallToolResult", &result);
        // Both channels carry the same JSON.
        let text: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, result["structuredContent"], "{tool}");
        result
    }

    fn tools(&mut self) -> Vec<Value> {
        let result = self.request("tools/list", json!({}));
        assert_valid(self.revision, "ListToolsResult", &result);
        result["tools"].as_array().unwrap().clone()
    }
}

fn notes() -> Value {
    json!({"by": "name", "name": "notes"})
}

fn typed_notes() -> VaultAddress {
    VaultAddress::name(VaultName::new("notes").unwrap())
}

/// The tool names the round-trip test walks: every served verb, each mode
/// verb followed by its preview. Nothing is listed as not served today.
fn expected_tools() -> Vec<String> {
    let mut names = Vec::new();
    for verb in Verb::ALL {
        names.push(verb.as_str().to_string());
        if verb.takes_mode() {
            names.push(format!("{}_preview", verb.as_str()));
        }
    }
    names
}

/// The fault class a refusal's envelope takes, read from the envelope.
fn class_of(envelope: &Value) -> Value {
    let envelope: norn_wire::ErrorEnvelope = serde_json::from_value(envelope.clone()).unwrap();
    serde_json::to_value(envelope.fault_class()).unwrap()
}

/// The first resolved plan anywhere in `value`.
fn resolved_plan(value: &Value) -> Option<Value> {
    match value {
        Value::Object(map) => {
            if map.get("plan").and_then(Value::as_str) == Some("resolved") {
                return Some(value.clone());
            }
            map.values().find_map(resolved_plan)
        }
        Value::Array(items) => items.iter().find_map(resolved_plan),
        _ => None,
    }
}

/// **MCP is enrolled.** The live tool list in each revision is exactly the
/// set the round-trip test walks; every tool's arguments here validate
/// against the input schema it advertises; every tool is answered over
/// loopback by its host method, never `serve/bad-request` or
/// `serve/unknown-verb`; and every tool a call can get wrong carries that
/// failure in band, an `isError` result with its class.
#[test]
fn every_served_mcp_tool_is_enrolled() {
    let (fixture, served, port, bearer) = host("serve-mcp-enrolled");
    let root = a_vault(&fixture, "notes");
    let mut legacy = Client::legacy(port, &bearer);
    let mut modern = Client::modern(port, &bearer);
    let listed = legacy.tools();
    assert_eq!(listed, modern.tools(), "one list, two revisions");
    let names: Vec<String> = listed
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, expected_tools());
    let schema_of = |name: &str| {
        let tool = listed.iter().find(|tool| tool["name"] == name).unwrap();
        validator(&tool["inputSchema"])
    };

    let mut called: BTreeSet<String> = BTreeSet::new();
    let mut parity = 0usize;
    // Each tool once, alternating revisions, its arguments checked against
    // its advertised schema first.
    let mut answered = |tool: &str, arguments: Value| -> Value {
        if let Err(error) = schema_of(tool).validate(&arguments) {
            panic!("{tool}: this suite's arguments break the advertised schema: {error}");
        }
        parity += 1;
        let client = if parity.is_multiple_of(2) {
            &mut legacy
        } else {
            &mut modern
        };
        let result = client.call(tool, arguments);
        if result["isError"] == true {
            let code = result["structuredContent"]["error"]["detail"]["code"]
                .as_str()
                .unwrap()
                .to_string();
            assert!(
                code.starts_with("vault/") || code == "host/entry-held",
                "`{tool}` was not served: {result}"
            );
        }
        called.insert(tool.to_string());
        result
    };

    answered(
        "vault_register",
        json!({"registration": {"name": "notes", "root": root}}),
    );
    answered("vault_list", json!({}));
    answered("vault_resolve", json!({"directory": root}));
    answered("vault_status", json!({"vault": notes()}));
    answered("doctor_registry", json!({}));
    answered(
        "find",
        serde_json::to_value(norn_wire::FindParams::new(typed_notes())).unwrap(),
    );
    answered(
        "search",
        serde_json::to_value(norn_wire::SearchParams::new(typed_notes(), "alpha")).unwrap(),
    );
    answered(
        "get",
        serde_json::to_value(norn_wire::GetParams::new(
            typed_notes(),
            norn_wire::ResolutionTarget::new("alpha").unwrap(),
        ))
        .unwrap(),
    );
    answered(
        "count",
        serde_json::to_value(norn_wire::CountParams::new(typed_notes())).unwrap(),
    );
    answered(
        "validate",
        serde_json::to_value(norn_wire::ValidateParams::new(typed_notes())).unwrap(),
    );
    answered(
        "describe",
        serde_json::to_value(norn_wire::DescribeParams::new(typed_notes())).unwrap(),
    );

    let set = |path: &str| {
        json!({"vault": notes(), "path": path,
               "changes": [{"change": "set", "field": "status", "value": "done"}]})
    };
    let previewed = answered("set_preview", set("beta.md"));
    answered("set", set("alpha.md"));
    assert!(
        std::fs::read_to_string(root.join("alpha.md"))
            .unwrap()
            .contains("status: done")
    );
    let edit = json!({"vault": notes(), "path": "alpha.md",
        "edits": [{"edit": "str_replace", "old_str": "alpha body", "new_str": "alpha text"}]});
    answered("edit_preview", edit.clone());
    answered("edit", edit);
    answered(
        "new_preview",
        json!({"vault": notes(), "path": "gamma.md", "content": "# Gamma\n"}),
    );
    answered(
        "new",
        json!({"vault": notes(), "path": "gamma.md", "content": "# Gamma\n"}),
    );
    let movement = json!({"vault": notes(), "from": "gamma.md", "to": "archive/gamma.md"});
    answered("move_preview", movement.clone());
    answered("move", movement);
    let rewrite = json!({"vault": notes(), "old": "beta", "new": "delta"});
    answered("rewrite_wikilink_preview", rewrite.clone());
    answered("rewrite_wikilink", rewrite);
    answered(
        "delete_preview",
        json!({"vault": notes(), "path": "archive/gamma.md"}),
    );
    answered(
        "delete",
        json!({"vault": notes(), "path": "archive/gamma.md"}),
    );
    let plan = resolved_plan(&previewed["structuredContent"]).expect("a preview carries its plan");
    answered("apply_preview", json!({"plan": plan.clone()}));
    answered("apply", json!({"plan": plan}));
    assert!(
        std::fs::read_to_string(root.join("beta.md"))
            .unwrap()
            .contains("status: done")
    );
    for tool in [
        "repair_preview",
        "repair",
        "init_preview",
        "init",
        "vault_migrate_preview",
        "vault_migrate",
    ] {
        answered(tool, json!({"vault": notes()}));
    }
    answered("vault_reload", json!({"vault": notes(), "dry_run": false}));
    answered(
        "vault_set",
        serde_json::to_value(norn_wire::VaultSetParams::new(
            VaultName::new("notes").unwrap(),
        ))
        .unwrap(),
    );
    answered(
        "vault_unregister",
        serde_json::to_value(norn_wire::UnregisterParams::new(
            VaultName::new("notes").unwrap(),
        ))
        .unwrap(),
    );
    assert_eq!(
        called.into_iter().collect::<Vec<_>>(),
        {
            let mut names = expected_tools();
            names.sort();
            names
        },
        "every tool is called"
    );

    // The failure channel, per tool: each argument a call can get wrong is
    // refused in band with its class. `vault_list` and `doctor_registry`
    // take no field a call can get wrong; the failure-channel test over
    // every code covers how their refusals compose.
    let absent = json!({"by": "name", "name": "absent"});
    for (index, name) in expected_tools().iter().enumerate() {
        let arguments = match name.trim_end_matches("_preview") {
            "vault_list" | "doctor_registry" => continue,
            "vault_register" => {
                json!({"registration": {"name": "absent", "root": "/nonexistent/absent"}})
            }
            "vault_unregister" | "vault_set" => json!({"name": "absent"}),
            "vault_resolve" => json!({"directory": ""}),
            "apply" => json!({"plan": {}}),
            _ => json!({"vault": absent}),
        };
        let client = if index.is_multiple_of(2) {
            &mut legacy
        } else {
            &mut modern
        };
        let result = client.call(name, arguments);
        assert_eq!(result["isError"], true, "{name}: {result}");
        let structured = &result["structuredContent"];
        assert_eq!(
            structured["class"],
            class_of(&structured["error"]),
            "{name}"
        );
    }
    // A JSON-RPC error answers only what is not a tool's outcome.
    let (reply, value) = legacy.exchange("tools/call", None, json!({"name": "absent"}));
    assert_eq!(reply.status, 200);
    assert_eq!(value["error"]["code"], -32602);
    assert_eq!(served.terminate().0.code(), Some(0));
}

/// Every key named `forecast` anywhere in `value`.
fn forecasts(value: &Value) -> usize {
    match value {
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| usize::from(key == "forecast") + forecasts(value))
            .sum(),
        Value::Array(items) => items.iter().map(forecasts).sum(),
        _ => 0,
    }
}

/// **A forecast is labelled a forecast in both channels.** Every preview
/// tool's result names its forecast as one, under the `forecast` key the wire
/// report carries, in `structuredContent` and in the text block alike, and
/// no write that applied answers with a forecast.
#[test]
fn every_preview_labels_its_forecast_in_both_channels() {
    let (fixture, served, port, bearer) = host("serve-mcp-forecast");
    let root = a_vault(&fixture, "notes");
    let mut client = Client::legacy(port, &bearer);
    client.call(
        "vault_register",
        json!({"registration": {"name": "notes", "root": root}}),
    );
    let set = json!({"vault": notes(), "path": "alpha.md",
        "changes": [{"change": "set", "field": "status", "value": "done"}]});
    let previewed = client.call("set_preview", set.clone());
    let plan = resolved_plan(&previewed["structuredContent"]).unwrap();
    for (tool, arguments) in [
        ("set_preview", set.clone()),
        (
            "edit_preview",
            json!({"vault": notes(), "path": "alpha.md",
                "edits": [{"edit": "str_replace", "old_str": "alpha body", "new_str": "x"}]}),
        ),
        (
            "new_preview",
            json!({"vault": notes(), "path": "dir/gamma.md", "content": "# Gamma\n"}),
        ),
        (
            "move_preview",
            json!({"vault": notes(), "from": "delta.md", "to": "archive/delta.md"}),
        ),
        (
            "delete_preview",
            json!({"vault": notes(), "path": "delta.md"}),
        ),
        (
            "rewrite_wikilink_preview",
            json!({"vault": notes(), "old": "beta", "new": "delta"}),
        ),
        ("apply_preview", json!({"plan": plan.clone()})),
        ("repair_preview", json!({"vault": notes()})),
        ("init_preview", json!({"vault": notes()})),
        ("vault_migrate_preview", json!({"vault": notes()})),
    ] {
        let result = client.call(tool, arguments);
        let structured = &result["structuredContent"];
        let text: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        if result["isError"] == true {
            // A preview the vault's own state refuses carries no forecast.
            let code = structured["error"]["detail"]["code"].as_str().unwrap();
            assert!(code.starts_with("vault/"), "{tool}: {result}");
            continue;
        }
        let labelled = forecasts(structured);
        assert!(
            labelled > 0 || is_unforecast(structured),
            "{tool}: a preview carries its forecast: {structured}"
        );
        assert_eq!(forecasts(&text), labelled, "{tool}: the text labels it too");
        assert_eq!(text, *structured, "{tool}");
    }
    let applied = client.call("set", set);
    assert_ne!(applied["isError"], true, "{applied}");
    assert_eq!(
        applied["structuredContent"]["report"]["outcome"], "applied",
        "{applied}"
    );
    assert_eq!(
        forecasts(&applied["structuredContent"]),
        0,
        "an applied write is not labelled a forecast: {applied}"
    );
    assert_eq!(served.terminate().0.code(), Some(0));
}

/// A preview whose report says there is nothing to plan: a vault already set
/// up, already current, or with nothing to repair.
fn is_unforecast(report: &Value) -> bool {
    let text = report.to_string();
    [
        "already_set_up",
        "already_current",
        "nothing_to_repair",
        "schema_elsewhere",
    ]
    .iter()
    .any(|outcome| text.contains(outcome))
}

/// **Every warning reaches both channels.** A find ordered by a key the
/// vault does not hold is answered with that part unsatisfied, and the
/// warning is carried whole in `structuredContent` and in the text block.
#[test]
fn every_warning_reaches_both_channels_of_a_result() {
    let (fixture, served, port, bearer) = host("serve-mcp-warnings");
    let root = a_vault(&fixture, "notes");
    for revision in [LEGACY, MODERN] {
        let mut client = if revision == LEGACY {
            Client::legacy(port, &bearer)
        } else {
            Client::modern(port, &bearer)
        };
        if revision == LEGACY {
            client.call(
                "vault_register",
                json!({"registration": {"name": "notes", "root": root}}),
            );
        }
        let find = norn_wire::FindParams::new(typed_notes()).with_sort(norn_wire::Sort::new(
            norn_wire::SortKey::field("no_such_key"),
            norn_wire::Direction::Ascending,
        ));
        let result = client.call("find", serde_json::to_value(find).unwrap());
        assert!(result.get("isError").is_none(), "{result}");
        let structured = &result["structuredContent"];
        let text: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        let warnings = structured["unsatisfied"].as_array().unwrap();
        assert!(!warnings.is_empty(), "{structured}");
        assert!(
            structured["unsatisfied"]
                .to_string()
                .contains("no_such_key")
        );
        for channel in ["unsatisfied", "advisories"] {
            assert_eq!(text[channel], structured[channel], "{revision} {channel}");
        }
    }
    assert_eq!(served.terminate().0.code(), Some(0));
}

#[cfg(feature = "counters")]
mod counters {
    use std::collections::BTreeMap;

    use super::*;

    type Counters = BTreeMap<String, u64>;

    fn counters(port: u16, bearer: &str) -> Counters {
        let reply = support::get(port, "/v1/debug/counters", Some(bearer));
        assert_eq!(reply.status, 200, "{reply:?}");
        serde_json::from_slice(&reply.body).unwrap()
    }

    fn moved(before: &Counters, after: &Counters, name: &str) -> u64 {
        after[name] - before[name]
    }

    #[test]
    fn a_mode_refusal_is_counted_without_dispatching() {
        let (_fixture, served, port, bearer) = host("serve-mcp-mode-counter");
        let mut legacy = Client::legacy(port, &bearer);
        let mut modern = Client::modern(port, &bearer);
        for client in [&mut legacy, &mut modern] {
            let before = counters(port, &bearer);
            let result = client.call("set_preview", json!({"vault": notes(), "mode": "apply"}));
            assert_eq!(result["isError"], true, "{result}");
            assert_eq!(
                result["structuredContent"]["error"]["detail"]["code"],
                "serve/bad-request"
            );
            let after = counters(port, &bearer);
            assert_eq!(moved(&before, &after, "serve.dispatches"), 0);
            assert_eq!(moved(&before, &after, "host.request.requests"), 0);
            assert_eq!(moved(&before, &after, "mcp.calls"), 1);
            assert_eq!(moved(&before, &after, "mcp.messages"), 1);
        }
        assert_eq!(served.terminate().0.code(), Some(0));
    }

    /// Attach the vault by a first read, which the readiness retry answers
    /// once it serves, then wait for its state to read ready.
    fn wait_for_ready(client: &mut Client) {
        let counted = client.call(
            "count",
            serde_json::to_value(norn_wire::CountParams::new(typed_notes())).unwrap(),
        );
        assert!(counted.get("isError").is_none(), "{counted}");
        support::eventually("the served vault ready", || {
            let status = client.call("vault_status", json!({"vault": notes()}));
            status["structuredContent"]["status"]["published"]["state"]["state"] == "ready"
        });
    }

    /// **Answers from constants cost no host work and no pool thread, the
    /// tool list is derived once per process, and a call that meets a Ready
    /// entry is exactly one host dispatch.**
    #[test]
    fn constant_answers_cost_nothing_and_a_ready_call_is_one_dispatch() {
        let (fixture, served, port, bearer) = host("serve-mcp-counters");
        let root = a_vault(&fixture, "notes");
        let mut legacy = Client::legacy(port, &bearer);
        let mut modern = Client::modern(port, &bearer);
        legacy.call(
            "vault_register",
            json!({"registration": {"name": "notes", "root": root}}),
        );
        wait_for_ready(&mut legacy);

        let before = counters(port, &bearer);
        for _ in 0..5 {
            let _ = Client::legacy(port, &bearer);
            legacy.tools();
            legacy.request("ping", json!({}));
            modern.request("server/discover", json!({}));
            modern.tools();
        }
        let after = counters(port, &bearer);
        assert_eq!(moved(&before, &after, "serve.dispatches"), 0);
        assert_eq!(moved(&before, &after, "host.request.requests"), 0);
        assert_eq!(moved(&before, &after, "mcp.calls"), 0);
        // Each round: a new client's two opening messages, and four requests.
        assert_eq!(moved(&before, &after, "mcp.messages"), 30);
        assert_eq!(after["mcp.tool_list_derivations"], 1);

        for client in [&mut legacy, &mut modern] {
            let before = counters(port, &bearer);
            let result = client.call(
                "get",
                serde_json::to_value(norn_wire::GetParams::new(
                    typed_notes(),
                    norn_wire::ResolutionTarget::new("alpha").unwrap(),
                ))
                .unwrap(),
            );
            assert!(result.get("isError").is_none(), "{result}");
            let after = counters(port, &bearer);
            assert_eq!(moved(&before, &after, "serve.dispatches"), 1);
            assert_eq!(moved(&before, &after, "serve.ready_retries"), 0);
            assert_eq!(moved(&before, &after, "mcp.calls"), 1);
            assert_eq!(moved(&before, &after, "mcp.messages"), 1);
        }
        assert_eq!(served.terminate().0.code(), Some(0));
    }
}
