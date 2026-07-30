//! The JSON Schema every wire type derives, and the facts about it that
//! surfaces depend on.
//!
//! The schema is not snapshotted. A full snapshot pins every doc comment and
//! every ordering decision schemars makes, and a suite that fails on a reworded
//! sentence stops being read. What is asserted here is the shape a surface
//! renders against: that a schema is produced at all, that an enum advertises
//! its tag as a pinned constant under the tag name the wire uses, that the
//! field names are the snake_case ones, that a reason code advertises the flat
//! namespaced string, and that a detail refers to the reason type rather than
//! restating it.

use norn_wire::{ErrorDetail, ErrorEnvelope, ReasonCode, TrustState, UntrustedReason};
use serde_json::Value;
use std::collections::BTreeSet;

fn schema_of<T: schemars::JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("a schema as JSON")
}

/// The branches of a `oneOf`, which is how schemars describes an enum.
fn branches(schema: &Value) -> &Vec<Value> {
    schema
        .get("oneOf")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("the schema describes no enum: {schema}"))
}

/// The constant a branch pins its `tag` property to, for an internally tagged
/// enum. Absent when the branch is not that shape at all — an externally
/// tagged enum names the variant as a property instead, and has no such
/// constant to report.
fn tag_constant<'a>(branch: &'a Value, tag: &str) -> Option<&'a str> {
    let property = branch.get("properties")?.get(tag)?;
    if property.get("type")? != "string" {
        return None;
    }
    let required: BTreeSet<&str> = branch
        .get("required")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    if !required.contains(tag) {
        return None;
    }
    property.get("const")?.as_str()
}

/// The constant a branch is pinned to, for an enum that is a flat string.
fn string_constant(branch: &Value) -> Option<&str> {
    if branch.get("type")? != "string" {
        return None;
    }
    branch.get("const")?.as_str()
}

fn property_names(schema: &Value) -> BTreeSet<&str> {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().map(String::as_str).collect())
        .unwrap_or_default()
}

/// Every constant an internally tagged enum's branches pin their tag to.
fn tag_constants<'a>(schema: &'a Value, tag: &str) -> Vec<&'a str> {
    branches(schema)
        .iter()
        .map(|branch| {
            tag_constant(branch, tag)
                .unwrap_or_else(|| panic!("a branch does not pin `{tag}` to a constant: {branch}"))
        })
        .collect()
}

// ── Every type derives one ───────────────────────────────────────────────

#[test]
fn every_wire_type_derives_a_schema() {
    for (name, schema) in [
        ("TrustState", schema_of::<TrustState>()),
        ("UntrustedReason", schema_of::<UntrustedReason>()),
        ("ReasonCode", schema_of::<ReasonCode>()),
        ("ErrorDetail", schema_of::<ErrorDetail>()),
        ("ErrorEnvelope", schema_of::<ErrorEnvelope>()),
    ] {
        assert_eq!(
            schema.get("title").and_then(Value::as_str),
            Some(name),
            "the schema does not name the type it describes: {schema}"
        );
        assert!(
            schema.get("$schema").is_some(),
            "the schema declares no dialect: {schema}"
        );
    }
}

// ── The tag representation ───────────────────────────────────────────────

#[test]
fn a_trust_state_advertises_its_state_tag() {
    let schema = schema_of::<TrustState>();
    assert_eq!(
        tag_constants(&schema, "state"),
        ["unattached", "warming", "ready", "untrusted"]
    );
}

#[test]
fn an_untrusted_reason_advertises_its_kind_tag() {
    let schema = schema_of::<UntrustedReason>();
    assert_eq!(
        tag_constants(&schema, "kind"),
        [
            "torn_increment",
            "watcher_overflow",
            "environmental_refusal"
        ]
    );
}

#[test]
fn an_error_detail_advertises_the_code_as_its_tag() {
    let schema = schema_of::<ErrorDetail>();
    assert_eq!(tag_constants(&schema, "code"), ["host/entry-untrusted"]);
}

/// External tagging would name each variant as a property of the branch —
/// `{"properties":{"Warming":{…}}}` — so a branch carrying a variant name as a
/// key is the shape this vocabulary does not use.
#[test]
fn no_branch_names_a_variant_as_a_property() {
    for (schema, tag, variants) in [
        (
            schema_of::<TrustState>(),
            "state",
            ["Unattached", "Warming", "Ready", "Untrusted"].as_slice(),
        ),
        (
            schema_of::<UntrustedReason>(),
            "kind",
            ["TornIncrement", "WatcherOverflow", "EnvironmentalRefusal"].as_slice(),
        ),
        (
            schema_of::<ErrorDetail>(),
            "code",
            ["EntryUntrusted"].as_slice(),
        ),
    ] {
        for branch in branches(&schema) {
            let properties = property_names(branch);
            assert!(
                properties.contains(tag),
                "a branch carries no `{tag}` property: {branch}"
            );
            for variant in variants {
                assert!(
                    !properties.contains(variant),
                    "a branch names the variant `{variant}` as a property: {branch}"
                );
            }
        }
    }
}

// ── The field names ──────────────────────────────────────────────────────

#[test]
fn a_warming_state_advertises_its_two_counters_in_snake_case() {
    let schema = schema_of::<TrustState>();
    let warming = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "state") == Some("warming"))
        .expect("the warming branch");
    assert_eq!(
        property_names(warming),
        ["state", "healed", "total_estimate"].into_iter().collect()
    );
}

#[test]
fn an_envelope_advertises_exactly_three_fields() {
    let schema = schema_of::<ErrorEnvelope>();
    assert_eq!(schema.get("type").and_then(Value::as_str), Some("object"));
    assert_eq!(
        property_names(&schema),
        ["code", "message", "detail"].into_iter().collect()
    );
    let required: BTreeSet<&str> = schema["required"]
        .as_array()
        .expect("the required list")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(
        required,
        ["code", "message", "detail"].into_iter().collect(),
        "every field of the envelope is present on every refusal"
    );
}

// ── The code, and what it pairs with ─────────────────────────────────────

/// A code is advertised as the flat namespaced string itself, not as a variant
/// name a reader has to translate.
#[test]
fn a_reason_code_advertises_its_flat_namespaced_string() {
    let schema = schema_of::<ReasonCode>();
    let codes: Vec<&str> = branches(&schema)
        .iter()
        .map(|branch| {
            string_constant(branch)
                .unwrap_or_else(|| panic!("a code branch is not a pinned string: {branch}"))
        })
        .collect();
    assert_eq!(codes, ["host/entry-untrusted"]);
}

/// The detail composes the reason type rather than restating its variants, so
/// one definition describes the reason wherever it appears.
#[test]
fn a_detail_refers_to_the_reason_type_it_carries() {
    let schema = schema_of::<ErrorDetail>();
    let branch = &branches(&schema)[0];
    assert_eq!(
        branch["properties"]["reason"]["$ref"].as_str(),
        Some("#/$defs/UntrustedReason")
    );
    assert!(
        schema["$defs"]["UntrustedReason"].is_object(),
        "the referenced definition is absent: {schema}"
    );
}

/// The envelope refers to both, so the schema a surface publishes carries the
/// code list and the detail vocabulary with it.
#[test]
fn an_envelope_refers_to_the_code_and_the_detail() {
    let schema = schema_of::<ErrorEnvelope>();
    assert_eq!(
        schema["properties"]["code"]["$ref"].as_str(),
        Some("#/$defs/ReasonCode")
    );
    assert_eq!(
        schema["properties"]["detail"]["$ref"].as_str(),
        Some("#/$defs/ErrorDetail")
    );
    for definition in ["ReasonCode", "ErrorDetail", "UntrustedReason"] {
        assert!(
            schema["$defs"][definition].is_object(),
            "the envelope's schema carries no definition of {definition}"
        );
    }
}
