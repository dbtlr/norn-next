//! The serde round trip, over every public type and every variant of each,
//! and what the read path does with bytes it was not handed by a writer of
//! this version.
//!
//! Three claims, and the second is what makes the first worth anything:
//!
//! 1. **A value survives the wire.** Serializing and deserializing hands back
//!    a value equal to the one that went in.
//! 2. **The bytes are the ones contracted.** A round trip is symmetric even
//!    when both halves are wrong together, so the encoding of each shape is
//!    pinned here as literal JSON.
//! 3. **The read path is strict where it is contracted to be.** An unknown
//!    field is dropped and an unknown variant is refused, and every value that
//!    is built here is built through the constructors a consumer has.

use norn_wire::{
    Addressing, Anchor, AnswerReading, AttachMode, ControlFile, Cursor, CursorKey,
    CursorOrderChanged, EngineSection, ErrorDetail, ErrorEnvelope, FacetKind, FindingKind,
    FindingScope, Freshness, LadderDeclaration, MaintainerIdentity, ModelIdentity, Moved,
    NonFiniteScore, NotReady, Page, PollBackend, Predicate, ReasonCode, ReloadFailure, ReloadStage,
    RequestScope, ResolutionTarget, Rung, RungReport, SchemaSource, Score, Severity, Snapshot,
    TrustState, UnknownAddressing, UnknownFindingKind, UnknownPollBackend, UnknownRequestScope,
    UnknownSeverity, UnknownVerb, Unsatisfied, UntrustedReason, VaultAddress, VaultAnswer,
    VaultName, VaultRoot, Verb, WarmingPhase, WatcherLossCause,
};
use serde::de::value::{Error as ValueError, F64Deserializer};
use serde::de::{DeserializeOwned, IntoDeserializer};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt::Debug;

/// Every cause a lost watcher carries.
fn watcher_loss_causes() -> Vec<WatcherLossCause> {
    vec![
        WatcherLossCause::Backend,
        WatcherLossCause::CoverageLost,
        WatcherLossCause::SynchronizationExpired,
    ]
}

/// Every reason the vocabulary carries.
fn untrusted_reasons() -> Vec<UntrustedReason> {
    let mut reasons = vec![UntrustedReason::WatcherOverflow];
    reasons.extend(
        watcher_loss_causes()
            .into_iter()
            .map(|cause| UntrustedReason::watcher_lost(cause, "the watch ended")),
    );
    reasons.push(UntrustedReason::environmental_refusal("the disk is full"));
    reasons.push(UntrustedReason::store_damaged_rebuilding(
        "the database disk image is malformed",
    ));
    reasons.push(UntrustedReason::store_damaged_awaiting_demand(
        "the database disk image is malformed",
    ));
    reasons.push(UntrustedReason::schema_unreadable(
        "the vault schema is at version 9 and this build reads 1",
    ));
    reasons.push(UntrustedReason::leg_unwound("the heal panicked"));
    reasons
}

/// Every mode a demand asks for its derived state under.
fn attach_modes() -> Vec<AttachMode> {
    vec![AttachMode::Durable, AttachMode::Throwaway]
}

/// Every kind a finding is filed under.
fn finding_kinds() -> Vec<FindingKind> {
    FindingKind::ALL.to_vec()
}

/// Every scope a kind's findings may stand in.
fn finding_scopes() -> Vec<FindingScope> {
    vec![FindingScope::Place, FindingScope::Document]
}

/// Every severity a finding carries.
fn severities() -> Vec<Severity> {
    Severity::ALL.to_vec()
}

/// Every phase an entry warms in.
fn warming_phases() -> Vec<WarmingPhase> {
    vec![
        WarmingPhase::InstallingCoverage,
        WarmingPhase::Healing,
        WarmingPhase::ReleasingCoverage,
    ]
}

/// Every trust state, with one entry per phase behind `Warming` and one per
/// reason behind `Untrusted`.
fn trust_states() -> Vec<TrustState> {
    let mut states = vec![TrustState::Unattached, TrustState::Ready];
    states.extend(warming_phases().into_iter().flat_map(|phase| {
        [
            TrustState::warming(phase, 0, None),
            TrustState::warming(phase, 0, Some(0)),
            TrustState::warming(phase, 12, Some(400)),
        ]
    }));
    states.extend(untrusted_reasons().into_iter().map(TrustState::untrusted));
    states
}

/// Every code the list holds.
fn reason_codes() -> Vec<ReasonCode> {
    vec![
        ReasonCode::HostDuplicateRoot,
        ReasonCode::HostEntryUntrusted,
        ReasonCode::HostMaintainerContended,
        ReasonCode::HostUnknownVault,
        ReasonCode::HostUnsupportedAttachMode,
        ReasonCode::HostAlreadyServed,
        ReasonCode::HostEntryHeld,
        ReasonCode::HostEntryNotReady,
        ReasonCode::HostReaderUnavailable,
        ReasonCode::HostRegistryUnwritable,
        ReasonCode::VaultAmbiguousRoot,
        ReasonCode::VaultReloadBusy,
        ReasonCode::VaultReloadFailed,
        ReasonCode::VaultCursorOrderChanged,
        ReasonCode::EngineNotEnabled,
        ReasonCode::EngineUnavailable,
        ReasonCode::EngineFailed,
    ]
}

/// Every state an entry that holds nothing yet publishes.
fn not_ready_states() -> Vec<NotReady> {
    let mut states = vec![NotReady::unattached()];
    states.extend(
        warming_phases()
            .into_iter()
            .map(|phase| NotReady::warming(phase, 12, Some(400))),
    );
    states
}

/// Every failure a reload carries.
fn reload_failures() -> Vec<ReloadFailure> {
    let mut failures = Vec::new();
    for file in [ControlFile::Schema, ControlFile::Config] {
        for stage in [ReloadStage::Read, ReloadStage::Parse, ReloadStage::Apply] {
            failures.push(ReloadFailure::control_file(
                file,
                stage,
                "the vault schema cannot be read",
            ));
        }
    }
    failures.push(ReloadFailure::environmental("the disk is full"));
    failures.push(ReloadFailure::store_damaged("the disk image is malformed"));
    failures.push(ReloadFailure::watcher_terminal(
        "the watcher backend stopped",
    ));
    failures.push(ReloadFailure::lost_maintainership());
    failures.push(ReloadFailure::maintainer_contended(
        MaintainerIdentity::unknown(),
    ));
    failures.push(ReloadFailure::unsupported());
    failures
}

/// Every detail variant, over every payload it can carry.
fn error_details() -> Vec<ErrorDetail> {
    let mut details: Vec<_> = untrusted_reasons()
        .into_iter()
        .map(ErrorDetail::entry_untrusted)
        .collect();
    details.extend([
        ErrorDetail::duplicate_root([name("notes"), name("vault")]),
        ErrorDetail::maintainer_contended(MaintainerIdentity::unknown()),
        ErrorDetail::maintainer_contended(MaintainerIdentity::named(41, "0.1.0", 1_700_000_000)),
        ErrorDetail::unknown_vault(name("notes")),
        ErrorDetail::already_served(name("notes")),
        ErrorDetail::entry_held(name("notes")),
        ErrorDetail::reader_unavailable("this coverage mints no read handle"),
        ErrorDetail::registry_unwritable("the registry file is read-only"),
        ErrorDetail::ambiguous_root([name("notes"), name("vault")]),
        ErrorDetail::reload_busy(),
        ErrorDetail::cursor_order_changed(CursorOrderChanged::new(
            "fp-1",
            Some("fp-2".to_string()),
        )),
        ErrorDetail::cursor_order_changed(CursorOrderChanged::new("fp-1", None)),
    ]);
    details.extend(
        not_ready_states()
            .into_iter()
            .map(ErrorDetail::entry_not_ready),
    );
    details.extend(
        reload_failures()
            .into_iter()
            .map(ErrorDetail::reload_failed),
    );
    for rung in rungs() {
        details.push(ErrorDetail::engine_not_enabled(rung, "enable the engine"));
        details.push(ErrorDetail::engine_unavailable(rung, "the slot is empty"));
        details.push(ErrorDetail::engine_failed(rung, "the answer failed"));
    }
    details.extend(
        attach_modes()
            .into_iter()
            .map(ErrorDetail::unsupported_attach_mode),
    );
    details
}

/// Every name the grammar accepts, spread across the punctuation it admits.
fn vault_names() -> Vec<VaultName> {
    ["a", "notes", "notes2", "a.b", "a+b", "a-b.c+d"]
        .into_iter()
        .map(|text| VaultName::new(text).expect("a legal vault name"))
        .collect()
}

/// Every watch backend the vocabulary holds.
fn poll_backends() -> Vec<PollBackend> {
    PollBackend::ALL.to_vec()
}

/// Every root the grammar accepts, spread across the shapes a path takes.
fn vault_roots() -> Vec<VaultRoot> {
    [
        "/",
        "/home/person/notes",
        "/home/person/n o t e s",
        "/tmp/a.b",
    ]
    .into_iter()
    .map(|text| VaultRoot::new(text).expect("an absolute root"))
    .collect()
}

/// Every schema source the grammar accepts.
fn schema_sources() -> Vec<SchemaSource> {
    ["/home/person/.config/norn/schemas/work.yaml", "/s.yaml"]
        .into_iter()
        .map(|text| SchemaSource::new(text).expect("an absolute schema source"))
        .collect()
}

/// Every way a request addresses a vault.
fn vault_addresses() -> Vec<VaultAddress> {
    let mut addresses: Vec<VaultAddress> =
        vault_names().into_iter().map(VaultAddress::name).collect();
    addresses.extend(vault_roots().into_iter().map(VaultAddress::root));
    addresses
}

/// Every verb the registry holds.
fn verbs() -> Vec<Verb> {
    Verb::ALL.to_vec()
}

/// Every scope a request is answered from.
fn request_scopes() -> Vec<RequestScope> {
    RequestScope::ALL.to_vec()
}

/// Every way a verb carries a vault address.
fn addressings() -> Vec<Addressing> {
    Addressing::ALL.to_vec()
}

/// One target a vector names a document by, parsed through the grammar the
/// type keeps.
fn target(text: &str) -> ResolutionTarget {
    ResolutionTarget::new(text).expect("a legal resolution target")
}

/// Every target shape the grammar admits: a bare suffix, a heading anchor and
/// a block anchor.
fn resolution_targets() -> Vec<ResolutionTarget> {
    [
        "glossary",
        "norn/glossary",
        "glossary#Design",
        "glossary#^a1",
    ]
    .into_iter()
    .map(target)
    .collect()
}

/// Every part the conjunction admits, one per operator.
fn predicates() -> Vec<Predicate> {
    vec![
        Predicate::equal_to("type", "note"),
        Predicate::not_equal_to("type", "note"),
        Predicate::in_any("type", ["note".to_string(), "task".to_string()]),
        Predicate::has("due"),
        Predicate::missing("due"),
        Predicate::before("due", "2026-01-01"),
        Predicate::after("due", "2026-01-01"),
        Predicate::matches("norn NEAR vault"),
        Predicate::path("docs/**"),
        Predicate::links_to(target("glossary#Design")),
        Predicate::resolves(target("norn/glossary")),
        Predicate::tag("draft"),
        Predicate::has_finding(FindingKind::UndeclaredTag),
    ]
}

/// Every kind a facet row is a facet of.
fn facet_kinds() -> Vec<FacetKind> {
    vec![
        FacetKind::DeclaredField,
        FacetKind::ObservedField,
        FacetKind::DeclaredTag,
        FacetKind::Folder,
        FacetKind::PathRule,
    ]
}

/// Every movement a continuation reports.
fn movements() -> Vec<Moved> {
    vec![Moved::Epoch, Moved::Generation, Moved::SidecarRevision]
}

/// A finite score, built through the grammar the type keeps.
fn score(value: f64) -> Score {
    Score::new(value).expect("a finite relevance score")
}

/// Every paged row type, with one key per shape its order takes.
fn cursor_keys() -> Vec<CursorKey> {
    let mut keys = vec![
        CursorKey::document(Some("2026-01-01".to_string()), "notes/a.md"),
        CursorKey::document(None, "notes/a.md"),
        CursorKey::hit(score(0.5), "notes/a.md"),
        CursorKey::tally(["note".to_string(), "open".to_string()]),
        CursorKey::finding(FindingKind::UndeclaredTag, "notes/a.md", 7),
        CursorKey::ordinal(3),
    ];
    keys.extend(
        facet_kinds()
            .into_iter()
            .map(|kind| CursorKey::facet(kind, "type")),
    );
    keys
}

/// Every cursor shape: one per key, across the two optional parts.
fn cursors() -> Vec<Cursor> {
    let mut cursors: Vec<Cursor> = cursor_keys()
        .into_iter()
        .map(|key| {
            Cursor::new(
                Snapshot::new("epoch-1", 12, Some("fp-1".to_string()), Some(4)),
                key,
            )
        })
        .collect();
    cursors.push(Cursor::new(
        Snapshot::new("epoch-1", 0, None, None),
        CursorKey::ordinal(0),
    ));
    cursors
}

/// Every rung a ladder declares.
fn rungs() -> Vec<Rung> {
    vec![Rung::Lexical, Rung::Vector, Rung::Expansion, Rung::Rerank]
}

/// Every freshness a stateful rung reports.
fn freshnesses() -> Vec<Freshness> {
    vec![
        Freshness::trailing(0),
        Freshness::trailing(3),
        Freshness::rescanning(),
    ]
}

/// Every section reading the host retains for a vault's engine.
fn engine_sections() -> Vec<EngineSection> {
    vec![
        EngineSection::absent(),
        EngineSection::disabled(),
        EngineSection::malformed("the `engine` table holds a string"),
        EngineSection::enabled(),
    ]
}

/// One rung report per rung: the model-free floor, the stateful rung across
/// every freshness it can report, and the two request-time rungs.
fn rung_reports() -> Vec<RungReport> {
    let mut reports = vec![RungReport::lexical()];
    reports.extend(
        freshnesses()
            .into_iter()
            .map(|freshness| RungReport::vector(ModelIdentity::new("stub", "1"), freshness)),
    );
    reports.push(RungReport::expansion(ModelIdentity::new("stub", "1")));
    reports.push(RungReport::rerank(ModelIdentity::new("stub", "1")));
    reports
}

/// Every reading an answer is taken under, with and without a ladder.
fn answer_readings() -> Vec<AnswerReading> {
    vec![
        AnswerReading::new(TrustState::Ready, "epoch-1", 12, None),
        AnswerReading::new(
            TrustState::Ready,
            "epoch-1",
            12,
            Some(LadderDeclaration::new(rung_reports(), false)),
        ),
        AnswerReading::new(
            TrustState::Ready,
            "epoch-1",
            12,
            Some(LadderDeclaration::new(vec![RungReport::lexical()], true)),
        ),
    ]
}

/// Every part a request can leave unapplied.
fn unsatisfied_parts() -> Vec<Unsatisfied> {
    vec![
        Unsatisfied::unknown_sort_key("due", vec!["date".to_string()]),
        Unsatisfied::unknown_projection_key("due", vec![]),
        Unsatisfied::unknown_predicate_key("due", vec!["date".to_string()]),
        Unsatisfied::bare_directory("docs"),
        Unsatisfied::malformed_glob("docs/[", "the character class does not close"),
        Unsatisfied::impossible_path("/etc/passwd"),
        Unsatisfied::missing_section("Design"),
        Unsatisfied::resolves_not_applicable(target("norn/glossary")),
    ]
}

fn round_trip<T>(value: &T)
where
    T: Serialize + DeserializeOwned + Debug + PartialEq,
{
    let json = serde_json::to_string(value).expect("serializing a wire type");
    let back: T =
        serde_json::from_str(&json).unwrap_or_else(|error| panic!("reading {json} back: {error}"));
    assert_eq!(&back, value, "the round trip through {json} changed it");
}

fn wire(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("serializing a wire type")
}

/// One name a vector names a vault by, parsed through the grammar the type
/// keeps.
fn name(text: &str) -> VaultName {
    VaultName::new(text).expect("a legal vault name")
}

// ── The vectors are the vocabulary ───────────────────────────────────────

/// The members a schema advertises, as the strings they are on the wire: the
/// constant each `oneOf` branch pins, read off the branch itself where the
/// enum is a flat string and off its `tag` property where the enum is
/// internally tagged.
fn advertised<T: schemars::JsonSchema>(tag: Option<&str>) -> BTreeSet<String> {
    let schema = serde_json::to_value(schemars::schema_for!(T)).expect("a schema as JSON");
    let branches = schema["oneOf"]
        .as_array()
        .unwrap_or_else(|| panic!("the schema describes no enum: {schema}"))
        .clone();
    assert!(!branches.is_empty(), "the schema advertises no member");
    branches
        .iter()
        .map(|branch| {
            let constant = match tag {
                Some(tag) => &branch["properties"][tag]["const"],
                None => &branch["const"],
            };
            constant
                .as_str()
                .unwrap_or_else(|| panic!("a branch pins no constant: {branch}"))
                .to_owned()
        })
        .collect()
}

/// The string a value is on the wire, where the enum is a flat string.
fn flat_string(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .expect("a wire value as JSON")
        .as_str()
        .expect("a flat string on the wire")
        .to_owned()
}

/// The constant a value pins its `tag` to, where the enum is internally
/// tagged.
fn tag_string(value: &impl Serialize, tag: &str) -> String {
    serde_json::to_value(value).expect("a wire value as JSON")[tag]
        .as_str()
        .expect("a tag on the wire")
        .to_owned()
}

/// **Every vector above holds the whole vocabulary, and the schema is what
/// says so.** The vectors are written out by hand — these enums are
/// `#[non_exhaustive]` and carry no list of their own — so a member minted
/// without a row above would be a member no case here round-trips, no case
/// here pins the bytes of, and nothing here reports missing. The schema is the
/// enum's own account of itself, and holding the two equal is what fails when
/// a vector falls behind the enum beside it.
#[test]
fn every_vector_here_holds_the_members_the_schema_advertises() {
    assert_eq!(
        untrusted_reasons()
            .iter()
            .map(|reason| tag_string(reason, "kind"))
            .collect::<BTreeSet<_>>(),
        advertised::<UntrustedReason>(Some("kind")),
        "the reasons built here are not the reasons the vocabulary holds"
    );
    assert_eq!(
        reason_codes()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>(),
        advertised::<ReasonCode>(None),
        "the codes built here are not the codes the vocabulary holds"
    );
    assert_eq!(
        watcher_loss_causes()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>(),
        advertised::<WatcherLossCause>(None),
        "the causes built here are not the causes the vocabulary holds"
    );
    assert_eq!(
        attach_modes()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>(),
        advertised::<AttachMode>(None),
        "the modes built here are not the modes the vocabulary holds"
    );
    assert_eq!(
        warming_phases()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>(),
        advertised::<WarmingPhase>(None),
        "the phases built here are not the phases the vocabulary holds"
    );
    assert_eq!(
        trust_states()
            .iter()
            .map(|state| tag_string(state, "state"))
            .collect::<BTreeSet<_>>(),
        advertised::<TrustState>(Some("state")),
        "the states built here are not the states the vocabulary holds"
    );
    assert_eq!(
        error_details()
            .iter()
            .map(|detail| tag_string(detail, "code"))
            .collect::<BTreeSet<_>>(),
        advertised::<ErrorDetail>(Some("code")),
        "the details built here are not the details the vocabulary holds"
    );
    assert_eq!(
        not_ready_states()
            .iter()
            .map(|state| tag_string(state, "state"))
            .collect::<BTreeSet<_>>(),
        advertised::<NotReady>(Some("state")),
        "the not-ready states built here are not the ones the vocabulary holds"
    );
    assert_eq!(
        reload_failures()
            .iter()
            .map(|failure| tag_string(failure, "kind"))
            .collect::<BTreeSet<_>>(),
        advertised::<ReloadFailure>(Some("kind")),
        "the reload failures built here are not the ones the vocabulary holds"
    );
    assert_eq!(
        rungs().iter().map(flat_string).collect::<BTreeSet<_>>(),
        advertised::<Rung>(None),
        "the rungs built here are not the rungs the vocabulary holds"
    );
    assert_eq!(
        freshnesses()
            .iter()
            .map(|freshness| tag_string(freshness, "state"))
            .collect::<BTreeSet<_>>(),
        advertised::<Freshness>(Some("state")),
        "the freshnesses built here are not the ones the vocabulary holds"
    );
    assert_eq!(
        engine_sections()
            .iter()
            .map(|section| tag_string(section, "state"))
            .collect::<BTreeSet<_>>(),
        advertised::<EngineSection>(Some("state")),
        "the sections built here are not the sections the vocabulary holds"
    );
    assert_eq!(
        unsatisfied_parts()
            .iter()
            .map(|part| tag_string(part, "part"))
            .collect::<BTreeSet<_>>(),
        advertised::<Unsatisfied>(Some("part")),
        "the parts built here are not the parts the vocabulary holds"
    );
    assert_eq!(
        cursor_keys()
            .iter()
            .map(|key| tag_string(key, "row"))
            .collect::<BTreeSet<_>>(),
        advertised::<CursorKey>(Some("row")),
        "the keys built here are not the keys the vocabulary holds"
    );
    assert_eq!(
        facet_kinds()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>(),
        advertised::<FacetKind>(None),
        "the facet kinds built here are not the kinds the vocabulary holds"
    );
    assert_eq!(
        movements().iter().map(flat_string).collect::<BTreeSet<_>>(),
        advertised::<Moved>(None),
        "the movements built here are not the movements the vocabulary holds"
    );
    assert_eq!(
        predicates()
            .iter()
            .map(|predicate| tag_string(predicate, "op"))
            .collect::<BTreeSet<_>>(),
        advertised::<Predicate>(Some("op")),
        "the parts built here are not the parts the vocabulary holds"
    );
    assert_eq!(
        verbs().iter().map(flat_string).collect::<BTreeSet<_>>(),
        advertised::<Verb>(None),
        "the verbs built here are not the verbs the registry holds"
    );
    assert_eq!(
        request_scopes()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>(),
        advertised::<RequestScope>(None),
        "the scopes built here are not the scopes the vocabulary holds"
    );
    assert_eq!(
        addressings()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>(),
        advertised::<Addressing>(None),
        "the addressings built here are not the ones the vocabulary holds"
    );
    assert_eq!(
        rung_reports()
            .iter()
            .map(|report| tag_string(report, "rung"))
            .collect::<BTreeSet<_>>(),
        advertised::<RungReport>(Some("rung")),
        "the rung reports built here are not the ones the vocabulary holds"
    );
    assert_eq!(
        poll_backends()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>(),
        advertised::<PollBackend>(None),
        "the backends built here are not the backends the vocabulary holds"
    );
    assert_eq!(
        vault_addresses()
            .iter()
            .map(|address| tag_string(address, "by"))
            .collect::<BTreeSet<_>>(),
        advertised::<VaultAddress>(Some("by")),
        "the addresses built here are not the addresses the vocabulary holds"
    );
}

// ── The round trip ───────────────────────────────────────────────────────

#[test]
fn every_trust_state_survives_the_round_trip() {
    for state in trust_states() {
        round_trip(&state);
    }
}

#[test]
fn every_untrusted_reason_survives_the_round_trip() {
    for reason in untrusted_reasons() {
        round_trip(&reason);
    }
}

#[test]
fn every_reason_code_survives_the_round_trip() {
    for code in reason_codes() {
        round_trip(&code);
    }
}

#[test]
fn every_attach_mode_survives_the_round_trip() {
    for mode in attach_modes() {
        round_trip(&mode);
    }
}

#[test]
fn every_vault_name_survives_the_round_trip() {
    for name in vault_names() {
        round_trip(&name);
    }
}

#[test]
fn every_finding_kind_survives_the_round_trip() {
    for kind in finding_kinds() {
        round_trip(&kind);
    }
}

#[test]
fn every_finding_scope_survives_the_round_trip() {
    for scope in finding_scopes() {
        round_trip(&scope);
    }
}

#[test]
fn every_severity_survives_the_round_trip() {
    for severity in severities() {
        round_trip(&severity);
    }
}

#[test]
fn every_error_detail_survives_the_round_trip() {
    for detail in error_details() {
        round_trip(&detail);
    }
}

#[test]
fn every_envelope_survives_the_round_trip() {
    for detail in error_details() {
        round_trip(&ErrorEnvelope::new("the entry is untrusted", detail));
    }
}

// ── The bytes ────────────────────────────────────────────────────────────

/// A trust state is an object tagged `state`, and the tag never becomes a key
/// wrapping the payload — that is the external tagging this vocabulary does
/// not use.
#[test]
fn a_trust_state_is_an_object_tagged_state() {
    assert_eq!(wire(&TrustState::Unattached), r#"{"state":"unattached"}"#);
    assert_eq!(wire(&TrustState::Ready), r#"{"state":"ready"}"#);
    assert_eq!(
        wire(&TrustState::warming(WarmingPhase::Healing, 12, Some(400))),
        r#"{"state":"warming","phase":"healing","healed":12,"total_estimate":400}"#
    );
    assert_eq!(
        wire(&TrustState::untrusted(UntrustedReason::WatcherOverflow)),
        r#"{"state":"untrusted","reason":{"kind":"watcher_overflow"}}"#
    );
}

/// An estimate nobody has yet is `null`, and the field is written either way:
/// a reader looks at one field to learn both that a heal is estimating and
/// that it cannot yet say a number. A reader handed the field absent takes it
/// as the same unknown.
#[test]
fn an_unknown_estimate_is_the_field_written_null() {
    assert_eq!(
        wire(&TrustState::warming(WarmingPhase::Healing, 7, None)),
        r#"{"state":"warming","phase":"healing","healed":7,"total_estimate":null}"#
    );
    for json in [
        r#"{"state":"warming","phase":"healing","healed":7,"total_estimate":null}"#,
        r#"{"state":"warming","phase":"healing","healed":7}"#,
    ] {
        let state: TrustState =
            serde_json::from_str(json).unwrap_or_else(|error| panic!("reading {json}: {error}"));
        assert_eq!(state, TrustState::warming(WarmingPhase::Healing, 7, None));
    }
}

/// The phase is a bare string beside the counters, and the two phases that
/// count nothing are read where zero healed against an unknown total is the
/// whole truth: acquiring what a read runs on, and giving it back.
#[test]
fn a_warming_phase_is_a_bare_string_beside_the_counters() {
    assert_eq!(
        wire(&TrustState::warming(
            WarmingPhase::InstallingCoverage,
            0,
            None
        )),
        r#"{"state":"warming","phase":"installing_coverage","healed":0,"total_estimate":null}"#
    );
    assert_eq!(
        wire(&TrustState::warming(
            WarmingPhase::ReleasingCoverage,
            0,
            None
        )),
        r#"{"state":"warming","phase":"releasing_coverage","healed":0,"total_estimate":null}"#
    );
}

/// The phase is required, and the two fields beside it are the contrast: an
/// absent estimate is the unknown the type already models, while an absent
/// phase is a warming state nobody said anything about. Nothing defaults it —
/// a reader handed one of these learns that the writer's phase vocabulary and
/// its own have parted, rather than being told the entry is installing
/// coverage or healing when no one claimed either.
#[test]
fn a_warming_state_without_a_phase_is_refused_rather_than_defaulted() {
    for json in [
        r#"{"state":"warming","healed":0,"total_estimate":null}"#,
        r#"{"state":"warming","healed":12,"total_estimate":400}"#,
        r#"{"state":"warming","healed":0}"#,
        r#"{"state":"warming","phase":null,"healed":0,"total_estimate":null}"#,
    ] {
        assert!(
            serde_json::from_str::<TrustState>(json).is_err(),
            "reading {json} produced a warming state with a phase nobody wrote"
        );
    }
}

#[test]
fn an_untrusted_reason_is_an_object_tagged_kind() {
    assert_eq!(
        wire(&UntrustedReason::WatcherOverflow),
        r#"{"kind":"watcher_overflow"}"#
    );
    assert_eq!(
        wire(&UntrustedReason::watcher_lost(
            WatcherLossCause::Backend,
            "the watcher stopped"
        )),
        r#"{"kind":"watcher_lost","cause":"backend","detail":"the watcher stopped"}"#
    );
    assert_eq!(
        wire(&UntrustedReason::watcher_lost(
            WatcherLossCause::CoverageLost,
            "the vault root left"
        )),
        r#"{"kind":"watcher_lost","cause":"coverage_lost","detail":"the vault root left"}"#
    );
    assert_eq!(
        wire(&UntrustedReason::environmental_refusal("the disk is full")),
        r#"{"kind":"environmental_refusal","detail":"the disk is full"}"#
    );
    assert_eq!(
        wire(&UntrustedReason::leg_unwound("the heal panicked")),
        r#"{"kind":"leg_unwound","detail":"the heal panicked"}"#
    );
}

/// Damaged derived state is two reasons, split by who resumes: an entry
/// holding the damaged database rebuilds it, and an entry holding none waits
/// for the demand that opens one. A client reads which of the two it has from
/// the `kind` tag, never from holdings no seam carries.
#[test]
fn the_two_damage_reasons_are_told_apart_by_their_kind() {
    assert_eq!(
        wire(&UntrustedReason::store_damaged_rebuilding(
            "the database disk image is malformed"
        )),
        r#"{"kind":"store_damaged_rebuilding","detail":"the database disk image is malformed"}"#
    );
    assert_eq!(
        wire(&UntrustedReason::store_damaged_awaiting_demand(
            "the database disk image is malformed"
        )),
        r#"{"kind":"store_damaged_awaiting_demand","detail":"the database disk image is malformed"}"#
    );
}

/// A mode is the bare string it is written as, and a string outside the pair
/// is refused rather than defaulted: a mode a later version writes stops a
/// reader of this one instead of arriving as the durable mode and being served
/// under terms nobody asked for.
#[test]
fn an_attach_mode_is_the_bare_string_it_renders_as() {
    let strings = ["durable", "throwaway"];
    assert_eq!(attach_modes().len(), strings.len());
    for (mode, string) in attach_modes().into_iter().zip(strings) {
        let json = format!("\"{string}\"");
        assert_eq!(wire(&mode), json);
        assert_eq!(
            serde_json::from_str::<AttachMode>(&json).expect("reading a mode back"),
            mode
        );
    }
    assert!(
        serde_json::from_str::<AttachMode>(r#""ephemeral""#).is_err(),
        "a mode nobody wrote was read as one of the two"
    );
}

/// A refused mode crosses as the typed mode rather than as prose, so a client
/// that asks for more than one learns which of them the host refused.
#[test]
fn a_refused_mode_crosses_as_the_mode_that_was_named() {
    assert_eq!(
        wire(&ErrorEnvelope::new(
            "the host holds no lifecycle for that mode",
            ErrorDetail::unsupported_attach_mode(AttachMode::Throwaway),
        )),
        concat!(
            r#"{"code":"host/unsupported-attach-mode","#,
            r#""message":"the host holds no lifecycle for that mode","#,
            r#""detail":{"code":"host/unsupported-attach-mode","mode":"throwaway"}}"#
        )
    );
}

/// A name is the string itself, and the read path is the grammar: a string
/// outside it has no representation on either side of the seam, so a reader
/// never holds a name that was not parsed.
#[test]
fn a_vault_name_is_the_string_it_renders_as_and_is_read_through_its_grammar() {
    assert_eq!(
        wire(&VaultName::new("notes").expect("a legal name")),
        r#""notes""#
    );
    for text in ["", "Notes", "1notes", "notes_1", "notes/deep", ".."] {
        let json = format!("\"{text}\"");
        assert!(
            serde_json::from_str::<VaultName>(&json).is_err(),
            "`{text}` was read as a vault name"
        );
    }
    let too_long = format!("\"a{}\"", "x".repeat(VaultName::MAXIMUM_BYTES));
    assert!(
        serde_json::from_str::<VaultName>(&too_long).is_err(),
        "a name past the bound was read as one"
    );
}

/// A finding kind is the flat namespaced string itself, and the rendering the
/// crate hands out is the string it serializes as — one spelling, whether a
/// reader took it off the wire or asked the type for it. The string reads back
/// as the kind it renders, so a row filed under a kind is read as that kind
/// rather than re-matched by hand; a string the registry does not hold is
/// refused.
#[test]
fn a_finding_kind_is_the_flat_namespaced_string_it_renders_as() {
    let strings = [
        "document/path-bytes-not-utf8",
        "document/path-names-no-document",
        "document/body-bytes-not-utf8",
        "document/frontmatter-too-large",
        "document/frontmatter-unclosed",
        "document/frontmatter-unreadable",
        "document/undeclared-tag",
    ];
    assert_eq!(finding_kinds().len(), strings.len());
    for (kind, string) in finding_kinds().into_iter().zip(strings) {
        assert_eq!(kind.as_str(), string);
        assert_eq!(kind.to_string(), string);
        assert_eq!(wire(&kind), format!("\"{string}\""));
        assert_eq!(FindingKind::try_from(string), Ok(kind));
    }
    assert_eq!(
        FindingKind::try_from("document/unreadable"),
        Err(UnknownFindingKind)
    );
}

/// A kind says where its findings may stand, and the two scopes partition the
/// registry. A place-scoped kind states that nothing is derived at its subject;
/// a document-scoped kind states something about the document derived there, so
/// it stands beside that document's row.
#[test]
fn the_two_scopes_partition_the_finding_kinds() {
    let spellings = |scope: FindingScope| {
        let mut kinds: Vec<&str> = finding_kinds()
            .into_iter()
            .filter(|kind| kind.scope() == scope)
            .map(|kind| kind.as_str())
            .collect();
        kinds.sort_unstable();
        kinds
    };
    let place = spellings(FindingScope::Place);
    let document = spellings(FindingScope::Document);
    assert_eq!(
        place,
        [
            "document/body-bytes-not-utf8",
            "document/path-bytes-not-utf8",
            "document/path-names-no-document",
        ]
    );
    assert_eq!(
        document,
        [
            "document/frontmatter-too-large",
            "document/frontmatter-unclosed",
            "document/frontmatter-unreadable",
            "document/undeclared-tag",
        ]
    );
    assert_eq!(place.len() + document.len(), FindingKind::ALL.len());
}

/// A scope is the bare string it serializes as. A string outside the pair is
/// refused rather than defaulted, so a scope a later version writes stops a
/// reader of this one instead of arriving as `Place` and taking the
/// withholding a place-scoped finding is subject to.
#[test]
fn a_finding_scope_is_the_bare_string_it_renders_as() {
    let strings = ["place", "document"];
    assert_eq!(finding_scopes().len(), strings.len());
    for (scope, string) in finding_scopes().into_iter().zip(strings) {
        let json = format!("\"{string}\"");
        assert_eq!(wire(&scope), json);
        assert_eq!(
            serde_json::from_str::<FindingScope>(&json).expect("reading a scope back"),
            scope
        );
    }
    assert!(
        serde_json::from_str::<FindingScope>(r#""vault""#).is_err(),
        "a scope nobody wrote was read as one of the two"
    );
}

/// Severity is the bare value a finding stores and renders, with one spelling
/// shared by serde, display and the walkable registry.
#[test]
fn a_severity_is_the_bare_string_it_renders_as() {
    let strings = ["error", "warning"];
    assert_eq!(severities().len(), strings.len());
    for (severity, string) in severities().into_iter().zip(strings) {
        assert_eq!(severity.as_str(), string);
        assert_eq!(severity.to_string(), string);
        assert_eq!(wire(&severity), format!("\"{string}\""));
        assert_eq!(Severity::try_from(string), Ok(severity));
    }
    assert_eq!(Severity::try_from("urgent"), Err(UnknownSeverity));
}

/// The envelope is three fields, and the detail is tagged with the same code
/// the `code` field carries.
#[test]
fn an_envelope_is_a_code_a_message_and_a_detail() {
    let envelope = ErrorEnvelope::new(
        "the entry is untrusted",
        ErrorDetail::entry_untrusted(UntrustedReason::environmental_refusal("the disk is full")),
    );
    assert_eq!(
        wire(&envelope),
        concat!(
            r#"{"code":"host/entry-untrusted","message":"the entry is untrusted","#,
            r#""detail":{"code":"host/entry-untrusted","#,
            r#""reason":{"kind":"environmental_refusal","detail":"the disk is full"}}}"#
        )
    );
}

/// A registry refusal names the vault it is about as typed data: duplicate-root
/// carries every colliding name, and unknown-vault carries the name the request
/// asked for.
#[test]
fn a_registry_refusal_carries_the_names_the_registry_holds() {
    assert_eq!(
        wire(&ErrorEnvelope::new(
            "two names resolve to one root",
            ErrorDetail::duplicate_root([name("notes"), name("vault")]),
        )),
        concat!(
            r#"{"code":"host/duplicate-root","message":"two names resolve to one root","#,
            r#""detail":{"code":"host/duplicate-root","aliases":["notes","vault"]}}"#
        )
    );
    assert_eq!(
        wire(&ErrorEnvelope::new(
            "no vault is registered as `notes`",
            ErrorDetail::unknown_vault(name("notes")),
        )),
        concat!(
            r#"{"code":"host/unknown-vault","message":"no vault is registered as `notes`","#,
            r#""detail":{"code":"host/unknown-vault","name":"notes"}}"#
        )
    );
}

/// A name outside the grammar refuses the whole envelope rather than landing
/// in it as a string.
///
/// Both registry refusals echo names, and both echo them as the typed name, so
/// a name that crossed is a name that parsed on the reading side too: an
/// envelope naming a vault no request could have named is refused entire —
/// code, message and detail together — rather than read into a value a later
/// reader would have to check. The names below are the shapes a string field
/// would have carried through: a traversal with a trailing newline, an empty
/// name, and a name outside the case the grammar admits.
#[test]
fn an_unknown_vault_refuses_a_name_outside_the_grammar() {
    for legal in ["notes", "a-b.c+d"] {
        assert!(
            serde_json::from_str::<ErrorEnvelope>(&unknown_vault_envelope(legal)).is_ok(),
            "`{legal}` was refused as the name an envelope echoes"
        );
        assert!(
            serde_json::from_str::<ErrorEnvelope>(&duplicate_root_envelope(legal)).is_ok(),
            "`{legal}` was refused as one of an envelope's colliding names"
        );
    }
    for hostile in ["../../etc/passwd\n", "", "Notes"] {
        let unknown = unknown_vault_envelope(hostile);
        assert!(
            serde_json::from_str::<ErrorEnvelope>(&unknown).is_err(),
            "reading {unknown} produced an envelope"
        );
        let colliding = duplicate_root_envelope(hostile);
        assert!(
            serde_json::from_str::<ErrorEnvelope>(&colliding).is_err(),
            "reading {colliding} produced an envelope"
        );
    }
}

/// A `host/unknown-vault` envelope as a writer sends it, echoing `named`.
fn unknown_vault_envelope(named: &str) -> String {
    serde_json::json!({
        "code": "host/unknown-vault",
        "message": "no vault is registered under that name",
        "detail": {"code": "host/unknown-vault", "name": named},
    })
    .to_string()
}

/// A `host/duplicate-root` envelope as a writer sends it, carrying `named`
/// beside a name the grammar accepts.
fn duplicate_root_envelope(named: &str) -> String {
    serde_json::json!({
        "code": "host/duplicate-root",
        "message": "more than one registered name resolves to one root",
        "detail": {"code": "host/duplicate-root", "aliases": ["archive", named]},
    })
    .to_string()
}

/// The colliding names ascend because the detail sorts them, so the order the
/// field promises holds whatever order a producer collected them in.
#[test]
fn duplicate_root_aliases_ascend_whatever_order_they_arrive_in() {
    assert_eq!(
        wire(&ErrorDetail::duplicate_root([
            name("vault"),
            name("archive"),
            name("notes"),
        ])),
        r#"{"code":"host/duplicate-root","aliases":["archive","notes","vault"]}"#
    );
}

#[test]
fn maintainer_contention_carries_the_incumbent_identity() {
    let envelope = ErrorEnvelope::new(
        "another process maintains this vault",
        ErrorDetail::maintainer_contended(MaintainerIdentity::named(41, "0.1.0", 1_700_000_000)),
    );
    assert_eq!(
        wire(&envelope),
        concat!(
            r#"{"code":"host/maintainer-contended","message":"another process maintains this vault","#,
            r#""detail":{"code":"host/maintainer-contended","incumbent":{"kind":"named","pid":41,"version":"0.1.0","started_unix_seconds":1700000000}}}"#
        )
    );
    assert_eq!(
        wire(&MaintainerIdentity::unknown()),
        r#"{"kind":"unknown"}"#
    );
}

/// The constructor takes the code from the detail, so an envelope cannot be
/// built naming one refusal and describing another.
#[test]
fn an_envelope_takes_its_code_from_its_detail() {
    for detail in error_details() {
        let envelope = ErrorEnvelope::new("refused", detail.clone());
        assert_eq!(envelope.code(), &detail.code());
        assert_eq!(envelope.detail(), &detail);
        assert_eq!(envelope.message(), "refused");
    }
}

/// The reason an entry reports and the reason a refusal carries are one type,
/// so they are one set of bytes too.
#[test]
fn a_refusal_carries_the_same_reason_the_state_does() {
    for reason in untrusted_reasons() {
        let state = TrustState::untrusted(reason.clone());
        let detail = ErrorDetail::entry_untrusted(reason.clone());
        let alone = serde_json::to_value(&reason).expect("a reason as JSON");
        let state = serde_json::to_value(&state).expect("a state as JSON");
        let detail = serde_json::to_value(&detail).expect("a detail as JSON");
        assert_eq!(state["reason"], alone);
        assert_eq!(detail["reason"], alone);
    }
}

// ── The read path ────────────────────────────────────────────────────────

/// A field a reader does not know is dropped rather than refused, so a writer
/// that gained one is still read here. The field does not survive: what is
/// read back is the envelope this version defines.
#[test]
fn a_struct_drops_a_field_it_does_not_know() {
    let json = concat!(
        r#"{"code":"host/entry-untrusted","message":"refused","retryable":true,"#,
        r#""detail":{"code":"host/entry-untrusted","reason":{"kind":"watcher_overflow"}}}"#
    );
    let envelope: ErrorEnvelope = serde_json::from_str(json)
        .expect("an envelope carrying a field this version has no name for");
    assert_eq!(
        envelope,
        ErrorEnvelope::new(
            "refused",
            ErrorDetail::entry_untrusted(UntrustedReason::WatcherOverflow)
        )
    );
    assert!(!wire(&envelope).contains("retryable"));
}

/// A variant a reader does not know fails the read. There is no fallback
/// variant to absorb it: a value nobody can interpret is refused rather than
/// carried on degraded.
#[test]
fn an_enum_refuses_a_variant_it_does_not_know() {
    for json in [
        r#"{"state":"quarantined"}"#,
        r#"{"state":"untrusted","reason":{"kind":"cosmic_ray"}}"#,
    ] {
        assert!(
            serde_json::from_str::<TrustState>(json).is_err(),
            "reading {json} produced a state"
        );
    }
    assert!(
        serde_json::from_str::<ReasonCode>(r#""host/entry-vanished""#).is_err(),
        "a code nobody minted read back as one"
    );
    assert!(
        serde_json::from_str::<ErrorDetail>(
            r#"{"code":"host/entry-vanished","reason":{"kind":"watcher_overflow"}}"#
        )
        .is_err(),
        "a detail under a code nobody minted read back as one"
    );
    assert!(
        serde_json::from_str::<UntrustedReason>(
            r#"{"kind":"watcher_lost","cause":"solar_flare","detail":"none"}"#
        )
        .is_err(),
        "a watcher-loss cause nobody minted read back as one"
    );
    assert!(
        serde_json::from_str::<TrustState>(
            r#"{"state":"warming","phase":"daydreaming","healed":0}"#
        )
        .is_err(),
        "a warming phase nobody minted read back as one"
    );
    assert!(
        serde_json::from_str::<TrustState>(r#"{"state":"warming","phase":null,"healed":0}"#)
            .is_err(),
        "a null phase read back as a phase"
    );
    assert!(
        serde_json::from_str::<FindingKind>(r#""document/unreadable""#).is_err(),
        "a finding kind nobody minted read back as one"
    );
    assert!(
        serde_json::from_str::<Severity>(r#""urgent""#).is_err(),
        "a severity nobody minted read back as one"
    );
}

// ── The address grammar ──────────────────────────────────────────────────

#[test]
fn every_poll_backend_survives_the_round_trip() {
    for backend in poll_backends() {
        round_trip(&backend);
    }
}

#[test]
fn every_vault_root_survives_the_round_trip() {
    for root in vault_roots() {
        round_trip(&root);
    }
}

#[test]
fn every_schema_source_survives_the_round_trip() {
    for source in schema_sources() {
        round_trip(&source);
    }
}

#[test]
fn every_vault_address_survives_the_round_trip() {
    for address in vault_addresses() {
        round_trip(&address);
    }
}

/// A backend is the bare value a registration records and renders, with one
/// spelling shared by serde, display and the walkable vocabulary.
#[test]
fn a_poll_backend_is_the_bare_string_it_renders_as() {
    let strings = ["poll"];
    assert_eq!(poll_backends().len(), strings.len());
    for (backend, string) in poll_backends().into_iter().zip(strings) {
        assert_eq!(backend.as_str(), string);
        assert_eq!(backend.to_string(), string);
        assert_eq!(wire(&backend), format!("\"{string}\""));
        assert_eq!(PollBackend::try_from(string), Ok(backend));
    }
    assert_eq!(PollBackend::try_from("inotify"), Err(UnknownPollBackend));
    assert!(
        serde_json::from_str::<PollBackend>(r#""inotify""#).is_err(),
        "a backend nobody minted read back as one"
    );
}

/// A root and a schema source are the strings they render as, and the read
/// path is the grammar: a relative path, an empty one, and bytes that are not
/// text have no representation on either side of the seam.
#[test]
fn a_recorded_path_is_the_string_it_renders_as_and_is_read_through_its_grammar() {
    assert_eq!(
        wire(&VaultRoot::new("/home/person/notes").expect("an absolute root")),
        r#""/home/person/notes""#
    );
    assert_eq!(
        wire(&SchemaSource::new("/s.yaml").expect("an absolute schema source")),
        r#""/s.yaml""#
    );
    for text in ["", "notes", "./notes", "../notes"] {
        let json = format!("\"{text}\"");
        assert!(
            serde_json::from_str::<VaultRoot>(&json).is_err(),
            "`{text}` was read as a vault root"
        );
        assert!(
            serde_json::from_str::<SchemaSource>(&json).is_err(),
            "`{text}` was read as a schema source"
        );
    }
}

/// The refusal names the path that was offered, which of the two grammars
/// refused it, and what that grammar wanted.
#[test]
fn a_refused_path_names_what_it_was_meant_to_be() {
    let refusal = VaultRoot::new("notes").expect_err("a relative root");
    assert_eq!(refusal.path(), "notes");
    assert_eq!(refusal.what(), "vault root");
    assert!(refusal.problem().contains("absolute"), "{refusal}");

    let refusal = SchemaSource::new("s.yaml").expect_err("a relative schema source");
    assert_eq!(refusal.what(), "schema source");
    assert_eq!(
        refusal.to_string(),
        format!("`s.yaml` is not a schema source: {}", refusal.problem())
    );
}

/// The text is the path: a root that crossed is a root the constructor
/// accepted, so asking for it either way hands back one spelling.
#[test]
fn a_root_is_one_spelling_as_text_and_as_a_path() {
    let root = VaultRoot::new("/home/person/notes").expect("an absolute root");
    assert_eq!(root.as_str(), "/home/person/notes");
    assert_eq!(root.as_path().to_str(), Some("/home/person/notes"));
    assert_eq!(root.to_string(), "/home/person/notes");
}

/// An address is an object tagged `by`, and the two ways a request names a
/// vault are told apart by that tag rather than by which key is present.
#[test]
fn a_vault_address_is_an_object_tagged_by() {
    assert_eq!(
        wire(&VaultAddress::name(name("notes"))),
        r#"{"by":"name","name":"notes"}"#
    );
    assert_eq!(
        wire(&VaultAddress::root(
            VaultRoot::new("/home/person/notes").expect("an absolute root")
        )),
        r#"{"by":"root","root":"/home/person/notes"}"#
    );
    assert!(
        serde_json::from_str::<VaultAddress>(r#"{"by":"url","url":"norn://notes"}"#).is_err(),
        "an address nobody minted read back as one"
    );
    assert!(
        serde_json::from_str::<VaultAddress>(r#"{"by":"root","root":"notes"}"#).is_err(),
        "a relative root read back as an address"
    );
}

// ── The verb registry ────────────────────────────────────────────────────

/// The registry holds fourteen verbs, and every one of them is the flat string
/// it renders as, read back as the verb it renders.
#[test]
fn every_verb_is_the_flat_string_it_renders_as() {
    let strings = [
        "find",
        "search",
        "get",
        "count",
        "validate",
        "describe",
        "vault_register",
        "vault_unregister",
        "vault_list",
        "vault_set",
        "vault_resolve",
        "vault_status",
        "vault_reload",
        "doctor_registry",
    ];
    assert_eq!(Verb::ALL.len(), 14);
    assert_eq!(verbs().len(), strings.len());
    for (verb, string) in verbs().into_iter().zip(strings) {
        assert_eq!(verb.as_str(), string);
        assert_eq!(verb.to_string(), string);
        assert_eq!(wire(&verb), format!("\"{string}\""));
        assert_eq!(Verb::try_from(string), Ok(verb));
        round_trip(&verb);
    }
    assert_eq!(Verb::try_from("vault_forget"), Err(UnknownVerb));
    assert!(
        serde_json::from_str::<Verb>(r#""vault_forget""#).is_err(),
        "a verb nobody minted read back as one"
    );
}

#[test]
fn every_request_scope_is_the_flat_string_it_renders_as() {
    let strings = ["vault", "registry", "installation"];
    assert_eq!(request_scopes().len(), strings.len());
    for (scope, string) in request_scopes().into_iter().zip(strings) {
        assert_eq!(scope.as_str(), string);
        assert_eq!(scope.to_string(), string);
        assert_eq!(wire(&scope), format!("\"{string}\""));
        assert_eq!(RequestScope::try_from(string), Ok(scope));
        round_trip(&scope);
    }
    assert_eq!(RequestScope::try_from("machine"), Err(UnknownRequestScope));
}

#[test]
fn every_addressing_is_the_flat_string_it_renders_as() {
    let strings = ["required", "none", "optional"];
    assert_eq!(addressings().len(), strings.len());
    for (addressing, string) in addressings().into_iter().zip(strings) {
        assert_eq!(addressing.as_str(), string);
        assert_eq!(addressing.to_string(), string);
        assert_eq!(wire(&addressing), format!("\"{string}\""));
        assert_eq!(Addressing::try_from(string), Ok(addressing));
        round_trip(&addressing);
    }
    assert_eq!(Addressing::try_from("maybe"), Err(UnknownAddressing));
}

/// Addressing is the verb's and scope is the request's, and this is where the
/// two meet: an optional addressing is a vault request with an address and a
/// registry request without one, and the other two answer the same whichever
/// way the flag reads, because the verb has already settled it.
#[test]
fn a_scope_follows_from_the_addressing_and_whether_an_address_was_carried() {
    assert_eq!(
        Addressing::Optional.scope_with(true),
        RequestScope::Vault,
        "an optional addressing carrying a vault is not a vault request"
    );
    assert_eq!(
        Addressing::Optional.scope_with(false),
        RequestScope::Registry,
        "an optional addressing carrying no vault is not a registry request"
    );
    for carried in [true, false] {
        assert_eq!(
            Addressing::Required.scope_with(carried),
            RequestScope::Vault
        );
        assert_eq!(Addressing::None.scope_with(carried), RequestScope::Registry);
    }
}

/// Every verb this layer lands carries a vault address or carries none, and
/// exactly one may carry one either way: `vault status` naming a vault reports
/// that entry, and naming none reports the serving set, under one registry
/// entry rather than two verbs sharing a name.
///
/// Nothing here acts on the installation: that scope is spelled so the
/// partition a surface reads is whole, and the verbs that carry it arrive with
/// the layer that lands them.
#[test]
fn every_verb_carries_a_vault_address_or_carries_none_and_one_may_carry_either() {
    let addressed = |addressing: Addressing| {
        let mut named: Vec<&str> = verbs()
            .into_iter()
            .filter(|verb| verb.addressing() == addressing)
            .map(|verb| verb.as_str())
            .collect();
        named.sort_unstable();
        named
    };
    assert_eq!(Verb::ALL.len(), 14);
    assert_eq!(
        addressed(Addressing::Required),
        [
            "count",
            "describe",
            "find",
            "get",
            "search",
            "validate",
            "vault_reload",
        ]
    );
    assert_eq!(
        addressed(Addressing::None),
        [
            "doctor_registry",
            "vault_list",
            "vault_register",
            "vault_resolve",
            "vault_set",
            "vault_unregister",
        ]
    );
    assert_eq!(addressed(Addressing::Optional), ["vault_status"]);
    assert_eq!(
        addressed(Addressing::Required).len()
            + addressed(Addressing::None).len()
            + addressed(Addressing::Optional).len(),
        Verb::ALL.len()
    );

    let scopes: Vec<RequestScope> = verbs()
        .into_iter()
        .flat_map(|verb| [true, false].map(move |carried| verb.addressing().scope_with(carried)))
        .collect();
    assert!(
        !scopes.contains(&RequestScope::Installation),
        "a verb this layer lands is answered from the installation"
    );
}

// ── The resolution target and the predicate grammar ──────────────────────

#[test]
fn every_resolution_target_survives_the_round_trip() {
    for target in resolution_targets() {
        round_trip(&target);
    }
}

#[test]
fn every_predicate_survives_the_round_trip() {
    for predicate in predicates() {
        round_trip(&predicate);
    }
}

/// A target is the one string it was written as, and the parse is what a
/// consumer reads afterwards: the suffix address before the first `#`, and the
/// anchor after it, a block where it opens with `^` and a heading otherwise.
#[test]
fn a_target_is_the_string_it_is_written_as_and_the_parse_of_it() {
    for (text, address, anchor) in [
        ("glossary", "glossary", None),
        ("norn/glossary", "norn/glossary", None),
        (
            "glossary#Design",
            "glossary",
            Some(Anchor::heading("Design")),
        ),
        ("glossary#^a1", "glossary", Some(Anchor::block("a1"))),
        ("glossary#a#b", "glossary", Some(Anchor::heading("a#b"))),
        ("glossary#", "glossary", Some(Anchor::heading(""))),
        ("glossary#^", "glossary", Some(Anchor::block(""))),
    ] {
        let parsed = target(text);
        assert_eq!(parsed.address(), address, "`{text}` addresses another");
        assert_eq!(parsed.anchor(), anchor.as_ref(), "`{text}` anchors another");
        assert_eq!(
            wire(&parsed),
            format!("\"{text}\""),
            "`{text}` is rewritten"
        );
        assert_eq!(parsed.to_string(), text);
    }
}

/// A target with no address names no document. The anchor half is optional and
/// the address half is not, so `#Design` refuses rather than arriving as a
/// heading in a document nobody named.
#[test]
fn a_target_with_no_address_is_refused() {
    for text in ["", "#Design", "#^a1", "#"] {
        let refusal = ResolutionTarget::new(text).expect_err(&format!("`{text}` is not a target"));
        assert_eq!(refusal.target(), text);
        assert!(refusal.problem().contains("path suffix"), "{refusal}");
        assert!(
            serde_json::from_str::<ResolutionTarget>(&format!("\"{text}\"")).is_err(),
            "`{text}` was read as a target"
        );
    }
}

/// Nothing is percent-decoded. What a client wrote is the address it named, so
/// two strings a person typed differently stay two addresses.
#[test]
fn a_target_is_not_percent_decoded() {
    let parsed = target("a%2Fb");
    assert_eq!(parsed.address(), "a%2Fb");
    assert_ne!(parsed, target("a/b"));
}

/// A part is an object tagged `op`, and the typed halves — a finding kind, a
/// target — cross as themselves rather than as strings a reader re-parses.
#[test]
fn a_predicate_is_an_object_tagged_op() {
    assert_eq!(
        wire(&Predicate::equal_to("type", "note")),
        r#"{"op":"eq","key":"type","value":"note"}"#
    );
    assert_eq!(
        wire(&Predicate::not_equal_to("type", "note")),
        r#"{"op":"not_eq","key":"type","value":"note"}"#
    );
    assert_eq!(
        wire(&Predicate::in_any(
            "type",
            ["note".to_string(), "task".to_string()]
        )),
        r#"{"op":"in","key":"type","values":["note","task"]}"#
    );
    assert_eq!(wire(&Predicate::has("due")), r#"{"op":"has","key":"due"}"#);
    assert_eq!(
        wire(&Predicate::matches("norn NEAR vault")),
        r#"{"op":"matches","query":"norn NEAR vault"}"#
    );
    assert_eq!(
        wire(&Predicate::links_to(target("glossary#^a1"))),
        r#"{"op":"links_to","target":"glossary#^a1"}"#
    );
    assert_eq!(
        wire(&Predicate::has_finding(FindingKind::UndeclaredTag)),
        r#"{"op":"has_finding","kind":"document/undeclared-tag"}"#
    );
    assert_eq!(
        wire(&Predicate::tag("draft")),
        r#"{"op":"tag","name":"draft"}"#
    );
}

/// A part carrying a target refuses one outside the grammar entire, rather
/// than reading it as a string a later reader would have to check.
#[test]
fn a_predicate_refuses_a_target_outside_the_grammar() {
    assert!(
        serde_json::from_str::<Predicate>(r##"{"op":"links_to","target":"#Design"}"##).is_err(),
        "a part carrying an addressless target read back as one"
    );
    assert!(
        serde_json::from_str::<Predicate>(r#"{"op":"has_finding","kind":"document/unreadable"}"#)
            .is_err(),
        "a part carrying a finding kind nobody minted read back as one"
    );
    assert!(
        serde_json::from_str::<Predicate>(r#"{"op":"near","query":"x"}"#).is_err(),
        "a part nobody minted read back as one"
    );
}

// ── The cursor envelope ──────────────────────────────────────────────────

#[test]
fn every_cursor_survives_the_round_trip() {
    for cursor in cursors() {
        round_trip(&cursor);
    }
}

#[test]
fn every_facet_kind_and_movement_survives_the_round_trip() {
    for kind in facet_kinds() {
        round_trip(&kind);
    }
    for movement in movements() {
        round_trip(&movement);
    }
}

/// A cursor is one opaque string in the URL-safe alphabet, and everything it
/// carries survives the trip through it.
#[test]
fn a_cursor_is_one_opaque_string_a_client_passes_back_unchanged() {
    for cursor in cursors() {
        let json = wire(&cursor);
        let text = serde_json::from_str::<String>(&json).expect("a cursor is a string");
        assert!(
            text.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "a cursor is spelled outside the URL-safe alphabet: {text}"
        );
        assert!(!text.is_empty(), "a cursor is spelled as nothing");
        let back: Cursor = serde_json::from_str(&json).expect("reading a cursor back");
        assert_eq!(back, cursor);
        assert_eq!(back.key(), cursor.key());
        assert_eq!(back.snapshot(), cursor.snapshot());
    }
}

/// One position has one spelling. A payload that carries a field the fields do
/// not hold, writes them in another order, or leaves an optional one out
/// decodes and parses and is still no position: the read path re-encodes what
/// it parsed and refuses a string that is not that encoding.
#[test]
fn a_cursor_spelled_any_other_way_names_no_position() {
    let canonical = concat!(
        r#"{"snapshot":{"epoch":"e","generation":1,"schema_fingerprint":null,"#,
        r#""sidecar_revision":null},"key":{"row":"ordinal","index":3}}"#
    );
    let minted = Cursor::new(Snapshot::new("e", 1, None, None), CursorKey::ordinal(3));
    assert_eq!(
        serde_json::from_str::<Cursor>(&opaque(canonical.as_bytes()))
            .expect("the canonical spelling reads"),
        minted,
        "the canonical spelling is not the one the type mints"
    );
    assert_eq!(opaque(canonical.as_bytes()), wire(&minted));

    let lax = [
        // An extra field the fields do not hold.
        concat!(
            r#"{"snapshot":{"epoch":"e","generation":1,"schema_fingerprint":null,"#,
            r#""sidecar_revision":null},"key":{"row":"ordinal","index":3},"page":2}"#
        ),
        // The same fields in another order.
        concat!(
            r#"{"key":{"row":"ordinal","index":3},"snapshot":{"epoch":"e","#,
            r#""generation":1,"schema_fingerprint":null,"sidecar_revision":null}}"#
        ),
        // The optional parts left out rather than written null.
        r#"{"snapshot":{"epoch":"e","generation":1},"key":{"row":"ordinal","index":3}}"#,
    ];
    for spelling in lax {
        let read = serde_json::from_str::<Cursor>(&opaque(spelling.as_bytes()));
        let error = read.expect_err(&format!("`{spelling}` was read as a cursor"));
        assert!(
            error.to_string().contains("names no position"),
            "`{spelling}` refused with another account: {error}"
        );
    }
}

/// A score orders the ranked rows a hit cursor continues, so it is finite: a
/// value that is not refuses when a hit key is built and when one is read.
#[test]
fn a_score_that_is_not_finite_is_no_score() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(Score::new(value), Err(NonFiniteScore), "{value} built");
    }
    assert_eq!(score(0.5).get(), 0.5);
    round_trip(&score(0.5));
    assert_eq!(wire(&score(0.5)), "0.5");

    // The read path is the constructor, so it refuses every non-finite value
    // a format can hand it. JSON spells none of the three, so the value is
    // handed to the read path directly, the way a format that does spell them
    // would hand it over.
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let number: F64Deserializer<ValueError> = value.into_deserializer();
        assert!(
            Score::deserialize(number).is_err(),
            "{value} was read back as a score"
        );
    }
    // A JSON literal out of the range a double holds is the nearest thing
    // JSON itself spells, and it is no score either.
    for spelling in ["1e400", "-1e400"] {
        assert!(
            serde_json::from_str::<Score>(spelling).is_err(),
            "`{spelling}` was read as a score"
        );
    }
    let overflowing = concat!(
        r#"{"snapshot":{"epoch":"e","generation":1,"schema_fingerprint":null,"#,
        r#""sidecar_revision":null},"key":{"row":"hit","score":1e400,"path":"a.md"}}"#
    );
    assert!(
        serde_json::from_str::<Cursor>(&opaque(overflowing.as_bytes())).is_err(),
        "a cursor scored beyond a double was read as one"
    );
}

/// A string nobody minted is not a position. It refuses whether it fails the
/// alphabet, decodes to bytes that are not the fields, or names a row type
/// this version does not hold.
#[test]
fn a_cursor_nobody_minted_refuses_the_read() {
    for text in ["", "not base64!", "Zg==", "Zh", "Zm9vYmFy"] {
        let json = serde_json::to_string(text).expect("a string as JSON");
        assert!(
            serde_json::from_str::<Cursor>(&json).is_err(),
            "`{text}` was read as a cursor"
        );
    }
    let unknown = concat!(
        r#"{"snapshot":{"epoch":"e","generation":1,"schema_fingerprint":null,"#,
        r#""sidecar_revision":null},"key":{"row":"shard","at":1}}"#
    );
    assert!(
        serde_json::from_str::<Cursor>(&opaque(unknown.as_bytes())).is_err(),
        "a cursor naming a row type nobody minted was read as one"
    );
}

/// `bytes` as the opaque string a cursor is spelled as, quoted as JSON, so a
/// test hands the read path a spelling the way a writer would.
fn opaque(bytes: &[u8]) -> String {
    serde_json::to_string(&norn_wire_test_base64(bytes)).expect("a string as JSON")
}

/// The URL-safe alphabet, unpadded, spelled here so the test builds a hostile
/// cursor the way a writer would rather than reaching into the crate.
fn norn_wire_test_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let group = (b0 << 16) | (b1 << 8) | b2;
        for position in 0..=chunk.len() {
            out.push(char::from(
                ALPHABET[((group >> (18 - 6 * position)) & 0x3f) as usize],
            ));
        }
    }
    out
}

/// The cursor every continuation rule below is read against.
fn minted() -> Cursor {
    Cursor::new(
        Snapshot::new("epoch-1", 12, Some("fp-1".to_string()), Some(4)),
        CursorKey::ordinal(1),
    )
}

/// An establishment that has not moved at all reports nothing.
#[test]
fn an_unmoved_establishment_reports_nothing() {
    let exact = Snapshot::new("epoch-1", 12, Some("fp-1".to_string()), Some(4));
    assert_eq!(minted().continuation(&exact), Ok(vec![]));
}

/// A database that is not the one the cursor was minted from reports `epoch`,
/// and the generation beside it is not reported: a rebuild restarts the write
/// count, so a count read against another database compares nothing.
#[test]
fn a_rebuilt_database_reports_the_epoch_and_not_the_generation() {
    let rebuilt = Snapshot::new("epoch-2", 3, Some("fp-1".to_string()), Some(4));
    assert_eq!(
        minted().continuation(&rebuilt),
        Ok(vec![Moved::Epoch, Moved::SidecarRevision])
    );
}

/// A generation that differs inside one epoch reports `generation` whichever
/// way it moved. Writes landing after the cursor was minted move it forward; a
/// count that moved backwards is a database that is not the one the cursor
/// named, and hiding that is worse than reporting it.
#[test]
fn a_generation_that_differs_in_either_direction_reports_the_generation() {
    for generation in [13, 11] {
        let written = Snapshot::new("epoch-1", generation, Some("fp-1".to_string()), Some(4));
        assert_eq!(
            minted().continuation(&written),
            Ok(vec![Moved::Generation]),
            "generation {generation} was not reported as moved"
        );
    }
}

/// A sidecar at another revision reports `sidecar_revision`, and so does one
/// at the same number under another epoch: the revision is epoch-qualified, so
/// two epochs share no scale for it to be compared on.
#[test]
fn a_sidecar_moves_with_its_revision_and_with_its_epoch() {
    let drained = Snapshot::new("epoch-1", 12, Some("fp-1".to_string()), Some(5));
    assert_eq!(
        minted().continuation(&drained),
        Ok(vec![Moved::SidecarRevision])
    );
    let requalified = Snapshot::new("epoch-2", 12, Some("fp-1".to_string()), Some(4));
    assert_eq!(
        minted().continuation(&requalified),
        Ok(vec![Moved::Epoch, Moved::SidecarRevision]),
        "a revision another database qualifies was read as the same revision"
    );
}

/// A cursor minted without a sidecar revision reports nothing about one,
/// whatever the sidecar now answers: its position was taken without one, so
/// there is no revision it moved from.
#[test]
fn a_cursor_that_read_no_sidecar_reports_nothing_about_one() {
    let without = Cursor::new(
        Snapshot::new("epoch-1", 12, Some("fp-1".to_string()), None),
        CursorKey::ordinal(1),
    );
    for revision in [None, Some(4)] {
        assert_eq!(
            without.continuation(&Snapshot::new(
                "epoch-1",
                12,
                Some("fp-1".to_string()),
                revision
            )),
            Ok(vec![]),
            "a cursor that read no sidecar reported one at {revision:?}"
        );
    }
}

/// Everything at once, in the fixed order the list promises.
#[test]
fn a_continuation_reports_every_part_in_one_fixed_order() {
    let inside = Snapshot::new("epoch-1", 99, Some("fp-1".to_string()), Some(5));
    assert_eq!(
        minted().continuation(&inside),
        Ok(vec![Moved::Generation, Moved::SidecarRevision])
    );
    let rebuilt = Snapshot::new("epoch-2", 99, Some("fp-1".to_string()), Some(5));
    assert_eq!(
        minted().continuation(&rebuilt),
        Ok(vec![Moved::Epoch, Moved::SidecarRevision])
    );
}

/// A cursor minted under one order and continued under another refuses: the
/// rows its key names a position in are in a sequence that no longer exists.
/// An establishment that reads no fingerprint at all is not walking that
/// sequence either, so a typed cursor refuses there too.
#[test]
fn a_changed_order_refuses_the_continuation() {
    let typed = Cursor::new(
        Snapshot::new("epoch-1", 12, Some("fp-1".to_string()), None),
        CursorKey::ordinal(1),
    );
    assert_eq!(
        typed.continuation(&Snapshot::new(
            "epoch-1",
            12,
            Some("fp-2".to_string()),
            None
        )),
        Err(CursorOrderChanged::new("fp-1", Some("fp-2".to_string())))
    );
    assert_eq!(
        typed.continuation(&Snapshot::new("epoch-1", 12, None, None)),
        Err(CursorOrderChanged::new("fp-1", None)),
        "a typed cursor continued where no fingerprint stands was answered"
    );
}

/// A raw order does not change with the schema. A cursor carrying no
/// fingerprint was not ordered by one, so nothing the establishment's schema
/// does is a change of its order.
#[test]
fn a_raw_order_never_changes() {
    let raw = Cursor::new(
        Snapshot::new("epoch-1", 12, None, None),
        CursorKey::ordinal(1),
    );
    for fingerprint in [None, Some("fp-1".to_string()), Some("fp-2".to_string())] {
        assert_eq!(
            raw.continuation(&Snapshot::new("epoch-1", 12, fingerprint, None)),
            Ok(vec![]),
            "a raw order was reported as changed"
        );
    }
}

/// A page carries its rows, where the next one begins, and what moved. The
/// last page continues at nothing.
#[test]
fn a_page_carries_its_rows_its_continuation_and_what_moved() {
    let page: Page<String> = Page::new(vec!["a".to_string()], None, vec![]);
    assert_eq!(wire(&page), r#"{"rows":["a"],"next":null,"moved":[]}"#);
    round_trip(&page);

    let cursor = Cursor::new(
        Snapshot::new("epoch-1", 1, None, None),
        CursorKey::ordinal(1),
    );
    let continued: Page<String> = Page::new(
        vec!["a".to_string()],
        Some(cursor.clone()),
        vec![Moved::Generation],
    );
    round_trip(&continued);
    assert_eq!(continued.next.as_ref(), Some(&cursor));
    assert_eq!(continued.moved, vec![Moved::Generation]);
}

/// The snapshot a continuation is judged against is a plain object, written in
/// the snake_case names the rest of the vocabulary uses.
#[test]
fn a_snapshot_is_the_reading_an_answer_was_established_under() {
    let snapshot = Snapshot::new("epoch-1", 12, Some("fp-1".to_string()), Some(4));
    assert_eq!(
        wire(&snapshot),
        concat!(
            r#"{"epoch":"epoch-1","generation":12,"#,
            r#""schema_fingerprint":"fp-1","sidecar_revision":4}"#
        )
    );
    round_trip(&snapshot);
}

// ── The answer reading and the read product ──────────────────────────────

#[test]
fn every_answer_reading_survives_the_round_trip() {
    for reading in answer_readings() {
        round_trip(&reading);
    }
    for section in engine_sections() {
        round_trip(&section);
    }
    for part in unsatisfied_parts() {
        round_trip(&part);
    }
}

/// A reading is the trust state, the database, how far its writes had got, and
/// the ladder where a model ran. A `null` ladder is an answer no model
/// contributed to.
#[test]
fn a_reading_carries_the_trust_state_the_database_and_the_ladder() {
    assert_eq!(
        wire(&AnswerReading::new(TrustState::Ready, "epoch-1", 12, None)),
        concat!(
            r#"{"trust":{"state":"ready"},"epoch":"epoch-1","generation":12,"#,
            r#""ladder":null}"#
        )
    );
}

/// A report carries what its rung has and nothing it does not: the floor
/// neither half, the stateful rung both, a request-time rung its model alone.
/// There is no spelling of a request-time rung with a lag, because the shape
/// holds no field for one.
#[test]
fn a_rung_report_carries_what_its_own_rung_holds() {
    assert_eq!(wire(&RungReport::lexical()), r#"{"rung":"lexical"}"#);
    assert_eq!(
        wire(&RungReport::vector(
            ModelIdentity::new("stub", "1"),
            Freshness::trailing(3)
        )),
        concat!(
            r#"{"rung":"vector","model":{"id":"stub","version":"1"},"#,
            r#""freshness":{"state":"trailing","generations":3}}"#
        )
    );
    assert_eq!(
        wire(&RungReport::expansion(ModelIdentity::new("stub", "1"))),
        r#"{"rung":"expansion","model":{"id":"stub","version":"1"}}"#
    );
    assert_eq!(
        wire(&RungReport::rerank(ModelIdentity::new("stub", "1"))),
        r#"{"rung":"rerank","model":{"id":"stub","version":"1"}}"#
    );
    assert_eq!(wire(&Freshness::rescanning()), r#"{"state":"rescanning"}"#);
    assert!(
        serde_json::from_str::<RungReport>(
            r#"{"rung":"expansion","model":{"id":"stub","version":"1"},"freshness":{"state":"rescanning"}}"#
        )
        .is_ok(),
        "a struct drops a field it does not know, and a report is a struct"
    );
}

/// Every report names the rung it is a report of, and the selector it names is
/// the one a request asks that rung for.
#[test]
fn every_rung_report_names_its_own_rung() {
    let named: Vec<Rung> = rung_reports().iter().map(RungReport::rung).collect();
    for rung in rungs() {
        assert!(
            named.contains(&rung),
            "no report here is a report of {rung:?}"
        );
    }
    assert_eq!(RungReport::lexical().rung(), Rung::Lexical);
    assert_eq!(
        RungReport::vector(ModelIdentity::new("stub", "1"), Freshness::rescanning()).rung(),
        Rung::Vector
    );
    assert_eq!(
        RungReport::expansion(ModelIdentity::new("stub", "1")).rung(),
        Rung::Expansion
    );
    assert_eq!(
        RungReport::rerank(ModelIdentity::new("stub", "1")).rung(),
        Rung::Rerank
    );
}

/// The section the host was delivered is four readings, and the malformed one
/// carries its account as prose beside the tag a client branches on.
#[test]
fn an_engine_section_is_an_object_tagged_state() {
    assert_eq!(wire(&EngineSection::absent()), r#"{"state":"absent"}"#);
    assert_eq!(wire(&EngineSection::disabled()), r#"{"state":"disabled"}"#);
    assert_eq!(wire(&EngineSection::enabled()), r#"{"state":"enabled"}"#);
    assert_eq!(
        wire(&EngineSection::malformed(
            "the `engine` table holds a string"
        )),
        r#"{"state":"malformed","detail":"the `engine` table holds a string"}"#
    );
}

/// An unsatisfied part is an object tagged `part`, and the one that carries a
/// target carries it as the typed target rather than as a string a reader
/// would have to parse again.
#[test]
fn an_unsatisfied_part_is_an_object_tagged_part() {
    assert_eq!(
        wire(&Unsatisfied::unknown_sort_key(
            "due",
            vec!["date".to_string()]
        )),
        r#"{"part":"unknown_sort_key","key":"due","did_you_mean":["date"]}"#
    );
    assert_eq!(
        wire(&Unsatisfied::bare_directory("docs")),
        r#"{"part":"bare_directory","path":"docs"}"#
    );
    assert_eq!(
        wire(&Unsatisfied::resolves_not_applicable(target(
            "norn/glossary"
        ))),
        r#"{"part":"resolves_not_applicable","target":"norn/glossary"}"#
    );
    assert!(
        serde_json::from_str::<Unsatisfied>(r#"{"part":"resolves_not_applicable","target":""}"#)
            .is_err(),
        "a part carrying an addressless target read back as one"
    );
}

/// The two answering exits are one type: a complete answer is one with no
/// unsatisfied parts, and a partial one is the same shape saying which parts
/// were not applied.
#[test]
fn a_vault_answer_is_complete_exactly_when_nothing_was_left_unapplied() {
    let reading = || AnswerReading::new(TrustState::Ready, "epoch-1", 12, None);
    let whole: VaultAnswer<u64> = VaultAnswer::new(reading(), vec![], 3);
    assert!(whole.is_complete());
    assert_eq!(whole.report, 3);
    round_trip(&whole);

    let partial: VaultAnswer<u64> = VaultAnswer::new(reading(), unsatisfied_parts(), 3);
    assert!(!partial.is_complete());
    assert_eq!(partial.unsatisfied.len(), unsatisfied_parts().len());
    round_trip(&partial);
}

// ── The codes minted for the verbs ───────────────────────────────────────

/// A state that is not ready yet is the same reading the trust state carries,
/// so a refusal and a poll report one thing. `Ready` and every untrusted state
/// are not readings of this kind: one holds something to act on and the other
/// refuses with a reason of its own.
#[test]
fn the_not_ready_reading_is_the_two_states_a_poll_walks_out_of() {
    assert_eq!(
        TrustState::Unattached.not_ready(),
        Some(NotReady::unattached())
    );
    for phase in warming_phases() {
        assert_eq!(
            TrustState::warming(phase, 12, Some(400)).not_ready(),
            Some(NotReady::warming(phase, 12, Some(400))),
            "a warming entry lost its counters on the way"
        );
    }
    assert_eq!(TrustState::Ready.not_ready(), None);
    for reason in untrusted_reasons() {
        assert_eq!(TrustState::untrusted(reason).not_ready(), None);
    }
}

/// A warming entry's not-ready reading carries the same bytes its trust state
/// carries, so a client reads one vocabulary whether it polled or was refused.
#[test]
fn a_not_ready_reading_is_the_bytes_the_trust_state_carries() {
    for state in trust_states() {
        let Some(not_ready) = state.not_ready() else {
            continue;
        };
        assert_eq!(
            serde_json::to_value(&not_ready).expect("a reading as JSON"),
            serde_json::to_value(&state).expect("a state as JSON"),
            "the reading and the state it came from are not one shape"
        );
    }
}

/// Every reload outcome is a fact about the vault, and the failure it carries
/// is typed where a client branches and prose where a person reads.
#[test]
fn a_reload_failure_is_an_object_tagged_kind() {
    assert_eq!(
        wire(&ReloadFailure::control_file(
            ControlFile::Schema,
            ReloadStage::Parse,
            "the vault schema is invalid"
        )),
        concat!(
            r#"{"kind":"control_file","file":"schema","stage":"parse","#,
            r#""detail":"the vault schema is invalid"}"#
        )
    );
    assert_eq!(
        wire(&ReloadFailure::unsupported()),
        r#"{"kind":"unsupported"}"#
    );
    assert_eq!(
        wire(&ReloadFailure::lost_maintainership()),
        r#"{"kind":"lost_maintainership"}"#
    );
}

/// Every code the three namespaces hold is a fact about the host's serving of
/// an entry, about the requested vault, or about that vault's engine.
#[test]
fn every_code_sits_in_one_of_the_three_namespaces() {
    let namespaces: BTreeSet<String> = reason_codes()
        .iter()
        .map(|code| {
            flat_string(code)
                .split_once('/')
                .expect("a code carries a namespace")
                .0
                .to_string()
        })
        .collect();
    assert_eq!(
        namespaces,
        ["host", "vault", "engine"]
            .map(str::to_string)
            .into_iter()
            .collect()
    );
}

/// A busy reload carries nothing beyond its code: the ask is repeated rather
/// than resolved, so there is no payload to act on.
#[test]
fn a_busy_reload_carries_nothing_but_its_code() {
    assert_eq!(
        wire(&ErrorEnvelope::new(
            "this vault is already being worked over",
            ErrorDetail::reload_busy()
        )),
        concat!(
            r#"{"code":"vault/reload-busy","#,
            r#""message":"this vault is already being worked over","#,
            r#""detail":{"code":"vault/reload-busy"}}"#
        )
    );
}

/// The candidates a root resolves under ascend because the detail sorts them.
#[test]
fn ambiguous_root_candidates_ascend_whatever_order_they_arrive_in() {
    assert_eq!(
        wire(&ErrorDetail::ambiguous_root([
            name("vault"),
            name("archive"),
            name("notes"),
        ])),
        r#"{"code":"vault/ambiguous-root","candidates":["archive","notes","vault"]}"#
    );
}
