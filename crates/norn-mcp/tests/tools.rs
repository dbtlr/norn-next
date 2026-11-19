mod fixtures;

use fixtures::{official, validator};
use norn_mcp::{ToolList, input_schema};
use norn_wire::*;
use serde_json::{Value, json};

#[test]
fn every_schema_preserves_root_fields_and_validates_against_both_revisions() {
    let list = ToolList::derive(Verb::ALL).unwrap();
    assert_eq!(list.tools.len(), 34);
    for revision in ["2025-11-25", "2026-07-28"] {
        let tool_validator = official(revision, "Tool");
        for tool in &list.tools {
            tool_validator
                .validate(&serde_json::to_value(tool).unwrap())
                .unwrap();
        }
        let prepared = list.prepare("0.0.0+test");
        let body = if revision == "2025-11-25" {
            prepared.legacy_body
        } else {
            prepared.modern_body
        };
        official(revision, "ListToolsResult")
            .validate(&serde_json::from_slice(&body).unwrap())
            .unwrap();
    }
    for tool in &list.tools {
        let mut original = serde_json::to_value(tool.verb.params_schema()).unwrap();
        if tool.verb.takes_mode() {
            original["properties"]
                .as_object_mut()
                .unwrap()
                .remove("mode");
            original["required"]
                .as_array_mut()
                .unwrap()
                .retain(|key| key != "mode");
        }
        for key in ["properties", "required", "additionalProperties", "$defs"] {
            assert_eq!(
                tool.input_schema.get(key),
                original.get(key),
                "{} {key}",
                tool.name
            );
        }
        for key in ["oneOf", "anyOf", "allOf", "not", "$schema"] {
            assert!(tool.input_schema.get(key).is_none(), "{} {key}", tool.name);
        }
        assert_eq!(tool.input_schema["type"], "object");
        validator(&tool.input_schema);
    }
}

#[test]
fn schema_guard_refuses_alternative_shapes_or_unknown_properties() {
    for branch in [
        json!({"type":"object"}),
        json!({"required":["missing"]}),
        json!({"properties":{"missing":{"const":true}}}),
        json!({"properties":{"path":{"type":"string"}}}),
        json!({"additionalProperties":false}),
    ] {
        assert!(
            input_schema(
                Verb::Set,
                json!({"properties":{"path":{"type":"string"}}, "oneOf":[branch]})
            )
            .is_err()
        );
    }
    let schema = json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false,
        "allOf":[{"not":{"required":["path"],"properties":{"path":{"const":"a.md"}}}}]});
    assert!(input_schema(Verb::Set, schema).unwrap().1);
}

/// Preparing the bodies reads the derived list; it derives nothing again.
/// The served count of derivations is `norn-serve`'s, around its one call.
#[test]
fn prepared_lists_reuse_one_derivation() {
    let list = ToolList::derive(Verb::ALL).unwrap();
    let prepared = list.prepare("test");
    for _ in 0..100 {
        assert_eq!(
            serde_json::from_slice::<Value>(&prepared.legacy_body).unwrap()["tools"]
                .as_array()
                .unwrap()
                .len(),
            34
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&prepared.modern_body).unwrap()["tools"]
                .as_array()
                .unwrap()
                .len(),
            34
        );
    }
}

#[test]
fn served_filter_order_groups_and_hints() {
    let list = ToolList::derive([Verb::VaultSet, Verb::New, Verb::Find, Verb::Init]).unwrap();
    assert_eq!(
        list.tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        [
            "find",
            "new",
            "new_preview",
            "init",
            "init_preview",
            "vault_set"
        ]
    );
    for (tool, group, destructive) in list
        .tools
        .iter()
        .zip(["read", "write", "read", "admin", "read", "admin"])
        .zip([None, Some(false), None, Some(false), None, Some(true)])
        .map(|((t, g), d)| (t, g, d))
    {
        assert_eq!(tool.meta, json!({"modgud/groups":[group]}));
        assert_eq!(tool.annotations.read_only_hint, group == "read");
        assert_eq!(tool.annotations.destructive_hint, destructive);
        let encoded = serde_json::to_value(tool).unwrap();
        assert_eq!(encoded["annotations"]["openWorldHint"], false);
        for key in ["title", "icons", "outputSchema"] {
            assert!(encoded.get(key).is_none());
        }
        assert!(encoded["annotations"].get("idempotentHint").is_none());
    }
}

fn representative(verb: Verb) -> Value {
    let vault = VaultAddress::name(VaultName::new("notes").unwrap());
    match verb {
        Verb::Find => return serde_json::to_value(FindParams::new(vault)).unwrap(),
        Verb::Search => return serde_json::to_value(SearchParams::new(vault, "notes")).unwrap(),
        Verb::Get => {
            return serde_json::to_value(GetParams::new(
                vault,
                ResolutionTarget::new("a").unwrap(),
            ))
            .unwrap();
        }
        Verb::Count => return serde_json::to_value(CountParams::new(vault)).unwrap(),
        Verb::Validate => return serde_json::to_value(ValidateParams::new(vault)).unwrap(),
        Verb::Describe => return serde_json::to_value(DescribeParams::new(vault)).unwrap(),
        Verb::VaultRegister => {
            return serde_json::to_value(RegisterParams::new(Registration::new(
                VaultName::new("notes").unwrap(),
                VaultRoot::new("/example/notes").unwrap(),
            )))
            .unwrap();
        }
        Verb::VaultUnregister => {
            return serde_json::to_value(UnregisterParams::new(VaultName::new("notes").unwrap()))
                .unwrap();
        }
        Verb::VaultList => return serde_json::to_value(ListParams::new()).unwrap(),
        Verb::VaultSet => {
            return serde_json::to_value(VaultSetParams::new(VaultName::new("notes").unwrap()))
                .unwrap();
        }
        Verb::VaultResolve => {
            return serde_json::to_value(ResolveParams::new(
                Directory::new("/example/notes").unwrap(),
            ))
            .unwrap();
        }
        Verb::VaultStatus => {
            return serde_json::to_value(StatusParams::new().with_vault(vault)).unwrap();
        }
        Verb::VaultReload => return serde_json::to_value(ReloadParams::new(vault)).unwrap(),
        Verb::DoctorRegistry => return serde_json::to_value(DoctorRegistryParams::new()).unwrap(),
        _ => {}
    }
    let mut params = json!({"vault":{"by":"name","name":"notes"}});
    match verb {
        Verb::Search => params["query"] = json!("notes"),
        Verb::Get => params["target"] = json!("a"),
        Verb::Set => {
            params["path"] = json!("a.md");
            params["changes"] = json!([{"change":"set","field":"status","value":"done"}]);
        }
        Verb::Edit => {
            params["path"] = json!("a.md");
            params["edits"] =
                serde_json::to_value(vec![DocumentEdit::replace_body("text")]).unwrap();
        }
        Verb::New => {
            params["path"] = json!("a.md");
            params["content"] = json!("text");
        }
        Verb::Move => {
            params["from"] = json!("a.md");
            params["to"] = json!("b.md");
        }
        Verb::Delete => params["path"] = json!("a.md"),
        Verb::RewriteWikilink => {
            params["old"] = json!("a");
            params["new"] = json!("b");
        }
        Verb::Apply => {
            params = json!({"plan":PlanDocument::operations(AuthoredPlan::new(
            VaultAddress::name(VaultName::new("notes").unwrap()),
            vec![Operation::new(OperationKind::create_document(DocumentPath::new("a.md").unwrap(), "text"))]))})
        }
        Verb::VaultRegister => {
            params = json!({"registration":{"name":"notes","root":"/example/notes"}})
        }
        Verb::VaultSet | Verb::VaultUnregister => params = json!({"name":"notes"}),
        Verb::VaultResolve => params = json!({"directory":"/example/notes"}),
        Verb::VaultList | Verb::DoctorRegistry => params = json!({}),
        _ => {}
    }
    if verb.takes_mode() {
        params["mode"] = json!("apply");
    }
    let mut params = canonical_params(verb, params);
    if verb.takes_mode() {
        params.as_object_mut().unwrap().remove("mode");
    }
    params
}

fn canonical_params(verb: Verb, params: Value) -> Value {
    match verb {
        Verb::Find => {
            serde_json::to_value(serde_json::from_value::<FindParams>(params).unwrap()).unwrap()
        }
        Verb::Search => {
            serde_json::to_value(serde_json::from_value::<SearchParams>(params).unwrap()).unwrap()
        }
        Verb::Get => {
            serde_json::to_value(serde_json::from_value::<GetParams>(params).unwrap()).unwrap()
        }
        Verb::Count => {
            serde_json::to_value(serde_json::from_value::<CountParams>(params).unwrap()).unwrap()
        }
        Verb::Validate => {
            serde_json::to_value(serde_json::from_value::<ValidateParams>(params).unwrap()).unwrap()
        }
        Verb::Describe => {
            serde_json::to_value(serde_json::from_value::<DescribeParams>(params).unwrap()).unwrap()
        }
        Verb::Apply => {
            serde_json::to_value(serde_json::from_value::<ApplyParams>(params).unwrap()).unwrap()
        }
        Verb::Set => {
            serde_json::to_value(serde_json::from_value::<SetParams>(params).unwrap()).unwrap()
        }
        Verb::Edit => {
            serde_json::to_value(serde_json::from_value::<EditParams>(params).unwrap()).unwrap()
        }
        Verb::New => {
            serde_json::to_value(serde_json::from_value::<NewParams>(params).unwrap()).unwrap()
        }
        Verb::Move => {
            serde_json::to_value(serde_json::from_value::<MoveParams>(params).unwrap()).unwrap()
        }
        Verb::Delete => {
            serde_json::to_value(serde_json::from_value::<DeleteParams>(params).unwrap()).unwrap()
        }
        Verb::RewriteWikilink => {
            serde_json::to_value(serde_json::from_value::<RewriteWikilinkParams>(params).unwrap())
                .unwrap()
        }
        Verb::Repair => {
            serde_json::to_value(serde_json::from_value::<RepairParams>(params).unwrap()).unwrap()
        }
        Verb::Init => {
            serde_json::to_value(serde_json::from_value::<InitParams>(params).unwrap()).unwrap()
        }
        Verb::VaultRegister => {
            serde_json::to_value(serde_json::from_value::<RegisterParams>(params).unwrap()).unwrap()
        }
        Verb::VaultUnregister => {
            serde_json::to_value(serde_json::from_value::<UnregisterParams>(params).unwrap())
                .unwrap()
        }
        Verb::VaultList => {
            serde_json::to_value(serde_json::from_value::<ListParams>(params).unwrap()).unwrap()
        }
        Verb::VaultSet => {
            serde_json::to_value(serde_json::from_value::<VaultSetParams>(params).unwrap()).unwrap()
        }
        Verb::VaultResolve => {
            serde_json::to_value(serde_json::from_value::<ResolveParams>(params).unwrap()).unwrap()
        }
        Verb::VaultStatus => {
            serde_json::to_value(serde_json::from_value::<StatusParams>(params).unwrap()).unwrap()
        }
        Verb::VaultReload => {
            serde_json::to_value(serde_json::from_value::<ReloadParams>(params).unwrap()).unwrap()
        }
        Verb::VaultMigrate => {
            serde_json::to_value(serde_json::from_value::<MigrateParams>(params).unwrap()).unwrap()
        }
        Verb::DoctorRegistry => {
            serde_json::to_value(serde_json::from_value::<DoctorRegistryParams>(params).unwrap())
                .unwrap()
        }
        _ => panic!("new verb needs a typed fixture"),
    }
}

fn parse_equal<T: serde::de::DeserializeOwned + serde::Serialize + PartialEq + std::fmt::Debug>(
    body: &[u8],
    native: &[u8],
) {
    let parsed: T = serde_json::from_slice(body).unwrap();
    let expected: T = serde_json::from_slice(native).unwrap();
    assert_eq!(parsed, expected);
}

/// The body the wire route parses for the same call: the caller's own
/// argument bytes with `mode` written in by hand, at the end, as a wire
/// caller would spell it. Independent of the splice under test, which writes
/// it at the front.
fn native_bytes(arguments: &str, mode: Option<ApplyMode>) -> Vec<u8> {
    let Some(mode) = mode else {
        return arguments.as_bytes().to_vec();
    };
    let mode = serde_json::to_string(&mode).unwrap();
    let inner = arguments
        .trim()
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .unwrap();
    if inner.trim().is_empty() {
        format!(r#"{{"mode":{mode}}}"#).into_bytes()
    } else {
        format!(r#"{{{inner},"mode":{mode}}}"#).into_bytes()
    }
}

#[test]
fn every_tool_round_trips_to_the_native_params_type() {
    for tool in ToolList::derive(Verb::ALL).unwrap().tools {
        let arguments = representative(tool.verb);
        validator(&tool.input_schema)
            .validate(&arguments)
            .unwrap_or_else(|e| panic!("{}: {e}", tool.name));
        // The arguments as a caller's bytes; the tool reads those bytes, never
        // a value parsed from them.
        let text = serde_json::to_string(&arguments).unwrap();
        let body = tool.wire_body(text.as_bytes()).unwrap();
        let native = &native_bytes(&text, tool.mode);
        match tool.verb {
            Verb::Find => parse_equal::<FindParams>(&body, native),
            Verb::Search => parse_equal::<SearchParams>(&body, native),
            Verb::Get => parse_equal::<GetParams>(&body, native),
            Verb::Count => parse_equal::<CountParams>(&body, native),
            Verb::Validate => parse_equal::<ValidateParams>(&body, native),
            Verb::Describe => parse_equal::<DescribeParams>(&body, native),
            Verb::Apply => parse_equal::<ApplyParams>(&body, native),
            Verb::Set => parse_equal::<SetParams>(&body, native),
            Verb::Edit => parse_equal::<EditParams>(&body, native),
            Verb::New => parse_equal::<NewParams>(&body, native),
            Verb::Move => parse_equal::<MoveParams>(&body, native),
            Verb::Delete => parse_equal::<DeleteParams>(&body, native),
            Verb::RewriteWikilink => parse_equal::<RewriteWikilinkParams>(&body, native),
            Verb::Repair => parse_equal::<RepairParams>(&body, native),
            Verb::Init => parse_equal::<InitParams>(&body, native),
            Verb::VaultRegister => parse_equal::<RegisterParams>(&body, native),
            Verb::VaultUnregister => parse_equal::<UnregisterParams>(&body, native),
            Verb::VaultList => parse_equal::<ListParams>(&body, native),
            Verb::VaultSet => parse_equal::<VaultSetParams>(&body, native),
            Verb::VaultResolve => parse_equal::<ResolveParams>(&body, native),
            Verb::VaultStatus => parse_equal::<StatusParams>(&body, native),
            Verb::VaultReload => parse_equal::<ReloadParams>(&body, native),
            Verb::VaultMigrate => parse_equal::<MigrateParams>(&body, native),
            Verb::DoctorRegistry => parse_equal::<DoctorRegistryParams>(&body, native),
            _ => panic!("new verb needs a representative"),
        }
    }
}

#[test]
fn all_new_set_delete_presence_combinations_keep_wire_refusals() {
    let list = ToolList::derive(Verb::ALL).unwrap();
    for tool in list
        .tools
        .iter()
        .filter(|tool| matches!(tool.verb, Verb::New | Verb::Set | Verb::Delete))
    {
        let schema = validator(&tool.input_schema);
        let original = validator(&serde_json::to_value(tool.verb.params_schema()).unwrap());
        let variants: Vec<(Value, bool)> = match tool.verb {
            Verb::New => (0..64)
                .map(|bits| {
                    let keys = [
                        ("path", json!("a.md")),
                        ("content", json!("text")),
                        ("as", json!("note")),
                        ("variables", json!({})),
                        ("fields", json!({})),
                        ("body", json!("text")),
                    ];
                    let mut value = json!({"vault":{"by":"name","name":"notes"}});
                    for (i, (key, v)) in keys.into_iter().enumerate() {
                        if bits & (1 << i) != 0 {
                            value[key] = v;
                        }
                    }
                    let path = bits & 1 != 0;
                    let content = bits & 2 != 0;
                    let rule = bits & 4 != 0;
                    let valid = if path {
                        content && !rule && bits & 56 == 0
                    } else if rule {
                        !content
                    } else {
                        !content && bits & 8 == 0
                    };
                    (value, valid)
                })
                .collect(),
            Verb::Set => (0..4)
                .map(|bits| {
                    let mut value = representative(Verb::Set);
                    value.as_object_mut().unwrap().remove("path");
                    if bits & 1 != 0 {
                        value["path"] = json!("a.md");
                    }
                    if bits & 2 != 0 {
                        value["where"] = json!([{"op":"path","glob":"*.md"}]);
                    }
                    (value, bits == 1 || bits == 2)
                })
                .collect(),
            Verb::Delete => [false, true]
                .into_iter()
                .flat_map(|rewrite| {
                    [None, Some(false), Some(true)]
                        .into_iter()
                        .map(move |broken| {
                            let mut value = representative(Verb::Delete);
                            if rewrite {
                                value["rewrite_to"] = json!("b");
                            }
                            if let Some(broken) = broken {
                                value["allow_broken_links"] = json!(broken);
                            }
                            (value, !(rewrite && broken == Some(true)))
                        })
                })
                .collect(),
            _ => unreachable!(),
        };
        for (arguments, valid) in variants {
            // Removed presence constraints remain enforced by the native reader,
            // while every combination still has the advertised property types.
            assert!(schema.is_valid(&arguments), "{} {arguments}", tool.name);
            let body = tool.wire_body(arguments.to_string().as_bytes()).unwrap();
            let parsed: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                original.is_valid(&parsed),
                valid,
                "{} {arguments}",
                tool.name
            );
            let accepted = match tool.verb {
                Verb::New => serde_json::from_slice::<NewParams>(&body).is_ok(),
                Verb::Set => serde_json::from_slice::<SetParams>(&body).is_ok(),
                Verb::Delete => serde_json::from_slice::<DeleteParams>(&body).is_ok(),
                _ => unreachable!(),
            };
            assert_eq!(accepted, valid, "{} {arguments}", tool.name);
        }
    }
}

#[test]
fn derivation_cost_and_prepared_sizes() {
    let start = std::time::Instant::now();
    let list = ToolList::derive(Verb::ALL).unwrap();
    let prepared = list.prepare("0.0.0+measurement");
    let elapsed = start.elapsed();
    eprintln!(
        "startup tool preparation: {elapsed:?}; tools={}; legacy_bytes={}; modern_bytes={}",
        list.tools.len(),
        prepared.legacy_body.len(),
        prepared.modern_body.len()
    );
    assert_eq!(list.tools.len(), 34);
}

#[test]
fn every_tool_has_the_contracted_effect_hints() {
    let list = ToolList::derive(Verb::ALL).unwrap();
    for tool in &list.tools {
        let (group, destructive) = if tool.name.ends_with("_preview") {
            ("read", None)
        } else {
            match tool.name.as_str() {
                "find" | "search" | "get" | "count" | "validate" | "describe" | "vault_list"
                | "vault_resolve" | "vault_status" | "doctor_registry" => ("read", None),
                "new" => ("write", Some(false)),
                "set" | "edit" | "move" | "delete" | "rewrite_wikilink" | "repair" | "apply" => {
                    ("write", Some(true))
                }
                "vault_register" | "vault_reload" | "init" => ("admin", Some(false)),
                "vault_unregister" | "vault_set" | "vault_migrate" => ("admin", Some(true)),
                name => panic!("unclassified tool {name}"),
            }
        };
        assert_eq!(tool.meta["modgud/groups"], json!([group]), "{}", tool.name);
        assert_eq!(
            tool.annotations.read_only_hint,
            group == "read",
            "{}",
            tool.name
        );
        assert_eq!(
            tool.annotations.destructive_hint, destructive,
            "{}",
            tool.name
        );
        assert!(!tool.annotations.open_world_hint);
    }
}

#[test]
fn tool_names_fix_mode_and_other_arguments_are_preserved() {
    for tool in ToolList::derive(Verb::ALL).unwrap().tools {
        if tool.mode.is_none() {
            continue;
        }
        let arguments = representative(tool.verb);
        let body: Value =
            serde_json::from_slice(&tool.wire_body(arguments.to_string().as_bytes()).unwrap())
                .unwrap();
        assert_eq!(
            body["mode"],
            if tool.name.ends_with("_preview") {
                "preview"
            } else {
                "apply"
            }
        );
        let mut restored = body;
        restored.as_object_mut().unwrap().remove("mode");
        assert_eq!(restored, arguments);
        for supplied in [json!("apply"), json!("preview"), Value::Null, json!(false)] {
            let mut override_attempt = arguments.clone();
            override_attempt["mode"] = supplied;
            assert!(
                tool.wire_body(override_attempt.to_string().as_bytes())
                    .is_err()
            );
        }
        // A key spelled with an escape is the key it spells.
        assert!(tool.wire_body(br#"{"mod\u0065":"apply"}"#).is_err());
        // A key nested deeper is not the tool's mode.
        assert!(tool.wire_body(br#"{"nested":{"mode":"apply"}}"#).is_ok());
        for not_an_object in [b"[]".as_slice(), b"null", b"\"mode\""] {
            assert!(tool.wire_body(not_an_object).is_err());
        }
    }
}

/// **The arguments reach the wire as the caller wrote them.** A `fields` map
/// keeps the caller's key order, which is not alphabetical here, and a
/// duplicate key is refused by the wire parser rather than collapsed: the
/// tool's body parses exactly as the same body sent to the wire route does.
#[test]
fn a_fields_map_keeps_its_order_and_a_duplicate_key_is_refused_as_on_the_wire() {
    let list = ToolList::derive(Verb::ALL).unwrap();
    for name in ["new", "new_preview"] {
        let tool = list.tools.iter().find(|tool| tool.name == name).unwrap();
        let ordered = r#"{"vault":{"by":"name","name":"notes"},"fields":{"title":"T","status":"draft","aardvark":1},"body":"x"}"#;
        let body = tool.wire_body(ordered.as_bytes()).unwrap();
        let native = native_bytes(ordered, tool.mode);
        let parsed: NewParams = serde_json::from_slice(&body).unwrap();
        let expected: NewParams = serde_json::from_slice(&native).unwrap();
        assert_eq!(parsed, expected, "{name}");
        let written = serde_json::to_string(&parsed).unwrap();
        let at = |key: &str| written.find(&format!("\"{key}\"")).unwrap();
        assert!(
            at("title") < at("status") && at("status") < at("aardvark"),
            "{name}: the caller's order is kept: {written}"
        );

        let duplicate = r#"{"vault":{"by":"name","name":"notes"},"fields":{"title":"a","title":"b"},"body":"x"}"#;
        let body = tool.wire_body(duplicate.as_bytes()).unwrap();
        let native = native_bytes(duplicate, tool.mode);
        // Positions differ by the splice; the reason does not.
        let reason = |error: serde_json::Error| {
            let text = error.to_string();
            text[..text.find(" at line").unwrap_or(text.len())].to_string()
        };
        let tool_error = reason(serde_json::from_slice::<NewParams>(&body).unwrap_err());
        let wire_error = reason(serde_json::from_slice::<NewParams>(&native).unwrap_err());
        assert_eq!(
            tool_error, wire_error,
            "{name}: refused for the same reason"
        );
        assert!(tool_error.contains("title"), "{name}: {tool_error}");
    }
}
