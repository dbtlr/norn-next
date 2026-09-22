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
    Addressing, Advisory, Anchor, AnswerReading, AttachMode, Attention, BlockRow, BodyText,
    CANDIDATE_HEAD, Candidate, CandidateHead, Change, Collection, CollectionPage,
    CollectionSelector, Column, ContainerKind, ControlFile, ControlFileFailure, CountParams,
    Cursor, CursorKey, CursorOrderChanged, DescribeParams, Direction, Directory,
    DoctorRegistryParams, DoctorRegistryReport, DocumentPath, DocumentRow, Drift, EmptyLadder,
    EngineHealth, EngineSection, EngineStatus, ErrorDetail, ErrorEnvelope, Facet, FacetKind,
    FieldType, FieldValue, FindParams, FindingKind, FindingRow, FindingScope, Fingerprints,
    Freshness, GetParams, GetReport, GroupKey, HeadingRow, Hint, Hit, KindTally, LadderDeclaration,
    LinkFamily, LinkHealth, LinkRow, ListParams, ListReport, MaintainerIdentity, ModelIdentity,
    Moved, NoProblems, NonFiniteScore, NotReady, Page, PathRuleKind, PollBackend, Predicate,
    Published, ReasonCode, RegisterParams, RegisterReport, Registration, RegistryProblem,
    RegistrySanity, ReloadFailure, ReloadOutcome, ReloadParams, ReloadReport, ReloadStage, Replace,
    RequestScope, ResolutionTarget, ResolveParams, ResolveReport, RollUp, Rung, RungReport,
    RungSet, SchemaSource, Score, SearchParams, SetParams, SetReport, Severity, Snapshot, Sort,
    SortKey, Span, StatusParams, StatusReport, TagRow, TagSource, TagStance, Tally, TotalBelowHead,
    TrustState, UnknownAddressing, UnknownFindingKind, UnknownPollBackend, UnknownRequestScope,
    UnknownSeverity, UnknownVerb, UnregisterParams, UnregisterReport, Unsatisfied, UntrustedReason,
    ValidateParams, ValidateReport, VaultAddress, VaultAnswer, VaultName, VaultRoot, VaultStatus,
    Verb, WarmingPhase, WatcherLossCause,
};
use serde::de::value::{Error as ValueError, F64Deserializer};
use serde::de::{DeserializeOwned, IntoDeserializer};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
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
        ReasonCode::VaultAmbiguousTarget,
        ReasonCode::VaultUnknownTarget,
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

/// Every control file a reload met a refusal on, at every boundary.
fn control_file_failures() -> Vec<ControlFileFailure> {
    let mut failures = Vec::new();
    for file in [ControlFile::Schema, ControlFile::Config] {
        for stage in [ReloadStage::Read, ReloadStage::Parse, ReloadStage::Apply] {
            failures.push(ControlFileFailure::new(
                file,
                stage,
                "the vault schema cannot be read",
            ));
        }
    }
    failures
}

/// Every failure a reload carries.
fn reload_failures() -> Vec<ReloadFailure> {
    let mut failures: Vec<ReloadFailure> = control_file_failures()
        .into_iter()
        .map(ReloadFailure::control_file)
        .collect();
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
        ErrorDetail::ambiguous_target(
            target("glossary"),
            head(
                [candidate("notes/glossary"), candidate("archive/glossary")],
                4,
            ),
            Hint::resolves(target("glossary")),
        ),
        ErrorDetail::unknown_target(target("glossary")),
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

/// Every directory the grammar accepts, spread across the shapes a path
/// takes. A directory is not a root: the deepest of these sits well under one.
fn directories() -> Vec<Directory> {
    [
        "/",
        "/home/person/notes",
        "/home/person/notes/journal/2026",
        "/tmp/a.b",
    ]
    .into_iter()
    .map(|text| Directory::new(text).expect("an absolute directory"))
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
        FacetKind::TagPattern,
        FacetKind::UndeclaredTags,
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
        CursorKey::tally([Some("note".to_string()), None]),
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
        Unsatisfied::missing_block("a1"),
        Unsatisfied::resolves_not_applicable(target("norn/glossary")),
    ]
}

/// One document path a vector names a document by, parsed through the grammar
/// the type keeps.
fn path(text: &str) -> DocumentPath {
    DocumentPath::new(text).expect("a legal document path")
}

/// One position, which every row that names one names the same way.
fn span() -> Span {
    Span::new(3, 1, 42)
}

/// Every part of a document a read can ask to have on a row.
fn columns() -> Vec<Column> {
    vec![
        Column::path(),
        Column::field("due"),
        Column::body(),
        Column::links(),
        Column::headings(),
        Column::blocks(),
        Column::tags(),
        Column::findings(),
        Column::fields(),
    ]
}

/// Every reading a resolved link's health takes.
fn link_healths() -> Vec<LinkHealth> {
    vec![
        LinkHealth::Healthy,
        LinkHealth::Broken,
        LinkHealth::Ambiguous,
    ]
}

/// Every grammar a link is written in.
fn link_families() -> Vec<LinkFamily> {
    vec![LinkFamily::Wikilink, LinkFamily::Markdown]
}

/// Every home a tag is read from.
fn tag_sources() -> Vec<TagSource> {
    vec![TagSource::Body, TagSource::Frontmatter]
}

/// Every container a frontmatter value sits in, and the two leaves that carry
/// no text beside them. The map holds a sequence and the sequence holds a map,
/// so the census walks the tree rather than its root alone.
fn field_values() -> Vec<FieldValue> {
    vec![
        FieldValue::scalar("note"),
        FieldValue::sequence([
            FieldValue::scalar("a"),
            FieldValue::map([("b".to_string(), FieldValue::scalar("c"))]),
        ]),
        nested_field_value(),
        FieldValue::null(),
        FieldValue::absent(),
    ]
}

/// A map holding a sequence holding a map, which is the deepest shape the
/// vocabulary admits without admitting a second parse.
fn nested_field_value() -> FieldValue {
    FieldValue::map([(
        "outer".to_string(),
        FieldValue::sequence([
            FieldValue::scalar("first"),
            FieldValue::map([
                ("inner".to_string(), FieldValue::scalar("deep")),
                ("missing".to_string(), FieldValue::absent()),
            ]),
        ]),
    )])
}

/// One link row per health, built through the constructor that derives it
/// from the total of the bounded head beside it.
fn link_rows() -> Vec<LinkRow> {
    [
        head([], 0),
        head([candidate("notes/a")], 1),
        head([candidate("notes/a"), candidate("archive/a")], 2),
    ]
    .into_iter()
    .map(link_row)
    .collect()
}

/// A link row resolving to `targets`, built through the constructor that
/// derives its health.
fn link_row(targets: CandidateHead) -> LinkRow {
    LinkRow::new(
        LinkFamily::Wikilink,
        false,
        None,
        "a",
        Some("A".to_string()),
        Some(Anchor::heading("Design")),
        span(),
        targets,
    )
}

/// The candidates a link row's head carries, as the bytes a reader is handed.
fn candidate_values(paths: &[&str]) -> Vec<serde_json::Value> {
    paths
        .iter()
        .map(|path| serde_json::json!({"path": path, "suffix": "a"}))
        .collect()
}

/// A link row as bytes, with the head and the health named apart so a test can
/// hand the reader halves that disagree.
fn link_row_json(health: &str, candidates: &[serde_json::Value], total: u64) -> String {
    serde_json::json!({
        "family": "wikilink",
        "embed": false,
        "protocol": null,
        "target": "a",
        "title": null,
        "anchor": null,
        "span": {"line": 1, "column": 1, "byte_offset": 0},
        "targets": {"candidates": candidates, "total": total},
        "health": health,
    })
    .to_string()
}

/// Every hint a bounded head points a client at.
fn hints() -> Vec<Hint> {
    vec![Hint::resolves(target("norn/glossary"))]
}

/// One candidate, which the two bounded heads are built out of.
fn candidate(text: &str) -> Candidate {
    Candidate::new(path(&format!("{text}.md")), text)
}

/// A candidate head, built through the constructor that bounds it.
fn head(candidates: impl IntoIterator<Item = Candidate>, total: u64) -> CandidateHead {
    CandidateHead::new(candidates, total).expect("a head no larger than its total")
}

/// The head a finding that is not about resolution carries: no candidate, out
/// of none.
fn no_head() -> CandidateHead {
    head([], 0)
}

/// A finding row, carrying the head the constructor bounded.
fn finding_row() -> FindingRow {
    FindingRow::new(
        7,
        FindingKind::UndeclaredTag,
        Severity::Warning,
        path("notes/a.md"),
        Some("draft".to_string()),
        Some(span()),
        head(
            [candidate("notes/glossary"), candidate("archive/glossary")],
            9,
        ),
        Some(Hint::resolves(target("glossary"))),
        "the tag is not declared",
        12,
    )
}

/// Every nested collection a request pages by ordinal.
fn collection_selectors() -> Vec<CollectionSelector> {
    vec![
        CollectionSelector::Links,
        CollectionSelector::Headings,
        CollectionSelector::Blocks,
        CollectionSelector::Tags,
        CollectionSelector::Findings,
    ]
}

/// One heading row, which two shapes carry.
fn heading_row() -> HeadingRow {
    HeadingRow::new(2, "Design", "design", span())
}

/// One page of each nested collection.
fn collection<T>(items: Vec<T>, total: u64) -> Collection<T>
where
    T: schemars::JsonSchema + Serialize + DeserializeOwned,
{
    Collection::new(items, total).expect("a total no smaller than its items")
}

/// A body cut to `byte_length`, built through the constructor that checks it.
fn body(text: &str, byte_length: u64) -> BodyText {
    BodyText::new(text, byte_length).expect("a length no smaller than its text")
}

fn collection_pages() -> Vec<CollectionPage> {
    vec![
        CollectionPage::links(Page::new(link_rows(), None, vec![])),
        CollectionPage::headings(Page::new(vec![heading_row()], None, vec![])),
        CollectionPage::blocks(Page::new(
            vec![BlockRow::new("a1", Some(span()))],
            None,
            vec![],
        )),
        CollectionPage::tags(Page::new(
            vec![TagRow::new("draft", TagSource::Body, None)],
            None,
            vec![],
        )),
        CollectionPage::findings(Page::new(vec![finding_row()], None, vec![])),
    ]
}

/// A document row carrying every column a projection can ask for.
fn whole_document_row() -> DocumentRow {
    DocumentRow::new(path("notes/a.md"))
        .with_fields(BTreeMap::from([(
            "type".to_string(),
            FieldValue::scalar("note"),
        )]))
        .with_body(body("Design\n", 4096))
        .with_links(collection(link_rows(), 9))
        .with_headings(collection(vec![heading_row()], 1))
        .with_blocks(collection(vec![BlockRow::new("a1", None)], 1))
        .with_tags(collection(
            vec![TagRow::new("draft", TagSource::Frontmatter, Some(span()))],
            1,
        ))
        .with_findings(collection(vec![finding_row()], 1))
}

/// Every shape a `get` answers with.
fn get_reports() -> Vec<GetReport> {
    let mut reports = vec![
        GetReport::record(whole_document_row()),
        GetReport::section(
            path("notes/a.md"),
            heading_row(),
            body("the section body", 16),
        ),
        GetReport::block(
            path("notes/a.md"),
            BlockRow::new("a1", Some(span())),
            body("the block body", 14),
        ),
    ];
    reports.extend(
        collection_pages()
            .into_iter()
            .map(|page| GetReport::collection(path("notes/a.md"), page)),
    );
    reports
}

/// Every shape a `validate` answers with.
fn validate_reports() -> Vec<ValidateReport> {
    vec![
        ValidateReport::findings(Page::new(vec![finding_row()], None, vec![])),
        ValidateReport::summary([KindTally::new(
            FindingKind::UndeclaredTag,
            Severity::Warning,
            3,
        )]),
    ]
}

/// Every key a `find` orders by.
fn sort_keys() -> Vec<SortKey> {
    vec![SortKey::field("due"), SortKey::path()]
}

/// Every way an order runs.
fn directions() -> Vec<Direction> {
    vec![Direction::Ascending, Direction::Descending]
}

/// Every key a `count` groups by.
fn group_keys() -> Vec<GroupKey> {
    vec![GroupKey::field("type"), GroupKey::tag()]
}

/// Every type a vault's schema declares a field under.
fn field_types() -> Vec<FieldType> {
    FieldType::ALL.to_vec()
}

/// Every container an observed field's values sit in.
fn container_kinds() -> Vec<ContainerKind> {
    vec![
        ContainerKind::Scalar,
        ContainerKind::Sequence,
        ContainerKind::Map,
    ]
}

/// Every rule a path rule states.
fn path_rule_kinds() -> Vec<PathRuleKind> {
    vec![PathRuleKind::AmbiguityIgnore]
}

/// Every facet a `describe` reports, one per shape.
fn facets() -> Vec<Facet> {
    let mut facets: Vec<Facet> = field_types()
        .into_iter()
        .map(|field_type| Facet::declared_field("due", field_type, true, None))
        .collect();
    facets.push(Facet::declared_field(
        "status",
        FieldType::Text,
        false,
        Some(vec!["draft".to_string(), "live".to_string()]),
    ));
    facets.extend(
        container_kinds()
            .into_iter()
            .map(|container| Facet::observed_field("due", container)),
    );
    facets.push(Facet::declared_tag("area"));
    facets.push(Facet::tag_pattern("person/**"));
    facets.push(Facet::folder("journal", Some("One per day".to_string())));
    facets.push(Facet::folder("archive", None));
    facets.extend(
        path_rule_kinds()
            .into_iter()
            .map(|rule| Facet::path_rule(rule, "archive/**")),
    );
    facets.extend(tag_stances().into_iter().map(Facet::undeclared_tags));
    facets
}

/// Every stance a vault takes on a tag its facet does not admit.
fn tag_stances() -> Vec<TagStance> {
    TagStance::ALL.to_vec()
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
    assert_eq!(
        columns()
            .iter()
            .map(|column| tag_string(column, "col"))
            .collect::<BTreeSet<_>>(),
        advertised::<Column>(Some("col")),
        "the columns built here are not the columns the vocabulary holds"
    );
    assert_eq!(
        publisheds()
            .iter()
            .map(|published| tag_string(published, "answer"))
            .collect::<BTreeSet<_>>(),
        advertised::<Published>(Some("answer")),
        "the published answers built here are not the ones the vocabulary holds"
    );
    assert_eq!(
        drifts()
            .iter()
            .map(|drift| tag_string(drift, "state"))
            .collect::<BTreeSet<_>>(),
        advertised::<Drift>(Some("state")),
        "the drifts built here are not the drifts the vocabulary holds"
    );
    assert_eq!(
        engine_statuses()
            .iter()
            .map(|engine| tag_string(engine, "state"))
            .collect::<BTreeSet<_>>(),
        advertised::<EngineStatus>(Some("state")),
        "the engine statuses built here are not the ones the vocabulary holds"
    );
    assert_eq!(
        advisories()
            .iter()
            .map(|advisory| tag_string(advisory, "kind"))
            .collect::<BTreeSet<_>>(),
        advertised::<Advisory>(Some("kind")),
        "the advisories built here are not the ones the vocabulary holds"
    );
    assert_eq!(
        attentions()
            .iter()
            .map(|attention| tag_string(attention, "attention"))
            .collect::<BTreeSet<_>>(),
        advertised::<Attention>(Some("attention")),
        "the attention reasons built here are not the ones the vocabulary holds"
    );
    assert_eq!(
        resolve_reports()
            .iter()
            .map(|report| tag_string(report, "outcome"))
            .collect::<BTreeSet<_>>(),
        advertised::<ResolveReport>(Some("outcome")),
        "the resolutions built here are not the ones the vocabulary holds"
    );
    assert_eq!(
        status_reports()
            .iter()
            .map(|report| tag_string(report, "shape"))
            .collect::<BTreeSet<_>>(),
        advertised::<StatusReport>(Some("shape")),
        "the status reports built here are not the ones the vocabulary holds"
    );
    assert_eq!(
        registry_problems()
            .iter()
            .map(|problem| tag_string(problem, "problem"))
            .collect::<BTreeSet<_>>(),
        advertised::<RegistryProblem>(Some("problem")),
        "the registry problems built here are not the ones the vocabulary holds"
    );
    assert_eq!(
        registry_sanities()
            .iter()
            .map(|sanity| tag_string(sanity, "state"))
            .collect::<BTreeSet<_>>(),
        advertised::<RegistrySanity>(Some("state")),
        "the sanity readings built here are not the ones the vocabulary holds"
    );
    assert_eq!(
        reload_outcomes()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>(),
        advertised::<ReloadOutcome>(None),
        "the reload outcomes built here are not the ones the vocabulary holds"
    );
    assert_eq!(
        changes()
            .iter()
            .map(|change| tag_string(change, "change"))
            .collect::<BTreeSet<_>>(),
        advertised::<Change<SchemaSource>>(Some("change")),
        "the changes built here are not the changes the vocabulary holds"
    );
    assert_eq!(
        replacements()
            .iter()
            .map(|replacement| tag_string(replacement, "change"))
            .collect::<BTreeSet<_>>(),
        advertised::<Replace<VaultRoot>>(Some("change")),
        "the replacements built here are not the ones the vocabulary holds"
    );
    assert_eq!(
        link_healths()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>(),
        advertised::<LinkHealth>(None),
        "the healths built here are not the healths the vocabulary holds"
    );
    assert_eq!(
        link_families()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>(),
        advertised::<LinkFamily>(None),
        "the families built here are not the families the vocabulary holds"
    );
    assert_eq!(
        tag_sources()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>(),
        advertised::<TagSource>(None),
        "the tag sources built here are not the ones the vocabulary holds"
    );
    assert_eq!(
        field_values()
            .iter()
            .map(|value| tag_string(value, "kind"))
            .collect::<BTreeSet<_>>(),
        advertised::<FieldValue>(Some("kind")),
        "the field values built here are not the ones the vocabulary holds"
    );
    assert_eq!(
        hints()
            .iter()
            .map(|hint| tag_string(hint, "hint"))
            .collect::<BTreeSet<_>>(),
        advertised::<Hint>(Some("hint")),
        "the hints built here are not the hints the vocabulary holds"
    );
    assert_eq!(
        collection_selectors()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>(),
        advertised::<CollectionSelector>(None),
        "the selectors built here are not the selectors the vocabulary holds"
    );
    assert_eq!(
        collection_pages()
            .iter()
            .map(|page| tag_string(page, "of"))
            .collect::<BTreeSet<_>>(),
        advertised::<CollectionPage>(Some("of")),
        "the collection pages built here are not the ones the vocabulary holds"
    );
    assert_eq!(
        get_reports()
            .iter()
            .map(|report| tag_string(report, "shape"))
            .collect::<BTreeSet<_>>(),
        advertised::<GetReport>(Some("shape")),
        "the get reports built here are not the ones the vocabulary holds"
    );
    assert_eq!(
        validate_reports()
            .iter()
            .map(|report| tag_string(report, "shape"))
            .collect::<BTreeSet<_>>(),
        advertised::<ValidateReport>(Some("shape")),
        "the validate reports built here are not the ones the vocabulary holds"
    );
    assert_eq!(
        sort_keys()
            .iter()
            .map(|key| tag_string(key, "by"))
            .collect::<BTreeSet<_>>(),
        advertised::<SortKey>(Some("by")),
        "the sort keys built here are not the keys the vocabulary holds"
    );
    assert_eq!(
        directions()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>(),
        advertised::<Direction>(None),
        "the directions built here are not the directions the vocabulary holds"
    );
    assert_eq!(
        group_keys()
            .iter()
            .map(|key| tag_string(key, "by"))
            .collect::<BTreeSet<_>>(),
        advertised::<GroupKey>(Some("by")),
        "the group keys built here are not the keys the vocabulary holds"
    );
    assert_eq!(
        field_types()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>(),
        advertised::<FieldType>(None),
        "the field types built here are not the types the vocabulary holds"
    );
    assert_eq!(
        container_kinds()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>(),
        advertised::<ContainerKind>(None),
        "the containers built here are not the containers the vocabulary holds"
    );
    assert_eq!(
        path_rule_kinds()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>(),
        advertised::<PathRuleKind>(None),
        "the path rules built here are not the rules the vocabulary holds"
    );
    assert_eq!(
        facets()
            .iter()
            .map(|facet| tag_string(facet, "facet"))
            .collect::<BTreeSet<_>>(),
        advertised::<Facet>(Some("facet")),
        "the facets built here are not the facets the vocabulary holds"
    );
    assert_eq!(
        tag_stances()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>(),
        advertised::<TagStance>(None),
        "the stances built here are not the stances the vocabulary holds"
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
        wire(&ReloadFailure::control_file(ControlFileFailure::new(
            ControlFile::Schema,
            ReloadStage::Parse,
            "the vault schema is invalid"
        ))),
        concat!(
            r#"{"kind":"control_file","failure":{"file":"schema","stage":"parse","#,
            r#""detail":"the vault schema is invalid"}}"#
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

// ── The document row and the columns it is projected onto ────────────────

#[test]
fn every_document_shape_survives_the_round_trip() {
    for column in columns() {
        round_trip(&column);
    }
    for value in field_values() {
        round_trip(&value);
    }
    for row in link_rows() {
        round_trip(&row);
    }
    for health in link_healths() {
        round_trip(&health);
    }
    for family in link_families() {
        round_trip(&family);
    }
    for source in tag_sources() {
        round_trip(&source);
    }
    round_trip(&span());
    round_trip(&heading_row());
    round_trip(&BlockRow::new("a1", Some(span())));
    round_trip(&TagRow::new("draft", TagSource::Body, None));
    round_trip(&whole_document_row());
    round_trip(&DocumentRow::new(path("notes/a.md")));
}

/// A path is the string itself, and the read path is the grammar: the one
/// string that names nothing has no representation on either side of the seam.
/// What else a path may hold is the store's grammar and is not re-checked
/// here, so a path a vault could not have is carried rather than refused.
#[test]
fn a_document_path_is_the_string_it_renders_as_and_is_not_empty() {
    assert_eq!(wire(&path("notes/a.md")), r#""notes/a.md""#);
    assert_eq!(path("notes/a.md").as_str(), "notes/a.md");
    assert_eq!(path("notes/a.md").to_string(), "notes/a.md");
    assert!(
        serde_json::from_str::<DocumentPath>(r#""""#).is_err(),
        "the empty string was read as a document path"
    );
    let refusal = DocumentPath::new("").expect_err("an empty document path");
    assert_eq!(refusal.what(), "document path");
    for text in ["../a.md", "a b.md"] {
        let json = format!("\"{text}\"");
        assert!(
            serde_json::from_str::<DocumentPath>(&json).is_ok(),
            "`{text}` was refused by a grammar this type does not keep"
        );
    }
}

/// A document path is relative to the vault root, which is what the schema
/// publishes, so a path that starts at a filesystem root is refused where one
/// is built and where one is read alike.
#[test]
fn a_rooted_document_path_is_no_document_path() {
    let refusal = DocumentPath::new("/etc/passwd").expect_err("a rooted document path");
    assert_eq!(refusal.what(), "document path");
    assert_eq!(
        refusal.problem(),
        "a document path is relative to the vault root"
    );
    for text in ["/etc/passwd", "/", "/notes/a.md"] {
        assert!(
            DocumentPath::new(text).is_err(),
            "`{text}` was built as a document path"
        );
        let json = format!("\"{text}\"");
        assert!(
            serde_json::from_str::<DocumentPath>(&json).is_err(),
            "`{text}` was read back as a document path"
        );
    }
}

/// A column is an object tagged `col`, and the one that names a key carries it
/// beside the tag rather than as the tag.
#[test]
fn a_column_is_an_object_tagged_col() {
    assert_eq!(wire(&Column::path()), r#"{"col":"path"}"#);
    assert_eq!(wire(&Column::fields()), r#"{"col":"fields"}"#);
    assert_eq!(
        wire(&Column::field("due")),
        r#"{"col":"field","key":"due"}"#
    );
    assert!(
        serde_json::from_str::<Column>(r#"{"col":"backlinks"}"#).is_err(),
        "a column nobody minted read back as one"
    );
}

/// A collection of a row type two of which are one value is a value two of
/// which are one, so a consumer compares two pages of blocks the way it
/// compares two blocks. The bound holds wherever the row holds it: a
/// collection of hits carries a score and is `PartialEq` alone, so this is a
/// compile-time check rather than a comparison.
#[test]
fn a_collection_of_an_equatable_row_is_equatable() {
    fn equatable<T: Eq>() {}
    equatable::<Collection<BlockRow>>();
    equatable::<Collection<FindingRow>>();
    assert_eq!(
        collection(vec![BlockRow::new("a1", None)], 1),
        collection(vec![BlockRow::new("a1", None)], 1)
    );
}

/// Every column a document row is projected onto holds a row type two of
/// which are one value — a path, a frontmatter tree, a body, and the four
/// collections — so the row itself is a value two of which are one. No column
/// carries a score, which is the one thing in the vocabulary that is near
/// without being equal, so this is a compile-time check rather than a
/// comparison.
#[test]
fn a_document_row_is_equatable() {
    fn equatable<T: Eq>() {}
    equatable::<DocumentRow>();
    let row = DocumentRow::new(path("notes/a.md"))
        .with_fields(BTreeMap::from([(
            "type".to_string(),
            FieldValue::scalar("note"),
        )]))
        .with_body(body("hello", 5));
    assert_eq!(row, row.clone());
}

/// A nested collection says in band what it cut. The items are the head and
/// the total is the vault's count, so a row whose links were bounded reports
/// the bound rather than handing back a short list that reads as the whole.
#[test]
fn a_collection_reports_the_cut_through_its_total() {
    let whole = collection(vec![BlockRow::new("a1", None)], 1);
    assert!(!whole.is_truncated());
    let cut = collection(vec![BlockRow::new("a1", None)], 12);
    assert!(cut.is_truncated());
    assert_eq!(
        wire(&cut),
        r#"{"items":[{"id":"a1","span":null}],"total":12}"#
    );
}

/// The total is what makes the items a head, so a total below them heads
/// nothing: it is refused where a collection is built and where one is read
/// alike, rather than landing as a row claiming to have been cut to more than
/// it holds.
#[test]
fn a_collection_total_below_its_items_heads_nothing() {
    let refusal = Collection::new(
        vec![BlockRow::new("a1", None), BlockRow::new("a2", None)],
        1,
    )
    .expect_err("a total below its items");
    assert_eq!(refusal.head(), 2);
    assert_eq!(refusal.total(), 1);
    assert_eq!(
        TotalBelowHead::to_string(&refusal),
        "a head of 2 cannot be the head of 1"
    );
    assert!(
        serde_json::from_str::<Collection<BlockRow>>(
            r#"{"items":[{"id":"a1","span":null},{"id":"a2","span":null}],"total":1}"#
        )
        .is_err(),
        "a collection claiming a total below its items read back"
    );
    assert!(
        serde_json::from_str::<Collection<BlockRow>>(
            r#"{"items":[{"id":"a1","span":null},{"id":"a2","span":null}],"total":2}"#
        )
        .is_ok(),
        "a collection whose total is its own count refused the read"
    );
}

/// A body reports the cut the handler's per-row ceiling made: the text is the
/// head and the byte length is the whole body's, so a length below the text
/// beside it heads nothing and refuses where one is built and where one is
/// read alike.
#[test]
fn a_body_reports_the_cut_through_its_byte_length() {
    let whole = body("Design\n", 7);
    assert!(!whole.is_truncated());
    assert_eq!(whole.text(), "Design\n");
    assert_eq!(whole.byte_length(), 7);
    let cut = body("Design\n", 4096);
    assert!(cut.is_truncated());
    assert_eq!(wire(&cut), r#"{"text":"Design\n","byte_length":4096}"#);
    round_trip(&cut);

    let refusal = BodyText::new("Design\n", 3).expect_err("a length below its text");
    assert_eq!(refusal.head(), 7);
    assert_eq!(refusal.total(), 3);
    assert!(
        serde_json::from_str::<BodyText>(r#"{"text":"Design\n","byte_length":3}"#).is_err(),
        "a body claiming a length below its text read back"
    );
}

/// Health is read off the documents a target resolved to, and the derivation
/// is the whole of it: none is broken, one is healthy, and more than one is
/// ambiguous.
#[test]
fn a_links_health_is_the_count_of_what_it_resolves_to() {
    let rows = link_rows();
    assert_eq!(rows.len(), link_healths().len());
    assert_eq!(rows[0].health(), LinkHealth::Broken);
    assert_eq!(rows[1].health(), LinkHealth::Healthy);
    assert_eq!(rows[2].health(), LinkHealth::Ambiguous);
    assert_eq!(LinkHealth::of_targets(0), LinkHealth::Broken);
    assert_eq!(LinkHealth::of_targets(1), LinkHealth::Healthy);
    assert_eq!(LinkHealth::of_targets(400), LinkHealth::Ambiguous);
}

/// The count the health is read off is the head's total, not the candidates
/// the head carries: a target that named nine documents is ambiguous on a row
/// whose head stops at five and on a row whose head carries one of them,
/// because what the health describes is the class rather than the part of it
/// that fit.
#[test]
fn a_links_health_is_the_heads_total_rather_than_its_length() {
    let nine: Vec<Candidate> = (0..9)
        .map(|index| candidate(&format!("notes/g{index}")))
        .collect();
    let five_of_nine = head(nine.clone(), 9);
    assert_eq!(five_of_nine.candidates().len(), CANDIDATE_HEAD);
    assert_eq!(link_row(five_of_nine).health(), LinkHealth::Ambiguous);
    let one_of_nine = head(nine.into_iter().take(1), 9);
    assert_eq!(link_row(one_of_nine).health(), LinkHealth::Ambiguous);
    assert_eq!(
        link_row(head([candidate("notes/a")], 1)).health(),
        LinkHealth::Healthy
    );
    assert_eq!(link_row(head([], 0)).health(), LinkHealth::Broken);
}

/// The two halves of one fact cannot arrive disagreeing: a row whose `health`
/// is not the health of the documents beside it is refused entire rather than
/// read into a value that says two things about one link.
#[test]
fn a_link_whose_health_is_not_its_targets_refuses_the_read() {
    for (health, paths, total) in [
        ("broken", &[][..], 0),
        ("healthy", &["notes/a.md"][..], 1),
        ("ambiguous", &["notes/a.md", "archive/a.md"][..], 2),
        ("ambiguous", &["notes/a.md"][..], 9),
    ] {
        let json = link_row_json(health, &candidate_values(paths), total);
        assert!(
            serde_json::from_str::<LinkRow>(&json).is_ok(),
            "reading {json} refused a row whose halves agree"
        );
    }
    for (health, paths, total) in [
        ("healthy", &[][..], 0),
        ("broken", &["notes/a.md"][..], 1),
        ("healthy", &["notes/a.md", "archive/a.md"][..], 2),
        ("healthy", &["notes/a.md"][..], 9),
    ] {
        let json = link_row_json(health, &candidate_values(paths), total);
        assert!(
            serde_json::from_str::<LinkRow>(&json).is_err(),
            "reading {json} produced a row whose halves disagree"
        );
    }
}

/// The bound the head keeps is the link row's too: a row carrying more
/// candidates than the bound is bytes nothing here minted, and the row refuses
/// the read through the head nested in it rather than landing a payload no
/// bound covers. The head cut to the bound reads back, total and all.
#[test]
fn a_link_row_whose_head_is_wider_than_the_bound_refuses_the_read() {
    let nine: Vec<serde_json::Value> = (0..9)
        .map(|index| serde_json::json!({"path": format!("notes/g{index}.md"), "suffix": "g"}))
        .collect();
    let json = link_row_json("ambiguous", &nine, 9);
    assert!(
        serde_json::from_str::<LinkRow>(&json).is_err(),
        "reading {json} produced a link row no bound covers"
    );
    let json = link_row_json("ambiguous", &nine[..CANDIDATE_HEAD], 9);
    let read: LinkRow =
        serde_json::from_str(&json).unwrap_or_else(|error| panic!("reading {json}: {error}"));
    assert_eq!(read.targets.candidates().len(), CANDIDATE_HEAD);
    assert_eq!(read.targets.total(), 9);
    assert!(read.targets.is_truncated());
    assert_eq!(read.health(), LinkHealth::Ambiguous);
}

/// A column the read did not project is left out of the bytes entirely, so a
/// client tells a column it did not ask for from a column the document does
/// not have. The path is carried whatever was asked for.
#[test]
fn an_unprojected_column_is_absent_from_the_row() {
    let bare = DocumentRow::new(path("notes/a.md"));
    assert_eq!(wire(&bare), r#"{"path":"notes/a.md"}"#);
    let projected = bare.clone().with_body(body("hello", 5));
    assert_eq!(
        wire(&projected),
        r#"{"path":"notes/a.md","body":{"text":"hello","byte_length":5}}"#
    );
    let read: DocumentRow =
        serde_json::from_str(r#"{"path":"notes/a.md"}"#).expect("a row projecting nothing");
    assert_eq!(read, bare);
    assert_eq!(read.body, None);
}

/// A frontmatter value crosses tagged by the container it sits in: a scalar
/// as the text it is written as, a sequence as a sequence of values, and a
/// map as a mapping of nested values, never as JSON in a string.
#[test]
fn a_field_value_is_an_object_tagged_kind() {
    assert_eq!(
        wire(&FieldValue::scalar("note")),
        r#"{"kind":"scalar","raw":"note"}"#
    );
    assert_eq!(
        wire(&FieldValue::sequence([FieldValue::scalar("a")])),
        r#"{"kind":"sequence","items":[{"kind":"scalar","raw":"a"}]}"#
    );
    assert_eq!(
        wire(&FieldValue::map([(
            "a".to_string(),
            FieldValue::scalar("1")
        )])),
        r#"{"kind":"map","entries":{"a":{"kind":"scalar","raw":"1"}}}"#
    );
    assert_eq!(wire(&FieldValue::null()), r#"{"kind":"null"}"#);
    assert_eq!(wire(&FieldValue::absent()), r#"{"kind":"absent"}"#);
}

/// A field written with no value and a field the document never wrote are two
/// values, not one. A projection that folded them together would leave a
/// client unable to tell an empty frontmatter key from a missing one, so the
/// two leaves are compared here as well as pinned in bytes.
#[test]
fn a_present_null_is_not_an_absent_field() {
    assert_ne!(FieldValue::null(), FieldValue::absent());
    round_trip(&FieldValue::null());
    let row = DocumentRow::new(path("notes/a.md")).with_fields(BTreeMap::from([
        ("due".to_string(), FieldValue::null()),
        ("area".to_string(), FieldValue::absent()),
    ]));
    assert_eq!(
        wire(&row),
        concat!(
            r#"{"path":"notes/a.md","fields":{"area":{"kind":"absent"},"#,
            r#""due":{"kind":"null"}}}"#
        )
    );
    round_trip(&row);
}

/// A nested value is a value, all the way down: a map holding a sequence
/// holding a map crosses as the tree it is and is read back as the same tree,
/// so a client reads a nested frontmatter value the way it reads a flat one
/// rather than parsing a string a second time.
#[test]
fn a_nested_field_value_crosses_as_a_tree_rather_than_as_json_in_a_string() {
    let value = nested_field_value();
    round_trip(&value);
    assert_eq!(
        wire(&value),
        concat!(
            r#"{"kind":"map","entries":{"outer":{"kind":"sequence","items":["#,
            r#"{"kind":"scalar","raw":"first"},"#,
            r#"{"kind":"map","entries":{"inner":{"kind":"scalar","raw":"deep"},"#,
            r#""missing":{"kind":"absent"}}}]}}}"#
        )
    );
    assert!(
        !wire(&value).contains(r#"raw_json"#),
        "a map crossed as JSON in a string"
    );
}

// ── The finding row and its bounded head ─────────────────────────────────

#[test]
fn every_finding_row_shape_survives_the_round_trip() {
    round_trip(&finding_row());
    for hint in hints() {
        round_trip(&hint);
    }
    round_trip(&candidate("notes/glossary"));
    round_trip(&no_head());
    round_trip(&head([candidate("notes/glossary")], 9));
}

/// The head is bounded where one is built, so a producer handing over more
/// candidates than the bound does not widen it. The total is untouched: it is
/// what makes the head a head.
#[test]
fn a_candidate_list_is_bounded_at_the_head_wherever_it_is_built() {
    let many: Vec<Candidate> = (0..12)
        .map(|index| candidate(&format!("notes/g{index}")))
        .collect();
    let bounded = head(many, 12);
    assert_eq!(bounded.candidates().len(), CANDIDATE_HEAD);
    assert_eq!(bounded.total(), 12);
    assert!(bounded.is_truncated());

    let row = FindingRow::new(
        1,
        FindingKind::UndeclaredTag,
        Severity::Error,
        path("notes/a.md"),
        None,
        None,
        bounded.clone(),
        None,
        "ambiguous",
        3,
    );
    let detail = ErrorDetail::ambiguous_target(
        target("glossary"),
        bounded,
        Hint::resolves(target("glossary")),
    );
    for json in [
        serde_json::to_value(&row).expect("a row as JSON"),
        serde_json::to_value(&detail).expect("a detail as JSON"),
    ] {
        assert_eq!(
            json["head"]["candidates"].as_array().map(Vec::len),
            Some(CANDIDATE_HEAD)
        );
        assert_eq!(json["head"]["total"].as_u64(), Some(12));
    }
}

/// The bound is the vocabulary's, so it holds on the way in as well as on the
/// way out: bytes carrying nine candidates are bytes nothing here minted, and
/// the finding row and the refusal refuse them rather than reading a head no bound
/// covers.
#[test]
fn a_head_wider_than_the_bound_refuses_the_read() {
    let nine: Vec<serde_json::Value> = (0..9)
        .map(|index| serde_json::json!({"path": format!("notes/g{index}.md"), "suffix": "g"}))
        .collect();
    let five = &nine[..CANDIDATE_HEAD];
    let head_json = |candidates: &[serde_json::Value], total: u64| serde_json::json!({"candidates": candidates, "total": total});
    let row_json = |candidates: &[serde_json::Value], total: u64| {
        serde_json::json!({
            "id": 1,
            "kind": "document/undeclared-tag",
            "severity": "error",
            "path": "notes/a.md",
            "target": null,
            "span": null,
            "head": head_json(candidates, total),
            "hint": null,
            "message": "ambiguous",
            "generation": 3,
        })
        .to_string()
    };
    let detail_json = |candidates: &[serde_json::Value], total: u64| {
        serde_json::json!({
            "code": "vault/ambiguous-target",
            "target": "glossary",
            "head": head_json(candidates, total),
            "hint": {"hint": "resolves", "target": "glossary"},
        })
        .to_string()
    };

    for json in [row_json(&nine, 12), row_json(five, 4)] {
        assert!(
            serde_json::from_str::<FindingRow>(&json).is_err(),
            "reading {json} produced a finding row no bound covers"
        );
    }
    for json in [detail_json(&nine, 12), detail_json(five, 4)] {
        assert!(
            serde_json::from_str::<ErrorDetail>(&json).is_err(),
            "reading {json} produced a refusal no bound covers"
        );
    }
    let json = row_json(five, 12);
    let row: FindingRow =
        serde_json::from_str(&json).unwrap_or_else(|error| panic!("reading {json}: {error}"));
    assert_eq!(row.head.candidates().len(), CANDIDATE_HEAD);
    assert_eq!(row.head.total(), 12);
    assert!(row.head.is_truncated());
    let json = detail_json(five, 12);
    assert!(
        serde_json::from_str::<ErrorDetail>(&json).is_ok(),
        "reading {json} refused a head the bound covers"
    );
}

/// A total below the head it heads describes no vault, and the one type both
/// carriers hold refuses it once.
#[test]
fn a_total_below_the_head_it_heads_is_refused() {
    let two = [candidate("notes/a"), candidate("notes/b")];
    let refusal = CandidateHead::new(two, 1).expect_err("a total below its head");
    assert_eq!(refusal.head(), 2);
    assert_eq!(refusal.total(), 1);
    assert_eq!(
        TotalBelowHead::to_string(&refusal),
        "a head of 2 cannot be the head of 1"
    );
}

/// A finding row is the row a report pages, and the typed halves — the kind,
/// the severity, the path, the head, the hint — cross as themselves rather
/// than as strings a reader re-parses.
#[test]
fn a_finding_row_carries_its_typed_halves() {
    let row = FindingRow::new(
        7,
        FindingKind::UndeclaredTag,
        Severity::Warning,
        path("notes/a.md"),
        Some("draft".to_string()),
        None,
        no_head(),
        Some(Hint::resolves(target("glossary"))),
        "the tag is not declared",
        12,
    );
    assert!(!row.head.is_truncated());
    assert_eq!(
        wire(&row),
        concat!(
            r#"{"id":7,"kind":"document/undeclared-tag","severity":"warning","#,
            r#""path":"notes/a.md","target":"draft","span":null,"#,
            r#""head":{"candidates":[],"total":0},"#,
            r#""hint":{"hint":"resolves","target":"glossary"},"#,
            r#""message":"the tag is not declared","generation":12}"#
        )
    );
}

/// The refusal a target that names more than one document earns carries the
/// same bounded head and the same hint a finding over that class carries, so
/// the two say one thing.
#[test]
fn an_ambiguous_target_refuses_with_the_head_a_finding_carries() {
    let envelope = ErrorEnvelope::new(
        "the target names more than one document",
        ErrorDetail::ambiguous_target(
            target("glossary"),
            head([candidate("notes/glossary")], 2),
            Hint::resolves(target("glossary")),
        ),
    );
    assert_eq!(
        wire(&envelope),
        concat!(
            r#"{"code":"vault/ambiguous-target","message":"the target names more than one document","#,
            r#""detail":{"code":"vault/ambiguous-target","target":"glossary","#,
            r#""head":{"candidates":[{"path":"notes/glossary.md","suffix":"notes/glossary"}],"total":2},"#,
            r#""hint":{"hint":"resolves","target":"glossary"}}}"#
        )
    );
    assert_eq!(
        wire(&ErrorEnvelope::new(
            "the target names no document",
            ErrorDetail::unknown_target(target("glossary")),
        )),
        concat!(
            r#"{"code":"vault/unknown-target","message":"the target names no document","#,
            r#""detail":{"code":"vault/unknown-target","target":"glossary"}}"#
        )
    );
}

/// The crate that declares the bound pins its value, so a surface rendering a
/// head and a store holding one are bounded at a number this suite would have
/// to be changed to move.
#[test]
fn the_candidate_head_is_five() {
    assert_eq!(CANDIDATE_HEAD, 5);
}

// ── The six read verbs ───────────────────────────────────────────────────

#[test]
fn every_read_report_shape_survives_the_round_trip() {
    for report in get_reports() {
        round_trip(&report);
    }
    for report in validate_reports() {
        round_trip(&report);
    }
    for page in collection_pages() {
        round_trip(&page);
    }
    for facet in facets() {
        round_trip(&facet);
    }
    for key in sort_keys() {
        round_trip(&key);
    }
    for key in group_keys() {
        round_trip(&key);
    }
    round_trip(&Tally::new([Some("note".to_string()), None], 7));
    round_trip(&KindTally::new(
        FindingKind::UndeclaredTag,
        Severity::Warning,
        3,
    ));
    round_trip(&Hit::new(path("notes/a.md"), score(0.5)));
    round_trip(&Hit::new(path("notes/a.md"), score(0.5)).with_document(whole_document_row()));
}

#[test]
fn every_read_params_shape_survives_the_round_trip() {
    let vault = VaultAddress::name(name("notes"));
    round_trip(&FindParams::new(vault.clone()));
    round_trip(
        &FindParams::new(vault.clone())
            .with_predicates(predicates())
            .with_sort(Sort::new(SortKey::field("due"), Direction::Descending))
            .with_columns(columns())
            .with_limit(20)
            .with_after(cursors().remove(0)),
    );
    round_trip(&SearchParams::new(vault.clone(), "norn"));
    round_trip(
        &SearchParams::new(vault.clone(), "norn")
            .with_predicates(predicates())
            .with_rungs(RungSet::of(rungs()).expect("a ladder that runs a rung"))
            .with_min_score(score(0.25))
            .with_columns(columns())
            .with_limit(20)
            .with_after(cursors().remove(0)),
    );
    round_trip(&GetParams::new(vault.clone(), target("glossary")));
    round_trip(
        &GetParams::new(vault.clone(), target("glossary#Design"))
            .with_columns(columns())
            .with_collection(CollectionSelector::Links)
            .with_limit(20)
            .with_after(cursors().remove(0)),
    );
    round_trip(&CountParams::new(vault.clone()));
    round_trip(
        &CountParams::new(vault.clone())
            .with_predicates(predicates())
            .with_by(group_keys())
            .with_limit(20)
            .with_after(cursors().remove(0)),
    );
    round_trip(&ValidateParams::new(vault.clone()));
    round_trip(
        &ValidateParams::new(vault.clone())
            .with_predicates(predicates())
            .with_kinds(finding_kinds())
            .with_severity(Severity::Error)
            .summarized()
            .with_limit(20)
            .with_after(cursors().remove(0)),
    );
    round_trip(&DescribeParams::new(vault.clone()));
    round_trip(
        &DescribeParams::new(vault)
            .with_facets(facet_kinds())
            .with_limit(20)
            .with_after(cursors().remove(0)),
    );
}

/// Each params type is built by naming what a request cannot be built without,
/// and every other part has a stated default. A `find` that named only its
/// vault is every document, ordered by path, projecting the path alone; a
/// `search` that named only its vault and its query runs the lexical floor.
#[test]
fn a_params_constructor_takes_the_required_parts_and_defaults_the_rest() {
    let vault = VaultAddress::name(name("notes"));
    let find = FindParams::new(vault.clone());
    assert_eq!(find.vault, vault);
    assert!(find.predicates.is_empty());
    assert!(find.columns.is_empty());
    assert_eq!(find.sort, None);
    assert_eq!(find.limit, None);
    assert_eq!(find.after, None);

    let search = SearchParams::new(vault.clone(), "norn");
    assert_eq!(search.query, "norn");
    assert_eq!(search.rungs, RungSet::lexical());
    assert_eq!(search.min_score, None);

    let validate = ValidateParams::new(vault.clone());
    assert!(!validate.summary);
    assert!(ValidateParams::new(vault.clone()).summarized().summary);

    let get = GetParams::new(vault.clone(), target("glossary"));
    assert_eq!(get.collection, None);
    assert_eq!(get.limit, None);
    assert!(CountParams::new(vault.clone()).by.is_empty());
    assert!(DescribeParams::new(vault).facets.is_empty());
}

/// A search runs at least the lexical floor, so a set naming no rung is no
/// ladder: it refuses where one is built and where one is read alike. The
/// preset spellings are a surface's and never cross.
#[test]
fn a_rung_set_that_names_no_rung_is_no_ladder() {
    assert_eq!(RungSet::of([]).expect_err("an empty ladder"), EmptyLadder);
    assert_eq!(
        EmptyLadder::to_string(&EmptyLadder),
        "a search runs at least the lexical floor"
    );
    assert!(
        serde_json::from_str::<RungSet>(r#"{"rungs":[]}"#).is_err(),
        "a set naming no rung read back as a ladder"
    );
    assert_eq!(wire(&RungSet::lexical()), r#"{"rungs":["lexical"]}"#);
    assert!(
        serde_json::from_str::<RungSet>(r#"{"rungs":["hybrid"]}"#).is_err(),
        "a preset spelling read back as a rung"
    );
    round_trip(&RungSet::of(rungs()).expect("a ladder that runs a rung"));
}

/// The set carries the rungs in ladder order whatever order a caller named
/// them in, and it holds each rung once.
#[test]
fn a_rung_set_is_the_resolved_set_in_ladder_order() {
    let named = RungSet::of([Rung::Rerank, Rung::Lexical, Rung::Rerank, Rung::Vector])
        .expect("a ladder that runs a rung");
    assert_eq!(wire(&named), r#"{"rungs":["lexical","vector","rerank"]}"#);
}

/// A get report is an object tagged `shape`, and the collection shape names
/// the collection it paged once: the page's own `of` tag, which
/// `CollectionPage::selector` is the one derivation of. There is no second
/// field beside it for the two to disagree in.
#[test]
fn a_get_report_names_the_collection_it_paged_once() {
    for page in collection_pages() {
        let selector = page.selector();
        let report = GetReport::collection(path("notes/a.md"), page);
        let json = serde_json::to_value(&report).expect("a report as JSON");
        assert_eq!(json["shape"].as_str(), Some("collection"));
        assert_eq!(
            json.as_object().map(|report| report.keys().count()),
            Some(3),
            "the collection report carries a field beside its path and its page: {json}"
        );
        assert!(
            json.get("selector").is_none(),
            "the collection report spells its selector a second time: {json}"
        );
        assert_eq!(json["page"]["of"], serde_json::to_value(selector).unwrap());
    }
    assert_eq!(
        collection_pages()
            .iter()
            .map(CollectionPage::selector)
            .collect::<Vec<_>>(),
        collection_selectors()
    );
}

/// A `#^block` anchor has a report shape of its own: the block the anchor
/// named, where it is defined, and its body. A block the document does not
/// define is an unsatisfied part beside the missing section.
#[test]
fn a_block_anchor_is_answered_with_the_block_it_named() {
    let report = GetReport::block(
        path("notes/a.md"),
        BlockRow::new("a1", Some(span())),
        body("the block body", 14),
    );
    assert_eq!(
        wire(&report),
        concat!(
            r#"{"shape":"block","path":"notes/a.md","#,
            r#""block":{"id":"a1","span":{"line":3,"column":1,"byte_offset":42}},"#,
            r#""body":{"text":"the block body","byte_length":14}}"#
        )
    );
    assert_eq!(
        wire(&Unsatisfied::missing_block("a1")),
        r#"{"part":"missing_block","id":"a1"}"#
    );
}

/// A tally carries one value per group key the request named, and `null`
/// where the document does not carry that key — so a reader lines the tuple up
/// with the request rather than guessing which key a short tuple skipped.
#[test]
fn a_tally_carries_one_value_per_group_key() {
    assert_eq!(
        wire(&Tally::new([Some("note".to_string()), None], 7)),
        r#"{"group":["note",null],"count":7}"#
    );
}

/// The row and the key it stops at spell the grouping tuple one way, and
/// `Tally::cursor_key` is the one function that turns one into the other: a
/// group member the document does not carry is `null` in the key as it is on
/// the row, so a continuation names the position the page reached.
#[test]
fn a_tally_cursor_key_spells_the_null_group_the_way_the_row_does() {
    let tally = Tally::new([Some("note".to_string()), None], 7);
    let key = tally.cursor_key();
    assert_eq!(
        key,
        CursorKey::tally([Some("note".to_string()), None]),
        "the key a tally stops at is not the tuple it carries"
    );
    round_trip(&key);
    let json = serde_json::to_value(&key).expect("a key as JSON");
    assert_eq!(json["group"], serde_json::json!(["note", null]));
    round_trip(&Cursor::new(
        Snapshot::new("epoch-1", 3, None, None),
        tally.cursor_key(),
    ));
}

/// A validate report is the findings or the tally of them, told apart by the
/// `shape` tag rather than by which key is present.
#[test]
fn a_validate_report_is_an_object_tagged_shape() {
    assert_eq!(
        wire(&ValidateReport::summary([KindTally::new(
            FindingKind::UndeclaredTag,
            Severity::Warning,
            3,
        )])),
        concat!(
            r#"{"shape":"summary","by_kind":[{"kind":"document/undeclared-tag","#,
            r#""severity":"warning","count":3}]}"#
        )
    );
}

/// A facet is an object tagged `facet`, and every facet names the kind a
/// request selects it by and a cursor orders it under. A tag pattern reports
/// its own kind rather than the declared-tag one: a pattern and a name are two
/// shapes, so `--facets tag_pattern` selects the patterns alone.
#[test]
fn every_facet_names_the_kind_a_cursor_orders_it_under() {
    for facet in facets() {
        let kind = facet.kind();
        let key = CursorKey::facet(kind, "type");
        round_trip(&key);
        let json = serde_json::to_value(&key).expect("a key as JSON");
        assert_eq!(json["kind"], serde_json::to_value(kind).unwrap());
    }
    let kinds: BTreeSet<String> = facets()
        .iter()
        .map(|facet| flat_string(&facet.kind()))
        .collect();
    assert_eq!(
        kinds,
        facet_kinds()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>()
    );
    assert_eq!(
        wire(&Facet::declared_field("due", FieldType::Date, true, None)),
        r#"{"facet":"declared_field","key":"due","field_type":"date","required":true,"one_of":null}"#
    );
    assert_eq!(
        wire(&Facet::undeclared_tags(TagStance::Report)),
        r#"{"facet":"undeclared_tags","stance":"report"}"#
    );
}

/// The map from a facet to its kind is injective: one kind per shape, so
/// `--facets` naming a kind names one shape and a page ordered by kind holds
/// one. Two shapes sharing a kind would make the selection ambiguous.
#[test]
fn every_facet_shape_maps_to_a_kind_of_its_own() {
    let shapes: BTreeSet<String> = facets()
        .iter()
        .map(|facet| tag_string(facet, "facet"))
        .collect();
    let kinds: BTreeSet<String> = facets()
        .iter()
        .map(|facet| flat_string(&facet.kind()))
        .collect();
    assert_eq!(
        kinds.len(),
        shapes.len(),
        "two facet shapes report one kind: {shapes:?} map onto {kinds:?}"
    );
    assert_eq!(
        Facet::tag_pattern("person/**").kind(),
        FacetKind::TagPattern
    );
    assert_eq!(
        Facet::undeclared_tags(TagStance::Allow).kind(),
        FacetKind::UndeclaredTags
    );
}

/// Every facet says where a page of facets stops at it, and the key it says is
/// the one text the facet itself spells. The key each shape hands back is
/// pinned here, so a shape keyed by another of its fields — or by a field it
/// gained — is a change this test reports rather than a page that resumes
/// somewhere else.
#[test]
fn every_facet_says_where_a_page_of_facets_stops_at_it() {
    for (facet, kind, key) in [
        (
            Facet::declared_field("due", FieldType::Date, true, None),
            FacetKind::DeclaredField,
            "due",
        ),
        (
            Facet::observed_field("author", ContainerKind::Sequence),
            FacetKind::ObservedField,
            "author",
        ),
        (Facet::declared_tag("area"), FacetKind::DeclaredTag, "area"),
        (
            Facet::tag_pattern("person/**"),
            FacetKind::TagPattern,
            "person/**",
        ),
        (
            Facet::folder("journal", Some("One per day".to_string())),
            FacetKind::Folder,
            "journal",
        ),
        (
            Facet::path_rule(PathRuleKind::AmbiguityIgnore, "archive/**"),
            FacetKind::PathRule,
            "archive/**",
        ),
        (
            Facet::undeclared_tags(TagStance::Allow),
            FacetKind::UndeclaredTags,
            "allow",
        ),
        (
            Facet::undeclared_tags(TagStance::Report),
            FacetKind::UndeclaredTags,
            "report",
        ),
    ] {
        let cursor_key = facet.cursor_key();
        assert_eq!(cursor_key, CursorKey::facet(kind, key));
        round_trip(&cursor_key);
        let json = serde_json::to_value(&cursor_key).expect("a key as JSON");
        assert_eq!(json["row"].as_str(), Some("facet"));
        assert_eq!(json["kind"].as_str(), Some(flat_string(&kind).as_str()));
        assert_eq!(json["key"].as_str(), Some(key));
    }
    for facet in facets() {
        let json = serde_json::to_value(facet.cursor_key()).expect("a key as JSON");
        assert_eq!(
            json["kind"].as_str(),
            Some(flat_string(&facet.kind()).as_str()),
            "a facet is keyed under a kind that is not its own: {facet:?}"
        );
        assert!(
            json["key"].as_str().is_some_and(|key| !key.is_empty()),
            "a facet stops a page at no text at all: {facet:?}"
        );
    }
}

/// A ranked hit carries its document row only where the request projected one,
/// and the row is left out of the bytes where it did not.
#[test]
fn a_hit_carries_a_document_row_only_where_one_was_projected() {
    assert_eq!(
        wire(&Hit::new(path("notes/a.md"), score(0.5))),
        r#"{"path":"notes/a.md","score":0.5}"#
    );
    let hydrated = Hit::new(path("notes/a.md"), score(0.5))
        .with_document(DocumentRow::new(path("notes/a.md")));
    assert_eq!(
        wire(&hydrated),
        r#"{"path":"notes/a.md","score":0.5,"document":{"path":"notes/a.md"}}"#
    );
}

// ── Every setter lands, in bytes ─────────────────────────────────────────

/// The vault address every pinned request below names, written once because
/// six of them name the same one.
const PINNED_VAULT: &str = r##"{"by":"name","name":"notes"}"##;

/// Every predicate the vocabulary holds, as the four verbs that filter by
/// them carry it.
const PINNED_PREDICATES: &str = r##"[{"op":"eq","key":"type","value":"note"},{"op":"not_eq","key":"type","value":"note"},{"op":"in","key":"type","values":["note","task"]},{"op":"has","key":"due"},{"op":"missing","key":"due"},{"op":"before","key":"due","value":"2026-01-01"},{"op":"after","key":"due","value":"2026-01-01"},{"op":"matches","query":"norn NEAR vault"},{"op":"path","glob":"docs/**"},{"op":"links_to","target":"glossary#Design"},{"op":"resolves","target":"norn/glossary"},{"op":"tag","name":"draft"},{"op":"has_finding","kind":"document/undeclared-tag"}]"##;

/// Every column a projection can ask for, as the three verbs that project
/// them carry it.
const PINNED_COLUMNS: &str = r##"[{"col":"path"},{"col":"field","key":"due"},{"col":"body"},{"col":"links"},{"col":"headings"},{"col":"blocks"},{"col":"tags"},{"col":"findings"},{"col":"fields"}]"##;

/// The opaque cursor every pinned request continues from.
const PINNED_AFTER: &str = r##"eyJzbmFwc2hvdCI6eyJlcG9jaCI6ImVwb2NoLTEiLCJnZW5lcmF0aW9uIjoxMiwic2NoZW1hX2ZpbmdlcnByaW50IjoiZnAtMSIsInNpZGVjYXJfcmV2aXNpb24iOjR9LCJrZXkiOnsicm93IjoiZG9jdW1lbnQiLCJzb3J0IjoiMjAyNi0wMS0wMSIsInBhdGgiOiJub3Rlcy9hLm1kIn19"##;

/// **A setter that does nothing is a setter nothing else catches.** A `with_`
/// method that dropped its argument still type-checks, still hands back a
/// value a caller can use, and still survives the round trip — the round trip
/// compares a value to itself, so a request that lost a part equals the
/// request it became. The bytes are what catch it: each test below builds a
/// value through every setter its type has and pins what it serializes to, so
/// a setter that stops landing changes those bytes and fails here.

#[test]
fn every_find_setter_lands_in_the_bytes() {
    let request = FindParams::new(VaultAddress::name(name("notes")))
        .with_predicates(predicates())
        .with_sort(Sort::new(SortKey::field("due"), Direction::Descending))
        .with_columns(columns())
        .with_limit(20)
        .with_after(cursors().remove(0));
    assert_eq!(
        wire(&request),
        [
            r##"{"vault":"##,
            PINNED_VAULT,
            r##","predicates":"##,
            PINNED_PREDICATES,
            r##","sort":{"key":{"by":"field","key":"due"},"direction":"descending"},"columns":"##,
            PINNED_COLUMNS,
            r##","limit":20,"after":""##,
            PINNED_AFTER,
            r##""}"##,
        ]
        .concat()
    );
}

#[test]
fn every_search_setter_lands_in_the_bytes() {
    let request = SearchParams::new(VaultAddress::name(name("notes")), "norn")
        .with_predicates(predicates())
        .with_rungs(RungSet::of(rungs()).expect("a ladder that runs a rung"))
        .with_min_score(score(0.25))
        .with_columns(columns())
        .with_limit(20)
        .with_after(cursors().remove(0));
    assert_eq!(
        wire(&request),
        [
            r##"{"vault":"##,
            PINNED_VAULT,
            r##","query":"norn","predicates":"##,
            PINNED_PREDICATES,
            r##","rungs":{"rungs":["lexical","vector","expansion","rerank"]},"min_score":0.25,"columns":"##,
            PINNED_COLUMNS,
            r##","limit":20,"after":""##,
            PINNED_AFTER,
            r##""}"##,
        ]
        .concat()
    );
}

#[test]
fn every_get_setter_lands_in_the_bytes() {
    let request = GetParams::new(VaultAddress::name(name("notes")), target("glossary#Design"))
        .with_columns(columns())
        .with_collection(CollectionSelector::Links)
        .with_limit(20)
        .with_after(cursors().remove(0));
    assert_eq!(
        wire(&request),
        [
            r##"{"vault":"##,
            PINNED_VAULT,
            r##","target":"glossary#Design","columns":"##,
            PINNED_COLUMNS,
            r##","collection":"links","limit":20,"after":""##,
            PINNED_AFTER,
            r##""}"##,
        ]
        .concat()
    );
}

#[test]
fn every_count_setter_lands_in_the_bytes() {
    let request = CountParams::new(VaultAddress::name(name("notes")))
        .with_predicates(predicates())
        .with_by(group_keys())
        .with_limit(20)
        .with_after(cursors().remove(0));
    assert_eq!(
        wire(&request),
        [
            r##"{"vault":"##,
            PINNED_VAULT,
            r##","predicates":"##,
            PINNED_PREDICATES,
            r##","by":[{"by":"field","key":"type"},{"by":"tag"}],"limit":20,"after":""##,
            PINNED_AFTER,
            r##""}"##,
        ]
        .concat()
    );
}

#[test]
fn every_validate_setter_lands_in_the_bytes() {
    let request = ValidateParams::new(VaultAddress::name(name("notes")))
        .with_predicates(predicates())
        .with_kinds(finding_kinds())
        .with_severity(Severity::Error)
        .summarized()
        .with_limit(20)
        .with_after(cursors().remove(0));
    assert_eq!(
        wire(&request),
        [
            r##"{"vault":"##,
            PINNED_VAULT,
            r##","predicates":"##,
            PINNED_PREDICATES,
            r##","kinds":["document/path-bytes-not-utf8","document/path-names-no-document","document/body-bytes-not-utf8","document/frontmatter-too-large","document/frontmatter-unclosed","document/frontmatter-unreadable","document/undeclared-tag"],"severity":"error","summary":true,"limit":20,"after":""##,
            PINNED_AFTER,
            r##""}"##,
        ]
        .concat()
    );
}

#[test]
fn every_describe_setter_lands_in_the_bytes() {
    let request = DescribeParams::new(VaultAddress::name(name("notes")))
        .with_facets(facet_kinds())
        .with_limit(20)
        .with_after(cursors().remove(0));
    assert_eq!(
        wire(&request),
        [
            r##"{"vault":"##,
            PINNED_VAULT,
            r##","facets":["declared_field","observed_field","declared_tag","folder","path_rule","tag_pattern","undeclared_tags"],"limit":20,"after":""##,
            PINNED_AFTER,
            r##""}"##,
        ]
        .concat()
    );
}

#[test]
fn every_document_row_setter_lands_in_the_bytes() {
    let request = whole_document_row();
    assert_eq!(
        wire(&request),
        [
            r##"{"path":"notes/a.md","fields":{"type":{"kind":"scalar","raw":"note"}},"body":{"text":"Design\n","byte_length":4096},"links":{"items":[{"family":"wikilink","embed":false,"protocol":null,"target":"a","title":"A","anchor":{"kind":"heading","text":"Design"},"span":{"line":3,"column":1,"byte_offset":42},"targets":{"candidates":[],"total":0},"health":"broken"},{"family":"wikilink","embed":false,"protocol":null,"target":"a","title":"A","anchor":{"kind":"heading","text":"Design"},"span":{"line":3,"column":1,"byte_offset":42},"targets":{"candidates":[{"path":"notes/a.md","suffix":"notes/a"}],"total":1},"health":"healthy"},{"family":"wikilink","embed":false,"protocol":null,"target":"a","title":"A","anchor":{"kind":"heading","text":"Design"},"span":{"line":3,"column":1,"byte_offset":42},"targets":{"candidates":[{"path":"notes/a.md","suffix":"notes/a"},{"path":"archive/a.md","suffix":"archive/a"}],"total":2},"health":"ambiguous"}],"total":9},"headings":{"items":[{"level":2,"text":"Design","slug":"design","span":{"line":3,"column":1,"byte_offset":42}}],"total":1},"blocks":{"items":[{"id":"a1","span":null}],"total":1},"tags":{"items":[{"name":"draft","source":"frontmatter","span":{"line":3,"column":1,"byte_offset":42}}],"total":1},"findings":{"items":[{"id":7,"kind":"document/undeclared-tag","severity":"warning","path":"notes/a.md","target":"draft","span":{"line":3,"column":1,"byte_offset":42},"head":{"candidates":[{"path":"notes/glossary.md","suffix":"notes/glossary"},{"path":"archive/glossary.md","suffix":"archive/glossary"}],"total":9},"hint":{"hint":"resolves","target":"glossary"},"message":"the tag is not declared","generation":12}],"total":1}}"##,
        ]
        .concat()
    );
}

// ── The vault namespace and doctor's registry half ───────────────────────

/// Every registration shape: the two fields that have no default, and each of
/// the two that do.
fn registrations() -> Vec<Registration> {
    let base = Registration::new(name("notes"), vault_roots().remove(1));
    vec![
        base.clone(),
        base.clone().with_schema_source(schema_sources().remove(0)),
        base.clone().with_poll_backend(PollBackend::Poll),
        base.with_schema_source(schema_sources().remove(0))
            .with_poll_backend(PollBackend::Poll),
    ]
}

/// The refusal a parked entry publishes.
fn park() -> ErrorEnvelope {
    ErrorEnvelope::new(
        "this vault's entry is parked",
        ErrorDetail::entry_untrusted(UntrustedReason::environmental_refusal("the disk is full")),
    )
}

/// Every published answer an entry carries: a trust state, or the park it is
/// held under.
fn publisheds() -> Vec<Published> {
    let mut publisheds: Vec<Published> = trust_states().into_iter().map(Published::state).collect();
    publisheds.push(Published::parked(park()));
    publisheds
}

/// Every fingerprint pair: a vault with a config file, and one without.
fn fingerprints() -> Vec<Fingerprints> {
    vec![
        Fingerprints::new("schema-1"),
        Fingerprints::new("schema-1").with_config("config-1"),
    ]
}

/// Every drift reading, over every failure an unreadable one carries.
fn drifts() -> Vec<Drift> {
    let mut drifts = vec![Drift::inactive(), Drift::current(), Drift::reload_pending()];
    drifts.extend(control_file_failures().into_iter().map(Drift::unreadable));
    drifts
}

/// Every engine status, over every freshness a standing one reports.
fn engine_statuses() -> Vec<EngineStatus> {
    let mut statuses = vec![
        EngineStatus::off(),
        EngineStatus::on(None, None),
        EngineStatus::on(Some("the drain refused".to_string()), None),
        EngineStatus::self_disabled("the slot took itself out of service"),
    ];
    statuses.extend(
        freshnesses()
            .into_iter()
            .map(|freshness| EngineStatus::on(None, Some(freshness))),
    );
    statuses
}

/// Every advisory the vocabulary holds.
fn advisories() -> Vec<Advisory> {
    vec![
        Advisory::tmp_fallback_in_use("/home/person/notes/.norn/tmp", true),
        Advisory::tmp_fallback_in_use("/home/person/notes/.norn/tmp", false),
        Advisory::symlink_skipped("/home/person/notes/elsewhere"),
    ]
}

/// Every attention reason a roll-up carries.
fn attentions() -> Vec<Attention> {
    let mut attentions: Vec<Attention> = untrusted_reasons()
        .into_iter()
        .map(|reason| Attention::untrusted(name("notes"), reason))
        .collect();
    attentions.extend(
        reason_codes()
            .into_iter()
            .map(|code| Attention::parked(name("notes"), code)),
    );
    attentions.push(Attention::reads_refusing(
        name("notes"),
        "this coverage mints no read handle",
    ));
    attentions.extend(
        control_file_failures()
            .into_iter()
            .map(|failure| Attention::reload_failed(name("notes"), failure)),
    );
    attentions.push(Attention::reload_pending(name("notes")));
    attentions.push(Attention::engine_self_disabled(
        name("notes"),
        "the slot took itself out of service",
    ));
    attentions.extend(
        advisories()
            .into_iter()
            .map(|advisory| Attention::advisory(name("notes"), advisory)),
    );
    attentions
}

/// One vault's standing, at every published answer, drift and engine status
/// there is, with and without each optional part.
fn vault_statuses() -> Vec<VaultStatus> {
    let mut statuses = Vec::new();
    for published in publisheds() {
        statuses.push(VaultStatus::new(
            registrations().remove(0),
            published,
            Drift::current(),
            EngineStatus::off(),
            EngineSection::absent(),
        ));
    }
    for drift in drifts() {
        statuses.push(VaultStatus::new(
            registrations().remove(0),
            Published::state(TrustState::Ready),
            drift,
            EngineStatus::off(),
            EngineSection::absent(),
        ));
    }
    for engine in engine_statuses() {
        for section in engine_sections() {
            statuses.push(VaultStatus::new(
                registrations().remove(0),
                Published::state(TrustState::Ready),
                Drift::current(),
                engine.clone(),
                section,
            ));
        }
    }
    for failure in control_file_failures() {
        statuses.push(
            VaultStatus::new(
                registrations().remove(0),
                Published::state(TrustState::Ready),
                Drift::current(),
                EngineStatus::off(),
                EngineSection::absent(),
            )
            .with_fingerprints(fingerprints().remove(1))
            .with_last_reload_failure(failure)
            .with_reads_refusing("this coverage mints no read handle")
            .with_advisories(advisories()),
        );
    }
    statuses
}

/// Every resolution a directory reaches.
fn resolve_reports() -> Vec<ResolveReport> {
    let mut reports: Vec<ResolveReport> = registrations()
        .into_iter()
        .map(ResolveReport::registered)
        .collect();
    reports.push(ResolveReport::none());
    reports
}

/// Both shapes a status answer takes.
fn status_reports() -> Vec<StatusReport> {
    let mut reports: Vec<StatusReport> = vault_statuses()
        .into_iter()
        .map(StatusReport::vault)
        .collect();
    reports.push(StatusReport::roll_up(RollUp::of(&[])));
    reports.push(StatusReport::roll_up(RollUp::of(&vault_statuses())));
    reports
}

/// Every problem the registry itself carries.
fn registry_problems() -> Vec<RegistryProblem> {
    vec![
        RegistryProblem::duplicate_root([name("notes"), name("vault")]),
        RegistryProblem::root_unreadable(name("notes"), "the directory cannot be read"),
        RegistryProblem::root_missing(name("notes")),
    ]
}

/// Both readings of the registry's own sanity.
fn registry_sanities() -> Vec<RegistrySanity> {
    vec![
        RegistrySanity::sound(),
        RegistrySanity::problems(registry_problems()).expect("problems that name one"),
    ]
}

/// Every part of the control files a reload applies.
fn reload_outcomes() -> Vec<ReloadOutcome> {
    vec![ReloadOutcome::ConfigOnly, ReloadOutcome::SchemaChanged]
}

/// Every change an edit does to a field with a default.
fn changes() -> Vec<Change<SchemaSource>> {
    vec![
        Change::keep(),
        Change::set(schema_sources().remove(0)),
        Change::clear(),
    ]
}

/// Every change an edit does to a field with no default.
fn replacements() -> Vec<Replace<VaultRoot>> {
    vec![Replace::keep(), Replace::set(vault_roots().remove(1))]
}

/// Every shape the vault namespace and doctor's registry half carry survives
/// the round trip.
#[test]
fn every_vault_namespace_shape_survives_the_round_trip() {
    for registration in registrations() {
        round_trip(&registration);
    }
    for published in publisheds() {
        round_trip(&published);
    }
    for pair in fingerprints() {
        round_trip(&pair);
    }
    for drift in drifts() {
        round_trip(&drift);
    }
    for engine in engine_statuses() {
        round_trip(&engine);
    }
    for advisory in advisories() {
        round_trip(&advisory);
    }
    for attention in attentions() {
        round_trip(&attention);
    }
    for failure in control_file_failures() {
        round_trip(&failure);
    }
    for directory in directories() {
        round_trip(&directory);
    }
    for status in vault_statuses() {
        round_trip(&status);
    }
    round_trip(&RollUp::of(&vault_statuses()));
    for report in resolve_reports() {
        round_trip(&report);
    }
    for report in status_reports() {
        round_trip(&report);
    }
    for problem in registry_problems() {
        round_trip(&problem);
    }
    for sanity in registry_sanities() {
        round_trip(&sanity);
    }
    for outcome in reload_outcomes() {
        round_trip(&outcome);
    }
    for change in changes() {
        round_trip(&change);
    }
    for replacement in replacements() {
        round_trip(&replacement);
    }
    round_trip(&EngineHealth::new(
        name("notes"),
        EngineSection::enabled(),
        EngineStatus::on(None, None),
    ));
}

/// Every params and report type the eight verbs are spelled by survives the
/// round trip.
#[test]
fn every_vault_namespace_params_and_report_shape_survives_the_round_trip() {
    round_trip(&RegisterParams::new(registrations().remove(3)));
    round_trip(&RegisterReport::new(
        registrations().remove(3),
        Published::state(TrustState::Ready),
    ));
    round_trip(&UnregisterParams::new(name("notes")));
    round_trip(&UnregisterParams::new(name("notes")).keeping_state());
    round_trip(&UnregisterReport::new(name("notes"), true));
    round_trip(&ListParams::new());
    round_trip(&ListReport::new([]));
    round_trip(&ListReport::new(registrations()));
    round_trip(&SetParams::new(name("notes")));
    round_trip(
        &SetParams::new(name("notes"))
            .with_root(replacements().remove(1))
            .with_schema_source(changes().remove(2))
            .with_poll_backend(Change::set(PollBackend::Poll)),
    );
    round_trip(&SetReport::new(
        registrations().remove(0),
        Published::parked(park()),
    ));
    for directory in directories() {
        round_trip(&ResolveParams::new(directory));
    }
    round_trip(&StatusParams::new());
    for address in vault_addresses() {
        round_trip(&StatusParams::new().with_vault(address.clone()));
        round_trip(&ReloadParams::new(address.clone()));
        round_trip(&ReloadParams::new(address).dry_run());
    }
    for outcome in reload_outcomes() {
        round_trip(&ReloadReport::new(outcome, fingerprints().remove(1)));
        round_trip(&ReloadReport::new(outcome, fingerprints().remove(0)).validated());
    }
    round_trip(&DoctorRegistryParams::new());
    for sanity in registry_sanities() {
        round_trip(&DoctorRegistryReport::new(
            RollUp::of(&vault_statuses()),
            sanity,
            [EngineHealth::new(
                name("notes"),
                EngineSection::enabled(),
                EngineStatus::on(None, None),
            )],
        ));
    }
}

/// A registration crosses as the four fields the registry holds, and a field
/// it does not carry is `null` rather than absent.
#[test]
fn a_registration_is_the_four_fields_the_registry_holds() {
    assert_eq!(
        wire(&registrations().remove(0)),
        r#"{"name":"notes","root":"/home/person/notes","schema_source":null,"poll_backend":null}"#
    );
    assert_eq!(
        wire(&registrations().remove(3)),
        r#"{"name":"notes","root":"/home/person/notes","schema_source":"/home/person/.config/norn/schemas/work.yaml","poll_backend":"poll"}"#
    );
}

/// What an entry publishes is one of two answers, and the park is carried
/// whole: a client reads the park's own code off it rather than being handed a
/// trust state that hides it.
#[test]
fn a_published_answer_is_the_demand_an_entry_answers_with() {
    assert_eq!(
        wire(&Published::state(TrustState::Ready)),
        r#"{"answer":"state","state":{"state":"ready"}}"#
    );
    assert_eq!(
        Published::of(Ok(TrustState::Ready)),
        Published::state(TrustState::Ready)
    );
    assert_eq!(Published::of(Err(park())), Published::parked(park()));
    assert_eq!(
        tag_string(&Published::of(Err(park())), "answer"),
        "parked",
        "a parked entry published a state"
    );
    assert!(
        wire(&Published::parked(park())).contains(r#""code":"host/entry-untrusted""#),
        "a park did not report its own code"
    );
}

/// The fingerprints a vault serves under, with the missing-file default
/// spelled as no config fingerprint at all.
#[test]
fn fingerprints_report_the_control_files_a_vault_serves() {
    assert_eq!(
        wire(&Fingerprints::new("schema-1")),
        r#"{"schema":"schema-1","config":null}"#
    );
    assert_eq!(
        wire(&Fingerprints::new("schema-1").with_config("config-1")),
        r#"{"schema":"schema-1","config":"config-1"}"#
    );
}

/// The three drift readings that carry nothing are objects tagged `state`
/// like the one that carries a failure, so a reader takes all four the same
/// way.
#[test]
fn a_drift_is_an_object_tagged_state() {
    assert_eq!(wire(&Drift::inactive()), r#"{"state":"inactive"}"#);
    assert_eq!(wire(&Drift::current()), r#"{"state":"current"}"#);
    assert_eq!(
        wire(&Drift::reload_pending()),
        r#"{"state":"reload_pending"}"#
    );
    assert_eq!(
        wire(&Drift::unreadable(ControlFileFailure::new(
            ControlFile::Schema,
            ReloadStage::Read,
            "the vault schema cannot be read"
        ))),
        concat!(
            r#"{"state":"unreadable","failure":{"file":"schema","stage":"read","#,
            r#""detail":"the vault schema cannot be read"}}"#
        )
    );
}

/// A standing engine reports both of the parts it has, and each is `null`
/// until there is one: nothing reports a watermark until the engine seams
/// land.
#[test]
fn an_engine_status_carries_what_the_slot_is_doing() {
    assert_eq!(wire(&EngineStatus::off()), r#"{"state":"off"}"#);
    assert_eq!(
        wire(&EngineStatus::on(None, None)),
        r#"{"state":"on","last_drain_error":null,"freshness":null}"#
    );
    assert_eq!(
        wire(&EngineStatus::on(
            Some("the drain refused".to_string()),
            Some(Freshness::trailing(3))
        )),
        r#"{"state":"on","last_drain_error":"the drain refused","freshness":{"state":"trailing","generations":3}}"#
    );
    assert_eq!(
        wire(&EngineStatus::self_disabled("out of service")),
        r#"{"state":"self_disabled","detail":"out of service"}"#
    );
}

/// The shadow-home advisory carries the directory and whether the vault
/// ignores it, which is the pair an operator acts on.
#[test]
fn an_advisory_is_an_object_tagged_kind() {
    assert_eq!(
        wire(&Advisory::tmp_fallback_in_use(
            "/home/person/notes/.norn/tmp",
            true
        )),
        r#"{"kind":"tmp_fallback_in_use","path":"/home/person/notes/.norn/tmp","gitignored":true}"#
    );
    assert_eq!(
        wire(&Advisory::symlink_skipped("/home/person/notes/elsewhere")),
        r#"{"kind":"symlink_skipped","path":"/home/person/notes/elsewhere"}"#
    );
}

/// The five counts partition the vaults counted, so they sum to `vaults`
/// whatever the statuses are. A roll-up cannot be handed in disagreeing with
/// itself, because it is derived from the statuses it rolls up.
#[test]
fn a_roll_ups_counts_sum_to_the_vaults_it_counted() {
    let statuses = vault_statuses();
    assert!(statuses.len() > 5, "the census counts too few vaults");
    let roll_up = RollUp::of(&statuses);
    assert_eq!(roll_up.vaults(), statuses.len() as u64);
    assert_eq!(
        roll_up.ready()
            + roll_up.warming()
            + roll_up.untrusted()
            + roll_up.parked()
            + roll_up.unattached(),
        roll_up.vaults(),
        "the counts do not partition the vaults counted"
    );
    assert_eq!(RollUp::of(&[]).vaults(), 0);
    assert!(RollUp::of(&[]).attention().is_empty());
}

/// Each published answer counts once and in the count it belongs to: a park is
/// parked whatever its derived state is doing underneath.
#[test]
fn a_roll_up_counts_each_entry_under_what_it_publishes() {
    let status = |published: Published| {
        VaultStatus::new(
            registrations().remove(0),
            published,
            Drift::current(),
            EngineStatus::off(),
            EngineSection::absent(),
        )
    };
    let roll_up = RollUp::of(&[
        status(Published::state(TrustState::Ready)),
        status(Published::state(TrustState::Unattached)),
        status(Published::state(TrustState::warming(
            WarmingPhase::Healing,
            0,
            None,
        ))),
        status(Published::state(TrustState::untrusted(
            UntrustedReason::WatcherOverflow,
        ))),
        status(Published::parked(park())),
    ]);
    assert_eq!(roll_up.vaults(), 5);
    assert_eq!(roll_up.ready(), 1);
    assert_eq!(roll_up.unattached(), 1);
    assert_eq!(roll_up.warming(), 1);
    assert_eq!(roll_up.untrusted(), 1);
    assert_eq!(roll_up.parked(), 1);
}

/// A roll-up names what wants attention off the statuses themselves, so a
/// vault wanting two things is named twice and a vault wanting nothing is
/// named not at all.
#[test]
fn a_roll_up_names_what_each_status_wants_attention_for() {
    let sound = VaultStatus::new(
        registrations().remove(0),
        Published::state(TrustState::Ready),
        Drift::current(),
        EngineStatus::off(),
        EngineSection::absent(),
    );
    assert!(RollUp::of(&[sound]).attention().is_empty());

    let wanting = VaultStatus::new(
        registrations().remove(0),
        Published::parked(park()),
        Drift::reload_pending(),
        EngineStatus::self_disabled("out of service"),
        EngineSection::enabled(),
    )
    .with_last_reload_failure(ControlFileFailure::new(
        ControlFile::Config,
        ReloadStage::Read,
        "the vault config cannot be read",
    ))
    .with_reads_refusing("this coverage mints no read handle")
    .with_advisories([Advisory::symlink_skipped("/home/person/notes/elsewhere")]);
    assert_eq!(
        RollUp::of(&[wanting]).attention(),
        [
            Attention::parked(name("notes"), ReasonCode::HostEntryUntrusted),
            Attention::reads_refusing(name("notes"), "this coverage mints no read handle"),
            Attention::reload_failed(
                name("notes"),
                ControlFileFailure::new(
                    ControlFile::Config,
                    ReloadStage::Read,
                    "the vault config cannot be read",
                )
            ),
            Attention::reload_pending(name("notes")),
            Attention::engine_self_disabled(name("notes"), "out of service"),
            Attention::advisory(
                name("notes"),
                Advisory::symlink_skipped("/home/person/notes/elsewhere")
            ),
        ]
    );
    let untrusted = VaultStatus::new(
        registrations().remove(0),
        Published::state(TrustState::untrusted(UntrustedReason::WatcherOverflow)),
        Drift::current(),
        EngineStatus::off(),
        EngineSection::absent(),
    );
    assert_eq!(
        RollUp::of(&[untrusted]).attention(),
        [Attention::untrusted(
            name("notes"),
            UntrustedReason::WatcherOverflow
        )]
    );
}

/// The nameless status answer is the roll-up alone: the counts and the typed
/// attention reasons. Where one entry stands is what naming that vault
/// reports, and nothing else carries a per-vault list.
#[test]
fn the_nameless_status_answer_carries_the_roll_up_alone() {
    let vaults = vault_statuses();
    let StatusReport::RollUp { roll_up, .. } = StatusReport::roll_up(RollUp::of(&vaults)) else {
        panic!("a roll-up report");
    };
    assert_eq!(roll_up, RollUp::of(&vaults));
    let rendered =
        serde_json::to_value(StatusReport::roll_up(RollUp::of(&vaults))).expect("a report as JSON");
    let members: BTreeSet<&str> = rendered
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        members,
        BTreeSet::from(["shape", "roll_up"]),
        "the nameless answer carries something beside the roll-up"
    );
}

/// An edit tells keeping a field apart from clearing it, which a value alone
/// cannot: both are spelled by the absence of a value.
#[test]
fn a_change_tells_keeping_a_field_from_clearing_it() {
    assert_eq!(
        wire(&Change::<SchemaSource>::keep()),
        r#"{"change":"keep"}"#
    );
    assert_eq!(
        wire(&Change::set(schema_sources().remove(0))),
        r#"{"change":"set","value":"/home/person/.config/norn/schemas/work.yaml"}"#
    );
    assert_eq!(
        wire(&Change::<SchemaSource>::clear()),
        r#"{"change":"clear"}"#
    );
    assert_eq!(Change::<SchemaSource>::default(), Change::keep());
    assert_eq!(Replace::<VaultRoot>::default(), Replace::keep());
}

/// A registration cannot be without a root, so the root's edit has no clear to
/// spell and a reader refuses one rather than a handler rejecting it later.
#[test]
fn a_root_cannot_be_cleared() {
    assert_eq!(
        serde_json::from_str::<Replace<VaultRoot>>(r#"{"change":"keep"}"#)
            .expect("keeping the root"),
        Replace::keep()
    );
    assert!(
        serde_json::from_str::<Replace<VaultRoot>>(r#"{"change":"clear"}"#).is_err(),
        "a cleared root read back as a replacement"
    );
    assert!(
        serde_json::from_str::<Change<SchemaSource>>(r#"{"change":"clear"}"#).is_ok(),
        "a cleared schema source is what returns a vault to the in-vault default"
    );
}

/// A resolution that reaches no registration is an outcome of its own rather
/// than a refusal.
#[test]
fn a_resolve_report_is_an_object_tagged_outcome() {
    assert_eq!(wire(&ResolveReport::none()), r#"{"outcome":"none"}"#);
    assert_eq!(
        wire(&ResolveReport::registered(registrations().remove(0))),
        r#"{"outcome":"registered","registration":{"name":"notes","root":"/home/person/notes","schema_source":null,"poll_backend":null}}"#
    );
}

/// Each params type is built by naming what a request cannot be built without,
/// and every other part has a stated default.
#[test]
fn a_vault_params_constructor_takes_the_required_parts_and_defaults_the_rest() {
    assert!(!UnregisterParams::new(name("notes")).keep_state);
    assert!(
        UnregisterParams::new(name("notes"))
            .keeping_state()
            .keep_state
    );
    assert_eq!(StatusParams::new().vault, None);
    assert_eq!(
        StatusParams::new()
            .with_vault(VaultAddress::name(name("notes")))
            .vault,
        Some(VaultAddress::name(name("notes")))
    );
    let reload = || ReloadParams::new(VaultAddress::name(name("notes")));
    assert!(!reload().dry_run);
    assert!(reload().dry_run().dry_run);
    let set = SetParams::new(name("notes"));
    assert_eq!(set.root, Replace::keep());
    assert_eq!(set.schema_source, Change::keep());
    assert_eq!(set.poll_backend, Change::keep());
    let registration = Registration::new(name("notes"), vault_roots().remove(1));
    assert_eq!(registration.schema_source, None);
    assert_eq!(registration.poll_backend, None);
    assert!(ListReport::new([]).registrations.is_empty());
}

/// A dry run reports that it activated nothing, which is the one thing that
/// tells its report from a reload's.
#[test]
fn a_dry_run_reports_that_it_activated_nothing() {
    assert_eq!(
        wire(&ReloadReport::new(
            ReloadOutcome::ConfigOnly,
            fingerprints().remove(1)
        )),
        r#"{"outcome":"config_only","fingerprints":{"schema":"schema-1","config":"config-1"},"activated":true}"#
    );
    assert_eq!(
        wire(
            &ReloadReport::new(ReloadOutcome::SchemaChanged, fingerprints().remove(0)).validated()
        ),
        r#"{"outcome":"schema_changed","fingerprints":{"schema":"schema-1","config":null},"activated":false}"#
    );
}

#[test]
fn every_register_setter_lands_in_the_bytes() {
    assert_eq!(
        wire(&RegisterParams::new(registrations().remove(3))),
        r#"{"registration":{"name":"notes","root":"/home/person/notes","schema_source":"/home/person/.config/norn/schemas/work.yaml","poll_backend":"poll"}}"#
    );
}

#[test]
fn every_unregister_setter_lands_in_the_bytes() {
    assert_eq!(
        wire(&UnregisterParams::new(name("notes")).keeping_state()),
        r#"{"name":"notes","keep_state":true}"#
    );
}

#[test]
fn every_set_setter_lands_in_the_bytes() {
    let request = SetParams::new(name("notes"))
        .with_root(replacements().remove(1))
        .with_schema_source(Change::clear())
        .with_poll_backend(Change::set(PollBackend::Poll));
    assert_eq!(
        wire(&request),
        r#"{"name":"notes","root":{"change":"set","value":"/home/person/notes"},"schema_source":{"change":"clear"},"poll_backend":{"change":"set","value":"poll"}}"#
    );
}

#[test]
fn every_status_and_reload_setter_lands_in_the_bytes() {
    assert_eq!(wire(&StatusParams::new()), r#"{"vault":null}"#);
    assert_eq!(
        wire(&StatusParams::new().with_vault(VaultAddress::name(name("notes")))),
        r#"{"vault":{"by":"name","name":"notes"}}"#
    );
    assert_eq!(
        wire(&ReloadParams::new(VaultAddress::name(name("notes"))).dry_run()),
        r#"{"vault":{"by":"name","name":"notes"},"dry_run":true}"#
    );
}

/// The two verbs that ask for nothing carry a params type all the same, and it
/// is an empty object on the wire.
#[test]
fn a_verb_that_asks_for_nothing_carries_an_empty_params_object() {
    assert_eq!(wire(&ListParams::new()), "{}");
    assert_eq!(wire(&DoctorRegistryParams::new()), "{}");
    assert_eq!(ListParams::default(), ListParams::new());
    assert_eq!(DoctorRegistryParams::default(), DoctorRegistryParams::new());
}

/// A roll-up is derived on the writing side, so a roll-up that arrives over
/// the wire is checked on the reading side: five counts that do not sum to the
/// vaults it says it counted describe no installation, and the message names
/// the counts that did not add up. The check is the type's own, so it holds
/// wherever a roll-up is read from — inside a status answer and inside
/// `doctor`'s registry reading alike.
#[test]
fn a_roll_up_whose_counts_do_not_sum_is_refused_on_read() {
    let summing = r#"{"vaults":3,"ready":1,"warming":1,"untrusted":0,"parked":1,"unattached":0,"attention":[]}"#;
    let not_summing = r#"{"vaults":3,"ready":1,"warming":0,"untrusted":0,"parked":1,"unattached":0,"attention":[]}"#;

    let read = serde_json::from_str::<RollUp>(summing).expect("a summing roll-up");
    assert_eq!(read.vaults(), 3);
    assert_eq!(read.ready(), 1);

    let refusal = serde_json::from_str::<RollUp>(not_summing)
        .expect_err("a roll-up whose counts do not sum")
        .to_string();
    for named in [
        "3",
        "2",
        "ready",
        "warming",
        "untrusted",
        "parked",
        "unattached",
    ] {
        assert!(
            refusal.contains(named),
            "the refusal `{refusal}` does not name `{named}`"
        );
    }

    let inside_a_status = format!(r#"{{"shape":"roll_up","roll_up":{not_summing}}}"#);
    assert!(
        serde_json::from_str::<StatusReport>(&inside_a_status).is_err(),
        "a status answer read back a roll-up that disagrees with itself"
    );
    let inside_a_reading =
        format!(r#"{{"roll_up":{not_summing},"registry":{{"state":"sound"}},"engines":[]}}"#);
    assert!(
        serde_json::from_str::<DoctorRegistryReport>(&inside_a_reading).is_err(),
        "a registry reading read back a roll-up that disagrees with itself"
    );
    let sound_reading =
        format!(r#"{{"roll_up":{summing},"registry":{{"state":"sound"}},"engines":[]}}"#);
    serde_json::from_str::<DoctorRegistryReport>(&sound_reading)
        .expect("a registry reading over a summing roll-up");
}

/// The sum a roll-up is checked against is itself computed from counts a
/// forger chose, so the addition is checked rather than taken: counts that run
/// past what a count holds refuse the read and name themselves, rather than
/// wrapping into a total that agrees with `vaults` or aborting the read with a
/// panic from inside the deserializer.
#[test]
fn a_roll_up_whose_counts_sum_past_a_count_is_refused_on_read() {
    let overflowing = r#"{"vaults":0,"ready":18446744073709551615,"warming":1,"untrusted":0,"parked":0,"unattached":0,"attention":[]}"#;

    let refusal = serde_json::from_str::<RollUp>(overflowing)
        .expect_err("a roll-up whose counts sum past a count")
        .to_string();
    for named in [
        "18446744073709551615",
        "ready",
        "warming",
        "untrusted",
        "parked",
        "unattached",
    ] {
        assert!(
            refusal.contains(named),
            "the refusal `{refusal}` does not name `{named}`"
        );
    }

    let inside_a_status = format!(r#"{{"shape":"roll_up","roll_up":{overflowing}}}"#);
    assert!(
        serde_json::from_str::<StatusReport>(&inside_a_status).is_err(),
        "a status answer read back a roll-up whose counts sum past a count"
    );
}

/// A listing crosses as the registrations it holds, in name order and each
/// whole: a constructor that dropped the registrations, or handed them back in
/// the order it was given them, does not produce these bytes.
#[test]
fn a_listing_pins_the_registrations_it_holds() {
    let notes = Registration::new(name("notes"), vault_roots().remove(1))
        .with_schema_source(schema_sources().remove(0))
        .with_poll_backend(PollBackend::Poll);
    let archive = Registration::new(name("archive"), vault_roots().remove(3));
    assert_eq!(
        wire(&ListReport::new([notes, archive])),
        concat!(
            r#"{"registrations":["#,
            r#"{"name":"archive","root":"/tmp/a.b","schema_source":null,"poll_backend":null},"#,
            r#"{"name":"notes","root":"/home/person/notes","#,
            r#""schema_source":"/home/person/.config/norn/schemas/work.yaml","#,
            r#""poll_backend":"poll"}"#,
            r#"]}"#
        )
    );
}

/// A duplicate-root problem crosses as every alias that reaches the root, in
/// name order: a constructor that dropped the aliases, or left them in the
/// order it was given them, does not produce these bytes.
#[test]
fn a_duplicate_root_problem_pins_the_aliases_that_reach_it() {
    assert_eq!(
        wire(&RegistryProblem::duplicate_root([
            name("vault"),
            name("notes")
        ])),
        r#"{"problem":"duplicate_root","aliases":["notes","vault"]}"#
    );
    assert_eq!(
        RegistryProblem::duplicate_root([name("notes"), name("notes")]),
        RegistryProblem::duplicate_root([name("notes")]),
        "an alias named twice is carried twice"
    );
}

/// The registry reading crosses with its problems and its engines whole, the
/// engines in name order: a constructor that dropped either collection, or
/// left the engines in the order it was given them, does not produce these
/// bytes.
#[test]
fn a_doctor_registry_reading_pins_its_problems_and_its_engines() {
    let report = DoctorRegistryReport::new(
        RollUp::of(&[]),
        RegistrySanity::problems([RegistryProblem::duplicate_root([
            name("vault"),
            name("notes"),
        ])])
        .expect("problems that name one"),
        [
            EngineHealth::new(
                name("vault"),
                EngineSection::absent(),
                EngineStatus::self_disabled("out of service"),
            ),
            EngineHealth::new(
                name("notes"),
                EngineSection::enabled(),
                EngineStatus::on(None, None),
            ),
        ],
    );
    assert_eq!(
        wire(&report),
        concat!(
            r#"{"roll_up":{"vaults":0,"ready":0,"warming":0,"untrusted":0,"parked":0,"#,
            r#""unattached":0,"attention":[]},"#,
            r#""registry":{"state":"problems","problems":["#,
            r#"{"problem":"duplicate_root","aliases":["notes","vault"]}]},"#,
            r#""engines":["#,
            r#"{"name":"notes","section":{"state":"enabled"},"#,
            r#""engine":{"state":"on","last_drain_error":null,"freshness":null}},"#,
            r#"{"name":"vault","section":{"state":"absent"},"#,
            r#""engine":{"state":"self_disabled","detail":"out of service"}}"#,
            r#"]}"#
        )
    );
}

/// A status with every optional part filled in crosses with all of them: a
/// setter that dropped its assignment does not produce these bytes.
#[test]
fn a_fully_set_vault_status_pins_every_setter() {
    let status = VaultStatus::new(
        Registration::new(name("notes"), vault_roots().remove(1)),
        Published::state(TrustState::Ready),
        Drift::reload_pending(),
        EngineStatus::off(),
        EngineSection::enabled(),
    )
    .with_fingerprints(Fingerprints::new("schema-1").with_config("config-1"))
    .with_last_reload_failure(ControlFileFailure::new(
        ControlFile::Config,
        ReloadStage::Apply,
        "the vault config cannot be applied",
    ))
    .with_reads_refusing("this coverage mints no read handle")
    .with_advisories([Advisory::symlink_skipped("/home/person/notes/elsewhere")]);
    assert_eq!(
        wire(&status),
        concat!(
            r#"{"registration":{"name":"notes","root":"/home/person/notes","#,
            r#""schema_source":null,"poll_backend":null},"#,
            r#""published":{"answer":"state","state":{"state":"ready"}},"#,
            r#""fingerprints":{"schema":"schema-1","config":"config-1"},"#,
            r#""drift":{"state":"reload_pending"},"#,
            r#""last_reload_failure":{"file":"config","stage":"apply","#,
            r#""detail":"the vault config cannot be applied"},"#,
            r#""reads_refusing":"this coverage mints no read handle","#,
            r#""engine":{"state":"off"},"#,
            r#""section":{"state":"enabled"},"#,
            r#""advisories":[{"kind":"symlink_skipped","#,
            r#""path":"/home/person/notes/elsewhere"}]}"#
        )
    );
}

/// A sound registry is a shape of its own, so a problems reading that names no
/// problem is a second spelling of `sound` and is refused at both doors: where
/// the reading is built and where one arrives over the wire.
#[test]
fn a_registry_sanity_naming_no_problem_is_refused_at_both_doors() {
    assert_eq!(RegistrySanity::problems([]), Err(NoProblems));

    let refusal = serde_json::from_str::<RegistrySanity>(r#"{"state":"problems","problems":[]}"#)
        .expect_err("a problems reading naming no problem")
        .to_string();
    assert!(
        refusal.contains(&NoProblems.to_string()),
        "the refusal `{refusal}` does not carry the reason the list names no reading"
    );

    serde_json::from_str::<RegistrySanity>(
        r#"{"state":"problems","problems":[{"problem":"root_missing","name":"notes"}]}"#,
    )
    .expect("a problems reading naming one problem");

    let inside_a_reading = concat!(
        r#"{"roll_up":{"vaults":0,"ready":0,"warming":0,"untrusted":0,"parked":0,"#,
        r#""unattached":0,"attention":[]},"#,
        r#""registry":{"state":"problems","problems":[]},"engines":[]}"#
    );
    assert!(
        serde_json::from_str::<DoctorRegistryReport>(inside_a_reading).is_err(),
        "a registry reading read back a problems reading that names no problem"
    );
}

/// A sound registry is a shape of its own rather than an empty problem list.
#[test]
fn a_registry_sanity_is_an_object_tagged_state() {
    assert_eq!(wire(&RegistrySanity::sound()), r#"{"state":"sound"}"#);
    assert_eq!(
        wire(
            &RegistrySanity::problems([RegistryProblem::root_missing(name("notes"))])
                .expect("problems that name one")
        ),
        r#"{"state":"problems","problems":[{"problem":"root_missing","name":"notes"}]}"#
    );
}
