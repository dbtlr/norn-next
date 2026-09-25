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
    Addressing, Advisory, Anchor, AnswerAdvisory, AnswerReading, AnswerShape, AttachMode,
    Attention, BlockRow, BodyText, CANDIDATE_HEAD, Candidate, CandidateHead, Change, Collection,
    CollectionPage, CollectionSelector, Column, ComparedBy, ContainerKind, ControlFileFailure,
    CountParams, CountReport, Cursor, CursorKey, DescribeParams, DescribeReport, Direction,
    Directory, DoctorRegistryParams, DoctorRegistryReport, DocumentPath, DocumentRow, Drift,
    EngineHealth, EngineSection, EngineStatus, ErrorDetail, ErrorEnvelope, Facet, FacetKind,
    FieldType, FieldValue, FindParams, FindReport, FindingKind, FindingRow, FindingScope,
    Fingerprints, Freshness, GetParams, GetReport, GroupKey, HeadingRow, Hint, Hit, KindTally,
    LadderDeclaration, LinkFamily, LinkHealth, LinkRow, ListParams, ListReport, MaintainerIdentity,
    Moved, NameSet, NotReady, Page, PagedRows, PathRuleKind, PollBackend, Predicate, Published,
    ReadFailure, ReasonCode, RegisterParams, RegisterReport, Registration, RegistryProblem,
    RegistrySanity, ReloadFailure, ReloadOutcome, ReloadParams, ReloadReport, Replace,
    RequestBound, RequestPart, RequestScope, ResolutionTarget, ResolveParams, ResolveReport,
    RollUp, Rung, RungReport, RungSelection, RungSet, RungSkipReason, SchemaSource, Score,
    SearchParams, SearchReport, SetParams, SetReport, Severity, SidecarRevision, Snapshot, Sort,
    SortKey, Span, StatusParams, StatusReport, TagRow, TagSource, TagStance, Tally, TrustState,
    UnregisterParams, UnregisterReport, Unsatisfied, UntrustedReason, ValidateParams,
    ValidateReport, VaultAddress, VaultAnswer, VaultName, VaultRoot, VaultStatus, Verb,
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

/// Every schema a surface renders against, built once and read by the census
/// tests below.
///
/// A generic carrier joins the census at one concrete instantiation —
/// `Page<String>`, `Collection<String>` and `VaultAnswer<String>` — because
/// what the census reads is the carrier's own type and field documentation,
/// which is the same whatever it carries. The row and report types a surface
/// fills them with are walked on lines of their own.
fn every_wire_schema() -> Vec<Value> {
    vec![
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
        schema_of::<NameSet>(),
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
        schema_of::<LadderDeclaration>(),
        schema_of::<Freshness>(),
        schema_of::<Score>(),
        schema_of::<SidecarRevision>(),
        schema_of::<EngineSection>(),
        schema_of::<Unsatisfied>(),
        schema_of::<ComparedBy>(),
        schema_of::<AnswerAdvisory>(),
        schema_of::<RungSkipReason>(),
        schema_of::<VaultAnswer<String>>(),
        schema_of::<NotReady>(),
        schema_of::<ControlFileFailure>(),
        schema_of::<ReloadFailure>(),
        schema_of::<DocumentPath>(),
        schema_of::<Span>(),
        schema_of::<Column>(),
        schema_of::<Collection<String>>(),
        schema_of::<BodyText>(),
        schema_of::<LinkHealth>(),
        schema_of::<LinkFamily>(),
        schema_of::<LinkRow>(),
        schema_of::<HeadingRow>(),
        schema_of::<BlockRow>(),
        schema_of::<TagSource>(),
        schema_of::<TagRow>(),
        schema_of::<FieldValue>(),
        schema_of::<DocumentRow>(),
        schema_of::<Candidate>(),
        schema_of::<CandidateHead>(),
        schema_of::<Hint>(),
        schema_of::<FindingRow>(),
        schema_of::<Direction>(),
        schema_of::<SortKey>(),
        schema_of::<Sort>(),
        schema_of::<RungSet>(),
        schema_of::<RungSelection>(),
        schema_of::<Hit>(),
        schema_of::<CollectionSelector>(),
        schema_of::<CollectionPage>(),
        schema_of::<GetReport>(),
        schema_of::<GroupKey>(),
        schema_of::<Tally>(),
        schema_of::<KindTally>(),
        schema_of::<ValidateReport>(),
        schema_of::<FieldType>(),
        schema_of::<ContainerKind>(),
        schema_of::<PathRuleKind>(),
        schema_of::<TagStance>(),
        schema_of::<Facet>(),
        schema_of::<FindParams>(),
        schema_of::<FindReport>(),
        schema_of::<SearchParams>(),
        schema_of::<SearchReport>(),
        schema_of::<GetParams>(),
        schema_of::<CountParams>(),
        schema_of::<CountReport>(),
        schema_of::<ValidateParams>(),
        schema_of::<DescribeParams>(),
        schema_of::<DescribeReport>(),
        schema_of::<Directory>(),
        schema_of::<Registration>(),
        schema_of::<Published>(),
        schema_of::<Fingerprints>(),
        schema_of::<Drift>(),
        schema_of::<EngineStatus>(),
        schema_of::<Advisory>(),
        schema_of::<Attention>(),
        schema_of::<VaultStatus>(),
        schema_of::<RollUp>(),
        schema_of::<Change<SchemaSource>>(),
        schema_of::<Replace<VaultRoot>>(),
        schema_of::<RegisterParams>(),
        schema_of::<RegisterReport>(),
        schema_of::<UnregisterParams>(),
        schema_of::<UnregisterReport>(),
        schema_of::<ListParams>(),
        schema_of::<ListReport>(),
        schema_of::<SetParams>(),
        schema_of::<SetReport>(),
        schema_of::<ResolveParams>(),
        schema_of::<ResolveReport>(),
        schema_of::<StatusParams>(),
        schema_of::<StatusReport>(),
        schema_of::<ReloadOutcome>(),
        schema_of::<ReloadParams>(),
        schema_of::<ReloadReport>(),
        schema_of::<DoctorRegistryParams>(),
        schema_of::<DoctorRegistryReport>(),
        schema_of::<RegistrySanity>(),
        schema_of::<RegistryProblem>(),
        schema_of::<EngineHealth>(),
    ]
}

#[test]
fn every_wire_type_derives_a_schema() {
    for schema in every_wire_schema() {
        assert!(
            schema.get("$schema").is_some(),
            "the schema declares no dialect: {schema}"
        );
    }
}

/// Every `description` the schemas carry, at every depth, with the pointer it
/// sits at so a failure names where to look.
///
/// `description` is a JSON Schema keyword and a field name a wire type may
/// carry at once: a schema property named `description` sits at
/// `properties/description` and holds a whole sub-schema of its own. The walk
/// therefore descends into every object and array value without exception, and
/// collects text only where the key is `description` and the value is a
/// string, so a property by that name is walked rather than mistaken for the
/// keyword and skipped.
fn descriptions(schema: &Value, at: String, found: &mut Vec<(String, String)>) {
    match schema {
        Value::Object(members) => {
            for (key, value) in members {
                if key == "description"
                    && let Some(text) = value.as_str()
                {
                    found.push((at.clone(), text.to_string()));
                }
                descriptions(value, format!("{at}/{key}"), found);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                descriptions(item, format!("{at}/{index}"), found);
            }
        }
        _ => {}
    }
}

/// Every description the wire schemas publish, each with the pointer it sits
/// at.
fn published_descriptions() -> Vec<(String, String)> {
    let mut found = Vec::new();
    for schema in every_wire_schema() {
        let title = schema
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("a schema")
            .to_string();
        descriptions(&schema, title, &mut found);
    }
    assert!(
        !found.is_empty(),
        "the census walked no descriptions at all"
    );
    found
}

/// Whether a description carries a Rust intralink, in any spelling rustdoc
/// accepts: a shortcut link, which opens with a bracketed backtick, and a
/// reference link whose target is a Rust path. `](crate:` covers the `crate::`
/// spelling as well as the bare `crate:` one rustdoc also resolves. A vault's
/// own link grammars — `[[target]]` and `[title](target)` — are wire
/// documentation rather than intralinks and carry none of these.
fn publishes_an_intralink(description: &str) -> bool {
    description.contains("[`")
        || description.contains("](crate:")
        || description.contains("](super::")
}

/// Whether a description carries a Rust attribute. An attribute is maintainer
/// documentation about the Rust type: a consumer reading the schema cannot see
/// one, cannot write one, and has no vocabulary for what it governs.
fn publishes_an_attribute(description: &str) -> bool {
    description.contains("#[")
}

/// A description is what an MCP consumer reads, and schemars lifts it verbatim
/// out of the doc comment on a type, a variant or a field. A Rust intralink
/// survives that lift as its own source text, naming a symbol the consumer
/// cannot follow and has no vocabulary for, so a bracketed link of either
/// spelling is a description that leaked maintainer documentation. Rationale
/// belongs in module documentation, which schemars does not lift.
#[test]
fn no_published_description_carries_a_rust_intralink() {
    for (at, description) in &published_descriptions() {
        assert!(
            !publishes_an_intralink(description),
            "the description at {at} publishes a Rust intralink: {description}"
        );
    }
}

/// A description is lifted out of a doc comment, so maintainer rationale
/// about the Rust shape leaks the same way an intralink does. An attribute is
/// that rationale at its plainest: `#[non_exhaustive]` governs what a Rust
/// caller may destructure and says nothing about the bytes a consumer reads.
/// Rationale belongs in module documentation, which schemars does not lift.
#[test]
fn no_published_description_carries_a_rust_attribute() {
    for (at, description) in &published_descriptions() {
        assert!(
            !publishes_an_attribute(description),
            "the description at {at} publishes a Rust attribute: {description}"
        );
    }
}

/// The census reads the spellings a Rust intralink takes and leaves the
/// vault's own link grammars alone, which are wire vocabulary a description is
/// entitled to name.
#[test]
fn the_intralink_census_reads_every_spelling_a_rust_link_takes() {
    for leaked in [
        "the head [`CandidateHead`] carries",
        "see [the head](crate::finding_row::CandidateHead)",
        "see [the head](crate:finding_row)",
        "see [the head](super::CandidateHead)",
    ] {
        assert!(
            publishes_an_intralink(leaked),
            "the census reads no intralink in: {leaked}"
        );
    }
    for kept in [
        "A wikilink, `[[target]]`.",
        "An inline Markdown link, `[title](target)`.",
    ] {
        assert!(
            !publishes_an_intralink(kept),
            "the census reads a link grammar as an intralink: {kept}"
        );
    }
}

/// A wire type may carry a field named `description`, and its sub-schema is
/// where a leak would hide if the walk read the name as the JSON Schema
/// keyword and stopped.
#[test]
fn the_census_walks_a_schema_property_named_description() {
    let schema = serde_json::json!({
        "title": "Facet",
        "description": "A facet of the vault's schema.",
        "properties": {
            "description": {
                "type": "string",
                "description": "What the schema says the folder is for.",
            },
        },
    });
    let mut found = Vec::new();
    descriptions(&schema, "Facet".to_string(), &mut found);
    assert!(
        found.contains(&(
            "Facet/properties/description".to_string(),
            "What the schema says the folder is for.".to_string()
        )),
        "the census walked past the sub-schema of a property named description: {found:?}"
    );
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
            "host/read-failed",
            "vault/ambiguous-root",
            "vault/ambiguous-target",
            "vault/unknown-target",
            "vault/reload-busy",
            "vault/reload-failed",
            "vault/cursor-order-changed",
            "vault/unreadable-bound",
            "request/out-of-bound",
            "request/part-not-taken",
            "request/cursor-not-taken",
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
            "host/read-failed",
            "vault/ambiguous-root",
            "vault/ambiguous-target",
            "vault/unknown-target",
            "vault/reload-busy",
            "vault/reload-failed",
            "vault/cursor-order-changed",
            "vault/unreadable-bound",
            "request/out-of-bound",
            "request/part-not-taken",
            "request/cursor-not-taken",
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

/// A severity floor admits itself and every severity more urgent: an error
/// floor admits errors alone, and a warning floor admits both.
#[test]
fn a_severity_floor_admits_itself_and_what_is_more_urgent() {
    let admitted = |floor: Severity| -> Vec<Severity> {
        Severity::ALL
            .into_iter()
            .filter(|severity| severity.is_at_least(floor))
            .collect()
    };
    assert_eq!(admitted(Severity::Error), vec![Severity::Error]);
    assert_eq!(
        admitted(Severity::Warning),
        vec![Severity::Error, Severity::Warning]
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

/// A name set advertises the array its read path accepts: names of the name's
/// own definition, at least two of them, and each named once. A surface
/// validating against it passes exactly the lists this crate reads.
#[test]
fn a_name_set_advertises_the_two_distinct_names_it_holds() {
    let schema = schema_of::<NameSet>();
    assert_eq!(schema["type"].as_str(), Some("array"));
    assert_eq!(schema["items"]["$ref"].as_str(), Some("#/$defs/VaultName"));
    assert_eq!(
        schema["minItems"].as_u64(),
        Some(2),
        "the name set advertises a list that names one collision party or none: {schema}"
    );
    assert_eq!(
        schema["uniqueItems"].as_bool(),
        Some(true),
        "the name set advertises a list that names one party twice: {schema}"
    );
    assert_eq!(
        schema["$defs"]["VaultName"]["pattern"].as_str(),
        schema_of::<VaultName>()["pattern"].as_str(),
        "the referenced definition is not the name's own grammar: {schema}"
    );
}

/// The three carriers of a collision between registered names advertise one
/// name set, so a surface validating an envelope or a registry problem holds
/// every name to the grammar a request names a vault through and to the floor
/// of two the fact itself has.
#[test]
fn every_collision_advertises_the_one_name_set() {
    let detail = schema_of::<ErrorDetail>();
    for (code, field) in [
        ("host/duplicate-root", "aliases"),
        ("vault/ambiguous-root", "candidates"),
    ] {
        let branch = branches(&detail)
            .iter()
            .find(|branch| tag_constant(branch, "code") == Some(code))
            .unwrap_or_else(|| panic!("the {code} branch is not advertised: {detail}"));
        assert_eq!(
            property_names(branch),
            ["code", field].into_iter().collect()
        );
        assert_eq!(
            branch["properties"][field]["$ref"].as_str(),
            Some("#/$defs/NameSet"),
            "{code} does not carry the one name set: {detail}"
        );
    }
    assert_eq!(
        detail["$defs"]["NameSet"]["minItems"].as_u64(),
        Some(2),
        "the referenced definition carries no floor: {detail}"
    );

    let problem = schema_of::<RegistryProblem>();
    let branch = branches(&problem)
        .iter()
        .find(|branch| tag_constant(branch, "problem") == Some("duplicate_root"))
        .unwrap_or_else(|| panic!("the duplicate-root problem is not advertised: {problem}"));
    assert_eq!(
        property_names(branch),
        ["problem", "aliases"].into_iter().collect()
    );
    assert_eq!(
        branch["properties"]["aliases"]["$ref"].as_str(),
        Some("#/$defs/NameSet"),
        "the problem does not carry the one name set: {problem}"
    );
    assert_eq!(
        problem["$defs"]["NameSet"]["uniqueItems"].as_bool(),
        Some(true),
        "the referenced definition holds no name to being named once: {problem}"
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
    assert_eq!(
        branch("in")["properties"]["values"]["minItems"].as_u64(),
        Some(1),
        "the in branch advertises a membership in no value: {schema}"
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
        ["row", "order", "sort", "path"].into_iter().collect()
    );
    assert_eq!(
        document["properties"]["order"]["$ref"].as_str(),
        Some("#/$defs/Sort"),
        "a document key names the order it was minted in as the order a request names"
    );
    let ordinal = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "row") == Some("ordinal"))
        .expect("the ordinal branch");
    assert_eq!(
        property_names(ordinal),
        ["row", "of", "index"].into_iter().collect(),
        "an ordinal names the collection it pages"
    );
    let hit = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "row") == Some("hit"))
        .expect("the hit branch");
    assert_eq!(
        property_names(hit),
        ["row", "ladder", "score", "path"].into_iter().collect()
    );
    assert_eq!(
        hit["properties"]["ladder"]["$ref"].as_str(),
        Some("#/$defs/RungSet"),
        "a hit key names the ladder it was ranked by as a rung set: {hit}"
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
            "tag_pattern",
            "undeclared_tags",
        ])
    );
    assert_eq!(
        sorted(
            members(&schema_of::<FacetKind>())
                .iter()
                .map(String::as_str)
        ),
        sorted(FacetKind::ALL.map(|kind| kind.as_str())),
        "the walkable list of facet kinds drifted from the enum"
    );
    assert_eq!(
        FacetKind::in_code_order().map(|kind| kind.as_str()),
        [
            "declared_field",
            "declared_tag",
            "folder",
            "observed_field",
            "path_rule",
            "tag_pattern",
            "undeclared_tags",
        ],
        "a page reads the facet kinds in the byte order of their codes"
    );
    let containers: Vec<String> = ContainerKind::ALL
        .iter()
        .map(|container| {
            serde_json::to_value(container)
                .expect("a container as JSON")
                .as_str()
                .expect("a container is a bare string")
                .to_owned()
        })
        .collect();
    assert_eq!(
        sorted(
            branches(&schema_of::<ContainerKind>())
                .iter()
                .filter_map(string_constant)
        ),
        sorted(containers.iter().map(String::as_str)),
        "the walkable list of containers drifted from the enum"
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
/// under the snake_case names the wire uses, and the sidecar part is the pair
/// of the sidecar's own epoch and its revision within it.
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
    assert_eq!(
        property_names(&schema_of::<SidecarRevision>()),
        ["epoch", "revision"].into_iter().collect()
    );
    assert!(
        schema.to_string().contains("#/$defs/SidecarRevision"),
        "the snapshot's sidecar part is not the epoch-qualified pair: {schema}"
    );
}

// ── The answer reading and the read product ──────────────────────────────

/// A reading advertises the three parts every answer is judged by, and refers
/// to the trust vocabulary rather than restating it.
#[test]
fn a_reading_advertises_the_parts_an_answer_is_judged_by() {
    let schema = schema_of::<AnswerReading>();
    assert_eq!(
        property_names(&schema),
        ["trust", "epoch", "generation"].into_iter().collect()
    );
    assert_eq!(
        schema["properties"]["trust"]["$ref"].as_str(),
        Some("#/$defs/TrustState")
    );
}

/// A search report advertises the ladder that ranked it and the page of hits,
/// both required, so a surface validating a search answer refuses one that
/// declares no ladder.
#[test]
fn a_search_report_advertises_its_ladder_and_its_page() {
    let schema = schema_of::<SearchReport>();
    assert_eq!(
        property_names(&schema),
        ["ladder", "page"].into_iter().collect()
    );
    let required: BTreeSet<&str> = schema["required"]
        .as_array()
        .expect("the report's required list")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(required, ["ladder", "page"].into_iter().collect());
    assert_eq!(
        schema["properties"]["ladder"]["$ref"].as_str(),
        Some("#/$defs/LadderDeclaration")
    );
    let page = &schema["properties"]["page"]["$ref"];
    let page = page
        .as_str()
        .and_then(|reference| reference.strip_prefix("#/$defs/"))
        .unwrap_or_else(|| panic!("the report's page is no definition: {schema}"));
    assert_eq!(
        property_names(&schema["$defs"][page]),
        ["rows", "next", "moved"].into_iter().collect()
    );
    assert!(
        schema["$defs"]["Hit"].is_object(),
        "the report carries no definition of a hit: {schema}"
    );
}

/// A ladder declaration advertises its rungs as reports holding a retrieval
/// rung, which implies at least one, so a surface validating a search answer
/// refuses a ladder of enhancers alone.
#[test]
fn a_ladder_declaration_advertises_a_retrieval_rung() {
    let schema = schema_of::<LadderDeclaration>();
    assert_eq!(
        property_names(&schema),
        ["rungs", "repeatable"].into_iter().collect()
    );
    let rungs = &schema["properties"]["rungs"];
    assert_eq!(rungs["minItems"].as_u64(), Some(1));
    assert_eq!(rungs["items"]["$ref"].as_str(), Some("#/$defs/RungReport"));
    assert_eq!(
        rungs["contains"]["properties"]["rung"]["enum"].as_array(),
        Some(&retrieval_rung_spellings()),
        "the declaration advertises a ladder holding no retrieval rung: {schema}"
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
            "unknown_group_key",
            "bare_directory",
            "malformed_glob",
            "malformed_query",
            "impossible_path",
            "missing_section",
            "missing_block",
            "resolves_not_applicable",
            "query_names_no_word",
            "links_to_ambiguous",
            "links_to_unknown",
        ])
    );
    let ambiguous = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "part") == Some("links_to_ambiguous"))
        .expect("the ambiguous links-to branch");
    assert_eq!(
        ambiguous["properties"]["candidates"]["$ref"].as_str(),
        Some("#/$defs/CandidateHead")
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

/// An answer advisory advertises its tag, where a mixed-offset comparison
/// was made as the flat strings the vocabulary holds, and why a rung was left
/// out as the refusal code a search naming it exactly meets.
#[test]
fn an_answer_advisory_advertises_its_tag_and_where_it_compared() {
    let schema = schema_of::<AnswerAdvisory>();
    assert_eq!(
        sorted(tag_constants(&schema, "advisory")),
        sorted(["mixed_offset", "rung_skipped", "rung_depth_reached"])
    );
    let skipped = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "advisory") == Some("rung_skipped"))
        .expect("the rung-skipped branch");
    assert_eq!(
        property_names(skipped),
        ["advisory", "rung", "reason"].into_iter().collect()
    );
    assert_eq!(
        skipped["properties"]["reason"]["$ref"].as_str(),
        Some("#/$defs/RungSkipReason")
    );
    assert_eq!(
        sorted(tag_constants(&schema_of::<RungSkipReason>(), "reason")),
        sorted(["engine/unavailable"])
    );
    let reached = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "advisory") == Some("rung_depth_reached"))
        .expect("the rung-depth-reached branch");
    assert_eq!(
        property_names(reached),
        ["advisory", "rung", "depth"].into_iter().collect()
    );
    let compared_by = schema_of::<ComparedBy>();
    assert_eq!(
        sorted(branches(&compared_by).iter().filter_map(string_constant)),
        sorted(["sort", "group", "predicate"])
    );
}

/// An answer advertises the reading, the unapplied parts, the advisories and
/// the verb's own report, so a surface publishing a verb publishes all four
/// together.
#[test]
fn a_vault_answer_advertises_its_reading_its_unsatisfied_parts_and_its_report() {
    let schema = schema_of::<VaultAnswer<String>>();
    assert_eq!(
        property_names(&schema),
        ["reading", "unsatisfied", "advisories", "report"]
            .into_iter()
            .collect()
    );
    for definition in [
        "AnswerReading",
        "Unsatisfied",
        "AnswerAdvisory",
        "TrustState",
    ] {
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

/// The read refusals advertise their facts as nested typed shapes: a bound,
/// a part and an answer, two rows, and a failure, each with its tag pinned.
#[test]
fn a_read_refusal_advertises_the_typed_facts_it_carries() {
    let schema = schema_of::<ErrorDetail>();
    let branch = |code: &str| {
        branches(&schema)
            .iter()
            .find(|branch| tag_constant(branch, "code") == Some(code))
            .unwrap_or_else(|| panic!("the {code} branch"))
            .clone()
    };
    let cases: [(&str, &[&str]); 4] = [
        ("request/out-of-bound", &["code", "bound"]),
        ("request/part-not-taken", &["code", "part", "answer"]),
        ("request/cursor-not-taken", &["code", "cursor", "paged"]),
        ("host/read-failed", &["code", "failure", "detail"]),
    ];
    for (code, properties) in cases {
        assert_eq!(
            property_names(&branch(code)),
            properties.iter().copied().collect(),
            "{code}"
        );
    }
    assert_eq!(
        sorted(tag_constants(&schema_of::<RequestBound>(), "kind")),
        sorted(["page_rows", "membership_values", "empty_membership"])
    );
    assert_eq!(
        sorted(tag_constants(&schema_of::<RequestPart>(), "kind")),
        sorted(["anchor", "column", "cursor", "limit", "unknown"])
    );
    assert_eq!(
        sorted(branches(&schema_of::<AnswerShape>()).iter().map(|branch| {
            string_constant(branch)
                .unwrap_or_else(|| panic!("an answer branch is not a pinned string: {branch}"))
        })),
        sorted(["collection_page", "record", "section", "block", "summary"])
    );
    assert_eq!(
        sorted(tag_constants(&schema_of::<PagedRows>(), "row")),
        sorted(["document", "hit", "tally", "finding", "facet", "collection"])
    );
    assert_eq!(
        sorted(tag_constants(&schema_of::<ReadFailure>(), "kind")),
        sorted(["statement", "declaration_not_pinned"])
    );
}

/// A changed order is carried whole: the detail refers to the continuation's
/// own type rather than re-spelling its fields, so a client that holds the
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
        ["minted_under", "current", "orders"].into_iter().collect()
    );
    let orders =
        serde_json::to_string(&schema["$defs"]["CursorOrderChanged"]["properties"]["orders"])
            .expect("the orders property serializes");
    assert!(
        orders.contains("#/$defs/OrderPair") && orders.contains("null"),
        "the two orders are one nullable pair: {orders}"
    );
    let pair = &schema["$defs"]["OrderPair"];
    assert_eq!(
        sorted(tag_constants(pair, "row")),
        sorted(["document", "hit"]),
        "a pair is tagged with the row kind its orders are orders of"
    );
    for branch in branches(pair) {
        let (row, half) = match tag_constant(branch, "row") {
            Some("document") => ("document", "#/$defs/Sort"),
            Some("hit") => ("hit", "#/$defs/RungSet"),
            other => panic!("a pair advertises the row kind {other:?}"),
        };
        assert_eq!(
            property_names(branch),
            ["row", "cursor", "request"].into_iter().collect()
        );
        for order in ["cursor", "request"] {
            assert_eq!(
                branch["properties"][order]["$ref"].as_str(),
                Some(half),
                "each half of a {row} pair is the order a {row} cursor names"
            );
        }
    }
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

// ── The document row and its columns ─────────────────────────────────────

/// A path advertises the one rule its reader keeps and no pattern: the rest of
/// what a document path may hold is the store's grammar, and a second
/// definition of it here would be a second grammar.
#[test]
fn a_document_path_advertises_the_grammar_it_is_parsed_through() {
    let schema = schema_of::<DocumentPath>();
    assert_eq!(schema["type"].as_str(), Some("string"));
    assert_eq!(schema["minLength"].as_u64(), Some(1));
    assert!(schema.get("pattern").is_none(), "{schema}");
    for sentence in ["Not empty", "relative to the vault root"] {
        assert!(
            schema["description"]
                .as_str()
                .is_some_and(|text| text.contains(sentence)),
            "the description does not say `{sentence}`: {schema}"
        );
    }
}

/// A column advertises its `col` tag, one branch per part of a document a read
/// can ask for, and the one that names a key carries it as a property beside
/// the tag.
#[test]
fn a_column_advertises_its_col_tag() {
    let schema = schema_of::<Column>();
    assert_eq!(
        sorted(tag_constants(&schema, "col")),
        sorted([
            "path", "field", "body", "links", "headings", "blocks", "tags", "findings", "fields",
        ])
    );
    let field = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "col") == Some("field"))
        .expect("the field branch");
    assert_eq!(property_names(field), ["col", "key"].into_iter().collect());
}

/// A nested collection advertises its head and its total, so a surface
/// publishing a row publishes what a cut looks like with it.
#[test]
fn a_collection_advertises_its_items_and_its_total() {
    let schema = schema_of::<Collection<String>>();
    assert_eq!(
        property_names(&schema),
        ["items", "total"].into_iter().collect()
    );
    assert_eq!(
        schema["properties"]["items"]["type"].as_str(),
        Some("array")
    );
}

/// A body advertises its head and the whole body's length beside it, so a
/// surface publishing a row publishes what a cut body looks like with it.
#[test]
fn a_body_advertises_its_text_and_the_length_of_the_whole() {
    let schema = schema_of::<BodyText>();
    assert_eq!(
        property_names(&schema),
        ["text", "byte_length"].into_iter().collect()
    );
    assert_eq!(
        schema["properties"]["text"]["type"].as_str(),
        Some("string")
    );
}

/// The row advertises the path and one property per column, and refers to the
/// row types its collections hold rather than restating them.
#[test]
fn a_document_row_advertises_its_path_and_one_property_per_column() {
    let schema = schema_of::<DocumentRow>();
    assert_eq!(
        property_names(&schema),
        [
            "path", "fields", "body", "links", "headings", "blocks", "tags", "findings",
        ]
        .into_iter()
        .collect()
    );
    let required: BTreeSet<&str> = schema["required"]
        .as_array()
        .expect("the row's required list")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(
        required,
        ["path"].into_iter().collect(),
        "a column a read did not project is advertised as required"
    );
    for definition in [
        "LinkRow",
        "HeadingRow",
        "BlockRow",
        "TagRow",
        "FindingRow",
        "BodyText",
    ] {
        assert!(
            schema["$defs"][definition].is_object(),
            "a row carries no definition of {definition}"
        );
    }
}

/// A link row advertises the syntactic half beside the resolved half, the
/// resolved half is the bounded head both other carriers advertise, and the
/// health it carries is the vocabulary rather than a sentence naming it.
#[test]
fn a_link_row_advertises_the_targets_and_the_health_read_off_them() {
    let schema = schema_of::<LinkRow>();
    assert_eq!(
        property_names(&schema),
        [
            "family", "embed", "protocol", "target", "title", "anchor", "span", "targets",
            "health",
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(
        schema["properties"]["targets"]["$ref"].as_str(),
        Some("#/$defs/CandidateHead"),
        "a link row restates the head rather than referring to it"
    );
    assert_eq!(
        schema["$defs"]["CandidateHead"]["properties"]["candidates"]["maxItems"].as_u64(),
        Some(CANDIDATE_HEAD as u64),
        "a link row's head is advertised without the bound it keeps"
    );
    assert_eq!(
        schema["properties"]["health"]["$ref"].as_str(),
        Some("#/$defs/LinkHealth")
    );
    assert_eq!(
        sorted(branches(&schema_of::<LinkHealth>()).iter().map(|branch| {
            string_constant(branch)
                .unwrap_or_else(|| panic!("a health branch is not a pinned string: {branch}"))
        })),
        sorted(["healthy", "broken", "ambiguous", "not_judged"])
    );
    assert_eq!(
        sorted(branches(&schema_of::<LinkFamily>()).iter().map(|branch| {
            string_constant(branch)
                .unwrap_or_else(|| panic!("a family branch is not a pinned string: {branch}"))
        })),
        sorted(["wikilink", "markdown"])
    );
}

#[test]
fn the_document_facts_advertise_their_vocabularies() {
    assert_eq!(
        property_names(&schema_of::<Span>()),
        ["line", "column", "byte_offset"].into_iter().collect()
    );
    assert_eq!(
        property_names(&schema_of::<HeadingRow>()),
        ["level", "text", "slug", "span"].into_iter().collect()
    );
    assert_eq!(
        property_names(&schema_of::<BlockRow>()),
        ["id", "span"].into_iter().collect()
    );
    assert_eq!(
        property_names(&schema_of::<TagRow>()),
        ["name", "source", "span"].into_iter().collect()
    );
    assert_eq!(
        sorted(branches(&schema_of::<TagSource>()).iter().map(|branch| {
            string_constant(branch)
                .unwrap_or_else(|| panic!("a source branch is not a pinned string: {branch}"))
        })),
        sorted(["body", "frontmatter"])
    );
    assert_eq!(
        sorted(tag_constants(&schema_of::<FieldValue>(), "kind")),
        sorted(["scalar", "sequence", "map", "null", "absent"])
    );
}

/// A field value is a recursive tree, and the schema says so: a sequence's
/// items and a map's entries refer to the value type itself rather than to a
/// string a consumer would have to parse a second time.
#[test]
fn a_field_value_advertises_a_tree_rather_than_json_in_a_string() {
    let schema = schema_of::<FieldValue>();
    let branch = |kind: &str| {
        branches(&schema)
            .iter()
            .find(|branch| tag_constant(branch, "kind") == Some(kind))
            .unwrap_or_else(|| panic!("the {kind} branch"))
            .clone()
    };
    let sequence = branch("sequence");
    assert_eq!(
        property_names(&sequence),
        ["kind", "items"].into_iter().collect()
    );
    assert_eq!(
        sequence["properties"]["items"]["items"]["$ref"].as_str(),
        Some("#"),
        "a sequence does not hold field values: {sequence}"
    );
    let map = branch("map");
    assert_eq!(
        property_names(&map),
        ["kind", "entries"].into_iter().collect()
    );
    assert_eq!(
        map["properties"]["entries"]["additionalProperties"]["$ref"].as_str(),
        Some("#"),
        "a map does not hold field values: {map}"
    );
    assert!(
        !schema.to_string().contains("raw_json"),
        "a field value advertises JSON in a string: {schema}"
    );
}

// ── The finding row ──────────────────────────────────────────────────────

/// The head advertises the ceiling its read path keeps, so a surface
/// validating a head against the schema refuses the same wider head this
/// crate refuses to read. The bound is read off the constant here rather than
/// written down a second time: the schema and [`CANDIDATE_HEAD`] are one
/// spelling, and the number itself is pinned where the constant is.
///
/// The description is pinned for the reason the name's is: this is a
/// hand-written schema, so the sentence the schema carries states the ladder
/// order and the total rule on its own, and nothing else holds it to them.
#[test]
fn a_candidate_head_advertises_the_ceiling_it_is_read_through() {
    let schema = schema_of::<CandidateHead>();
    assert_eq!(
        schema["description"].as_str(),
        Some(
            "The bounded head of the documents a target could have named, with how many there were. The candidates are in the resolution ladder's deterministic order and stop at the ceiling this schema advertises, and the total beside them is never below the candidates carried: a smaller total heads nothing, and the read refuses it."
        ),
        "the hand-written head states less than the read enforces: {schema}"
    );
    let candidates = &schema["properties"]["candidates"];
    assert_eq!(candidates["type"].as_str(), Some("array"));
    assert_eq!(
        candidates["maxItems"].as_u64(),
        Some(CANDIDATE_HEAD as u64),
        "the head does not advertise the ceiling it is read through: {schema}"
    );
    assert_eq!(
        candidates["items"]["$ref"].as_str(),
        Some("#/$defs/Candidate"),
        "the head restates a candidate rather than referring to it: {schema}"
    );
}

/// A finding row advertises the bounded head, the total that makes it a head,
/// and the hint that names what enumerates the rest.
#[test]
fn a_finding_row_advertises_its_bounded_head_and_its_hint() {
    let schema = schema_of::<FindingRow>();
    assert_eq!(
        property_names(&schema),
        [
            "id",
            "kind",
            "severity",
            "path",
            "target",
            "span",
            "head",
            "hint",
            "message",
            "generation",
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(
        schema["properties"]["head"]["$ref"].as_str(),
        Some("#/$defs/CandidateHead")
    );
    let head = schema_of::<CandidateHead>();
    assert_eq!(
        property_names(&head),
        ["candidates", "total"].into_iter().collect()
    );
    assert_eq!(
        head["properties"]["candidates"]["type"].as_str(),
        Some("array")
    );
    assert!(
        schema["$defs"]["Candidate"].is_object(),
        "a finding row carries no definition of a candidate: {schema}"
    );
    assert_eq!(
        property_names(&schema_of::<Candidate>()),
        ["path", "suffix"].into_iter().collect()
    );
    assert_eq!(
        sorted(tag_constants(&schema_of::<Hint>(), "hint")),
        ["resolves"]
    );
}

/// The two target refusals advertise the typed target, and the ambiguous one
/// advertises the same head and hint a finding row carries.
#[test]
fn the_target_refusals_advertise_the_typed_target_they_are_about() {
    let schema = schema_of::<ErrorDetail>();
    let branch = |code: &str| {
        branches(&schema)
            .iter()
            .find(|branch| tag_constant(branch, "code") == Some(code))
            .unwrap_or_else(|| panic!("the {code} branch"))
            .clone()
    };
    let ambiguous = branch("vault/ambiguous-target");
    assert_eq!(
        property_names(&ambiguous),
        ["code", "target", "head", "hint"].into_iter().collect()
    );
    assert_eq!(
        ambiguous["properties"]["head"]["$ref"].as_str(),
        Some("#/$defs/CandidateHead"),
        "a refusal restates the head rather than referring to it"
    );
    assert_eq!(
        ambiguous["properties"]["target"]["$ref"].as_str(),
        Some("#/$defs/ResolutionTarget")
    );
    assert_eq!(
        ambiguous["properties"]["hint"]["$ref"].as_str(),
        Some("#/$defs/Hint")
    );
    let unknown = branch("vault/unknown-target");
    assert_eq!(
        property_names(&unknown),
        ["code", "target"].into_iter().collect()
    );
}

// ── The six read verbs ───────────────────────────────────────────────────

/// Each params type advertises the whole of what a request carries, the vault
/// first, so a surface renders a verb from one type rather than assembling a
/// request out of parts it chose.
#[test]
fn every_read_params_advertises_the_whole_of_what_a_request_carries() {
    assert_eq!(
        property_names(&schema_of::<FindParams>()),
        ["vault", "predicates", "sort", "columns", "limit", "after"]
            .into_iter()
            .collect()
    );
    assert_eq!(
        property_names(&schema_of::<SearchParams>()),
        [
            "vault",
            "query",
            "predicates",
            "rungs",
            "min_score",
            "columns",
            "limit",
            "after",
        ]
        .into_iter()
        .collect(),
        "a search advertises an order it does not hold"
    );
    assert_eq!(
        property_names(&schema_of::<GetParams>()),
        ["vault", "target", "columns", "collection", "limit", "after"]
            .into_iter()
            .collect()
    );
    assert_eq!(
        property_names(&schema_of::<CountParams>()),
        ["vault", "predicates", "by", "limit", "after"]
            .into_iter()
            .collect(),
        "a count advertises a projection it hydrates nothing for"
    );
    assert_eq!(
        property_names(&schema_of::<ValidateParams>()),
        [
            "vault",
            "predicates",
            "kinds",
            "severity",
            "summary",
            "limit",
            "after",
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(
        property_names(&schema_of::<DescribeParams>()),
        ["vault", "facets", "limit", "after"].into_iter().collect()
    );
}

/// The order a `find` runs in is a key and a direction, and `search` carries
/// neither: a ranked answer is ordered by its ranking.
#[test]
fn an_order_advertises_its_key_and_its_direction() {
    assert_eq!(
        property_names(&schema_of::<Sort>()),
        ["key", "direction"].into_iter().collect()
    );
    assert_eq!(
        sorted(tag_constants(&schema_of::<SortKey>(), "by")),
        sorted(["field", "path"])
    );
    assert_eq!(
        sorted(branches(&schema_of::<Direction>()).iter().map(|branch| {
            string_constant(branch)
                .unwrap_or_else(|| panic!("a direction branch is not a pinned string: {branch}"))
        })),
        sorted(["ascending", "descending"])
    );
}

/// The retrieval rungs as the schemas spell them, derived from the rung
/// vocabulary itself.
fn retrieval_rung_spellings() -> Vec<Value> {
    [Rung::Lexical, Rung::Vector, Rung::Expansion, Rung::Rerank]
        .into_iter()
        .filter(|rung| rung.retrieves())
        .map(|rung| serde_json::to_value(rung).expect("a rung as JSON"))
        .collect()
}

/// The rung set advertises an array of rungs, each once, holding a retrieval
/// rung, and refers to the rung vocabulary rather than restating it. The
/// preset spellings are a surface's rendering and are advertised nowhere here.
#[test]
fn a_rung_set_advertises_the_resolved_set_and_no_preset() {
    let schema = schema_of::<RungSet>();
    assert_eq!(schema["type"].as_str(), Some("array"));
    assert_eq!(
        schema["minItems"].as_u64(),
        Some(1),
        "the set advertises a ladder that runs no rung: {schema}"
    );
    assert_eq!(schema["uniqueItems"].as_bool(), Some(true));
    assert_eq!(
        schema["contains"]["enum"].as_array(),
        Some(&retrieval_rung_spellings()),
        "the set advertises a ladder holding no retrieval rung: {schema}"
    );
    assert!(
        schema["$defs"]["Rung"].is_object(),
        "a rung set carries no definition of a rung: {schema}"
    );
    for preset in ["hybrid", "semantic"] {
        assert!(
            !schema.to_string().contains(preset),
            "the set advertises the preset spelling `{preset}`"
        );
    }
}

/// A selection advertises its two members under the `select` tag, each holding
/// its own field and refusing every other, so a surface validating a request
/// refuses one that both names a set and subtracts from one. No preset is
/// advertised.
#[test]
fn a_rung_selection_advertises_two_disjoint_members_and_no_preset() {
    let schema = schema_of::<RungSelection>();
    assert_eq!(
        sorted(tag_constants(&schema, "select")),
        sorted(["enabled", "exactly"])
    );
    for branch in branches(&schema) {
        let fields: BTreeSet<&str> = property_names(branch);
        let expected: BTreeSet<&str> = match tag_constant(branch, "select") {
            Some("enabled") => ["select", "without"].into_iter().collect(),
            Some("exactly") => ["select", "rungs"].into_iter().collect(),
            other => panic!("a selection advertises the member {other:?}"),
        };
        assert_eq!(fields, expected, "a selection holds another's field");
        if tag_constant(branch, "select") == Some("enabled") {
            let without = &schema["$defs"]["RungSubtraction"];
            assert_eq!(
                branch["properties"]["without"]["$ref"].as_str(),
                Some("#/$defs/RungSubtraction")
            );
            assert_eq!(without["type"].as_str(), Some("array"));
            assert_eq!(without["uniqueItems"].as_bool(), Some(true));
            let every: Vec<Value> = without["not"]["allOf"]
                .as_array()
                .expect("a subtraction advertises what it may not leave out")
                .iter()
                .map(|clause| clause["contains"]["const"].clone())
                .collect();
            assert_eq!(
                every,
                retrieval_rung_spellings(),
                "a subtraction advertises that it may leave out every retrieval rung: {without}"
            );
        }
        assert_eq!(
            branch["additionalProperties"].as_bool(),
            Some(false),
            "a selection advertises that it drops a field it does not hold: {branch}"
        );
    }
    for preset in ["hybrid", "semantic"] {
        assert!(
            !schema.to_string().contains(preset),
            "the selection advertises the preset spelling `{preset}`"
        );
    }
}

/// A hit advertises the path, the score its order sorts by, and the projected
/// row where one was asked for.
#[test]
fn a_hit_advertises_its_score_and_its_optional_row() {
    let schema = schema_of::<Hit>();
    assert_eq!(
        property_names(&schema),
        ["path", "score", "document"].into_iter().collect()
    );
    let required: BTreeSet<&str> = schema["required"]
        .as_array()
        .expect("the hit's required list")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(required, ["path", "score"].into_iter().collect());
}

/// A get report advertises its `shape` tag, and the collection shape names the
/// collection it paged once: the page's own tag, with no selector beside it.
#[test]
fn a_get_report_advertises_its_shape_tag() {
    let schema = schema_of::<GetReport>();
    assert_eq!(
        sorted(tag_constants(&schema, "shape")),
        sorted(["record", "section", "block", "collection"])
    );
    let collection = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "shape") == Some("collection"))
        .expect("the collection branch");
    assert_eq!(
        property_names(collection),
        ["shape", "path", "page"].into_iter().collect(),
        "the collection report advertises a selector beside the page that names one"
    );
    let block = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "shape") == Some("block"))
        .expect("the block branch");
    assert_eq!(
        property_names(block),
        ["shape", "path", "block", "body"].into_iter().collect()
    );
    assert_eq!(
        sorted(tag_constants(&schema_of::<CollectionPage>(), "of")),
        sorted(["links", "headings", "blocks", "tags", "findings"])
    );
    assert_eq!(
        sorted(
            branches(&schema_of::<CollectionSelector>())
                .iter()
                .map(|branch| string_constant(branch).unwrap_or_else(|| panic!(
                    "a selector branch is not a pinned string: {branch}"
                )))
        ),
        sorted(["links", "headings", "blocks", "tags", "findings"])
    );
}

/// A count advertises what it groups by and what a tally holds: one value per
/// group key, and the count.
#[test]
fn a_count_advertises_its_grouping_and_its_tally() {
    assert_eq!(
        sorted(tag_constants(&schema_of::<GroupKey>(), "by")),
        sorted(["field", "tag"])
    );
    assert_eq!(
        property_names(&schema_of::<Tally>()),
        ["group", "count"].into_iter().collect()
    );
}

/// A validate report advertises its `shape` tag, and the summary carries the
/// tally of kinds that `count` does not.
#[test]
fn a_validate_report_advertises_its_shape_tag() {
    assert_eq!(
        sorted(tag_constants(&schema_of::<ValidateReport>(), "shape")),
        sorted(["findings", "summary"])
    );
    assert_eq!(
        property_names(&schema_of::<KindTally>()),
        ["kind", "severity", "count"].into_iter().collect()
    );
}

/// A facet advertises its `facet` tag, one branch per thing a vault says about
/// itself or its documents carry, and the declared field advertises the typed
/// declaration rather than a sentence naming it.
#[test]
fn a_facet_advertises_its_facet_tag_and_the_types_behind_it() {
    let schema = schema_of::<Facet>();
    assert_eq!(
        sorted(tag_constants(&schema, "facet")),
        sorted([
            "declared_field",
            "observed_field",
            "declared_tag",
            "tag_pattern",
            "folder",
            "path_rule",
            "undeclared_tags",
        ])
    );
    let declared = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "facet") == Some("declared_field"))
        .expect("the declared_field branch");
    assert_eq!(
        property_names(declared),
        ["facet", "key", "field_type", "required", "one_of"]
            .into_iter()
            .collect()
    );
    assert_eq!(
        declared["properties"]["field_type"]["$ref"].as_str(),
        Some("#/$defs/FieldType")
    );
    assert_eq!(
        sorted(branches(&schema_of::<FieldType>()).iter().map(|branch| {
            string_constant(branch)
                .unwrap_or_else(|| panic!("a type branch is not a pinned string: {branch}"))
        })),
        sorted(["text", "number", "boolean", "date", "tags"])
    );
    assert_eq!(
        sorted(
            branches(&schema_of::<ContainerKind>())
                .iter()
                .map(|branch| string_constant(branch).unwrap_or_else(|| panic!(
                    "a container branch is not a pinned string: {branch}"
                )))
        ),
        sorted(["scalar", "sequence", "map"])
    );
    assert_eq!(
        sorted(branches(&schema_of::<PathRuleKind>()).iter().map(|branch| {
            string_constant(branch)
                .unwrap_or_else(|| panic!("a rule branch is not a pinned string: {branch}"))
        })),
        ["ambiguity_ignore"]
    );
    let observed = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "facet") == Some("observed_field"))
        .expect("the observed_field branch");
    assert_eq!(
        property_names(observed),
        ["facet", "key", "containers"].into_iter().collect()
    );
    assert_eq!(
        observed["properties"]["containers"]["items"]["$ref"].as_str(),
        Some("#/$defs/ContainerKind"),
        "an observed field carries the set of containers it is held in: {observed}"
    );
    let undeclared = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "facet") == Some("undeclared_tags"))
        .expect("the undeclared_tags branch");
    assert_eq!(
        undeclared["properties"]["stance"]["$ref"].as_str(),
        Some("#/$defs/TagStance")
    );
    assert_eq!(
        sorted(branches(&schema_of::<TagStance>()).iter().map(|branch| {
            string_constant(branch)
                .unwrap_or_else(|| panic!("a stance branch is not a pinned string: {branch}"))
        })),
        sorted(["allow", "report"])
    );
}

/// Every paged read report but search's is a page of its own row type, so a
/// surface publishing a verb publishes the continuation with the rows. A
/// search report holds its page beside the ladder that ranked it.
#[test]
fn every_paged_read_report_is_a_page_of_its_row() {
    for (report, row) in [
        (schema_of::<FindReport>(), "DocumentRow"),
        (schema_of::<CountReport>(), "Tally"),
        (schema_of::<DescribeReport>(), "Facet"),
    ] {
        assert_eq!(
            property_names(&report),
            ["rows", "next", "moved"].into_iter().collect()
        );
        assert!(
            report["$defs"][row].is_object(),
            "the page carries no definition of {row}: {report}"
        );
        assert!(
            report["$defs"]["Cursor"].is_object(),
            "the page carries no definition of the cursor: {report}"
        );
    }
}

// ── The vault namespace and doctor's registry half ───────────────────────

/// A registration advertises the four fields the registry holds and no
/// others: what a host learns by looking at a vault is reported beside a
/// registration rather than in it.
#[test]
fn a_registration_advertises_the_four_fields_the_registry_holds() {
    let schema = schema_of::<Registration>();
    assert_eq!(
        property_names(&schema),
        ["name", "root", "schema_source", "poll_backend"]
            .into_iter()
            .collect()
    );
    assert_eq!(
        schema["properties"]["root"]["$ref"].as_str(),
        Some("#/$defs/VaultRoot"),
        "a registration names its root by something other than the root grammar"
    );
}

/// What an entry publishes is a tagged pair, and the parked shape refers to
/// the envelope rather than restating a code and a message of its own.
#[test]
fn a_published_answer_advertises_its_answer_tag() {
    let schema = schema_of::<Published>();
    assert_eq!(
        sorted(tag_constants(&schema, "answer")),
        sorted(["state", "parked"])
    );
    let parked = branches(&schema)
        .iter()
        .find(|branch| tag_constant(branch, "answer") == Some("parked"))
        .expect("the parked branch");
    assert_eq!(
        parked["properties"]["refusal"]["$ref"].as_str(),
        Some("#/$defs/ErrorEnvelope"),
        "a park advertises something other than the refusal it publishes"
    );
}

/// The readings a status carries each advertise their own tag, and the two
/// that carry a payload refer to the typed half rather than restating it.
#[test]
fn the_status_readings_advertise_their_tags() {
    assert_eq!(
        sorted(tag_constants(&schema_of::<Drift>(), "state")),
        sorted(["inactive", "current", "reload_pending", "unreadable"])
    );
    let unreadable = branches(&schema_of::<Drift>())
        .iter()
        .find(|branch| tag_constant(branch, "state") == Some("unreadable"))
        .expect("the unreadable branch")
        .clone();
    assert_eq!(
        unreadable["properties"]["failure"]["$ref"].as_str(),
        Some("#/$defs/ControlFileFailure")
    );
    assert_eq!(
        sorted(tag_constants(&schema_of::<EngineStatus>(), "state")),
        sorted(["off", "on", "self_disabled"])
    );
    assert_eq!(
        sorted(tag_constants(&schema_of::<Advisory>(), "kind")),
        sorted(["tmp_fallback_in_use", "symlink_skipped"])
    );
    assert_eq!(
        sorted(tag_constants(&schema_of::<Attention>(), "attention")),
        sorted([
            "untrusted",
            "parked",
            "reads_refusing",
            "reload_failed",
            "reload_pending",
            "engine_self_disabled",
            "advisory",
        ])
    );
    assert_eq!(
        property_names(&schema_of::<Fingerprints>()),
        ["schema", "config"].into_iter().collect()
    );
}

/// A status reports the authoring fact beside the slot fact, so a config that
/// enables an engine which is not standing is two readings that disagree.
#[test]
fn a_vault_status_advertises_the_whole_of_what_an_entry_stands_at() {
    let schema = schema_of::<VaultStatus>();
    assert_eq!(
        property_names(&schema),
        [
            "registration",
            "published",
            "fingerprints",
            "drift",
            "last_reload_failure",
            "reads_refusing",
            "engine",
            "section",
            "advisories",
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(
        schema["properties"]["section"]["$ref"].as_str(),
        Some("#/$defs/EngineSection"),
        "a status advertises no delivered section beside its engine"
    );
}

/// A roll-up advertises the five counts, the total they sum to, and what wants
/// attention.
#[test]
fn a_roll_up_advertises_its_counts_and_its_attention() {
    assert_eq!(
        property_names(&schema_of::<RollUp>()),
        [
            "vaults",
            "ready",
            "warming",
            "untrusted",
            "parked",
            "unattached",
            "attention",
        ]
        .into_iter()
        .collect()
    );
}

/// An edit advertises three members where the field has a default to fall back
/// to and two where it has none, so a reader is told a root cannot be cleared
/// rather than finding out from a handler.
#[test]
fn a_change_advertises_its_change_tag_and_a_root_that_cannot_be_cleared() {
    assert_eq!(
        sorted(tag_constants(
            &schema_of::<Change<SchemaSource>>(),
            "change"
        )),
        sorted(["keep", "set", "clear"])
    );
    assert_eq!(
        sorted(tag_constants(&schema_of::<Replace<VaultRoot>>(), "change")),
        sorted(["keep", "set"])
    );
}

/// Each vault-namespace params type advertises the whole of what a request
/// carries, and a registry verb's carries no vault at all.
#[test]
fn every_vault_params_advertises_the_whole_of_what_a_request_carries() {
    assert_eq!(
        property_names(&schema_of::<RegisterParams>()),
        ["registration"].into_iter().collect()
    );
    assert_eq!(
        property_names(&schema_of::<UnregisterParams>()),
        ["name", "keep_state"].into_iter().collect()
    );
    assert!(
        property_names(&schema_of::<ListParams>()).is_empty(),
        "a listing advertises something to ask for"
    );
    assert_eq!(
        property_names(&schema_of::<SetParams>()),
        ["name", "root", "schema_source", "poll_backend"]
            .into_iter()
            .collect()
    );
    assert_eq!(
        property_names(&schema_of::<ResolveParams>()),
        ["directory"].into_iter().collect()
    );
    assert_eq!(
        property_names(&schema_of::<StatusParams>()),
        ["vault"].into_iter().collect()
    );
    assert_eq!(
        property_names(&schema_of::<ReloadParams>()),
        ["vault", "dry_run"].into_iter().collect()
    );
    assert!(
        property_names(&schema_of::<DoctorRegistryParams>()).is_empty(),
        "a doctor advertises something to ask for"
    );
}

/// Each vault-namespace report advertises the whole of what an answer holds,
/// and the two that are sums advertise their own tags.
#[test]
fn every_vault_report_advertises_the_whole_of_what_an_answer_holds() {
    for report in [schema_of::<RegisterReport>(), schema_of::<SetReport>()] {
        assert_eq!(
            property_names(&report),
            ["registration", "published"].into_iter().collect()
        );
    }
    assert_eq!(
        property_names(&schema_of::<UnregisterReport>()),
        ["name", "state_discarded"].into_iter().collect()
    );
    assert_eq!(
        property_names(&schema_of::<ListReport>()),
        ["registrations"].into_iter().collect()
    );
    assert_eq!(
        sorted(tag_constants(&schema_of::<ResolveReport>(), "outcome")),
        sorted(["registered", "none"])
    );
    assert_eq!(
        sorted(tag_constants(&schema_of::<StatusReport>(), "shape")),
        sorted(["vault", "roll_up"])
    );
    assert_eq!(
        property_names(&schema_of::<ReloadReport>()),
        ["outcome", "fingerprints", "activated"]
            .into_iter()
            .collect()
    );
    assert_eq!(
        sorted(
            branches(&schema_of::<ReloadOutcome>())
                .iter()
                .map(|branch| string_constant(branch).expect("a bare string"))
        ),
        sorted(["config_only", "schema_changed"])
    );
}

/// The doctor's registry half advertises the roll-up, the registry's own
/// sanity and the engines, with sound and unsound as two shapes.
#[test]
fn a_doctor_registry_report_advertises_the_registry_it_read() {
    assert_eq!(
        property_names(&schema_of::<DoctorRegistryReport>()),
        ["roll_up", "registry", "engines"].into_iter().collect()
    );
    assert_eq!(
        sorted(tag_constants(&schema_of::<RegistrySanity>(), "state")),
        sorted(["sound", "problems"])
    );
    assert_eq!(
        sorted(tag_constants(&schema_of::<RegistryProblem>(), "problem")),
        sorted(["duplicate_root", "root_unreadable", "root_missing"])
    );
    let sanity = schema_of::<RegistrySanity>();
    let problems = branches(&sanity)
        .iter()
        .find(|branch| tag_constant(branch, "state") == Some("problems"))
        .unwrap_or_else(|| panic!("the sanity reading advertises no problems branch: {sanity}"));
    assert_eq!(
        problems["properties"]["problems"]["minItems"].as_u64(),
        Some(1),
        "the problems branch advertises a list that names no problem: {sanity}"
    );
    assert_eq!(
        property_names(&schema_of::<EngineHealth>()),
        ["name", "section", "engine"].into_iter().collect()
    );
}
