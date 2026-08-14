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
    AttachMode, ErrorDetail, ErrorEnvelope, FindingKind, FindingScope, MaintainerIdentity,
    ReasonCode, Severity, TrustState, UnknownFindingKind, UnknownSeverity, UntrustedReason,
    VaultName, WarmingPhase, WatcherLossCause,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
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
    ]
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
    ]);
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
