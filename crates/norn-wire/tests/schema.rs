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

use norn_wire::{
    ErrorDetail, ErrorEnvelope, FindingKind, FindingScope, MaintainerIdentity, ReasonCode,
    Severity, TrustState, UntrustedReason, WarmingPhase, WatcherLossCause,
};
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

/// The same members in ascending order. A membership assertion sorts both
/// sides, so it pins the set a surface renders and leaves the order schemars
/// declared the variants in unpinned.
fn sorted<'a>(members: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let mut members: Vec<&str> = members.into_iter().collect();
    members.sort_unstable();
    members
}

// ── Every type derives one ───────────────────────────────────────────────

#[test]
fn every_wire_type_derives_a_schema() {
    for schema in [
        schema_of::<TrustState>(),
        schema_of::<UntrustedReason>(),
        schema_of::<WatcherLossCause>(),
        schema_of::<WarmingPhase>(),
        schema_of::<ReasonCode>(),
        schema_of::<FindingKind>(),
        schema_of::<FindingScope>(),
        schema_of::<Severity>(),
        schema_of::<MaintainerIdentity>(),
        schema_of::<ErrorDetail>(),
        schema_of::<ErrorEnvelope>(),
    ] {
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
        sorted(tag_constants(&schema, "state")),
        sorted(["unattached", "warming", "ready", "untrusted"])
    );
}

#[test]
fn an_untrusted_reason_advertises_its_kind_tag() {
    let schema = schema_of::<UntrustedReason>();
    assert_eq!(
        sorted(tag_constants(&schema, "kind")),
        sorted([
            "watcher_overflow",
            "watcher_lost",
            "environmental_refusal",
            "store_damaged",
        ])
    );
}

/// A lost watcher advertises the cause as a nested enum and the detail as a
/// string, so a surface renders the branchable half and the prose half apart.
#[test]
fn a_lost_watcher_advertises_a_typed_cause_beside_its_prose() {
    let schema = schema_of::<UntrustedReason>();
    let lost = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "kind") == Some("watcher_lost"))
        .expect("the watcher_lost branch");
    assert_eq!(
        property_names(lost),
        ["kind", "cause", "detail"].into_iter().collect()
    );
    assert_eq!(
        lost["properties"]["cause"]["$ref"].as_str(),
        Some("#/$defs/WatcherLossCause")
    );
    assert_eq!(
        lost["properties"]["detail"]["type"].as_str(),
        Some("string")
    );
    assert_eq!(
        sorted(
            branches(&schema_of::<WatcherLossCause>())
                .iter()
                .map(|branch| string_constant(branch)
                    .unwrap_or_else(|| panic!("a cause branch is not a pinned string: {branch}")))
        ),
        sorted(["backend", "coverage_lost", "synchronization_expired",])
    );
}

#[test]
fn an_error_detail_advertises_the_code_as_its_tag() {
    let schema = schema_of::<ErrorDetail>();
    assert_eq!(
        sorted(tag_constants(&schema, "code")),
        sorted([
            "host/duplicate-root",
            "host/entry-untrusted",
            "host/maintainer-contended",
            "host/unknown-vault"
        ])
    );
}

#[test]
fn a_maintainer_identity_advertises_named_and_unknown_shapes() {
    let schema = schema_of::<MaintainerIdentity>();
    assert_eq!(
        sorted(tag_constants(&schema, "kind")),
        sorted(["named", "unknown"])
    );
    let named = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "kind") == Some("named"))
        .expect("the named branch");
    assert_eq!(
        property_names(named),
        ["kind", "pid", "version", "started_unix_seconds"]
            .into_iter()
            .collect()
    );
}

// ── The field names ──────────────────────────────────────────────────────

/// The counters are advertised under the names the wire uses, and the estimate
/// is advertised as a number or `null`: a surface rendering progress is told
/// that the denominator may not be known. The phase is advertised beside them
/// as a nested enum, so a surface renders what the entry is doing apart from
/// how far it has come.
#[test]
fn a_warming_state_advertises_its_phase_and_its_two_counters_in_snake_case() {
    let schema = schema_of::<TrustState>();
    let warming = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "state") == Some("warming"))
        .expect("the warming branch");
    assert_eq!(
        property_names(warming),
        ["state", "phase", "healed", "total_estimate"]
            .into_iter()
            .collect()
    );
    assert_eq!(
        warming["properties"]["phase"]["$ref"].as_str(),
        Some("#/$defs/WarmingPhase")
    );
    assert_eq!(
        sorted(branches(&schema_of::<WarmingPhase>()).iter().map(|branch| {
            string_constant(branch)
                .unwrap_or_else(|| panic!("a phase branch is not a pinned string: {branch}"))
        })),
        sorted(["installing_coverage", "healing", "releasing_coverage"])
    );
    let estimate: BTreeSet<&str> = warming["properties"]["total_estimate"]["type"]
        .as_array()
        .expect("the estimate's advertised types")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(estimate, ["integer", "null"].into_iter().collect());
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
    assert_eq!(
        sorted(codes),
        sorted([
            "host/duplicate-root",
            "host/entry-untrusted",
            "host/maintainer-contended",
            "host/unknown-vault"
        ])
    );
}

/// A finding kind is advertised the same way a reason code is: the flat
/// namespaced string itself, so one grammar describes both registries.
#[test]
fn a_finding_kind_advertises_its_flat_namespaced_string() {
    let schema = schema_of::<FindingKind>();
    let kinds: Vec<&str> = branches(&schema)
        .iter()
        .map(|branch| {
            string_constant(branch)
                .unwrap_or_else(|| panic!("a kind branch is not a pinned string: {branch}"))
        })
        .collect();
    assert_eq!(
        sorted(kinds.clone()),
        sorted([
            "document/path-bytes-not-utf8",
            "document/path-names-no-document",
            "document/body-bytes-not-utf8",
            "document/frontmatter-too-large",
            "document/frontmatter-unclosed",
            "document/frontmatter-unreadable"
        ])
    );
    // The derived schema enumerates the enum itself, so holding ALL equal to
    // it keeps the walkable registry from drifting behind a new variant.
    assert_eq!(
        sorted(kinds),
        sorted(FindingKind::ALL.map(|kind| kind.as_str()))
    );
}

/// Severity is advertised as the same bare string a finding stores, and the
/// walkable list cannot drift behind the enum surfaces render.
#[test]
fn a_severity_advertises_its_bare_string() {
    let schema = schema_of::<Severity>();
    let severities: Vec<&str> = branches(&schema)
        .iter()
        .map(|branch| {
            string_constant(branch)
                .unwrap_or_else(|| panic!("a severity branch is not a pinned string: {branch}"))
        })
        .collect();
    assert_eq!(sorted(severities.clone()), sorted(["error", "warning"]));
    assert_eq!(
        sorted(severities),
        sorted(Severity::ALL.map(|severity| severity.as_str()))
    );
}

/// The detail composes the reason type rather than restating its variants, so
/// one definition describes the reason wherever it appears.
#[test]
fn a_detail_refers_to_the_reason_type_it_carries() {
    let schema = schema_of::<ErrorDetail>();
    let branch = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "code") == Some("host/entry-untrusted"))
        .expect("the entry-untrusted branch");
    assert_eq!(
        branch["properties"]["reason"]["$ref"].as_str(),
        Some("#/$defs/UntrustedReason")
    );
    assert!(
        schema["$defs"]["UntrustedReason"].is_object(),
        "the referenced definition is absent: {schema}"
    );
}

/// An unknown vault advertises the requested name as a field of its detail, so
/// a surface reads the name the request asked for as typed data rather than
/// out of the message.
#[test]
fn an_unknown_vault_advertises_the_requested_name() {
    let schema = schema_of::<ErrorDetail>();
    let branch = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "code") == Some("host/unknown-vault"))
        .expect("the unknown-vault branch");
    assert_eq!(
        property_names(branch),
        ["code", "name"].into_iter().collect()
    );
    assert_eq!(
        branch["properties"]["name"]["type"].as_str(),
        Some("string")
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
    for definition in [
        "ReasonCode",
        "ErrorDetail",
        "UntrustedReason",
        "MaintainerIdentity",
    ] {
        assert!(
            schema["$defs"][definition].is_object(),
            "the envelope's schema carries no definition of {definition}"
        );
    }
}
