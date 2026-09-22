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
    Addressing, Anchor, AnswerReading, AttachMode, Cursor, CursorKey, EngineSection, ErrorDetail,
    ErrorEnvelope, FacetKind, FindingKind, FindingScope, Freshness, MaintainerIdentity, Moved,
    NotReady, Page, PollBackend, Predicate, ReasonCode, ReloadFailure, RequestScope,
    ResolutionTarget, Rung, RungReport, SchemaSource, Score, Severity, Snapshot, TrustState,
    Unsatisfied, UntrustedReason, VaultAddress, VaultAnswer, VaultName, VaultRoot, Verb,
    WarmingPhase, WatcherLossCause,
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
        schema_of::<AttachMode>(),
        schema_of::<VaultName>(),
        schema_of::<VaultRoot>(),
        schema_of::<SchemaSource>(),
        schema_of::<PollBackend>(),
        schema_of::<VaultAddress>(),
        schema_of::<Verb>(),
        schema_of::<RequestScope>(),
        schema_of::<Addressing>(),
        schema_of::<ResolutionTarget>(),
        schema_of::<Anchor>(),
        schema_of::<Predicate>(),
        schema_of::<Cursor>(),
        schema_of::<CursorKey>(),
        schema_of::<FacetKind>(),
        schema_of::<Moved>(),
        schema_of::<Snapshot>(),
        schema_of::<Page<String>>(),
        schema_of::<AnswerReading>(),
        schema_of::<Rung>(),
        schema_of::<RungReport>(),
        schema_of::<Freshness>(),
        schema_of::<Score>(),
        schema_of::<EngineSection>(),
        schema_of::<Unsatisfied>(),
        schema_of::<VaultAnswer<String>>(),
        schema_of::<NotReady>(),
        schema_of::<ReloadFailure>(),
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
            "store_damaged_rebuilding",
            "store_damaged_awaiting_demand",
            "schema_unreadable",
            "leg_unwound",
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
            "host/unknown-vault",
            "host/unsupported-attach-mode",
            "host/already-served",
            "host/entry-held",
            "host/entry-not-ready",
            "host/reader-unavailable",
            "host/registry-unwritable",
            "vault/ambiguous-root",
            "vault/reload-busy",
            "vault/reload-failed",
            "vault/cursor-order-changed",
            "engine/not-enabled",
            "engine/unavailable",
            "engine/failed",
        ])
    );
}

/// A refused mode advertises the mode as a nested enum, so a surface renders
/// the branchable fact rather than a sentence naming it.
#[test]
fn a_refused_mode_advertises_the_typed_mode_it_carries() {
    let schema = schema_of::<ErrorDetail>();
    let refused = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "code") == Some("host/unsupported-attach-mode"))
        .expect("the unsupported-attach-mode branch");
    assert_eq!(
        property_names(refused),
        ["code", "mode"].into_iter().collect()
    );
    assert_eq!(
        refused["properties"]["mode"]["$ref"].as_str(),
        Some("#/$defs/AttachMode")
    );
    assert_eq!(
        sorted(branches(&schema_of::<AttachMode>()).iter().map(|branch| {
            string_constant(branch)
                .unwrap_or_else(|| panic!("a mode branch is not a pinned string: {branch}"))
        })),
        sorted(["durable", "throwaway"])
    );
}

/// A name advertises the grammar its constructor keeps: a surface validating
/// against the schema refuses the strings the reader refuses.
///
/// The description is pinned here because a name is the one type whose schema
/// is written by hand: a derive lifts its description out of the type's doc
/// comment, and this one is a second spelling of that doc. The literal below
/// is the third, and it is what makes the two say the same sentence — the
/// bound clause at its end is the clause the hand-written spelling can lose
/// while the type's doc keeps it.
#[test]
fn a_vault_name_advertises_the_grammar_it_is_parsed_through() {
    let schema = schema_of::<VaultName>();
    assert_eq!(schema["type"].as_str(), Some("string"));
    assert_eq!(schema["pattern"].as_str(), Some(VaultName::PATTERN));
    assert_eq!(
        schema["maxLength"].as_u64(),
        Some(VaultName::MAXIMUM_BYTES as u64)
    );
    assert_eq!(
        schema["description"].as_str(),
        Some(
            "A vault's name: a lowercase letter, then lowercase letters, digits, `+`, `.` and `-`, at most 255 bytes of them."
        )
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
            "host/unknown-vault",
            "host/unsupported-attach-mode",
            "host/already-served",
            "host/entry-held",
            "host/entry-not-ready",
            "host/reader-unavailable",
            "host/registry-unwritable",
            "vault/ambiguous-root",
            "vault/reload-busy",
            "vault/reload-failed",
            "vault/cursor-order-changed",
            "engine/not-enabled",
            "engine/unavailable",
            "engine/failed",
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
            "document/frontmatter-unreadable",
            "document/undeclared-tag"
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
/// out of the message. The field refers to the name's own definition, so the
/// echo a refusal carries advertises the grammar the request was parsed
/// through rather than a bare string.
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
        branch["properties"]["name"]["$ref"].as_str(),
        Some("#/$defs/VaultName")
    );
    assert_eq!(
        schema["$defs"]["VaultName"]["pattern"].as_str(),
        schema_of::<VaultName>()["pattern"].as_str(),
        "the referenced definition is not the name's own grammar: {schema}"
    );
}

/// A duplicate root advertises its colliding names as an array of the name's
/// own definition, so a surface validating an envelope holds every alias to the
/// grammar a request names a vault through.
#[test]
fn a_duplicate_root_advertises_its_colliding_names() {
    let schema = schema_of::<ErrorDetail>();
    let branch = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "code") == Some("host/duplicate-root"))
        .expect("the duplicate-root branch");
    assert_eq!(
        property_names(branch),
        ["code", "aliases"].into_iter().collect()
    );
    assert_eq!(
        branch["properties"]["aliases"]["type"].as_str(),
        Some("array")
    );
    assert_eq!(
        branch["properties"]["aliases"]["items"]["$ref"].as_str(),
        Some("#/$defs/VaultName")
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

// ── The address grammar ──────────────────────────────────────────────────

/// A recorded path advertises the grammar its constructor keeps, in words.
/// There is no `pattern`: whether a path is absolute is a platform question,
/// and a regular expression that answered it on one platform would answer it
/// wrongly on another, admitting paths the reader refuses.
///
/// The descriptions are pinned here for the reason the name's is: these are
/// hand-written schemas, so the sentence the schema carries and the sentence
/// the type's doc carries are two spellings that nothing else holds equal.
#[test]
fn a_recorded_path_advertises_the_grammar_it_is_parsed_through() {
    for (schema, description) in [
        (
            schema_of::<VaultRoot>(),
            "A vault's root directory: an absolute UTF-8 path.",
        ),
        (
            schema_of::<SchemaSource>(),
            "Where a vault's schema is read from: an absolute UTF-8 path.",
        ),
    ] {
        assert_eq!(schema["type"].as_str(), Some("string"));
        assert_eq!(schema["description"].as_str(), Some(description));
        assert!(
            schema.get("pattern").is_none(),
            "a recorded path advertises a pattern: {schema}"
        );
    }
}

#[test]
fn a_poll_backend_advertises_its_bare_string() {
    let schema = schema_of::<PollBackend>();
    let backends: Vec<&str> = branches(&schema)
        .iter()
        .map(|branch| {
            string_constant(branch)
                .unwrap_or_else(|| panic!("a backend branch is not a pinned string: {branch}"))
        })
        .collect();
    assert_eq!(sorted(backends.clone()), sorted(["poll"]));
    assert_eq!(
        sorted(backends),
        sorted(PollBackend::ALL.map(|backend| backend.as_str()))
    );
}

/// An address advertises its `by` tag, and each branch refers to the grammar
/// its payload is parsed through rather than restating it as a bare string.
#[test]
fn a_vault_address_advertises_its_by_tag_and_the_grammars_behind_it() {
    let schema = schema_of::<VaultAddress>();
    assert_eq!(
        sorted(tag_constants(&schema, "by")),
        sorted(["name", "root"])
    );
    for (tag, field, definition) in [("name", "name", "VaultName"), ("root", "root", "VaultRoot")] {
        let branch = branches(&schema)
            .iter()
            .find(|branch| tag_constant(branch, "by") == Some(tag))
            .unwrap_or_else(|| panic!("the {tag} branch"));
        assert_eq!(property_names(branch), ["by", field].into_iter().collect());
        assert_eq!(
            branch["properties"][field]["$ref"].as_str(),
            Some(format!("#/$defs/{definition}").as_str())
        );
        assert!(
            schema["$defs"][definition].is_object(),
            "the referenced definition is absent: {schema}"
        );
    }
}

// ── The verb registry ────────────────────────────────────────────────────

/// A verb and a scope are advertised as the bare strings they are on the wire,
/// and the walkable lists cannot drift behind the enums surfaces render.
#[test]
fn a_verb_and_a_scope_advertise_their_bare_strings() {
    let members = |schema: &Value| -> Vec<String> {
        branches(schema)
            .iter()
            .map(|branch| {
                string_constant(branch)
                    .unwrap_or_else(|| panic!("a branch is not a pinned string: {branch}"))
                    .to_owned()
            })
            .collect()
    };
    let verbs = members(&schema_of::<Verb>());
    assert_eq!(verbs.len(), Verb::ALL.len());
    assert_eq!(
        sorted(verbs.iter().map(String::as_str)),
        sorted(Verb::ALL.map(|verb| verb.as_str()))
    );
    let scopes = members(&schema_of::<RequestScope>());
    assert_eq!(
        sorted(scopes.iter().map(String::as_str)),
        sorted(["vault", "registry", "installation"])
    );
}

// ── The resolution target and the predicate grammar ──────────────────────

/// A target advertises the grammar its constructor keeps, in words. There is
/// no `pattern`: the address half is a path suffix, and the one rule the
/// reader applies to it is stated plainly instead.
#[test]
fn a_resolution_target_advertises_the_grammar_it_is_parsed_through() {
    let schema = schema_of::<ResolutionTarget>();
    assert_eq!(schema["type"].as_str(), Some("string"));
    assert_eq!(
        schema["description"].as_str(),
        Some(
            "What a request names one document by: a path suffix, optionally followed by `#` and a heading, or `#^` and a block identifier. The path suffix is not empty."
        )
    );
    assert!(
        schema.get("pattern").is_none(),
        "a target advertises a pattern: {schema}"
    );
}

/// The conjunction advertises one branch per operator, tagged `op`, and the
/// typed halves refer to their own definitions rather than restating them.
#[test]
fn a_predicate_advertises_its_op_tag_and_the_types_behind_it() {
    let schema = schema_of::<Predicate>();
    assert_eq!(
        sorted(tag_constants(&schema, "op")),
        sorted([
            "eq",
            "not_eq",
            "in",
            "has",
            "missing",
            "before",
            "after",
            "matches",
            "path",
            "links_to",
            "resolves",
            "tag",
            "has_finding",
        ])
    );
    let branch = |op: &str| {
        branches(&schema)
            .iter()
            .find(|branch| tag_constant(branch, "op") == Some(op))
            .unwrap_or_else(|| panic!("the {op} branch"))
            .clone()
    };
    assert_eq!(
        property_names(&branch("eq")),
        ["op", "key", "value"].into_iter().collect()
    );
    for op in ["links_to", "resolves"] {
        assert_eq!(
            branch(op)["properties"]["target"]["$ref"].as_str(),
            Some("#/$defs/ResolutionTarget"),
            "the {op} branch restates the target grammar"
        );
    }
    assert_eq!(
        branch("has_finding")["properties"]["kind"]["$ref"].as_str(),
        Some("#/$defs/FindingKind")
    );
    for definition in ["ResolutionTarget", "FindingKind"] {
        assert!(
            schema["$defs"][definition].is_object(),
            "the referenced definition is absent: {schema}"
        );
    }
}

/// The anchor is read back as an object tagged `kind`, so a consumer that
/// holds a parsed target branches on which place it names.
#[test]
fn an_anchor_advertises_its_kind_tag() {
    let schema = schema_of::<Anchor>();
    assert_eq!(
        sorted(tag_constants(&schema, "kind")),
        sorted(["heading", "block"])
    );
}

// ── The cursor envelope ──────────────────────────────────────────────────

/// A cursor advertises one opaque string, and the pattern is the alphabet it
/// is spelled in — which is the whole of what a validator can say about a
/// value a consumer is told to read nothing out of.
#[test]
fn a_cursor_advertises_one_opaque_string() {
    let schema = schema_of::<Cursor>();
    assert_eq!(schema["type"].as_str(), Some("string"));
    assert_eq!(schema["pattern"].as_str(), Some("^[A-Za-z0-9_-]+$"));
    let description = schema["description"]
        .as_str()
        .expect("a cursor advertises a description");
    assert!(
        description.contains("Opaque") && description.contains("unchanged"),
        "the description does not say the string is opaque: {description}"
    );
    assert!(
        schema.get("properties").is_none(),
        "a cursor advertises fields a consumer could read: {schema}"
    );
}

/// The key advertises its `row` tag, one branch per paged row type, so the
/// shape stays a derive even though the wrapping around it is hand-written.
#[test]
fn a_cursor_key_advertises_its_row_tag() {
    let schema = schema_of::<CursorKey>();
    assert_eq!(
        sorted(tag_constants(&schema, "row")),
        sorted(["document", "hit", "tally", "finding", "facet", "ordinal"])
    );
    let document = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "row") == Some("document"))
        .expect("the document branch");
    assert_eq!(
        property_names(document),
        ["row", "sort", "path"].into_iter().collect()
    );
    let hit = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "row") == Some("hit"))
        .expect("the hit branch");
    assert_eq!(
        property_names(hit),
        ["row", "score", "path"].into_iter().collect()
    );
    assert_eq!(
        hit["properties"]["score"]["$ref"].as_str(),
        Some("#/$defs/Score"),
        "a hit restates the score grammar: {hit}"
    );
    assert_eq!(schema["$defs"]["Score"]["type"].as_str(), Some("number"));
}

/// A score is advertised as the bare number it is on the wire, so a surface
/// renders the value rather than an object wrapping it. Finiteness is the read
/// path's and is not a constraint a JSON Schema validator can state.
#[test]
fn a_score_advertises_the_number_it_is() {
    let schema = schema_of::<Score>();
    assert_eq!(schema["type"].as_str(), Some("number"));
    assert!(
        schema.get("properties").is_none(),
        "a score advertises fields: {schema}"
    );
}

#[test]
fn a_facet_kind_and_a_movement_advertise_their_bare_strings() {
    let members = |schema: &Value| -> Vec<String> {
        branches(schema)
            .iter()
            .map(|branch| {
                string_constant(branch)
                    .unwrap_or_else(|| panic!("a branch is not a pinned string: {branch}"))
                    .to_owned()
            })
            .collect()
    };
    assert_eq!(
        sorted(
            members(&schema_of::<FacetKind>())
                .iter()
                .map(String::as_str)
        ),
        sorted([
            "declared_field",
            "observed_field",
            "declared_tag",
            "folder",
            "path_rule",
        ])
    );
    assert_eq!(
        sorted(members(&schema_of::<Moved>()).iter().map(String::as_str)),
        sorted(["epoch", "generation", "sidecar_revision"])
    );
}

/// A page advertises its three fields, and the continuation refers to the
/// cursor's own definition, so a surface publishing a page publishes the
/// opaque string with it.
#[test]
fn a_page_advertises_its_rows_its_continuation_and_what_moved() {
    let schema = schema_of::<Page<String>>();
    assert_eq!(
        property_names(&schema),
        ["rows", "next", "moved"].into_iter().collect()
    );
    assert_eq!(schema["properties"]["rows"]["type"].as_str(), Some("array"));
    assert!(
        schema["$defs"]["Cursor"].is_object(),
        "a page carries no definition of the cursor: {schema}"
    );
}

/// The snapshot advertises the four parts a continuation is judged against,
/// under the snake_case names the wire uses.
#[test]
fn a_snapshot_advertises_the_parts_a_continuation_is_judged_against() {
    let schema = schema_of::<Snapshot>();
    assert_eq!(
        property_names(&schema),
        [
            "epoch",
            "generation",
            "schema_fingerprint",
            "sidecar_revision",
        ]
        .into_iter()
        .collect()
    );
}

// ── The answer reading and the read product ──────────────────────────────

/// A reading advertises the four parts a consumer judges an answer by, and
/// refers to the trust vocabulary rather than restating it.
#[test]
fn a_reading_advertises_the_parts_an_answer_is_judged_by() {
    let schema = schema_of::<AnswerReading>();
    assert_eq!(
        property_names(&schema),
        ["trust", "epoch", "generation", "ladder"]
            .into_iter()
            .collect()
    );
    assert_eq!(
        schema["properties"]["trust"]["$ref"].as_str(),
        Some("#/$defs/TrustState")
    );
    assert!(
        schema["$defs"]["RungReport"].is_object(),
        "the reading carries no definition of a rung report: {schema}"
    );
}

/// Addressing is a flat string of its own: what a verb carries, apart from the
/// scope a request under it is answered from.
#[test]
fn an_addressing_advertises_its_bare_strings() {
    assert_eq!(
        sorted(
            branches(&schema_of::<Addressing>())
                .iter()
                .map(|branch| string_constant(branch).unwrap_or_else(|| panic!(
                    "an addressing branch is not a pinned string: {branch}"
                )))
        ),
        sorted(["required", "none", "optional"])
    );
    assert_eq!(
        sorted(branches(&schema_of::<RequestScope>()).iter().map(|branch| {
            string_constant(branch)
                .unwrap_or_else(|| panic!("a scope branch is not a pinned string: {branch}"))
        })),
        sorted(["vault", "registry", "installation"])
    );
}

/// A rung report advertises its `rung` tag, one branch per rung, and each
/// branch carries what that rung holds: the floor nothing, a request-time rung
/// its model, the stateful rung its model and its freshness.
#[test]
fn a_rung_report_advertises_its_rung_tag_and_what_each_rung_holds() {
    let schema = schema_of::<RungReport>();
    assert_eq!(
        sorted(tag_constants(&schema, "rung")),
        sorted(["lexical", "vector", "expansion", "rerank"])
    );
    let branch = |rung: &str| {
        branches(&schema)
            .iter()
            .find(|branch| tag_constant(branch, "rung") == Some(rung))
            .unwrap_or_else(|| panic!("the {rung} branch"))
            .clone()
    };
    assert_eq!(
        property_names(&branch("lexical")),
        ["rung"].into_iter().collect()
    );
    assert_eq!(
        property_names(&branch("vector")),
        ["rung", "model", "freshness"].into_iter().collect()
    );
    for rung in ["expansion", "rerank"] {
        assert_eq!(
            property_names(&branch(rung)),
            ["rung", "model"].into_iter().collect(),
            "the {rung} branch advertises a lag a request-time rung does not hold"
        );
    }
    assert_eq!(
        branch("vector")["properties"]["freshness"]["$ref"].as_str(),
        Some("#/$defs/Freshness")
    );
    assert_eq!(
        branch("rerank")["properties"]["model"]["$ref"].as_str(),
        Some("#/$defs/ModelIdentity")
    );
}

#[test]
fn a_rung_a_freshness_and_a_section_advertise_their_vocabularies() {
    assert_eq!(
        sorted(branches(&schema_of::<Rung>()).iter().map(|branch| {
            string_constant(branch)
                .unwrap_or_else(|| panic!("a rung branch is not a pinned string: {branch}"))
        })),
        sorted(["lexical", "vector", "expansion", "rerank"])
    );
    assert_eq!(
        sorted(tag_constants(&schema_of::<Freshness>(), "state")),
        sorted(["trailing", "rescanning"])
    );
    assert_eq!(
        sorted(tag_constants(&schema_of::<EngineSection>(), "state")),
        sorted(["absent", "disabled", "malformed", "enabled"])
    );
}

/// The unsatisfied vocabulary advertises its `part` tag, one branch per part a
/// request can leave unapplied.
#[test]
fn an_unsatisfied_part_advertises_its_part_tag() {
    let schema = schema_of::<Unsatisfied>();
    assert_eq!(
        sorted(tag_constants(&schema, "part")),
        sorted([
            "unknown_sort_key",
            "unknown_projection_key",
            "unknown_predicate_key",
            "bare_directory",
            "malformed_glob",
            "impossible_path",
            "missing_section",
            "resolves_not_applicable",
        ])
    );
    let resolves = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "part") == Some("resolves_not_applicable"))
        .expect("the resolves branch");
    assert_eq!(
        resolves["properties"]["target"]["$ref"].as_str(),
        Some("#/$defs/ResolutionTarget")
    );
}

/// An answer advertises the reading, the unapplied parts and the verb's own
/// report, so a surface publishing a verb publishes all three together.
#[test]
fn a_vault_answer_advertises_its_reading_its_unsatisfied_parts_and_its_report() {
    let schema = schema_of::<VaultAnswer<String>>();
    assert_eq!(
        property_names(&schema),
        ["reading", "unsatisfied", "report"].into_iter().collect()
    );
    for definition in ["AnswerReading", "Unsatisfied", "TrustState"] {
        assert!(
            schema["$defs"][definition].is_object(),
            "an answer carries no definition of {definition}"
        );
    }
}

/// The two vocabularies the new codes carry as payloads advertise their tags,
/// so a client branches on the typed half and reads the prose beside it.
#[test]
fn a_not_ready_state_and_a_reload_failure_advertise_their_tags() {
    assert_eq!(
        sorted(tag_constants(&schema_of::<NotReady>(), "state")),
        sorted(["warming", "unattached"])
    );
    assert_eq!(
        sorted(tag_constants(&schema_of::<ReloadFailure>(), "kind")),
        sorted([
            "control_file",
            "environmental",
            "store_damaged",
            "watcher_terminal",
            "lost_maintainership",
            "maintainer_contended",
            "unsupported",
        ])
    );
    let detail = schema_of::<ErrorDetail>();
    let not_ready = branches(&detail)
        .iter()
        .find(|branch| tag_constant(branch, "code") == Some("host/entry-not-ready"))
        .expect("the entry-not-ready branch");
    assert_eq!(
        not_ready["properties"]["state"]["$ref"].as_str(),
        Some("#/$defs/NotReady")
    );
}

/// A changed order is carried whole: the detail refers to the continuation's
/// own type rather than re-spelling its two fields, so a client that holds the
/// struct holds what the refusal carries.
#[test]
fn a_changed_order_is_advertised_as_the_type_the_continuation_answers_with() {
    let schema = schema_of::<ErrorDetail>();
    let changed = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "code") == Some("vault/cursor-order-changed"))
        .expect("the cursor-order-changed branch");
    assert_eq!(
        property_names(changed),
        ["code", "order"].into_iter().collect()
    );
    assert_eq!(
        changed["properties"]["order"]["$ref"].as_str(),
        Some("#/$defs/CursorOrderChanged")
    );
    assert_eq!(
        property_names(&schema["$defs"]["CursorOrderChanged"]),
        ["minted_under", "current"].into_iter().collect()
    );
}

/// The registry-write refusal advertises its account as prose beside the code,
/// which is the whole of what it carries.
#[test]
fn an_unwritable_registry_advertises_its_account_as_prose() {
    let schema = schema_of::<ErrorDetail>();
    let unwritable = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "code") == Some("host/registry-unwritable"))
        .expect("the registry-unwritable branch");
    assert_eq!(
        property_names(unwritable),
        ["code", "detail"].into_iter().collect()
    );
    assert_eq!(
        unwritable["properties"]["detail"]["type"].as_str(),
        Some("string")
    );
}
