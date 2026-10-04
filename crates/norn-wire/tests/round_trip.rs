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
    Addressing, Advisory, AmbiguousEnd, Anchor, AnswerAdvisory, AnswerReading, AnswerShape,
    AppliedTarget, ApplyMode, ApplyParams, ApplyReport, AttachMode, Attention, AuthorCondition,
    AuthoredPlan, AuthoredValue, Backlinks, BlockRow, BodyText, CANDIDATE_HEAD, Candidate,
    CandidateHead, ChangesetOutcome, Collection, CollectionPage, CollectionSelector, Column,
    ComparedBy, ContainerKind, ContentHash, ControlFile, ControlFileFailure, CountParams, Cursor,
    CursorKey, CursorOrderChanged, DeleteParams, DescribeParams, Direction, Directory,
    DoctorRegistryParams, DoctorRegistryReport, DocumentEdit, DocumentPath, DocumentRow, Drift,
    EditParams, ElsewhereNamesDocuments, EngineHealth, EngineSection, EngineStatus, ErrorDetail,
    ErrorEnvelope, ExpectedField, Facet, FacetKind, FieldChange, FieldType, FieldValue, FilePath,
    FileState, FindParams, FindingKind, FindingRow, FindingScope, Fingerprints, FolderPath,
    Forecast, Freshness, GetParams, GetReport, GroupKey, HeadingRow, Hint, Hit, IllegalContentHash,
    IllegalOperationId, InitParams, InitReport, InterruptionCause, KindTally, LadderDeclaration,
    LinkAddress, LinkAdvisory, LinkFamily, LinkHealth, LinkKey, LinkRewrite, LinkRow, ListParams,
    ListReport, MaintainerIdentity, MalformedLadder, MigrateParams, MigrateReport,
    MigrationRefusal, ModelIdentity, MoveParams, MoveSubject, Moved, NameSet, NewParams,
    NewSubject, NoProblems, NoRetrievalRung, NonFiniteScore, NotReady, Operation, OperationId,
    OperationKind, OperationsTag, Page, PagedRows, PathProblem, PathRuleKind, PlanCondition,
    PlanDocument, PlanFault, PollBackend, Predicate, Provenance, Published, ReadFailure,
    ReasonCode, RefusedCheck, RegisterParams, RegisterReport, Registration, RegistryProblem,
    RegistrySanity, ReloadFailure, ReloadOutcome, ReloadParams, ReloadReport, ReloadStage,
    RequestBound, RequestPart, RequestScope, ResolutionTarget, ResolveParams, ResolveReport,
    ResolvedPlan, ResolvedTag, Resolves, RewriteWikilinkParams, RollUp, RootIdentity, Rung,
    RungReport, RungSelection, RungSet, RungSkipReason, SchemaSource, SchemaViolation, Score,
    SearchParams, SearchReport, SetParams, Severity, SidecarRevision, SkippedFinding, Snapshot,
    Sort, SortKey, Span, StatusParams, StatusReport, TagRow, TagSource, TagStance, Tally,
    TargetResult, TotalBelowHead, Transition, TrustState, UnknownAddressing, UnknownFindingKind,
    UnknownPollBackend, UnknownRequestScope, UnknownSeverity, UnknownVerb, UnregisterParams,
    UnregisterReport, UnresolvedOperation, UnresolvedReason, Unsatisfied, UntrustedReason,
    ValidateParams, ValidateReport, ValueMap, Variables, VaultAddress, VaultAnswer, VaultChange,
    VaultName, VaultReplace, VaultRoot, VaultSetParams, VaultSetReport, VaultStatus, Verb,
    WarmingPhase, WatcherLossCause, WriteTarget, is_refused_character, is_refused_segment,
    leaf_stem,
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
        ReasonCode::HostSharedSchema,
        ReasonCode::HostEntryUntrusted,
        ReasonCode::HostMaintainerContended,
        ReasonCode::HostUnknownVault,
        ReasonCode::HostUnsupportedAttachMode,
        ReasonCode::HostAlreadyServed,
        ReasonCode::HostEntryHeld,
        ReasonCode::HostEntryNotReady,
        ReasonCode::HostReaderUnavailable,
        ReasonCode::HostRegistryUnwritable,
        ReasonCode::HostReadFailed,
        ReasonCode::HostApplyNotRun,
        ReasonCode::HostApplyOutcomeUnknown,
        ReasonCode::VaultAmbiguousRoot,
        ReasonCode::VaultAmbiguousTarget,
        ReasonCode::VaultUnknownTarget,
        ReasonCode::VaultReloadBusy,
        ReasonCode::VaultReloadFailed,
        ReasonCode::VaultCursorOrderChanged,
        ReasonCode::VaultUnreadableBound,
        ReasonCode::VaultPlanRefused,
        ReasonCode::VaultRootChanged,
        ReasonCode::VaultPlanInterrupted,
        ReasonCode::VaultWriteFailed,
        ReasonCode::VaultMigrationRefused,
        ReasonCode::RequestOutOfBound,
        ReasonCode::RequestPartNotTaken,
        ReasonCode::RequestCursorNotTaken,
        ReasonCode::RequestPlanInvalid,
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

/// The set a collision is spelled with, from names this coverage knows are
/// two or more distinct legal ones.
fn names(named: impl IntoIterator<Item = VaultName>) -> NameSet {
    NameSet::new(named).expect("at least two distinct names")
}

/// Every detail variant, over every payload it can carry.
fn error_details() -> Vec<ErrorDetail> {
    let mut details: Vec<_> = untrusted_reasons()
        .into_iter()
        .map(ErrorDetail::entry_untrusted)
        .collect();
    details.extend([
        ErrorDetail::duplicate_root(names([name("notes"), name("vault")])),
        ErrorDetail::shared_schema(names([name("notes"), name("vault")])),
        ErrorDetail::maintainer_contended(MaintainerIdentity::unknown()),
        ErrorDetail::maintainer_contended(MaintainerIdentity::named(41, "0.1.0", 1_700_000_000)),
        ErrorDetail::unknown_vault(name("notes")),
        ErrorDetail::already_served(name("notes")),
        ErrorDetail::entry_held(name("notes")),
        ErrorDetail::reader_unavailable("this coverage mints no read handle"),
        ErrorDetail::registry_unwritable("the registry file is read-only"),
        ErrorDetail::read_failed(
            ReadFailure::statement(),
            "the database disk image is malformed",
        ),
        ErrorDetail::read_failed(
            ReadFailure::declaration_not_pinned(Some("fp-1".to_string()), None),
            "the declaration was read from the schema `fp-1`, and the snapshot pins no schema",
        ),
        ErrorDetail::read_failed(
            ReadFailure::declaration_not_pinned(None, Some("fp-2".to_string())),
            "the declaration was read from no schema, and the snapshot pins the schema `fp-2`",
        ),
        ErrorDetail::ambiguous_root(names([name("notes"), name("vault")])),
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
        ErrorDetail::cursor_order_changed(CursorOrderChanged::minted_raw(Some("fp-2".to_string()))),
        ErrorDetail::cursor_order_changed(CursorOrderChanged::minted_raw(None).in_orders(
            Sort::new(SortKey::path(), Direction::Ascending),
            Sort::new(SortKey::field("due"), Direction::Descending),
        )),
        ErrorDetail::unreadable_bound("due", "not-a-date"),
        ErrorDetail::out_of_bound(RequestBound::page_rows(5_000, 1_024)),
        ErrorDetail::out_of_bound(RequestBound::page_rows(0, 1_024)),
        ErrorDetail::out_of_bound(RequestBound::membership_values("status", 257, 256)),
        ErrorDetail::out_of_bound(RequestBound::empty_membership("type")),
        ErrorDetail::part_not_taken(RequestPart::Anchor, Some(AnswerShape::CollectionPage)),
        ErrorDetail::part_not_taken(RequestPart::Column, Some(AnswerShape::Section)),
        ErrorDetail::part_not_taken(RequestPart::Column, Some(AnswerShape::Block)),
        ErrorDetail::part_not_taken(RequestPart::Limit, Some(AnswerShape::Record)),
        ErrorDetail::part_not_taken(RequestPart::Cursor, Some(AnswerShape::Summary)),
        ErrorDetail::part_not_taken(RequestPart::unknown("a sort key"), None),
        ErrorDetail::cursor_not_taken(PagedRows::Tally, PagedRows::Document),
        ErrorDetail::cursor_not_taken(PagedRows::Document, PagedRows::Tally),
        ErrorDetail::cursor_not_taken(PagedRows::Hit, PagedRows::Finding),
        ErrorDetail::cursor_not_taken(PagedRows::Finding, PagedRows::Facet),
        ErrorDetail::cursor_not_taken(PagedRows::Facet, PagedRows::Hit),
        ErrorDetail::cursor_not_taken(
            PagedRows::Collection {
                of: CollectionSelector::Headings,
            },
            PagedRows::Collection {
                of: CollectionSelector::Tags,
            },
        ),
        ErrorDetail::cursor_order_changed(
            CursorOrderChanged::minted_raw(None).in_ladders(RungSet::lexical(), fused_ladder()),
        ),
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
    details.extend(apply_details());
    details.extend(migration_refusals().into_iter().flat_map(|reason| {
        [ControlFile::Schema, ControlFile::Config]
            .map(|file| ErrorDetail::migration_refused(file, reason.clone()))
    }));
    details
}

/// Every reason a migration is refused for.
fn migration_refusals() -> Vec<MigrationRefusal> {
    vec![
        MigrationRefusal::unreadable("the file is not YAML"),
        MigrationRefusal::version_ahead(3, 1),
        MigrationRefusal::no_step(0, 1),
        MigrationRefusal::comment_lost("# the owner reads this"),
        MigrationRefusal::changed(),
    ]
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
    FacetKind::ALL.to_vec()
}

/// Every movement a continuation reports.
fn movements() -> Vec<Moved> {
    vec![Moved::Epoch, Moved::Generation, Moved::SidecarRevision]
}

/// A finite score, built through the grammar the type keeps.
fn score(value: f64) -> Score {
    Score::new(value).expect("a finite relevance score")
}

/// The sidecar state at `revision` within the sidecar epoch `sidecar-1`.
fn sidecar(revision: u64) -> SidecarRevision {
    SidecarRevision::new("sidecar-1", revision)
}

/// A ladder above the floor: the lexical and vector rungs fused.
fn fused_ladder() -> RungSet {
    RungSet::of([Rung::Lexical, Rung::Vector]).expect("a ladder that runs a rung")
}

/// Every paged row type, with one key per shape its order takes.
fn cursor_keys() -> Vec<CursorKey> {
    let mut keys = vec![
        CursorKey::document(
            Sort::new(SortKey::field("due"), Direction::Descending),
            Some("2026-01-01".to_string()),
            "notes/a.md",
        ),
        CursorKey::document(
            Sort::new(SortKey::path(), Direction::Ascending),
            None,
            "notes/a.md",
        ),
        CursorKey::hit(RungSet::lexical(), score(0.5), "notes/a.md"),
        CursorKey::hit(fused_ladder(), score(0.5), "notes/a.md"),
        CursorKey::tally([Some("note".to_string()), None]),
        CursorKey::finding(FindingKind::UndeclaredTag, "notes/a.md", None, 7),
        CursorKey::finding(FindingKind::Broken, "notes/a.md", Some(3), 7),
        CursorKey::document_finding("notes/a.md", None, FindingKind::UndeclaredTag, 7),
        CursorKey::document_finding("notes/a.md", Some(3), FindingKind::Broken, 7),
    ];
    keys.extend(
        [
            CollectionSelector::Links,
            CollectionSelector::Headings,
            CollectionSelector::Blocks,
            CollectionSelector::Tags,
        ]
        .into_iter()
        .map(|of| CursorKey::ordinal(of, 3)),
    );
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
                Snapshot::new("epoch-1", 12, Some("fp-1".to_string()), Some(sidecar(4))),
                key,
            )
        })
        .collect();
    cursors.push(Cursor::new(
        Snapshot::new("epoch-1", 0, None, None),
        CursorKey::ordinal(CollectionSelector::Tags, 0),
    ));
    cursors
}

/// Every rung a ladder declares.
fn rungs() -> Vec<Rung> {
    vec![Rung::Lexical, Rung::Vector, Rung::Expansion, Rung::Rerank]
}

/// Every selection a search asks for its rungs by: the enabled set whole, the
/// enabled set less a rung, and an exact set.
fn rung_selections() -> Vec<RungSelection> {
    vec![
        RungSelection::enabled(),
        RungSelection::enabled_without([Rung::Vector])
            .expect("a subtraction leaving a retrieval rung"),
        RungSelection::exactly(RungSet::of(rungs()).expect("a ladder that runs a rung")),
    ]
}

/// Every freshness a stateful rung reports.
fn freshnesses() -> Vec<Freshness> {
    vec![
        Freshness::trailing(0),
        Freshness::trailing(3),
        Freshness::rescanning(),
    ]
}

/// Every section reading a status reports for a vault's engine: the four
/// the host retains from a delivery, and the one it reports where no
/// delivery stands.
fn engine_sections() -> Vec<EngineSection> {
    vec![
        EngineSection::undelivered(),
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

/// Every reading an answer is taken under.
fn answer_readings() -> Vec<AnswerReading> {
    vec![AnswerReading::new(TrustState::Ready, "epoch-1", 12)]
}

/// A ladder a search declares: the floor alone, every rung at once, and a
/// ladder of vectors without the floor.
fn ladder_declarations() -> Vec<LadderDeclaration> {
    vec![
        LadderDeclaration::lexical(),
        LadderDeclaration::new(
            vec![
                RungReport::lexical(),
                RungReport::vector(ModelIdentity::new("stub", "1"), Freshness::trailing(3)),
                RungReport::expansion(ModelIdentity::new("stub", "1")),
                RungReport::rerank(ModelIdentity::new("stub", "1")),
            ],
            false,
        )
        .expect("a ladder in ladder order holding a retrieval rung"),
        LadderDeclaration::new(
            vec![RungReport::vector(
                ModelIdentity::new("stub", "1"),
                Freshness::rescanning(),
            )],
            false,
        )
        .expect("a ladder in ladder order holding a retrieval rung"),
    ]
}

/// Every part a request can leave unapplied.
fn unsatisfied_parts() -> Vec<Unsatisfied> {
    vec![
        Unsatisfied::unknown_sort_key("due", vec!["date".to_string()]),
        Unsatisfied::unknown_projection_key("due", vec![]),
        Unsatisfied::unknown_predicate_key("due", vec!["date".to_string()]),
        Unsatisfied::unknown_group_key("due", vec!["date".to_string()]),
        Unsatisfied::bare_directory("docs"),
        Unsatisfied::malformed_glob("docs/[", "the character class does not close"),
        Unsatisfied::malformed_query("design AND", "fts5: syntax error near \"\""),
        Unsatisfied::impossible_path("/etc/passwd"),
        Unsatisfied::missing_section("Design"),
        Unsatisfied::missing_block("a1"),
        Unsatisfied::resolves_not_applicable(target("norn/glossary")),
        Unsatisfied::query_names_no_word("-- !!"),
        Unsatisfied::links_to_ambiguous(
            target("glossary"),
            head(
                [candidate("notes/glossary"), candidate("archive/glossary")],
                2,
            ),
        ),
        Unsatisfied::links_to_unknown(target("nowhere")),
    ]
}

/// One advisory of each kind an answer carries.
fn answer_advisories() -> Vec<AnswerAdvisory> {
    vec![
        AnswerAdvisory::mixed_offset("due", ComparedBy::Sort),
        AnswerAdvisory::mixed_offset("due", ComparedBy::Group),
        AnswerAdvisory::mixed_offset("due", ComparedBy::Predicate),
        AnswerAdvisory::rung_skipped(
            Rung::Vector,
            RungSkipReason::unavailable("the engine slot is empty"),
        ),
        AnswerAdvisory::rung_depth_reached(Rung::Vector),
        AnswerAdvisory::rung_short_of_depth(Rung::Vector, 1000),
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

/// Every reading a link's health takes.
fn link_healths() -> Vec<LinkHealth> {
    vec![
        LinkHealth::Healthy,
        LinkHealth::Broken,
        LinkHealth::Ambiguous,
        LinkHealth::NotJudged,
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
/// from the link's addressing and the total of the bounded head beside it.
fn link_rows() -> Vec<LinkRow> {
    let mut rows: Vec<LinkRow> = [
        head([], 0),
        head([candidate("notes/a")], 1),
        head([candidate("notes/a"), candidate("archive/a")], 2),
    ]
    .into_iter()
    .map(link_row)
    .collect();
    rows.push(addressed_row(
        Some("https"),
        "example.com/page",
        head([], 0),
    ));
    rows
}

/// A link row written with `protocol` and `target`, resolving to `targets`,
/// built through the constructor that derives its health.
fn addressed_row(protocol: Option<&str>, target: &str, targets: CandidateHead) -> LinkRow {
    LinkRow::new(
        LinkFamily::Markdown,
        false,
        protocol.map(str::to_string),
        target,
        Some(String::new()),
        None,
        span(),
        targets,
    )
    .expect("a document link, or an elsewhere link resolving to none")
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
    .expect("a wikilink names a document, never elsewhere")
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
    addressed_row_json(None, "a", health, candidates, total)
}

/// A link row as bytes, written with `protocol` and `target`.
fn addressed_row_json(
    protocol: Option<&str>,
    target: &str,
    health: &str,
    candidates: &[serde_json::Value],
    total: u64,
) -> String {
    serde_json::json!({
        "family": "wikilink",
        "embed": false,
        "protocol": protocol,
        "target": target,
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

/// Every nested collection a request pages.
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
    ContainerKind::ALL.to_vec()
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
            .map(|container| Facet::observed_field("due", [container])),
    );
    facets.push(Facet::observed_field("aliases", container_kinds()));
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
    facets.push(Facet::creation_rule(
        "task",
        "tasks/{{var.project}}-{{seq}}.md",
        vec!["project".to_string()],
        ValueMap::new([
            ("status".to_string(), AuthoredValue::string("todo")),
            ("rank".to_string(), AuthoredValue::Integer(2)),
        ])
        .expect("each key once"),
        Some("# {{var.project}}\n".to_string()),
    ));
    facets.push(Facet::creation_rule(
        "note",
        "notes/{{date}}.md",
        Vec::new(),
        ValueMap::default(),
        None,
    ));
    facets.push(Facet::inbox("inbox/{{date}}-{{seq}}.md"));
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
        rung_selections()
            .iter()
            .map(|selection| tag_string(selection, "select"))
            .collect::<BTreeSet<_>>(),
        advertised::<RungSelection>(Some("select")),
        "the selections built here are not the selections the vocabulary holds"
    );
    assert_eq!(
        answer_advisories()
            .iter()
            .map(|advisory| tag_string(advisory, "advisory"))
            .collect::<BTreeSet<_>>(),
        advertised::<AnswerAdvisory>(Some("advisory")),
        "the advisories built here are not the advisories the vocabulary holds"
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
        advertised::<VaultChange<SchemaSource>>(Some("change")),
        "the changes built here are not the changes the vocabulary holds"
    );
    assert_eq!(
        replacements()
            .iter()
            .map(|replacement| tag_string(replacement, "change"))
            .collect::<BTreeSet<_>>(),
        advertised::<VaultReplace<VaultRoot>>(Some("change")),
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
    for container in container_kinds() {
        assert_eq!(
            container.as_str(),
            flat_string(&container),
            "a container names itself otherwise than the wire writes it"
        );
    }
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

/// A read refusal about the request's own shape crosses as typed facts: the
/// bound with the count named and the most it may be, the part with the
/// answer that does not take it, and the rows a cursor names against the rows
/// the request pages.
#[test]
fn a_request_refusal_crosses_as_the_shape_facts_it_names() {
    assert_eq!(
        wire(&ErrorDetail::out_of_bound(RequestBound::page_rows(
            0, 1_024
        ))),
        r#"{"code":"request/out-of-bound","bound":{"kind":"page_rows","given":0,"ceiling":1024}}"#
    );
    assert_eq!(
        wire(&ErrorDetail::out_of_bound(RequestBound::membership_values(
            "status", 257, 256
        ))),
        concat!(
            r#"{"code":"request/out-of-bound","#,
            r#""bound":{"kind":"membership_values","key":"status","given":257,"ceiling":256}}"#
        )
    );
    assert_eq!(
        wire(&ErrorDetail::out_of_bound(RequestBound::empty_membership(
            "type"
        ))),
        r#"{"code":"request/out-of-bound","bound":{"kind":"empty_membership","key":"type"}}"#
    );
    assert_eq!(
        wire(&ErrorDetail::part_not_taken(
            RequestPart::Cursor,
            Some(AnswerShape::Summary)
        )),
        r#"{"code":"request/part-not-taken","part":{"kind":"cursor"},"answer":"summary"}"#
    );
    assert_eq!(
        wire(&ErrorDetail::part_not_taken(
            RequestPart::unknown("a sort key"),
            None
        )),
        concat!(
            r#"{"code":"request/part-not-taken","#,
            r#""part":{"kind":"unknown","name":"a sort key"},"answer":null}"#
        )
    );
    assert_eq!(
        wire(&ErrorDetail::cursor_not_taken(
            PagedRows::Collection {
                of: CollectionSelector::Findings
            },
            PagedRows::DocumentFinding,
        )),
        concat!(
            r#"{"code":"request/cursor-not-taken","#,
            r#""cursor":{"row":"collection","of":"findings"},"paged":{"row":"document_finding"}}"#
        )
    );
}

/// A failed read names which failure it was, and a declaration read under
/// another schema than the pinned one carries both fingerprints, `null` for
/// no schema.
#[test]
fn a_failed_read_crosses_as_the_failure_it_names() {
    assert_eq!(
        wire(&ErrorDetail::read_failed(
            ReadFailure::statement(),
            "the database disk image is malformed"
        )),
        concat!(
            r#"{"code":"host/read-failed","failure":{"kind":"statement"},"#,
            r#""detail":"the database disk image is malformed"}"#
        )
    );
    assert_eq!(
        wire(&ErrorDetail::read_failed(
            ReadFailure::declaration_not_pinned(Some("fp-1".to_string()), None),
            "declared elsewhere"
        )),
        concat!(
            r#"{"code":"host/read-failed","#,
            r#""failure":{"kind":"declaration_not_pinned","declared_under":"fp-1","pinned":null},"#,
            r#""detail":"declared elsewhere"}"#
        )
    );
}

/// A cursor key names the rows it is a position among, and a nested
/// collection's key names its collection.
#[test]
fn a_cursor_key_names_the_rows_it_is_a_position_among() {
    let order = Sort::new(SortKey::path(), Direction::Ascending);
    let cases = [
        (
            CursorKey::document(order, None, "notes/a.md"),
            PagedRows::Document,
        ),
        (
            CursorKey::hit(
                RungSet::lexical(),
                Score::new(0.5).expect("a finite score"),
                "notes/a.md",
            ),
            PagedRows::Hit,
        ),
        (
            CursorKey::tally([Some("open".to_string())]),
            PagedRows::Tally,
        ),
        (
            CursorKey::finding(FindingKind::UndeclaredTag, "notes/a.md", None, 1),
            PagedRows::Finding,
        ),
        (
            CursorKey::document_finding("notes/a.md", None, FindingKind::UndeclaredTag, 1),
            PagedRows::DocumentFinding,
        ),
        (
            CursorKey::facet(FacetKind::Folder, "notes"),
            PagedRows::Facet,
        ),
        (
            CursorKey::ordinal(CollectionSelector::Links, 3),
            PagedRows::Collection {
                of: CollectionSelector::Links,
            },
        ),
    ];
    for (key, rows) in cases {
        assert_eq!(key.rows(), rows, "{key:?}");
    }
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
        "link/broken",
        "link/ambiguous",
        "link/missing-anchor",
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
            "link/ambiguous",
            "link/broken",
            "link/missing-anchor",
        ]
    );
    assert_eq!(place.len() + document.len(), FindingKind::ALL.len());
}

/// ADR 0027's three link-health kinds are each about one link in a document
/// that still derives whole, so each stands beside that document's row rather
/// than withheld in its place, and each is a warning rather than an error: a
/// broken, ambiguous, or anchor-missing link deserves attention, not the
/// correction a document norn cannot read demands.
#[test]
fn link_kinds_are_document_scoped_warnings() {
    for kind in [
        FindingKind::Broken,
        FindingKind::Ambiguous,
        FindingKind::MissingAnchor,
    ] {
        assert_eq!(kind.scope(), FindingScope::Document, "{kind}");
        assert_eq!(kind.default_severity(), Severity::Warning, "{kind}");
    }
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
            ErrorDetail::duplicate_root(names([name("notes"), name("vault")])),
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

/// The colliding names ascend because the name set they cross as sorts them,
/// so the order the field promises holds whatever order a producer collected
/// them in.
#[test]
fn duplicate_root_aliases_ascend_whatever_order_they_arrive_in() {
    assert_eq!(
        wire(&ErrorDetail::duplicate_root(names([
            name("vault"),
            name("archive"),
            name("notes"),
        ]))),
        r#"{"code":"host/duplicate-root","aliases":["archive","notes","vault"]}"#
    );
}

/// A collision between registered names is a fact about at least two of them,
/// and the set is where that is decided: a list naming one name or none —
/// whether it arrived empty, one-long, or as the same name twice — is no
/// collision. The three shapes that carry a collision take the set rather than
/// the names, so none of them can be built out of a list that names fewer,
/// and this is the one door all three are entered through.
#[test]
fn a_collision_refuses_a_list_that_names_fewer_than_two_vaults() {
    for named in [
        Vec::new(),
        vec![name("notes")],
        vec![name("notes"), name("notes")],
    ] {
        assert!(
            NameSet::new(named.clone()).is_err(),
            "{named:?} was carried as a collision"
        );
    }
    let set = NameSet::new([name("vault"), name("notes")]).expect("two distinct names");
    assert_eq!(set.names(), [name("notes"), name("vault")]);
    let _: ErrorDetail = ErrorDetail::duplicate_root(set.clone());
    let _: ErrorDetail = ErrorDetail::ambiguous_root(set.clone());
    let _: RegistryProblem = RegistryProblem::duplicate_root(set);
}

/// The read path refuses what the constructors refuse. Bytes naming one vault
/// or none are bytes no producer here mints, and a reader that took them would
/// hold a collision between one party: the refusal is refused entire rather
/// than read into a value a later reader would have to check.
#[test]
fn a_collision_refuses_bytes_that_name_fewer_than_two_vaults() {
    for names in [r#"[]"#, r#"["notes"]"#, r#"["notes","notes"]"#] {
        let detail = format!(r#"{{"code":"host/duplicate-root","aliases":{names}}}"#);
        assert!(
            serde_json::from_str::<ErrorDetail>(&detail).is_err(),
            "reading {detail} produced a detail"
        );
        let candidates = format!(r#"{{"code":"vault/ambiguous-root","candidates":{names}}}"#);
        assert!(
            serde_json::from_str::<ErrorDetail>(&candidates).is_err(),
            "reading {candidates} produced a detail"
        );
        let problem = format!(r#"{{"problem":"duplicate_root","aliases":{names}}}"#);
        assert!(
            serde_json::from_str::<RegistryProblem>(&problem).is_err(),
            "reading {problem} produced a problem"
        );
    }
    assert_eq!(
        serde_json::from_str::<ErrorDetail>(
            r#"{"code":"host/duplicate-root","aliases":["notes","vault"]}"#
        )
        .expect("two distinct colliding names"),
        ErrorDetail::duplicate_root(names([name("notes"), name("vault")]))
    );
    assert_eq!(
        serde_json::from_str::<ErrorDetail>(
            r#"{"code":"vault/ambiguous-root","candidates":["notes","vault"]}"#
        )
        .expect("two distinct candidate names"),
        ErrorDetail::ambiguous_root(names([name("notes"), name("vault")]))
    );
    assert_eq!(
        serde_json::from_str::<RegistryProblem>(
            r#"{"problem":"duplicate_root","aliases":["notes","vault"]}"#
        )
        .expect("two distinct colliding names"),
        RegistryProblem::duplicate_root(names([name("notes"), name("vault")]))
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

/// The registry holds twenty-three verbs, and every one of them is the flat
/// string it renders as, read back as the verb it renders.
#[test]
fn every_verb_is_the_flat_string_it_renders_as() {
    let strings = [
        "find",
        "search",
        "get",
        "count",
        "validate",
        "describe",
        "apply",
        "set",
        "edit",
        "new",
        "move",
        "delete",
        "rewrite_wikilink",
        "init",
        "vault_register",
        "vault_unregister",
        "vault_list",
        "vault_set",
        "vault_resolve",
        "vault_status",
        "vault_reload",
        "vault_migrate",
        "doctor_registry",
    ];
    assert_eq!(Verb::ALL.len(), 23);
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
    assert_eq!(Verb::ALL.len(), 23);
    assert_eq!(
        addressed(Addressing::Required),
        [
            "apply",
            "count",
            "delete",
            "describe",
            "edit",
            "find",
            "get",
            "init",
            "move",
            "new",
            "rewrite_wikilink",
            "search",
            "set",
            "validate",
            "vault_migrate",
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

/// A membership part naming no value is a part no document satisfies, and the
/// read path refuses it entire rather than carrying it to a store that would
/// refuse it later. One value is a membership.
#[test]
fn a_membership_part_refuses_bytes_that_name_no_value() {
    assert!(
        serde_json::from_str::<Predicate>(r#"{"op":"in","key":"type","values":[]}"#).is_err(),
        "a membership in no value read back as one"
    );
    assert_eq!(
        serde_json::from_str::<Predicate>(r#"{"op":"in","key":"type","values":["note"]}"#)
            .expect("a membership in one value"),
        Predicate::in_any("type", ["note".to_string()])
    );
}

// ── The cursor envelope ──────────────────────────────────────────────────

#[test]
fn every_cursor_survives_the_round_trip() {
    for cursor in cursors() {
        round_trip(&cursor);
    }
}

/// **A hit cursor carries its score exactly.** A continuation resumes after
/// the score a page stopped at by comparing it with the scores it ranks, so a
/// score read back one unit in the last place away from the one written names
/// another position — and a spelling that re-encodes to other bytes is no
/// cursor at all. Every finite double, spread across the whole range by its bit
/// pattern and dense among the small relevances a ranking computes, reads back
/// bit for bit through the opaque string.
#[test]
fn a_hit_cursor_carries_its_score_bit_for_bit() {
    let mut bits: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut spread = Vec::new();
    for _ in 0..20_000 {
        // A 64-bit linear congruential step, so the doubles are the same on
        // every run and reach every exponent.
        bits = bits
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        spread.push(f64::from_bits(bits));
        spread.push(f64::from_bits(bits) * 1e-300_f64.max(f64::MIN_POSITIVE));
        spread.push((bits >> 11) as f64 / (1u64 << 53) as f64 * 1e-5);
    }
    for value in spread.into_iter().filter(|value| value.is_finite()) {
        let cursor = Cursor::new(
            Snapshot::new("epoch-1", 3, None, None),
            CursorKey::hit(RungSet::lexical(), score(value), "notes/a.md"),
        );
        let json = wire(&cursor);
        let back: Cursor = serde_json::from_str(&json).unwrap_or_else(|error| {
            panic!("the cursor scored {value:e} did not read back: {error}")
        });
        let CursorKey::Hit { score: read, .. } = back.key() else {
            panic!("a hit cursor read back as {:?}", back.key());
        };
        assert_eq!(
            read.get().to_bits(),
            value.to_bits(),
            "the cursor scored {value:e} read back as {:e}",
            read.get()
        );
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
        r#""sidecar_revision":null},"key":{"row":"ordinal","of":"headings","index":3}}"#
    );
    let minted = Cursor::new(
        Snapshot::new("e", 1, None, None),
        CursorKey::ordinal(CollectionSelector::Headings, 3),
    );
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
            r#""sidecar_revision":null},"key":{"row":"ordinal","of":"headings","index":3},"page":2}"#
        ),
        // The same fields in another order.
        concat!(
            r#"{"key":{"row":"ordinal","of":"headings","index":3},"snapshot":{"epoch":"e","#,
            r#""generation":1,"schema_fingerprint":null,"sidecar_revision":null}}"#
        ),
        // The optional parts left out rather than written null.
        r#"{"snapshot":{"epoch":"e","generation":1},"key":{"row":"ordinal","of":"headings","index":3}}"#,
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
        r#""sidecar_revision":null},"key":{"row":"hit","ladder":["lexical"],"score":1e400,"path":"a.md"}}"#
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
        Snapshot::new("epoch-1", 12, Some("fp-1".to_string()), Some(sidecar(4))),
        CursorKey::ordinal(CollectionSelector::Headings, 1),
    )
}

/// An establishment that has not moved at all reports nothing.
#[test]
fn an_unmoved_establishment_reports_nothing() {
    let exact = Snapshot::new("epoch-1", 12, Some("fp-1".to_string()), Some(sidecar(4)));
    assert_eq!(minted().continuation(&exact), Ok(vec![]));
}

/// A database that is not the one the cursor was minted from reports `epoch`,
/// and the generation beside it is not reported: a rebuild restarts the write
/// count, so a count read against another database compares nothing. A
/// sidecar whose own epoch and revision have not moved is not reported: its
/// epoch is its own, not the store's.
#[test]
fn a_rebuilt_database_reports_the_epoch_and_not_the_generation() {
    let rebuilt = Snapshot::new("epoch-2", 3, Some("fp-1".to_string()), Some(sidecar(4)));
    assert_eq!(minted().continuation(&rebuilt), Ok(vec![Moved::Epoch]));
}

/// A generation that differs inside one epoch reports `generation` whichever
/// way it moved. Writes landing after the cursor was minted move it forward; a
/// count that moved backwards is a database that is not the one the cursor
/// named, and hiding that is worse than reporting it.
#[test]
fn a_generation_that_differs_in_either_direction_reports_the_generation() {
    for generation in [13, 11] {
        let written = Snapshot::new(
            "epoch-1",
            generation,
            Some("fp-1".to_string()),
            Some(sidecar(4)),
        );
        assert_eq!(
            minted().continuation(&written),
            Ok(vec![Moved::Generation]),
            "generation {generation} was not reported as moved"
        );
    }
}

/// A sidecar at another revision reports `sidecar_revision`, and so does one
/// at the same revision under another sidecar epoch: a rebuilt sidecar counts
/// its revisions again from the start, so two sidecar epochs share no scale
/// for a revision to be compared on. A sidecar gone from the answer has moved
/// too.
#[test]
fn a_sidecar_moves_with_its_revision_and_with_its_epoch() {
    let drained = Snapshot::new("epoch-1", 12, Some("fp-1".to_string()), Some(sidecar(5)));
    assert_eq!(
        minted().continuation(&drained),
        Ok(vec![Moved::SidecarRevision])
    );
    let rebuilt = Snapshot::new(
        "epoch-1",
        12,
        Some("fp-1".to_string()),
        Some(SidecarRevision::new("sidecar-2", 4)),
    );
    assert_eq!(
        minted().continuation(&rebuilt),
        Ok(vec![Moved::SidecarRevision]),
        "an equal revision under another sidecar epoch was read as the same state"
    );
    let gone = Snapshot::new("epoch-1", 12, Some("fp-1".to_string()), None);
    assert_eq!(
        minted().continuation(&gone),
        Ok(vec![Moved::SidecarRevision])
    );
}

/// A cursor minted without a sidecar revision reports nothing about one,
/// whatever the sidecar now answers: its position was taken without one, so
/// there is no revision it moved from.
#[test]
fn a_cursor_that_read_no_sidecar_reports_nothing_about_one() {
    let without = Cursor::new(
        Snapshot::new("epoch-1", 12, Some("fp-1".to_string()), None),
        CursorKey::ordinal(CollectionSelector::Headings, 1),
    );
    for revision in [None, Some(sidecar(4))] {
        assert_eq!(
            without.continuation(&Snapshot::new(
                "epoch-1",
                12,
                Some("fp-1".to_string()),
                revision.clone()
            )),
            Ok(vec![]),
            "a cursor that read no sidecar reported one at {revision:?}"
        );
    }
}

/// Everything at once, in the fixed order the list promises.
#[test]
fn a_continuation_reports_every_part_in_one_fixed_order() {
    let inside = Snapshot::new("epoch-1", 99, Some("fp-1".to_string()), Some(sidecar(5)));
    assert_eq!(
        minted().continuation(&inside),
        Ok(vec![Moved::Generation, Moved::SidecarRevision])
    );
    let rebuilt = Snapshot::new(
        "epoch-2",
        99,
        Some("fp-1".to_string()),
        Some(SidecarRevision::new("sidecar-2", 4)),
    );
    assert_eq!(
        minted().continuation(&rebuilt),
        Ok(vec![Moved::Epoch, Moved::SidecarRevision])
    );
}

/// **A refused continuation names at most one pair of orders, of one row
/// kind**, tagged `row` as the cursor key is: two document orders, or two
/// ladders. Both halves are named, or neither is: a pair missing one does not
/// parse.
#[test]
fn a_changed_orders_pair_is_one_pair_of_one_row_kind() {
    let documents = CursorOrderChanged::minted_raw(None).in_orders(
        Sort::new(SortKey::field("due"), Direction::Ascending),
        Sort::new(SortKey::field("due"), Direction::Descending),
    );
    assert_eq!(
        wire(&documents),
        concat!(
            r#"{"minted_under":null,"current":null,"orders":{"row":"document","#,
            r#""cursor":{"key":{"by":"field","key":"due"},"direction":"ascending"},"#,
            r#""request":{"key":{"by":"field","key":"due"},"direction":"descending"}}}"#
        )
    );
    let hits = CursorOrderChanged::minted_raw(None).in_ladders(RungSet::lexical(), fused_ladder());
    assert_eq!(
        wire(&hits),
        concat!(
            r#"{"minted_under":null,"current":null,"orders":{"row":"hit","#,
            r#""cursor":["lexical"],"request":["lexical","vector"]}}"#
        )
    );
    for changed in [&documents, &hits] {
        round_trip(changed);
    }
    assert_eq!(
        documents
            .clone()
            .in_ladders(RungSet::lexical(), fused_ladder()),
        hits,
        "a change named a second pair beside the first"
    );
    for half in [
        r#"{"minted_under":null,"current":null,"orders":{"row":"document","cursor":{"key":{"by":"path"},"direction":"ascending"}}}"#,
        r#"{"minted_under":null,"current":null,"orders":{"row":"hit","cursor":["lexical"]}}"#,
        r#"{"minted_under":null,"current":null,"orders":{"row":"hit","cursor":{"key":{"by":"path"},"direction":"ascending"},"request":{"key":{"by":"path"},"direction":"ascending"}}}"#,
    ] {
        assert!(
            serde_json::from_str::<CursorOrderChanged>(half).is_err(),
            "`{half}` parsed as a pair"
        );
    }
}

/// A hit cursor is a position in the ranking its ladder makes, on that
/// ladder's scale. Continued under another ladder it is refused, naming both
/// ladders; continued under its own it resumes at its score and path and is
/// judged as any continuation is, and a refusal on its fingerprint names both
/// ladders too.
#[test]
fn a_hit_cursor_continued_under_another_ladder_is_refused_naming_both() {
    let snapshot = || Snapshot::new("epoch-1", 12, None, Some(sidecar(4)));
    let lexical = Cursor::new(
        snapshot(),
        CursorKey::hit(RungSet::lexical(), score(0.5), "notes/a.md"),
    );
    assert_eq!(
        lexical.ranked_continuation(&snapshot(), &fused_ladder()),
        Some(Err(
            CursorOrderChanged::minted_raw(None).in_ladders(RungSet::lexical(), fused_ladder())
        ))
    );
    let resumed = |now: &Snapshot| {
        let resume = lexical
            .ranked_continuation(now, &RungSet::lexical())
            .expect("a hit cursor continues a ranking")
            .expect("a hit cursor continued under its own ladder");
        (resume.score, resume.path.to_string(), resume.moved)
    };
    assert_eq!(
        resumed(&snapshot()),
        (score(0.5), "notes/a.md".to_string(), vec![])
    );
    let drained = Snapshot::new("epoch-1", 13, None, Some(sidecar(5)));
    assert_eq!(
        resumed(&drained),
        (
            score(0.5),
            "notes/a.md".to_string(),
            vec![Moved::Generation, Moved::SidecarRevision]
        )
    );

    let typed = Cursor::new(
        Snapshot::new("epoch-1", 12, Some("fp-1".to_string()), None),
        CursorKey::hit(fused_ladder(), score(0.5), "notes/a.md"),
    );
    let unfingerprinted = Snapshot::new("epoch-1", 12, None, None);
    assert_eq!(
        typed.ranked_continuation(&unfingerprinted, &fused_ladder()),
        Some(Err(
            CursorOrderChanged::new("fp-1", None).in_ladders(fused_ladder(), fused_ladder())
        ))
    );
    assert_eq!(
        typed.ranked_continuation(&unfingerprinted, &RungSet::lexical()),
        Some(Err(
            CursorOrderChanged::new("fp-1", None).in_ladders(fused_ladder(), RungSet::lexical())
        ))
    );
}

/// A cursor that is no hit's names no position in any ranking, so a ranked
/// continuation of it is no continuation at all rather than one judged
/// without a ladder.
#[test]
fn a_cursor_that_is_no_hits_is_no_ranked_continuation() {
    let now = Snapshot::new("epoch-1", 12, Some("fp-1".to_string()), Some(sidecar(4)));
    for key in cursor_keys()
        .into_iter()
        .filter(|key| !matches!(key, CursorKey::Hit { .. }))
    {
        assert_eq!(
            Cursor::new(now.clone(), key.clone()).ranked_continuation(&now, &RungSet::lexical()),
            None,
            "the cursor keyed {key:?} continued a ranking"
        );
    }
}

/// A cursor minted under one order and continued under another refuses: the
/// rows its key names a position in are in a sequence that no longer exists.
/// An establishment that reads no fingerprint at all is not walking that
/// sequence either, so a typed cursor refuses there too.
#[test]
fn a_changed_order_refuses_the_continuation() {
    let typed = Cursor::new(
        Snapshot::new("epoch-1", 12, Some("fp-1".to_string()), None),
        CursorKey::ordinal(CollectionSelector::Headings, 1),
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
        CursorKey::ordinal(CollectionSelector::Headings, 1),
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
        CursorKey::ordinal(CollectionSelector::Headings, 1),
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
    let snapshot = Snapshot::new("epoch-1", 12, Some("fp-1".to_string()), Some(sidecar(4)));
    assert_eq!(
        wire(&snapshot),
        concat!(
            r#"{"epoch":"epoch-1","generation":12,"#,
            r#""schema_fingerprint":"fp-1","sidecar_revision":{"epoch":"sidecar-1","revision":4}}"#
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
    for advisory in answer_advisories() {
        round_trip(&advisory);
    }
}

/// A reading is the trust state, the database, and how far its writes had
/// got. The ladder a search ran is the search report's, not the reading's, so
/// it has one home.
#[test]
fn a_reading_carries_the_trust_state_and_the_database() {
    assert_eq!(
        wire(&AnswerReading::new(TrustState::Ready, "epoch-1", 12)),
        r#"{"trust":{"state":"ready"},"epoch":"epoch-1","generation":12}"#
    );
}

/// A search report is the ladder that ranked it and the page of hits, each in
/// a field of its own. There is no search report without a ladder: the lexical
/// floor alone is declared as `[lexical]`, repeatable, and a report arriving
/// without one does not parse.
#[test]
fn a_search_report_declares_its_ladder_beside_its_page() {
    let report = SearchReport::new(
        LadderDeclaration::lexical(),
        Page::new(vec![Hit::new(path("notes/a.md"), score(0.5))], None, vec![]),
    );
    assert_eq!(
        wire(&report),
        concat!(
            r#"{"ladder":{"rungs":[{"rung":"lexical"}],"repeatable":true},"#,
            r#""page":{"rows":[{"path":"notes/a.md","score":0.5}],"next":null,"moved":[]}}"#
        )
    );
    for ladder in ladder_declarations() {
        round_trip(&SearchReport::new(ladder, Page::new(vec![], None, vec![])));
    }
    assert!(
        serde_json::from_str::<SearchReport>(r#"{"page":{"rows":[],"next":null,"moved":[]}}"#)
            .is_err(),
        "a search report declaring no ladder parsed"
    );
    assert_eq!(
        LadderDeclaration::lexical(),
        LadderDeclaration::new(vec![RungReport::lexical()], true).expect("the floor alone")
    );
}

/// A declaration names the rungs that ran in ladder order, each once, and
/// holds a retrieval rung; one that does not is refused where it is built and
/// where it is read alike. The rung set a hit cursor names is derived from the
/// declaration, never built beside it.
#[test]
fn a_ladder_declaration_is_a_ladder_in_ladder_order() {
    let model = || ModelIdentity::new("stub", "1");
    let vector = || RungReport::vector(model(), Freshness::trailing(0));
    for (rungs, refusal) in [
        (vec![], MalformedLadder::NoRetrievalRung),
        (
            vec![RungReport::rerank(model())],
            MalformedLadder::NoRetrievalRung,
        ),
        (
            vec![RungReport::expansion(model()), RungReport::rerank(model())],
            MalformedLadder::NoRetrievalRung,
        ),
        (
            vec![RungReport::lexical(), RungReport::lexical()],
            MalformedLadder::OutOfLadderOrder,
        ),
        (
            vec![vector(), RungReport::lexical()],
            MalformedLadder::OutOfLadderOrder,
        ),
        (
            vec![RungReport::lexical(), vector(), vector()],
            MalformedLadder::OutOfLadderOrder,
        ),
    ] {
        let named: Vec<Rung> = rungs.iter().map(RungReport::rung).collect();
        assert_eq!(
            LadderDeclaration::new(rungs.clone(), true),
            Err(refusal),
            "{named:?} declared as a ladder"
        );
    }
    for malformed in [
        r#"{"rungs":[],"repeatable":true}"#,
        r#"{"rungs":[{"rung":"rerank","model":{"id":"stub","version":"1"}}],"repeatable":true}"#,
        r#"{"rungs":[{"rung":"lexical"},{"rung":"lexical"}],"repeatable":true}"#,
        r#"{"rungs":[{"rung":"vector","model":{"id":"stub","version":"1"},"freshness":{"state":"rescanning"}},{"rung":"lexical"}],"repeatable":true}"#,
    ] {
        assert!(
            serde_json::from_str::<LadderDeclaration>(malformed).is_err(),
            "`{malformed}` read back as a ladder"
        );
    }
    assert_eq!(LadderDeclaration::lexical().rung_set(), RungSet::lexical());
    for ladder in ladder_declarations() {
        let declared: Vec<Rung> = ladder.rungs().iter().map(RungReport::rung).collect();
        assert_eq!(
            ladder.rung_set(),
            RungSet::of(declared).expect("a declared ladder holds a retrieval rung")
        );
    }
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

/// The section the host was delivered is four readings, the malformed one
/// carrying its account as prose beside the tag a client branches on, and a
/// fifth says no section has been delivered at all.
#[test]
fn an_engine_section_is_an_object_tagged_state() {
    assert_eq!(
        wire(&EngineSection::undelivered()),
        r#"{"state":"undelivered"}"#
    );
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
        wire(&Unsatisfied::malformed_query(
            "design AND",
            "fts5: syntax error near \"\""
        )),
        r#"{"part":"malformed_query","query":"design AND","problem":"fts5: syntax error near \"\""}"#
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
    assert_eq!(
        wire(&Unsatisfied::query_names_no_word("-- !!")),
        r#"{"part":"query_names_no_word","query":"-- !!"}"#
    );
    assert_eq!(
        wire(&Unsatisfied::links_to_unknown(target("nowhere"))),
        r#"{"part":"links_to_unknown","target":"nowhere"}"#
    );
    assert_eq!(
        wire(&Unsatisfied::links_to_ambiguous(
            target("glossary"),
            head([candidate("notes/glossary")], 3),
        )),
        r#"{"part":"links_to_ambiguous","target":"glossary","candidates":{"candidates":[{"path":"notes/glossary.md","suffix":"notes/glossary"}],"total":3}}"#
    );
    assert!(
        serde_json::from_str::<Unsatisfied>(r#"{"part":"links_to_unknown","target":""}"#).is_err(),
        "a links-to part carrying an addressless target read back as one"
    );
}

/// A rung left out of the enabled set is advised with the rung and the reason,
/// and the reason carries, as its `code`, the refusal code a search naming the
/// rung exactly meets. A rung that reached its depth is advised with the rung
/// alone: the depth is every rung's one `RUNG_DEPTH`, which the advisory does
/// not repeat. A rung that fell short of it is advised with the rung and how
/// many candidates it delivered.
#[test]
fn a_rung_advisory_names_the_rung_and_why() {
    let reason = RungSkipReason::unavailable("the engine slot is empty");
    assert_eq!(reason.code(), ReasonCode::EngineUnavailable);
    assert_eq!(
        tag_string(&reason, "code"),
        flat_string(&ReasonCode::EngineUnavailable),
        "a skip reason is spelled as another code than the refusal it stands for"
    );
    assert_eq!(
        wire(&AnswerAdvisory::rung_skipped(Rung::Vector, reason)),
        r#"{"advisory":"rung_skipped","rung":"vector","reason":{"code":"engine/unavailable","detail":"the engine slot is empty"}}"#
    );
    assert!(
        serde_json::from_str::<RungSkipReason>(r#"{"code":"engine/failed","detail":"x"}"#).is_err(),
        "a reason nobody skips a rung for read back as one"
    );
    assert_eq!(
        wire(&AnswerAdvisory::rung_depth_reached(Rung::Vector)),
        r#"{"advisory":"rung_depth_reached","rung":"vector"}"#
    );
    assert_eq!(
        wire(&AnswerAdvisory::rung_short_of_depth(Rung::Vector, 1000)),
        r#"{"advisory":"rung_short_of_depth","rung":"vector","delivered":1000}"#
    );
}

/// An answer advisory is an object tagged `advisory`, naming the key whose
/// values a comparison assumed something about and where the request compared
/// them.
#[test]
fn an_answer_advisory_is_an_object_tagged_advisory() {
    assert_eq!(
        wire(&AnswerAdvisory::mixed_offset("due", ComparedBy::Sort)),
        r#"{"advisory":"mixed_offset","key":"due","compared_by":"sort"}"#
    );
    assert_eq!(
        wire(&AnswerAdvisory::mixed_offset("due", ComparedBy::Group)),
        r#"{"advisory":"mixed_offset","key":"due","compared_by":"group"}"#
    );
    assert_eq!(
        wire(&AnswerAdvisory::mixed_offset("due", ComparedBy::Predicate)),
        r#"{"advisory":"mixed_offset","key":"due","compared_by":"predicate"}"#
    );
    assert!(
        serde_json::from_str::<AnswerAdvisory>(
            r#"{"advisory":"mixed_offset","key":"due","compared_by":"projection"}"#
        )
        .is_err(),
        "an advisory compared somewhere the vocabulary does not name read back as one"
    );
}

/// The two answering exits are one type: a complete answer is one with no
/// unsatisfied parts, and a partial one is the same shape saying which parts
/// were not applied. An advisory is not a part left unapplied, so an answer
/// carrying one is still complete.
#[test]
fn a_vault_answer_is_complete_exactly_when_nothing_was_left_unapplied() {
    let reading = || AnswerReading::new(TrustState::Ready, "epoch-1", 12);
    let whole: VaultAnswer<u64> = VaultAnswer::new(reading(), vec![], 3);
    assert!(whole.is_complete());
    assert!(whole.advisories.is_empty());
    assert_eq!(whole.report, 3);
    round_trip(&whole);
    assert_eq!(
        wire(&whole),
        r#"{"reading":{"trust":{"state":"ready"},"epoch":"epoch-1","generation":12},"unsatisfied":[],"advisories":[],"report":3}"#
    );

    let advised: VaultAnswer<u64> =
        VaultAnswer::new(reading(), vec![], 3).with_advisories(answer_advisories());
    assert!(advised.is_complete());
    assert_eq!(advised.advisories, answer_advisories());
    round_trip(&advised);

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

/// Every code the four namespaces hold is a fact about the host's serving of
/// an entry, about the requested vault, about that vault's engine, or about
/// the request's own shape.
#[test]
fn every_code_sits_in_one_of_the_four_namespaces() {
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
        ["host", "vault", "engine", "request"]
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

/// The candidates a directory resolves under ascend because the name set they
/// cross as sorts them.
#[test]
fn ambiguous_root_candidates_ascend_whatever_order_they_arrive_in() {
    assert_eq!(
        wire(&ErrorDetail::ambiguous_root(names([
            name("vault"),
            name("archive"),
            name("notes"),
        ]))),
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

/// A path is the string itself, and the read path is the grammar.
#[test]
fn a_document_path_is_the_string_it_renders_as() {
    assert_eq!(wire(&path("notes/a.md")), r#""notes/a.md""#);
    assert_eq!(path("notes/a.md").as_str(), "notes/a.md");
    assert_eq!(path("notes/a.md").to_string(), "notes/a.md");
    for text in [
        "a b.md",
        "été.md",
        ".gitignore",
        "notes/.hidden.md",
        "v1.2.md",
        "a/..b/c.md",
    ] {
        assert_eq!(path(text).as_str(), text);
        round_trip(&path(text));
    }
}

/// **A document path is one the store can hold, and nothing else.** Each
/// spelling the grammar refuses is refused where a path is built and where
/// one is read alike, with the grammar's own reason, so a request cannot
/// carry a path the vault could not hold a document at.
#[test]
fn a_document_path_refuses_what_no_vault_holds_a_document_at() {
    for (text, problem) in [
        ("", PathProblem::Empty),
        ("/notes/a.md", PathProblem::Absolute),
        ("/", PathProblem::Absolute),
        ("notes\\a.md", PathProblem::Backslash),
        ("a\u{1}.md", PathProblem::ControlCharacter),
        ("a\0.md", PathProblem::ControlCharacter),
        ("a\u{7f}.md", PathProblem::ControlCharacter),
        ("a//b.md", PathProblem::EmptySegment),
        ("a/", PathProblem::EmptySegment),
        ("./a.md", PathProblem::DotSegment),
        ("a/../b.md", PathProblem::DotSegment),
        ("..", PathProblem::DotSegment),
        ("..md", PathProblem::DotStem),
        ("notes/...md", PathProblem::DotStem),
    ] {
        assert_eq!(PathProblem::of_document(text), Some(problem), "{text:?}");
        let refusal = DocumentPath::new(text).expect_err(text);
        assert_eq!(refusal.what(), "document path");
        assert_eq!(refusal.problem(), problem.message(), "{text:?}");
        assert!(
            serde_json::from_str::<DocumentPath>(&wire(&text)).is_err(),
            "{text:?} was read back as a document path"
        );
    }
    // A request and a resolved plan read their paths through the same
    // grammar, so neither carries one the store would refuse.
    let request = wire(&DeleteParams::new(
        notes(),
        ApplyMode::Apply,
        path("notes/b.md"),
    ));
    let plan = wire(&a_bare_resolved_plan());
    for text in ["./notes/b.md", "notes//b.md", "notes\\b.md", "notes/..md"] {
        let refused = request.replace(r#""notes/b.md""#, &wire(&text));
        assert!(
            serde_json::from_str::<DeleteParams>(&refused).is_err(),
            "{refused} read as a delete request"
        );
        let refused = plan.replace(r#""notes/b.md""#, &wire(&text));
        assert_ne!(refused, plan, "the plan names `notes/b.md`");
        assert!(
            serde_json::from_str::<ResolvedPlan>(&refused).is_err(),
            "{refused} read as a resolved plan"
        );
    }
}

/// A folder holds no leaf, so a folder name is judged by the segment rules
/// alone: a name a document could not take is still a folder's.
#[test]
fn a_folder_name_is_judged_by_its_segments_alone() {
    for text in ["..b", "notes/...md", "a/..md"] {
        assert_eq!(PathProblem::of_segments(text), None, "{text:?}");
        assert_eq!(
            PathProblem::of_document(text),
            Some(PathProblem::DotStem),
            "{text:?}"
        );
    }
    for (text, problem) in [
        ("", PathProblem::Empty),
        ("/a", PathProblem::Absolute),
        ("a\\b", PathProblem::Backslash),
        ("a//b", PathProblem::EmptySegment),
        ("a/./b", PathProblem::DotSegment),
    ] {
        assert_eq!(PathProblem::of_segments(text), Some(problem), "{text:?}");
    }
}

/// A leaf's extension is what follows its last dot inside the name: a dot
/// leading the name opens it rather than an extension.
#[test]
fn a_leaf_stem_drops_the_extension_a_leading_dot_does_not_open() {
    for (leaf, stem) in [
        ("a.md", "a"),
        ("a.tar.gz", "a.tar"),
        ("a", "a"),
        (".gitignore", ".gitignore"),
        ("..md", "."),
        ("...md", ".."),
        ("a.", "a"),
    ] {
        assert_eq!(leaf_stem(leaf), stem, "{leaf:?}");
    }
}

/// A character the grammar refuses and a segment it refuses are the two
/// halves a refused spelling is made of, which is what renders one.
#[test]
fn a_refused_character_and_segment_are_the_grammars_own() {
    for character in ['\\', '\0', '\u{1}', '\n', '\u{7f}', '\u{85}'] {
        assert!(is_refused_character(character), "{character:?}");
    }
    for character in ['a', '/', '.', ' ', 'é', '\u{fffd}'] {
        assert!(!is_refused_character(character), "{character:?}");
    }
    for segment in ["", ".", ".."] {
        assert!(is_refused_segment(segment), "{segment:?}");
    }
    for segment in ["...", ".a", "a"] {
        assert!(!is_refused_segment(segment), "{segment:?}");
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
    assert_eq!(rows[3].health(), LinkHealth::NotJudged);
}

/// **A link's address is selected protocol first and family second.** A
/// protocol other than `vault` addresses no document, and a `vault://` stem is
/// read from the vault root under its family's rules: a wikilink's as a name,
/// kept as written, and a Markdown link's as a path, its query cut off. With
/// no protocol, an empty target names the document holding the link, a
/// wikilink's target is a suffix address — a colon in it is a character like
/// any other — and a Markdown target is a path: from the vault root where it opens with the separator, and from the
/// holding document's directory otherwise, its query cut off. A Markdown
/// target opening with a URI scheme, `://` or not, addresses no document; a
/// scheme opens with a letter and ends at the first colon, before any
/// separator.
#[test]
fn a_links_address_is_selected_protocol_first_and_family_second() {
    use LinkFamily::{Markdown, Wikilink};
    for (family, protocol, target, address) in [
        (
            Markdown,
            Some("https"),
            "example.com",
            LinkAddress::Elsewhere,
        ),
        (
            Wikilink,
            Some("https"),
            "example.com",
            LinkAddress::Elsewhere,
        ),
        (
            Wikilink,
            Some("vault"),
            "notes/x",
            LinkAddress::RootedName("notes/x"),
        ),
        (
            Wikilink,
            Some("vault"),
            "notes/my%20x.md?raw=1",
            LinkAddress::RootedName("notes/my%20x.md?raw=1"),
        ),
        (
            Markdown,
            Some("vault"),
            "notes/x.md?raw=1",
            LinkAddress::Rooted("notes/x.md"),
        ),
        (Wikilink, None, "", LinkAddress::HoldingDocument),
        (Markdown, None, "", LinkAddress::HoldingDocument),
        (Markdown, None, "?x=1", LinkAddress::HoldingDocument),
        (Wikilink, None, "notes/x", LinkAddress::Suffix("notes/x")),
        (Wikilink, None, "mailto:x", LinkAddress::Suffix("mailto:x")),
        (
            Markdown,
            None,
            "mailto:someone@example.com",
            LinkAddress::Elsewhere,
        ),
        (Markdown, None, "tel:", LinkAddress::Elsewhere),
        (
            Markdown,
            None,
            "Note+1.x-y:draft.md",
            LinkAddress::Elsewhere,
        ),
        (
            Markdown,
            None,
            "a/b:c.md",
            LinkAddress::Relative("a/b:c.md"),
        ),
        (Markdown, None, "1a:b.md", LinkAddress::Relative("1a:b.md")),
        (Markdown, None, ":b.md", LinkAddress::Relative(":b.md")),
        (Markdown, None, "q.md?x=1", LinkAddress::Relative("q.md")),
        (Markdown, None, "../x.md", LinkAddress::Relative("../x.md")),
        (Markdown, None, "/x.md", LinkAddress::Rooted("x.md")),
    ] {
        assert_eq!(
            LinkAddress::of(family, protocol, target),
            address,
            "{family:?} {protocol:?} `{target}`"
        );
    }
}

/// **A link is judged by resolving it first.** A link addressed elsewhere —
/// written with a protocol other than `vault`, or a Markdown target opening
/// with a URI scheme — is not judged, and a nonzero head for one is refused
/// rather than built. Any other link that resolves to documents is judged by
/// how many, whatever its leaf carries. One that resolves to none is not
/// judged where its leaf carries an extension other than the document
/// extension, which names an attachment, and is broken otherwise: a leaf with
/// no extension, or the document extension in any ASCII case. A dot in a
/// directory segment or leading a name is no extension.
#[test]
fn a_link_is_judged_by_resolving_it_first() {
    use LinkFamily::{Markdown, Wikilink};
    let judged = |family, protocol: Option<&str>, target: &str, total: u64| {
        let candidates: Vec<Candidate> = (0..total)
            .map(|index| candidate(&format!("notes/c{index}")))
            .collect();
        LinkRow::new(
            family,
            false,
            protocol.map(str::to_string),
            target,
            None,
            None,
            span(),
            head(candidates, total),
        )
    };
    for (family, protocol, target) in [
        (Markdown, Some("https"), "example.com/page"),
        (Wikilink, Some("https"), "example.com/wiki"),
        (Markdown, None, "mailto:someone@example.com"),
        (Markdown, None, "tel:+1-555-0100"),
    ] {
        let label = format!("{family:?} {protocol:?} `{target}`");
        assert_eq!(
            judged(family, protocol, target, 0)
                .unwrap_or_else(|error| panic!("{label} resolving to 0: {error}"))
                .health(),
            LinkHealth::NotJudged,
            "{label}"
        );
        for total in [1, 2] {
            assert!(
                judged(family, protocol, target, total).is_err(),
                "{label} addressed elsewhere accepted a head of {total}"
            );
        }
    }
    for (family, protocol, target, unresolved) in [
        (Wikilink, None, "picture.png", LinkHealth::NotJudged),
        (Markdown, None, "assets/pic.png", LinkHealth::NotJudged),
        (Markdown, None, "pic.png?size=2", LinkHealth::NotJudged),
        (Wikilink, None, "archive.tar.gz", LinkHealth::NotJudged),
        (Wikilink, None, "v1.2", LinkHealth::NotJudged),
        (
            Wikilink,
            Some("vault"),
            "assets/pic.png",
            LinkHealth::NotJudged,
        ),
        (Wikilink, None, "notes", LinkHealth::Broken),
        (Markdown, None, "notes.md", LinkHealth::Broken),
        (Wikilink, None, "Notes.MD", LinkHealth::Broken),
        (Wikilink, None, "a.b/c", LinkHealth::Broken),
        (Wikilink, None, ".hidden", LinkHealth::Broken),
        (Wikilink, None, "note:draft", LinkHealth::Broken),
        (Markdown, None, "../x/my%20note.md", LinkHealth::Broken),
        (Markdown, None, "", LinkHealth::Broken),
        (Wikilink, Some("vault"), "Notes", LinkHealth::Broken),
    ] {
        let label = format!("{family:?} {protocol:?} `{target}`");
        assert_eq!(
            judged(family, protocol, target, 0)
                .unwrap_or_else(|error| panic!("{label} resolving to 0: {error}"))
                .health(),
            unresolved,
            "{label}"
        );
        assert_eq!(
            judged(family, protocol, target, 1)
                .unwrap_or_else(|error| panic!("{label} resolving to 1: {error}"))
                .health(),
            LinkHealth::Healthy,
            "{label}"
        );
        assert_eq!(
            judged(family, protocol, target, 2)
                .unwrap_or_else(|error| panic!("{label} resolving to 2: {error}"))
                .health(),
            LinkHealth::Ambiguous,
            "{label}"
        );
    }
}

/// A link addressed elsewhere carries no documents: `LinkRow::new` refuses a
/// nonzero head for one ([`ElsewhereNamesDocuments`]), over every other
/// combination of family, addressing and head size a row built here or read
/// off the wire accepts and round-trips.
#[test]
fn link_row_new_accepts_a_head_only_where_its_address_names_documents() {
    use LinkFamily::{Markdown, Wikilink};
    for (family, protocol, target) in [
        (Markdown, Some("https"), "example.com"),
        (Wikilink, Some("https"), "example.com"),
        (Markdown, None, "mailto:someone@example.com"),
        (Wikilink, Some("vault"), "notes/x"),
        (Markdown, Some("vault"), "notes/x.md?raw=1"),
        (Wikilink, None, ""),
        (Markdown, None, ""),
        (Wikilink, None, "notes/x"),
        (Markdown, None, "notes/x.md"),
        (Markdown, None, "/notes/x.md"),
        (Markdown, None, "picture.png"),
    ] {
        let elsewhere = LinkAddress::of(family, protocol, target) == LinkAddress::Elsewhere;
        let label = format!("{family:?} {protocol:?} `{target}`");
        for total in [0, 1, 2] {
            let candidates: Vec<Candidate> = (0..total)
                .map(|index| candidate(&format!("notes/c{index}")))
                .collect();
            let row = LinkRow::new(
                family,
                false,
                protocol.map(str::to_string),
                target,
                None,
                None,
                span(),
                head(candidates, total),
            );
            if elsewhere && total != 0 {
                let refusal: ElsewhereNamesDocuments =
                    row.expect_err(&format!("{label} accepted a head of {total}"));
                assert_eq!(refusal.total(), total, "{label} resolving to {total}");
            } else {
                round_trip(
                    &row.unwrap_or_else(|error| panic!("{label} resolving to {total}: {error}")),
                );
            }
        }
    }
}

/// A link that is not judged reads back only as not judged and only with no
/// document beside it; a document link reads back only with the health its
/// targets give it.
#[test]
fn a_links_judgement_is_read_back_off_its_addressing() {
    let json = addressed_row_json(Some("https"), "example.com", "not_judged", &[], 0);
    let read: LinkRow =
        serde_json::from_str(&json).unwrap_or_else(|error| panic!("reading {json}: {error}"));
    assert_eq!(read.health(), LinkHealth::NotJudged);
    let json = addressed_row_json(
        None,
        "pic.png",
        "healthy",
        &candidate_values(&["pic.png"]),
        1,
    );
    let read: LinkRow =
        serde_json::from_str(&json).unwrap_or_else(|error| panic!("reading {json}: {error}"));
    assert_eq!(read.health(), LinkHealth::Healthy);
    for json in [
        addressed_row_json(Some("https"), "example.com", "broken", &[], 0),
        addressed_row_json(
            Some("https"),
            "example.com",
            "not_judged",
            &candidate_values(&["example.md"]),
            1,
        ),
        addressed_row_json(None, "pic.png", "broken", &[], 0),
        addressed_row_json(None, "notes", "not_judged", &[], 0),
        addressed_row_json(
            None,
            "pic.png",
            "not_judged",
            &candidate_values(&["pic.png"]),
            1,
        ),
    ] {
        assert!(
            serde_json::from_str::<LinkRow>(&json).is_err(),
            "reading {json} produced a row whose judgement is not its addressing's"
        );
    }
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
    for selection in rung_selections() {
        round_trip(&SearchParams::new(vault.clone(), "norn").with_rungs(selection));
    }
    round_trip(
        &SearchParams::new(vault.clone(), "norn")
            .with_predicates(predicates())
            .with_rungs(RungSelection::exactly(
                RungSet::of(rungs()).expect("a ladder that runs a rung"),
            ))
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
/// `search` that named only its vault and its query selects the vault's
/// enabled set whole.
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
    assert_eq!(search.rungs, RungSelection::enabled());
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

/// A search runs at least one retrieval rung, so a set holding none is no
/// ladder: the empty set, and a set of enhancers alone, which have no
/// candidates to expand or re-order. Each refuses where one is built and where
/// one is read alike. A set holding a retrieval rung is a ladder, the floor or
/// no. The preset spellings are a surface's and never cross.
#[test]
fn a_rung_set_holding_no_retrieval_rung_is_no_ladder() {
    for enhancers in [
        vec![],
        vec![Rung::Rerank],
        vec![Rung::Expansion],
        vec![Rung::Expansion, Rung::Rerank],
    ] {
        assert_eq!(
            RungSet::of(enhancers.clone()),
            Err(NoRetrievalRung),
            "{enhancers:?} built as a ladder"
        );
    }
    assert_eq!(
        NoRetrievalRung::to_string(&NoRetrievalRung),
        "a search runs at least one retrieval rung: lexical or vector"
    );
    for unladdered in ["[]", r#"["rerank"]"#, r#"["expansion","rerank"]"#] {
        assert!(
            serde_json::from_str::<RungSet>(unladdered).is_err(),
            "`{unladdered}` read back as a ladder"
        );
    }
    assert_eq!(wire(&RungSet::lexical()), r#"["lexical"]"#);
    for (spelled, rungs) in [
        (r#"["vector"]"#, vec![Rung::Vector]),
        (r#"["vector","rerank"]"#, vec![Rung::Vector, Rung::Rerank]),
        (
            r#"["lexical","expansion"]"#,
            vec![Rung::Lexical, Rung::Expansion],
        ),
    ] {
        assert_eq!(
            serde_json::from_str::<RungSet>(spelled).expect("a ladder holding a retrieval rung"),
            RungSet::of(rungs).expect("a ladder holding a retrieval rung")
        );
    }
    assert!(
        serde_json::from_str::<RungSet>(r#"["hybrid"]"#).is_err(),
        "a preset spelling read back as a rung"
    );
    round_trip(&RungSet::of(rungs()).expect("a ladder holding a retrieval rung"));
}

/// A retrieval rung finds candidates of its own; an enhancer expands or
/// re-orders what a retrieval rung found.
#[test]
fn the_retrieval_rungs_are_the_lexical_floor_and_vectors() {
    let retrieving: Vec<Rung> = rungs()
        .into_iter()
        .filter(|rung| rung.retrieves())
        .collect();
    assert_eq!(retrieving, [Rung::Lexical, Rung::Vector]);
}

/// A set arrives holding each rung once, as the schema's `uniqueItems` says:
/// a set naming a rung twice is refused on read rather than folded, in an
/// exact selection and in a subtraction alike.
#[test]
fn a_rung_named_twice_is_refused_on_read() {
    for twice in [
        r#"["lexical","lexical"]"#,
        r#"["lexical","vector","lexical"]"#,
    ] {
        assert!(
            serde_json::from_str::<RungSet>(twice).is_err(),
            "`{twice}` read back as a set"
        );
    }
    for twice in [
        r#"{"select":"exactly","rungs":["lexical","lexical"]}"#,
        r#"{"select":"enabled","without":["vector","vector"]}"#,
        r#"{"select":"enabled","without":["rerank","vector","rerank"]}"#,
    ] {
        assert!(
            serde_json::from_str::<RungSelection>(twice).is_err(),
            "`{twice}` read back as a selection"
        );
    }
    assert!(
        serde_json::from_str::<RungSelection>(
            r#"{"select":"enabled","without":["rerank","vector"]}"#
        )
        .is_ok(),
        "a subtraction naming each rung once was refused"
    );
}

/// A subtraction leaving no retrieval rung selects no ladder whatever the vault
/// enables, so it is refused where one is built and where one is read alike.
/// A subtraction of one retrieval rung leaves the other.
#[test]
fn a_subtraction_of_every_retrieval_rung_is_refused() {
    for every in [
        vec![Rung::Lexical, Rung::Vector],
        vec![Rung::Lexical, Rung::Vector, Rung::Rerank],
    ] {
        assert_eq!(
            RungSelection::enabled_without(every.clone()),
            Err(NoRetrievalRung),
            "{every:?} built as a subtraction"
        );
    }
    for every in [
        r#"{"select":"enabled","without":["lexical","vector"]}"#,
        r#"{"select":"enabled","without":["vector","lexical","expansion"]}"#,
    ] {
        assert!(
            serde_json::from_str::<RungSelection>(every).is_err(),
            "`{every}` read back as a selection"
        );
    }
    for rung in [Rung::Lexical, Rung::Vector] {
        round_trip(
            &RungSelection::enabled_without([rung])
                .expect("a subtraction leaving a retrieval rung"),
        );
    }
}

/// A selection is the enabled set less the rungs it names, or exactly the
/// rungs it names, each an object tagged `select`. A request that both names a
/// set and subtracts from one has no spelling: each selection refuses the
/// other's field rather than dropping it, and an exact set naming no rung is
/// no ladder.
#[test]
fn a_rung_selection_subtracts_from_the_enabled_set_or_names_one_exactly() {
    for selection in rung_selections() {
        round_trip(&selection);
    }
    assert_eq!(
        wire(&RungSelection::enabled()),
        r#"{"select":"enabled","without":[]}"#
    );
    assert_eq!(
        wire(
            &RungSelection::enabled_without([Rung::Rerank, Rung::Vector])
                .expect("a subtraction leaving a retrieval rung")
        ),
        r#"{"select":"enabled","without":["vector","rerank"]}"#
    );
    assert_eq!(
        wire(&RungSelection::exactly(RungSet::lexical())),
        r#"{"select":"exactly","rungs":["lexical"]}"#
    );
    for combined in [
        r#"{"select":"exactly","rungs":["lexical"],"without":["vector"]}"#,
        r#"{"select":"enabled","without":["vector"],"rungs":["lexical"]}"#,
    ] {
        assert!(
            serde_json::from_str::<RungSelection>(combined).is_err(),
            "`{combined}` read back as a selection"
        );
    }
    for lax in [
        r#"{"select":"exactly","rungs":[]}"#,
        r#"{"select":"exactly","rungs":["rerank"]}"#,
        r#"{"select":"hybrid"}"#,
        r#"{"select":"enabled"}"#,
    ] {
        assert!(
            serde_json::from_str::<RungSelection>(lax).is_err(),
            "`{lax}` read back as a selection"
        );
    }
}

/// The set carries the rungs in ladder order whatever order a caller named
/// them in, and it holds each rung once.
#[test]
fn a_rung_set_is_the_resolved_set_in_ladder_order() {
    let named = RungSet::of([Rung::Rerank, Rung::Lexical, Rung::Rerank, Rung::Vector])
        .expect("a ladder that runs a rung");
    assert_eq!(wire(&named), r#"["lexical","vector","rerank"]"#);
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
/// where the document carries no scalar value for that key — so a reader lines
/// the tuple up with the request rather than guessing which key a short tuple
/// skipped.
#[test]
fn a_tally_carries_one_value_per_group_key() {
    assert_eq!(
        wire(&Tally::new([Some("note".to_string()), None], 7)),
        r#"{"group":["note",null],"count":7}"#
    );
}

/// The row and the key it stops at spell the grouping tuple one way, and
/// `Tally::cursor_key` is the one function that turns one into the other: a
/// group member for which the document carries no scalar value is `null` in
/// the key as it is on the row, so a continuation names the position the page
/// reached.
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
    assert_eq!(
        wire(&Facet::creation_rule(
            "task",
            "tasks/{{seq}}.md",
            vec!["title".to_string()],
            ValueMap::new([
                ("status".to_string(), AuthoredValue::string("todo")),
                (
                    "meta".to_string(),
                    AuthoredValue::list([AuthoredValue::Integer(1), AuthoredValue::Null]),
                ),
            ])
            .expect("each key once"),
            Some("# {{var.title}}\n".to_string()),
        )),
        r##"{"facet":"creation_rule","name":"task","target":"tasks/{{seq}}.md","variables":["title"],"frontmatter_defaults":{"status":"todo","meta":[1,null]},"body":"# {{var.title}}\n"}"##,
        "a creation rule reports its templates as their source text and its defaults as the values they are written as"
    );
    assert_eq!(
        wire(&Facet::inbox("inbox/{{seq}}.md")),
        r#"{"facet":"inbox","target":"inbox/{{seq}}.md"}"#
    );
    assert_eq!(
        wire(&Facet::observed_field(
            "aliases",
            [
                ContainerKind::Sequence,
                ContainerKind::Scalar,
                ContainerKind::Sequence
            ]
        )),
        r#"{"facet":"observed_field","key":"aliases","containers":["scalar","sequence"]}"#,
        "an observed field lists each container once, in the vocabulary's order"
    );
    for kind in facet_kinds() {
        assert_eq!(kind.as_str(), flat_string(&kind));
    }
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
            Facet::observed_field("author", [ContainerKind::Sequence]),
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
        (
            Facet::creation_rule(
                "task",
                "tasks/{{seq}}.md",
                Vec::new(),
                ValueMap::default(),
                None,
            ),
            FacetKind::CreationRule,
            "task",
        ),
        (
            Facet::inbox("inbox/{{seq}}.md"),
            FacetKind::Inbox,
            "inbox/{{seq}}.md",
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
const PINNED_AFTER: &str = r##"eyJzbmFwc2hvdCI6eyJlcG9jaCI6ImVwb2NoLTEiLCJnZW5lcmF0aW9uIjoxMiwic2NoZW1hX2ZpbmdlcnByaW50IjoiZnAtMSIsInNpZGVjYXJfcmV2aXNpb24iOnsiZXBvY2giOiJzaWRlY2FyLTEiLCJyZXZpc2lvbiI6NH19LCJrZXkiOnsicm93IjoiZG9jdW1lbnQiLCJvcmRlciI6eyJrZXkiOnsiYnkiOiJmaWVsZCIsImtleSI6ImR1ZSJ9LCJkaXJlY3Rpb24iOiJkZXNjZW5kaW5nIn0sInNvcnQiOiIyMDI2LTAxLTAxIiwicGF0aCI6Im5vdGVzL2EubWQifX0"##;

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
        .with_rungs(RungSelection::exactly(
            RungSet::of(rungs()).expect("a ladder that runs a rung"),
        ))
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
            r##","rungs":{"select":"exactly","rungs":["lexical","vector","expansion","rerank"]},"min_score":0.25,"columns":"##,
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
            r##","kinds":["document/path-bytes-not-utf8","document/path-names-no-document","document/body-bytes-not-utf8","document/frontmatter-too-large","document/frontmatter-unclosed","document/frontmatter-unreadable","document/undeclared-tag","link/broken","link/ambiguous","link/missing-anchor"],"severity":"error","summary":true,"limit":20,"after":""##,
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
            r##","facets":["declared_field","observed_field","declared_tag","folder","path_rule","tag_pattern","undeclared_tags","creation_rule","inbox"],"limit":20,"after":""##,
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
            r##"{"path":"notes/a.md","fields":{"type":{"kind":"scalar","raw":"note"}},"body":{"text":"Design\n","byte_length":4096},"links":{"items":[{"family":"wikilink","embed":false,"protocol":null,"target":"a","title":"A","anchor":{"kind":"heading","text":"Design"},"span":{"line":3,"column":1,"byte_offset":42},"targets":{"candidates":[],"total":0},"health":"broken"},{"family":"wikilink","embed":false,"protocol":null,"target":"a","title":"A","anchor":{"kind":"heading","text":"Design"},"span":{"line":3,"column":1,"byte_offset":42},"targets":{"candidates":[{"path":"notes/a.md","suffix":"notes/a"}],"total":1},"health":"healthy"},{"family":"wikilink","embed":false,"protocol":null,"target":"a","title":"A","anchor":{"kind":"heading","text":"Design"},"span":{"line":3,"column":1,"byte_offset":42},"targets":{"candidates":[{"path":"notes/a.md","suffix":"notes/a"},{"path":"archive/a.md","suffix":"archive/a"}],"total":2},"health":"ambiguous"},{"family":"markdown","embed":false,"protocol":"https","target":"example.com/page","title":"","anchor":null,"span":{"line":3,"column":1,"byte_offset":42},"targets":{"candidates":[],"total":0},"health":"not_judged"}],"total":9},"headings":{"items":[{"level":2,"text":"Design","slug":"design","span":{"line":3,"column":1,"byte_offset":42}}],"total":1},"blocks":{"items":[{"id":"a1","span":null}],"total":1},"tags":{"items":[{"name":"draft","source":"frontmatter","span":{"line":3,"column":1,"byte_offset":42}}],"total":1},"findings":{"items":[{"id":7,"kind":"document/undeclared-tag","severity":"warning","path":"notes/a.md","target":"draft","span":{"line":3,"column":1,"byte_offset":42},"head":{"candidates":[{"path":"notes/glossary.md","suffix":"notes/glossary"},{"path":"archive/glossary.md","suffix":"archive/glossary"}],"total":9},"hint":{"hint":"resolves","target":"glossary"},"message":"the tag is not declared","generation":12}],"total":1}}"##,
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
        Advisory::schema_absent(".norn/schema.yaml"),
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
        RegistryProblem::duplicate_root(names([name("notes"), name("vault")])),
        RegistryProblem::shared_schema(names([name("notes"), name("vault")])),
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
fn changes() -> Vec<VaultChange<SchemaSource>> {
    vec![
        VaultChange::keep(),
        VaultChange::set(schema_sources().remove(0)),
        VaultChange::clear(),
    ]
}

/// Every change an edit does to a field with no default.
fn replacements() -> Vec<VaultReplace<VaultRoot>> {
    vec![
        VaultReplace::keep(),
        VaultReplace::set(vault_roots().remove(1)),
    ]
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
    round_trip(&VaultSetParams::new(name("notes")));
    round_trip(
        &VaultSetParams::new(name("notes"))
            .with_root(replacements().remove(1))
            .with_schema_source(changes().remove(2))
            .with_poll_backend(VaultChange::set(PollBackend::Poll)),
    );
    round_trip(&VaultSetReport::new(
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
        tag_string(&Published::parked(park()), "answer"),
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
    assert_eq!(
        wire(&Advisory::schema_absent(".norn/schema.yaml")),
        r#"{"kind":"schema_absent","path":".norn/schema.yaml"}"#
    );
    assert!(
        Advisory::schema_absent(".norn/schema.yaml").wants_attention(),
        "a vault declaring no schema is one an operator acts on"
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

/// **An untrusted state that tells the vault's last reload failure is that
/// failure, named once**: the roll-up names the reload failure alone. An
/// untrusted state with a cause of its own beside a reload failure is two
/// causes, and is named for both.
#[test]
fn an_untrusted_state_telling_the_last_reload_failure_is_named_once() {
    let failure = ControlFileFailure::new(
        ControlFile::Config,
        ReloadStage::Read,
        "the vault config cannot be read",
    );
    let untrusted_for = |detail: &str| {
        VaultStatus::new(
            registrations().remove(0),
            Published::state(TrustState::untrusted(
                UntrustedReason::environmental_refusal(detail),
            )),
            Drift::current(),
            EngineStatus::off(),
            EngineSection::absent(),
        )
        .with_last_reload_failure(failure.clone())
    };
    assert_eq!(
        RollUp::of(&[untrusted_for("the vault config cannot be read")]).attention(),
        [Attention::reload_failed(name("notes"), failure.clone())]
    );
    assert_eq!(
        RollUp::of(&[untrusted_for("the disk is full")]).attention(),
        [
            Attention::untrusted(
                name("notes"),
                UntrustedReason::environmental_refusal("the disk is full")
            ),
            Attention::reload_failed(name("notes"), failure),
        ]
    );
}

/// **`doctor` names a duplicate root once, among the registry's problems**:
/// the park that duplicate raises on each name the problem names is left
/// out of the roll-up's attention, and a duplicate-root park on a name no
/// problem names is kept, as is every other reason. The counts are the
/// roll-up's own.
#[test]
fn doctor_names_a_duplicate_root_once_among_the_registry_problems() {
    let parked_on_duplicate = |vault: &str| {
        VaultStatus::new(
            Registration::new(name(vault), vault_roots().remove(1)),
            Published::parked(ErrorEnvelope::new(
                "this vault's root is reached by another registration",
                ErrorDetail::duplicate_root(names([name("alpha"), name("beta")])),
            )),
            Drift::reload_pending(),
            EngineStatus::off(),
            EngineSection::absent(),
        )
    };
    let statuses = [
        parked_on_duplicate("alpha"),
        parked_on_duplicate("beta"),
        parked_on_duplicate("gamma"),
    ];
    let roll_up = RollUp::of(&statuses);
    let report = DoctorRegistryReport::new(
        roll_up.clone(),
        RegistrySanity::problems([RegistryProblem::duplicate_root(names([
            name("alpha"),
            name("beta"),
        ]))])
        .expect("problems that name one"),
        [],
    );

    assert_eq!(report.roll_up.parked(), roll_up.parked());
    assert_eq!(
        report.roll_up.attention(),
        [
            Attention::reload_pending(name("alpha")),
            Attention::reload_pending(name("beta")),
            Attention::parked(name("gamma"), ReasonCode::HostDuplicateRoot),
            Attention::reload_pending(name("gamma")),
        ]
    );
}

/// **`doctor` names a root it cannot read once, among the registry's
/// problems**: the park the entry raises when it cannot read its root's
/// identity is left out of the roll-up's attention for each name a missing or
/// unreadable root names, and kept for a name no such problem names. A park
/// of any other code on such a name is a different cause and is kept. The
/// counts are the roll-up's own.
#[test]
fn doctor_names_an_unreadable_root_once_among_the_registry_problems() {
    let parked_on_identity = |vault: &str| {
        VaultStatus::new(
            Registration::new(name(vault), vault_roots().remove(1)),
            Published::parked(ErrorEnvelope::new(
                "the registry cannot read this vault's root",
                ErrorDetail::entry_untrusted(UntrustedReason::environmental_refusal(
                    "the root cannot be read",
                )),
            )),
            Drift::reload_pending(),
            EngineStatus::off(),
            EngineSection::absent(),
        )
    };
    let parked_on_contention = VaultStatus::new(
        Registration::new(name("delta"), vault_roots().remove(1)),
        Published::parked(ErrorEnvelope::new(
            "another process maintains this vault's derived state",
            ErrorDetail::maintainer_contended(MaintainerIdentity::unknown()),
        )),
        Drift::reload_pending(),
        EngineStatus::off(),
        EngineSection::absent(),
    );
    let statuses = [
        parked_on_identity("alpha"),
        parked_on_identity("beta"),
        parked_on_contention,
        parked_on_identity("gamma"),
    ];
    let roll_up = RollUp::of(&statuses);
    let report = DoctorRegistryReport::new(
        roll_up.clone(),
        RegistrySanity::problems([
            RegistryProblem::root_missing(name("alpha")),
            RegistryProblem::root_unreadable(name("beta"), "permission denied"),
            RegistryProblem::root_missing(name("delta")),
        ])
        .expect("problems that name one"),
        [],
    );

    assert_eq!(report.roll_up.parked(), roll_up.parked());
    assert_eq!(
        report.roll_up.attention(),
        [
            Attention::reload_pending(name("alpha")),
            Attention::reload_pending(name("beta")),
            Attention::parked(name("delta"), ReasonCode::HostMaintainerContended),
            Attention::reload_pending(name("delta")),
            Attention::parked(name("gamma"), ReasonCode::HostEntryUntrusted),
            Attention::reload_pending(name("gamma")),
        ]
    );
}

/// **A vault-local shadow fallback wants attention only where the vault does
/// not ignore it.** Both are reported on the vault's status; the roll-up
/// names the vault for the one whose staged shadows the vault's own tooling
/// will pick up.
#[test]
fn a_fallback_wants_attention_only_where_the_vault_does_not_ignore_it() {
    let falling_back = |gitignored| {
        VaultStatus::new(
            registrations().remove(0),
            Published::state(TrustState::Ready),
            Drift::current(),
            EngineStatus::off(),
            EngineSection::absent(),
        )
        .with_advisories([Advisory::tmp_fallback_in_use(".norn/tmp/key", gitignored)])
    };
    assert!(RollUp::of(&[falling_back(true)]).attention().is_empty());
    assert_eq!(
        RollUp::of(&[falling_back(false)]).attention(),
        [Attention::advisory(
            name("notes"),
            Advisory::tmp_fallback_in_use(".norn/tmp/key", false)
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
        wire(&VaultChange::<SchemaSource>::keep()),
        r#"{"change":"keep"}"#
    );
    assert_eq!(
        wire(&VaultChange::set(schema_sources().remove(0))),
        r#"{"change":"set","value":"/home/person/.config/norn/schemas/work.yaml"}"#
    );
    assert_eq!(
        wire(&VaultChange::<SchemaSource>::clear()),
        r#"{"change":"clear"}"#
    );
    assert_eq!(VaultChange::<SchemaSource>::default(), VaultChange::keep());
    assert_eq!(VaultReplace::<VaultRoot>::default(), VaultReplace::keep());
}

/// A registration cannot be without a root, so the root's edit has no clear to
/// spell and a reader refuses one rather than a handler rejecting it later.
#[test]
fn a_root_cannot_be_cleared() {
    assert_eq!(
        serde_json::from_str::<VaultReplace<VaultRoot>>(r#"{"change":"keep"}"#)
            .expect("keeping the root"),
        VaultReplace::keep()
    );
    assert!(
        serde_json::from_str::<VaultReplace<VaultRoot>>(r#"{"change":"clear"}"#).is_err(),
        "a cleared root read back as a replacement"
    );
    assert!(
        serde_json::from_str::<VaultChange<SchemaSource>>(r#"{"change":"clear"}"#).is_ok(),
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
    let set = VaultSetParams::new(name("notes"));
    assert_eq!(set.root, VaultReplace::keep());
    assert_eq!(set.schema_source, VaultChange::keep());
    assert_eq!(set.poll_backend, VaultChange::keep());
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
    let request = VaultSetParams::new(name("notes"))
        .with_root(replacements().remove(1))
        .with_schema_source(VaultChange::clear())
        .with_poll_backend(VaultChange::set(PollBackend::Poll));
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
        wire(&RegistryProblem::duplicate_root(names([
            name("vault"),
            name("notes")
        ]))),
        r#"{"problem":"duplicate_root","aliases":["notes","vault"]}"#
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
        RegistrySanity::problems([RegistryProblem::duplicate_root(names([
            name("vault"),
            name("notes"),
        ]))])
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

// ── The plan grammars ────────────────────────────────────────────────────

/// A hash of some bytes, spelled from a digest this coverage chose.
fn content_hash(fill: u8) -> ContentHash {
    ContentHash::from_sha256([fill; 32])
}

/// The hash of the empty file, as SHA-256 spells it.
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// The digest of the empty file, as the 32 bytes SHA-256 produces.
fn empty_sha256_digest() -> [u8; 32] {
    let mut digest = [0u8; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte =
            u8::from_str_radix(&EMPTY_SHA256[index * 2..index * 2 + 2], 16).expect("a hex byte");
    }
    digest
}

/// A hash built from the 32 bytes of a digest is the prefixed, lowercase hex
/// string a hash is on the wire, and reads back as the same hash.
#[test]
fn a_content_hash_is_the_prefixed_hex_of_its_digest() {
    let hash = ContentHash::from_sha256(empty_sha256_digest());
    assert_eq!(hash.as_str(), format!("sha256:{EMPTY_SHA256}"));
    assert_eq!(hash.hex(), EMPTY_SHA256);
    assert_eq!(wire(&hash), format!("\"sha256:{EMPTY_SHA256}\""));
    assert_eq!(
        ContentHash::new(format!("sha256:{EMPTY_SHA256}")),
        Ok(hash.clone())
    );
    round_trip(&hash);
    round_trip(&content_hash(0));
    round_trip(&content_hash(0xff));
}

/// The reader refuses what the constructor refuses: another prefix, another
/// case, another length, and another algorithm's hash of the same width.
#[test]
fn a_content_hash_refuses_a_string_outside_its_grammar() {
    let blake3_looking = format!("blake3:{EMPTY_SHA256}");
    let refused = [
        String::new(),
        EMPTY_SHA256.to_string(),
        format!("SHA256:{EMPTY_SHA256}"),
        format!("sha256:{}", EMPTY_SHA256.to_uppercase()),
        format!("sha256:{}", &EMPTY_SHA256[..63]),
        format!("sha256:{EMPTY_SHA256}0"),
        format!("sha256:{}g", &EMPTY_SHA256[..63]),
        format!("sha256: {}", &EMPTY_SHA256[..63]),
        blake3_looking,
    ];
    for text in refused {
        assert_eq!(
            ContentHash::new(&text),
            Err(IllegalContentHash),
            "`{text}` was built as a content hash"
        );
        assert!(
            serde_json::from_str::<ContentHash>(&format!("\"{text}\"")).is_err(),
            "`{text}` was read back as a content hash"
        );
    }
}

/// A root identity is one opaque string built from a device and an inode,
/// the same pair building the same identity and another pair another.
#[test]
fn a_root_identity_is_one_opaque_string_per_device_and_inode() {
    let identity = RootIdentity::from_device_and_inode(66_306, 2);
    assert_eq!(wire(&identity), r#""00000000000103020000000000000002""#);
    assert_eq!(identity, RootIdentity::from_device_and_inode(66_306, 2));
    assert_ne!(identity, RootIdentity::from_device_and_inode(66_306, 3));
    assert_ne!(identity, RootIdentity::from_device_and_inode(2, 66_306));
    for (device, inode) in [(0, 0), (u64::MAX, u64::MAX), (66_306, 2)] {
        round_trip(&RootIdentity::from_device_and_inode(device, inode));
    }
}

/// The reader accepts exactly the strings the constructor can build, so an
/// identity nobody built is refused rather than compared.
#[test]
fn a_root_identity_refuses_a_string_nobody_built() {
    for text in [
        "",
        "junk",
        "0000000000010302000000000000000",
        "000000000001030200000000000000020",
        "00000000000103020000000000000002 ",
        "0000000000010302000000000000000G",
        "00000000000103020000000000000O02",
        "0000000000010302:000000000000002",
        "00000000000103020000000000000A02",
    ] {
        assert!(
            serde_json::from_str::<RootIdentity>(&format!("\"{text}\"")).is_err(),
            "`{text}` was read back as a root identity"
        );
    }
}

// ── The plan documents ───────────────────────────────────────────────────

/// The wire spelling of the hash `content_hash(fill)` builds.
fn hash_text(fill: u8) -> String {
    format!("sha256:{}", format!("{fill:02x}").repeat(32))
}

fn operation_id(text: &str) -> OperationId {
    OperationId::new(text).expect("a legal operation id")
}

/// The identity of the root every plan here is made against.
fn a_root() -> RootIdentity {
    RootIdentity::from_device_and_inode(66_306, 2)
}

/// The predicate list a `where` target here matches by.
fn drafts() -> Vec<Predicate> {
    vec![Predicate::equal_to("status", "draft")]
}

/// Every kind an operation is authored in, one each; the frontmatter kinds
/// name their documents by path and by predicate list between them.
fn operation_kinds() -> Vec<OperationKind> {
    vec![
        OperationKind::create_document(path("notes/new.md"), "# New\n"),
        OperationKind::create_by_rule(
            Some("meeting".to_string()),
            variables(&[("project", "norn")]),
            a_value_map(),
            Some("Agenda.\n".to_string()),
        ),
        OperationKind::str_replace(path("notes/a.md"), "draft", "final"),
        OperationKind::move_document(path("notes/a.md"), path("archive/a.md")),
        OperationKind::delete_document(path("notes/b.md")),
        OperationKind::move_folder(folder("notes"), folder("archive/notes")),
        OperationKind::rewrite_link(path("notes/c.md"), LinkFamily::Wikilink, "a", "archive/a"),
        OperationKind::rewrite_wikilink(target("a"), target("archive/a")),
        OperationKind::set_frontmatter(
            WriteTarget::path(path("notes/a.md")),
            "status",
            AuthoredValue::string("done"),
        ),
        OperationKind::remove_frontmatter(WriteTarget::matching(drafts()), "due"),
        OperationKind::push_frontmatter(
            WriteTarget::path(path("notes/a.md")),
            "tags",
            AuthoredValue::string("project"),
        ),
        OperationKind::pop_frontmatter(
            WriteTarget::matching(drafts()),
            "tags",
            AuthoredValue::string("stale"),
        ),
        OperationKind::replace_body(path("notes/a.md"), "Body.\n"),
        OperationKind::replace_section(path("notes/a.md"), "Notes", "New notes.\n"),
        OperationKind::append_to_section(path("notes/a.md"), "Log", "- done\n"),
        OperationKind::delete_section(path("notes/a.md"), "Scratch"),
        OperationKind::insert_before_heading(path("notes/a.md"), "Notes", "Intro.\n"),
        OperationKind::insert_after_heading(path("notes/a.md"), "Notes", "First.\n"),
        OperationKind::write_control_file(ControlFile::Schema, "version: 1\n"),
    ]
}

/// The variables `entries` names, in the order given.
fn variables(entries: &[(&str, &str)]) -> Variables {
    Variables::new(
        entries
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string())),
    )
    .expect("variables each named once")
}

/// Frontmatter field values with a nested map and list, in a written order.
fn a_value_map() -> ValueMap {
    ValueMap::new([
        ("status".to_string(), AuthoredValue::string("draft")),
        (
            "owner".to_string(),
            AuthoredValue::map([
                ("name".to_string(), AuthoredValue::string("drew")),
                (
                    "tags".to_string(),
                    AuthoredValue::list([AuthoredValue::string("a"), AuthoredValue::string("b")]),
                ),
            ])
            .expect("a map of distinct keys"),
        ),
    ])
    .expect("a map of distinct keys")
}

/// Every condition an author writes on an operation, the expected value
/// both present and absent.
fn author_conditions() -> Vec<AuthorCondition> {
    vec![
        AuthorCondition::content_hash(path("notes/a.md"), content_hash(0xab)),
        AuthorCondition::expected_value(
            path("notes/a.md"),
            "status",
            ExpectedField::present(AuthoredValue::string("draft")),
        ),
        AuthorCondition::expected_value(path("notes/a.md"), "owner", ExpectedField::absent()),
    ]
}

/// The link `[[a]]` in `notes/c.md`.
fn a_link_key() -> LinkKey {
    LinkKey::new(path("notes/c.md"), LinkFamily::Wikilink, "a")
}

/// Every condition a resolved plan carries: a file's content, and an entry of
/// the resolution change set over every resolution on each side.
fn plan_conditions() -> Vec<PlanCondition> {
    vec![
        PlanCondition::content_hash(path("notes/c.md"), content_hash(0xcd)),
        PlanCondition::link_resolution(
            a_link_key(),
            Resolves::one(path("notes/a.md")),
            Resolves::none(),
        ),
        PlanCondition::link_resolution(
            LinkKey::new(path("notes/d.md"), LinkFamily::Markdown, "vault://notes/a"),
            Resolves::several(),
            Resolves::one(path("archive/a.md")),
        ),
    ]
}

/// Every state a side of a transition holds.
fn file_states() -> Vec<FileState> {
    vec![
        FileState::absent(),
        FileState::present(content_hash(0x01)),
        FileState::quarantined(content_hash(0x02)),
    ]
}

/// The link cascade a move of `notes/a.md` to `archive/a.md` carries: one
/// rewrite per document holding a link to it.
fn a_cascade() -> Vec<LinkRewrite> {
    vec![
        LinkRewrite::new(path("notes/c.md"), LinkFamily::Wikilink, "a", "archive/a"),
        LinkRewrite::new(
            path("notes/d.md"),
            LinkFamily::Markdown,
            "a.md",
            "../archive/a.md",
        ),
    ]
}

/// Every kind bare, a move carrying its cascade, and one operation carrying
/// every part an author writes.
fn operations() -> Vec<Operation> {
    let mut operations: Vec<Operation> =
        operation_kinds().into_iter().map(Operation::new).collect();
    operations.push(
        Operation::new(OperationKind::move_document(
            path("notes/a.md"),
            path("archive/a.md"),
        ))
        .with_cascade(a_cascade()),
    );
    operations.push(
        Operation::new(OperationKind::str_replace(
            path("notes/a.md"),
            "draft",
            "final",
        ))
        .with_id(operation_id("edit-a"))
        .with_requires(vec![operation_id("make-b")])
        .with_footnote("marks it final")
        .with_conditions(author_conditions()),
    );
    operations
}

fn a_transition() -> Transition {
    Transition::new(
        path("notes/a.md"),
        FileState::present(content_hash(0xab)),
        FileState::present(content_hash(0x01)),
    )
}

/// A resolved plan carrying one of everything it can carry.
fn a_resolved_plan() -> ResolvedPlan {
    ResolvedPlan::new(
        VaultAddress::name(name("notes")),
        a_root(),
        vec![Operation::new(OperationKind::str_replace(
            path("notes/a.md"),
            "draft",
            "final",
        ))],
        vec![a_transition()],
        plan_conditions(),
    )
    .with_provenance(Provenance::new(
        7,
        vec![SkippedFinding::new(42, "the target names two documents")],
    ))
    .with_footnote("finish the draft")
}

/// The fewest parts a resolved plan is written with.
fn a_bare_resolved_plan() -> ResolvedPlan {
    ResolvedPlan::new(
        VaultAddress::name(name("notes")),
        a_root(),
        vec![Operation::new(OperationKind::delete_document(path(
            "notes/b.md",
        )))],
        vec![Transition::new(
            path("notes/b.md"),
            FileState::present(content_hash(0x02)),
            FileState::absent(),
        )],
        Vec::new(),
    )
}

fn an_authored_plan() -> AuthoredPlan {
    AuthoredPlan::new(VaultAddress::name(name("notes")), operations())
}

/// Both documents a caller holds, each bare and each carrying every part.
fn plan_documents() -> Vec<PlanDocument> {
    vec![
        PlanDocument::operations(an_authored_plan()),
        PlanDocument::operations(an_authored_plan().with_footnote("tidy the notes")),
        PlanDocument::resolved(a_bare_resolved_plan()),
        PlanDocument::resolved(a_resolved_plan()),
    ]
}

/// The pinned bytes of `a_resolved_plan`, without the document's tag.
fn resolved_plan_json() -> String {
    format!(
        concat!(
            r#"{{"plan":"resolved","vault":{{"by":"name","name":"notes"}},"root":"00000000000103020000000000000002","#,
            r#""operations":[{{"kind":"str_replace","fields":{{"path":"notes/a.md","old_str":"draft","new_str":"final"}}}}],"#,
            r#""transitions":[{{"path":"notes/a.md","before":{{"state":"present","hash":"{ab}"}},"#,
            r#""after":{{"state":"present","hash":"{one}"}}}}],"#,
            r#""conditions":[{{"condition":"content_hash","path":"notes/c.md","hash":"{cd}"}},"#,
            r#"{{"condition":"link_resolution","link":{{"holder":"notes/c.md","syntax":"wikilink","address":"a"}},"#,
            r#""before":{{"resolves":"one","path":"notes/a.md"}},"after":{{"resolves":"none"}}}},"#,
            r#"{{"condition":"link_resolution","link":{{"holder":"notes/d.md","syntax":"markdown","address":"vault://notes/a"}},"#,
            r#""before":{{"resolves":"several"}},"after":{{"resolves":"one","path":"archive/a.md"}}}}],"#,
            r#""provenance":{{"finding_generation":7,"skipped":[{{"finding":42,"reason":"the target names two documents"}}]}},"#,
            r#""footnote":"finish the draft"}}"#
        ),
        ab = hash_text(0xab),
        one = hash_text(0x01),
        cd = hash_text(0xcd),
    )
}

#[test]
fn every_plan_shape_survives_the_round_trip() {
    for kind in operation_kinds() {
        round_trip(&kind);
    }
    for operation in operations() {
        round_trip(&operation);
    }
    for condition in author_conditions() {
        round_trip(&condition);
    }
    for condition in plan_conditions() {
        round_trip(&condition);
    }
    for state in file_states() {
        round_trip(&state);
    }
    round_trip(&a_transition());
    round_trip(&a_resolved_plan());
    round_trip(&a_bare_resolved_plan());
    round_trip(&an_authored_plan());
    for document in plan_documents() {
        round_trip(&document);
    }
}

/// Each kind is its name under `kind` and its own fields under `fields`, and
/// a bare operation carries nothing else.
#[test]
fn an_operation_is_a_kind_and_its_fields() {
    let pinned = [
        r##"{"kind":"create_document","fields":{"path":"notes/new.md","content":"# New\n"}}"##,
        r#"{"kind":"create_by_rule","fields":{"rule":"meeting","variables":{"project":"norn"},"fields":{"status":"draft","owner":{"name":"drew","tags":["a","b"]}},"body":"Agenda.\n"}}"#,
        r#"{"kind":"str_replace","fields":{"path":"notes/a.md","old_str":"draft","new_str":"final"}}"#,
        r#"{"kind":"move_document","fields":{"from":"notes/a.md","to":"archive/a.md"}}"#,
        r#"{"kind":"delete_document","fields":{"path":"notes/b.md"}}"#,
        r#"{"kind":"move_folder","fields":{"from":"notes","to":"archive/notes"}}"#,
        r#"{"kind":"rewrite_link","fields":{"path":"notes/c.md","syntax":"wikilink","from":"a","to":"archive/a"}}"#,
        r#"{"kind":"rewrite_wikilink","fields":{"old":"a","new":"archive/a"}}"#,
        r#"{"kind":"set_frontmatter","fields":{"path":"notes/a.md","field":"status","value":"done"}}"#,
        r#"{"kind":"remove_frontmatter","fields":{"where":[{"op":"eq","key":"status","value":"draft"}],"field":"due"}}"#,
        r#"{"kind":"push_frontmatter","fields":{"path":"notes/a.md","field":"tags","value":"project"}}"#,
        r#"{"kind":"pop_frontmatter","fields":{"where":[{"op":"eq","key":"status","value":"draft"}],"field":"tags","value":"stale"}}"#,
        r#"{"kind":"replace_body","fields":{"path":"notes/a.md","content":"Body.\n"}}"#,
        r#"{"kind":"replace_section","fields":{"path":"notes/a.md","heading":"Notes","content":"New notes.\n"}}"#,
        r#"{"kind":"append_to_section","fields":{"path":"notes/a.md","heading":"Log","content":"- done\n"}}"#,
        r#"{"kind":"delete_section","fields":{"path":"notes/a.md","heading":"Scratch"}}"#,
        r#"{"kind":"insert_before_heading","fields":{"path":"notes/a.md","heading":"Notes","content":"Intro.\n"}}"#,
        r#"{"kind":"insert_after_heading","fields":{"path":"notes/a.md","heading":"Notes","content":"First.\n"}}"#,
        r#"{"kind":"write_control_file","fields":{"file":"schema","content":"version: 1\n"}}"#,
    ];
    assert_eq!(operation_kinds().len(), pinned.len());
    for (kind, json) in operation_kinds().into_iter().zip(pinned) {
        assert_eq!(wire(&Operation::new(kind.clone())), json);
        assert_eq!(wire(&kind), json);
    }
}

/// An operation's optional parts sit beside its kind and its fields, and each
/// is left out of the bytes where it is not written.
#[test]
fn an_operation_carries_its_identifier_requirements_footnote_and_conditions() {
    let full = operations()
        .pop()
        .expect("the operation carrying every part");
    assert_eq!(
        wire(&full),
        format!(
            concat!(
                r#"{{"kind":"str_replace","fields":{{"path":"notes/a.md","old_str":"draft","new_str":"final"}},"#,
                r#""id":"edit-a","requires":["make-b"],"footnote":"marks it final","#,
                r#""conditions":[{{"condition":"content_hash","path":"notes/a.md","hash":"{ab}"}},"#,
                r#"{{"condition":"expected_value","path":"notes/a.md","field":"status","expect":{{"state":"present","value":"draft"}}}},"#,
                r#"{{"condition":"expected_value","path":"notes/a.md","field":"owner","expect":{{"state":"absent"}}}}]}}"#
            ),
            ab = hash_text(0xab)
        )
    );
}

/// A `fields` key written before the `kind` it belongs to still reads.
#[test]
fn an_operation_reads_its_fields_before_its_kind() {
    let read: Operation = serde_json::from_str(
        r#"{"fields":{"from":"notes/a.md","to":"archive/a.md"},"id":"move-a","kind":"move_document"}"#,
    )
    .expect("an operation whose fields precede its kind");
    assert_eq!(
        read,
        Operation::new(OperationKind::move_document(
            path("notes/a.md"),
            path("archive/a.md")
        ))
        .with_id(operation_id("move-a"))
    );
}

/// A kind's fields are its own: a field it lacks, another kind's field, and
/// an identifier that names nothing each refuse the read.
#[test]
fn an_operation_refuses_fields_that_are_not_its_kinds() {
    for json in [
        r#"{"kind":"delete_document","fields":{}}"#,
        r#"{"kind":"delete_document","fields":{"from":"notes/a.md","to":"archive/a.md"}}"#,
        r#"{"kind":"create_document","fields":{"path":"notes/new.md"}}"#,
        r#"{"kind":"delete_document"}"#,
        r#"{"kind":"delete_document","fields":null}"#,
        r#"{"kind":"delete_document","fields":{"path":""}}"#,
        r#"{"kind":"delete_document","fields":{"path":"notes/b.md"},"id":""}"#,
        r#"{"kind":"delete_document","fields":{"path":"notes/b.md"},"requires":[""]}"#,
        r#"{"fields":{"path":"notes/b.md"}}"#,
    ] {
        assert!(
            serde_json::from_str::<Operation>(json).is_err(),
            "reading {json} produced an operation"
        );
    }
    assert_eq!(OperationId::new(""), Err(IllegalOperationId));
}

/// **A delete says out loud what becomes of the links naming its
/// document.** It rewrites them to `rewrite_to`, or leaves them broken where
/// `allow_broken_links` is `true`; each is left out where it is not written,
/// `false` reads as absent, and saying both is refused at the read.
#[test]
fn a_delete_rewrites_or_breaks_its_backlinks_only_where_it_says_so() {
    let rewriting = OperationKind::delete_document_rewriting(path("notes/b.md"), target("c"));
    let breaking = OperationKind::delete_document_breaking_links(path("notes/b.md"));
    for (kind, json) in [
        (
            &rewriting,
            r#"{"kind":"delete_document","fields":{"path":"notes/b.md","rewrite_to":"c"}}"#,
        ),
        (
            &breaking,
            r#"{"kind":"delete_document","fields":{"path":"notes/b.md","allow_broken_links":true}}"#,
        ),
    ] {
        assert_eq!(wire(kind), json);
        round_trip(kind);
        round_trip(&Operation::new(kind.clone()));
    }
    assert_eq!(
        serde_json::from_str::<OperationKind>(
            r#"{"kind":"delete_document","fields":{"path":"notes/b.md","rewrite_to":"c","allow_broken_links":false}}"#
        )
        .ok(),
        Some(rewriting)
    );
    for json in [
        r#"{"kind":"delete_document","fields":{"path":"notes/b.md","rewrite_to":"c","allow_broken_links":true}}"#,
        r#"{"kind":"delete_document","fields":{"path":"notes/b.md","rewrite_to":null}}"#,
        r#"{"kind":"delete_document","fields":{"path":"notes/b.md","allow_broken_links":null}}"#,
        r##"{"kind":"delete_document","fields":{"path":"notes/b.md","rewrite_to":"c#Notes"}}"##,
        r##"{"kind":"delete_document","fields":{"path":"notes/b.md","rewrite_to":"c#^a1"}}"##,
        r#"{"kind":"move_document","fields":{"from":"a.md","to":"b.md","allow_broken_links":true}}"#,
    ] {
        assert!(
            serde_json::from_str::<Operation>(json).is_err(),
            "reading {json} produced an operation"
        );
    }
}

/// **The link kinds read each end through its own grammar.** A folder move's
/// ends are folder paths, a link rewrite's are target texts and a wikilink
/// rewrite's are resolution targets naming a document — one that need not
/// stand — and never a place inside one.
#[test]
fn a_link_kind_reads_its_ends_through_their_own_grammars() {
    let read: OperationKind = serde_json::from_str(
        r#"{"kind":"rewrite_wikilink","fields":{"old":"gone/never-was","new":"here"}}"#,
    )
    .expect("a wikilink rewrite of a document no vault need hold");
    assert_eq!(
        read,
        OperationKind::rewrite_wikilink(target("gone/never-was"), target("here"))
    );
    let rewrite: OperationKind = serde_json::from_str(
        r#"{"kind":"rewrite_link","fields":{"path":"notes/c.md","syntax":"markdown","from":"../a.md","to":"../archive/a.md"}}"#,
    )
    .expect("a Markdown link rewrite");
    assert_eq!(
        rewrite,
        OperationKind::rewrite_link(
            path("notes/c.md"),
            LinkFamily::Markdown,
            "../a.md",
            "../archive/a.md"
        )
    );
    for json in [
        r#"{"kind":"move_folder","fields":{"from":"","to":"archive"}}"#,
        r#"{"kind":"move_folder","fields":{"from":"notes","to":"/archive"}}"#,
        r#"{"kind":"move_document","fields":{"from":"","to":"b.md"}}"#,
        r#"{"kind":"move_document","fields":{"from":"a.md","to":"/b.md"}}"#,
        r#"{"kind":"rewrite_link","fields":{"path":"notes/c.md","from":"a","to":"b"}}"#,
        r#"{"kind":"rewrite_link","fields":{"path":"notes/c.md","syntax":"embed","from":"a","to":"b"}}"#,
        r#"{"kind":"rewrite_link","fields":{"path":"notes/c.md","syntax":"wikilink","from":"a","to":"b","old":"a"}}"#,
        r##"{"kind":"rewrite_wikilink","fields":{"old":"a#Notes","new":"b"}}"##,
        r##"{"kind":"rewrite_wikilink","fields":{"old":"a","new":"b#^a1"}}"##,
        r##"{"kind":"rewrite_wikilink","fields":{"old":"#Notes","new":"b"}}"##,
        r#"{"kind":"rewrite_wikilink","fields":{"old":"a"}}"#,
        r#"{"kind":"rewrite_wikilink","fields":{"old":"a","new":"b","path":"notes/c.md"}}"#,
    ] {
        assert!(
            serde_json::from_str::<Operation>(json).is_err(),
            "reading {json} produced an operation"
        );
    }
}

/// **A link rewrite names an address to respell and keeps its protocol**,
/// as an operation and as a cascade's rewrite alike. An empty `from` is
/// refused — an anchor-only link names its holder wherever it goes, so no
/// rewrite respells one — and so is a `to` written under another protocol
/// than `from`, which no rewrite can write into the link. An empty `to`, a
/// `to` equal to `from`, and two addresses under one protocol all read.
#[test]
fn a_link_rewrite_respells_an_address_and_never_its_protocol() {
    let kind = |from: &str, to: &str| {
        format!(
            r#"{{"kind":"rewrite_link","fields":{{"path":"notes/c.md","syntax":"wikilink","from":"{from}","to":"{to}"}}}}"#
        )
    };
    let rewrite = |from: &str, to: &str| {
        format!(r#"{{"path":"notes/c.md","syntax":"wikilink","from":"{from}","to":"{to}"}}"#)
    };
    for (from, to) in [
        ("", "b"),
        ("vault://a", "b"),
        ("a", "vault://b"),
        ("vault://a", "https://a"),
    ] {
        assert!(
            serde_json::from_str::<OperationKind>(&kind(from, to)).is_err(),
            "a rewrite of `{from}` to `{to}` read as an operation"
        );
        assert!(
            serde_json::from_str::<LinkRewrite>(&rewrite(from, to)).is_err(),
            "a rewrite of `{from}` to `{to}` read as a cascade's rewrite"
        );
    }
    for (from, to) in [
        ("a", ""),
        ("a", "a"),
        ("vault://notes/a", "vault://archive/a"),
        ("a", "b"),
        // Not a protocol: the sentinel is a lowercase scheme, `://` and a
        // stem, so each end reads as a plain address.
        ("HTTPS://a", "b"),
        ("note:draft", "b"),
    ] {
        assert_eq!(
            serde_json::from_str::<OperationKind>(&kind(from, to)).ok(),
            Some(OperationKind::rewrite_link(
                path("notes/c.md"),
                LinkFamily::Wikilink,
                from,
                to
            )),
            "a rewrite of `{from}` to `{to}`"
        );
        assert_eq!(
            serde_json::from_str::<LinkRewrite>(&rewrite(from, to)).ok(),
            Some(LinkRewrite::new(
                path("notes/c.md"),
                LinkFamily::Wikilink,
                from,
                to
            )),
            "a cascade's rewrite of `{from}` to `{to}`"
        );
    }
}

/// A file state is absent, or present with the hash of what it holds.
#[test]
fn a_file_state_is_an_object_tagged_state() {
    assert_eq!(wire(&FileState::absent()), r#"{"state":"absent"}"#);
    assert_eq!(
        wire(&FileState::present(content_hash(0x01))),
        format!(r#"{{"state":"present","hash":"{}"}}"#, hash_text(0x01))
    );
}

/// **A present file is quarantined only where it says so.** Bytes that do
/// not decode as a vault document write `"quarantined":true` beside their
/// hash and read back quarantined; a file that decodes leaves the flag out,
/// and a state written without it — every plan made before it existed —
/// reads as one that decodes. `null`, another type and a second flag are
/// refused.
#[test]
fn a_present_file_is_quarantined_only_where_it_says_so() {
    let hash = hash_text(0x01);
    let quarantined = FileState::quarantined(content_hash(0x01));
    let json = format!(r#"{{"state":"present","hash":"{hash}","quarantined":true}}"#);
    assert_eq!(wire(&quarantined), json);
    round_trip(&quarantined);
    assert!(!quarantined.is_document());
    assert_eq!(
        serde_json::from_str::<FileState>(&format!(r#"{{"state":"present","hash":"{hash}"}}"#))
            .ok(),
        Some(FileState::present(content_hash(0x01)))
    );
    assert_eq!(
        serde_json::from_str::<FileState>(&format!(
            r#"{{"state":"present","hash":"{hash}","quarantined":false}}"#
        ))
        .ok(),
        Some(FileState::present(content_hash(0x01)))
    );
    assert!(FileState::present(content_hash(0x01)).is_document());
    assert!(!FileState::absent().is_document());
    for state in [
        format!(r#"{{"state":"present","hash":"{hash}","quarantined":null}}"#),
        format!(r#"{{"state":"present","hash":"{hash}","quarantined":"yes"}}"#),
        format!(r#"{{"state":"present","hash":"{hash}","quarantined":true,"quarantined":false}}"#),
        r#"{"state":"absent","quarantined":true}"#.to_string(),
    ] {
        assert!(
            serde_json::from_str::<FileState>(&state).is_err(),
            "{state} read as a file state"
        );
    }
}

/// **Two states hold the same content where both are absent or both hash
/// alike**, whatever either says of whether the bytes decode: that follows
/// from the bytes, so it is no part of which bytes a file holds.
#[test]
fn two_states_hold_the_same_content_by_their_hash_alone() {
    let present = FileState::present(content_hash(0x01));
    let quarantined = FileState::quarantined(content_hash(0x01));
    assert!(present.same_content(&quarantined));
    assert!(present.same_content(&present));
    assert!(FileState::absent().same_content(&FileState::absent()));
    assert!(!present.same_content(&FileState::present(content_hash(0x02))));
    assert!(!present.same_content(&FileState::absent()));
    assert_eq!(quarantined.hash(), Some(&content_hash(0x01)));
    assert_eq!(FileState::absent().hash(), None);
}

/// **A link is keyed by its holder, its syntax and its address as written,
/// protocol prefix included**, and an entry of the resolution change set
/// names what that link resolves to on each side of the plan; a refusal names
/// an entry computed again that the plan does not record in the same shape.
#[test]
fn a_link_resolution_is_one_entry_of_the_change_set() {
    let entry = PlanCondition::link_resolution(
        a_link_key(),
        Resolves::several(),
        Resolves::one(path("notes/a.md")),
    );
    let json = r#"{"condition":"link_resolution","link":{"holder":"notes/c.md","syntax":"wikilink","address":"a"},"before":{"resolves":"several"},"after":{"resolves":"one","path":"notes/a.md"}}"#;
    assert_eq!(wire(&entry), json);
    round_trip(&entry);
    // An anchor-only link has the empty address, and its resolution changes
    // when its holder moves, so the empty address is a key: here of a link
    // whose holder moved from `notes/c.md`, named where it stands after.
    round_trip(&PlanCondition::link_resolution(
        LinkKey::new(path("archive/c.md"), LinkFamily::Wikilink, ""),
        Resolves::one(path("notes/c.md")),
        Resolves::one(path("archive/c.md")),
    ));
    assert_eq!(
        wire(&RefusedCheck::condition_unrecorded(entry.clone())),
        format!(r#"{{"check":"condition_unrecorded","condition":{json}}}"#)
    );
    for refused in [
        json.replace(r#""resolves":"several""#, r#""resolves":"many""#),
        json.replace(
            r#""resolves":"several""#,
            r#""resolves":"several","path":"a.md""#,
        ),
        json.replace(
            r#""resolves":"one","path":"notes/a.md""#,
            r#""resolves":"one""#,
        ),
        json.replace(r#""address":"a""#, r#""address":"a","anchor":"x""#),
        json.replace(r#""syntax":"wikilink","#, ""),
    ] {
        assert!(
            serde_json::from_str::<PlanCondition>(&refused).is_err(),
            "{refused} read as a condition"
        );
    }
}

/// An author's condition and a plan's condition are two types, each tagged
/// `condition`.
#[test]
fn a_condition_is_an_object_tagged_condition() {
    assert_eq!(
        wire(&AuthorCondition::content_hash(
            path("notes/a.md"),
            content_hash(0xab)
        )),
        format!(
            r#"{{"condition":"content_hash","path":"notes/a.md","hash":"{}"}}"#,
            hash_text(0xab)
        )
    );
    assert_eq!(
        wire(&PlanCondition::content_hash(
            path("notes/c.md"),
            content_hash(0xcd)
        )),
        format!(
            r#"{{"condition":"content_hash","path":"notes/c.md","hash":"{}"}}"#,
            hash_text(0xcd)
        )
    );
}

/// A resolved plan is its vault, its root, its operations, one transition per
/// file and its conditions, with its provenance and footnote where it has
/// them; the document a caller sends names which plan it is under `plan`.
#[test]
fn a_resolved_plan_is_the_bytes_a_caller_sends_back() {
    assert_eq!(wire(&a_resolved_plan()), resolved_plan_json());
    assert_eq!(
        wire(&PlanDocument::resolved(a_resolved_plan())),
        resolved_plan_json(),
        "a resolved plan and the document carrying it are not one set of bytes"
    );
    assert_eq!(
        wire(&a_bare_resolved_plan()),
        format!(
            concat!(
                r#"{{"plan":"resolved","vault":{{"by":"name","name":"notes"}},"root":"00000000000103020000000000000002","#,
                r#""operations":[{{"kind":"delete_document","fields":{{"path":"notes/b.md"}}}}],"#,
                r#""transitions":[{{"path":"notes/b.md","before":{{"state":"present","hash":"{two}"}},"#,
                r#""after":{{"state":"absent"}}}}],"conditions":[]}}"#
            ),
            two = hash_text(0x02)
        )
    );
}

/// An authored plan is its vault and its operations, with a footnote where
/// it has one.
#[test]
fn an_authored_plan_is_its_vault_and_its_operations() {
    let plan = AuthoredPlan::new(
        VaultAddress::name(name("notes")),
        vec![Operation::new(OperationKind::delete_document(path(
            "notes/b.md",
        )))],
    );
    let operations = r#""operations":[{"kind":"delete_document","fields":{"path":"notes/b.md"}}]"#;
    assert_eq!(
        wire(&PlanDocument::operations(plan.clone())),
        format!(r#"{{"plan":"operations","vault":{{"by":"name","name":"notes"}},{operations}}}"#)
    );
    assert_eq!(
        wire(&PlanDocument::operations(plan.with_footnote("tidy"))),
        format!(
            r#"{{"plan":"operations","vault":{{"by":"name","name":"notes"}},{operations},"footnote":"tidy"}}"#
        )
    );
}

/// Insert an unknown key into the object `pointer` names inside `document`.
fn with_surprise(document: &serde_json::Value, pointer: &str) -> String {
    let mut edited = document.clone();
    edited
        .pointer_mut(pointer)
        .and_then(serde_json::Value::as_object_mut)
        .unwrap_or_else(|| panic!("{pointer} names no object in {document}"))
        .insert("surprise".to_string(), serde_json::Value::Bool(true));
    edited.to_string()
}

/// **A plan refuses a field it does not know, at every level.** A plan flows
/// into the host, and a field dropped on the way in — a newer caller's
/// condition — would weaken a check without a word; the refusal is the
/// version-mismatch signal.
#[test]
fn a_plan_refuses_a_field_it_does_not_know_at_every_level() {
    let resolved = serde_json::to_value(PlanDocument::resolved(a_resolved_plan()))
        .expect("a resolved plan as JSON");
    for pointer in [
        "",
        "/operations/0",
        "/operations/0/fields",
        "/transitions/0",
        "/transitions/0/before",
        "/transitions/0/after",
        "/conditions/0",
        "/conditions/1",
        "/conditions/1/link",
        "/conditions/1/before",
        "/conditions/1/after",
        "/provenance",
        "/provenance/skipped/0",
    ] {
        let json = with_surprise(&resolved, pointer);
        let refusal = serde_json::from_str::<PlanDocument>(&json)
            .expect_err(&format!("a plan carrying an unknown field at `{pointer}`"));
        assert!(
            refusal.to_string().contains("surprise"),
            "the refusal at `{pointer}` does not name the field: {refusal}"
        );
    }
    let bare = serde_json::to_value(a_resolved_plan()).expect("a resolved plan as JSON");
    assert!(
        serde_json::from_str::<ResolvedPlan>(&with_surprise(&bare, "")).is_err(),
        "a resolved plan carried outside a document dropped a field it does not know"
    );

    let authored = serde_json::to_value(PlanDocument::operations(an_authored_plan()))
        .expect("an authored plan as JSON");
    // The authored plan holds one operation of every kind, then a cascading
    // move, then an operation carrying conditions.
    let (cascading, conditioned) = (operation_kinds().len(), operation_kinds().len() + 1);
    for pointer in [
        String::new(),
        format!("/operations/{cascading}/cascade/0"),
        format!("/operations/{conditioned}"),
        format!("/operations/{conditioned}/fields"),
        format!("/operations/{conditioned}/conditions/0"),
    ] {
        let pointer = pointer.as_str();
        let json = with_surprise(&authored, pointer);
        assert!(
            serde_json::from_str::<PlanDocument>(&json).is_err(),
            "a plan carrying an unknown field at `{pointer}` read back"
        );
    }
}

/// A kind, a condition, a state or a document nobody minted fails the read.
#[test]
fn a_plan_refuses_a_variant_it_does_not_know() {
    for json in [
        r#"{"kind":"rename_document","fields":{"path":"notes/b.md"}}"#,
        r#"{"kind":"delete_document","fields":{"path":"notes/b.md"},"conditions":[{"condition":"expected_value","path":"notes/b.md"}]}"#,
    ] {
        assert!(
            serde_json::from_str::<Operation>(json).is_err(),
            "reading {json} produced an operation"
        );
    }
    assert!(serde_json::from_str::<FileState>(r#"{"state":"missing"}"#).is_err());
    assert!(
        serde_json::from_str::<FileState>(r#"{"state":"absent","surprise":true}"#).is_err(),
        "an absent state dropped a field it does not know"
    );
    assert!(
        serde_json::from_str::<PlanCondition>(r#"{"condition":"backlinks","path":"notes/b.md"}"#)
            .is_err()
    );
    assert!(
        serde_json::from_str::<PlanDocument>(
            r#"{"plan":"draft","vault":{"by":"name","name":"notes"},"operations":[]}"#
        )
        .is_err()
    );
    assert!(
        serde_json::from_str::<PlanDocument>(
            r#"{"vault":{"by":"name","name":"notes"},"operations":[]}"#
        )
        .is_err(),
        "a document naming no plan read back as one"
    );
}

/// `json` with its one occurrence of `find` replaced by `with`.
fn spliced(json: &str, find: &str, with: &str) -> String {
    assert_eq!(
        json.matches(find).count(),
        1,
        "`{find}` does not occur exactly once in {json}"
    );
    json.replacen(find, with, 1)
}

/// Every place a resolved plan is read: alone, as a document, as a request's
/// plan, inside a report and inside a refusal's detail. Each reading's
/// refusal, or `None` where it read.
fn every_reading_of_a_resolved_plan(plan: &str) -> Vec<(&'static str, Option<String>)> {
    fn refusal<T: DeserializeOwned>(json: &str) -> Option<String> {
        serde_json::from_str::<T>(json)
            .err()
            .map(|error| error.to_string())
    }
    vec![
        ("the plan alone", refusal::<ResolvedPlan>(plan)),
        ("a document", refusal::<PlanDocument>(plan)),
        (
            "a request",
            refusal::<ApplyParams>(&format!(r#"{{"mode":"apply","plan":{plan}}}"#)),
        ),
        (
            "a report",
            refusal::<ApplyReport>(&format!(
                r#"{{"outcome":"previewed","plan":{plan},"forecast":{}}}"#,
                wire(&a_forecast())
            )),
        ),
        (
            "a detail",
            refusal::<ErrorDetail>(&format!(
                r#"{{"code":"host/apply-outcome-unknown","plan":{plan}}}"#
            )),
        ),
    ]
}

/// **A key written twice is refused wherever a plan is read.** A plan read
/// inside a request, a report or a refusal is read by the same derive as the
/// plan alone, so a duplicate the plan alone refuses — at its own level or
/// inside an operation, its fields, a condition, a transition, a file state or
/// its provenance — is refused everywhere, rather than one reading keeping the
/// last value where another refuses.
#[test]
fn a_plan_refuses_a_key_written_twice_wherever_it_is_read() {
    let bare = wire(&a_bare_resolved_plan());
    let full = resolved_plan_json();
    let constructions = [
        (
            "the plan's conditions",
            spliced(
                &bare,
                r#""conditions":[]"#,
                r#""conditions":[],"conditions":[]"#,
            ),
        ),
        (
            "the plan's tag",
            spliced(
                &bare,
                r#"{"plan":"resolved","#,
                r#"{"plan":"resolved","plan":"resolved","#,
            ),
        ),
        (
            "an operation",
            spliced(
                &bare,
                r#"{"kind":"delete_document","#,
                r#"{"kind":"delete_document","id":"a","id":"b","#,
            ),
        ),
        (
            "an operation's fields",
            spliced(
                &bare,
                r#""fields":{"path":"notes/b.md"}"#,
                r#""fields":{"path":"notes/b.md","path":"notes/z.md"}"#,
            ),
        ),
        (
            "a condition",
            spliced(
                &full,
                r#""path":"notes/c.md","#,
                r#""path":"notes/c.md","path":"notes/z.md","#,
            ),
        ),
        (
            "a transition",
            spliced(
                &bare,
                r#"{"path":"notes/b.md","before""#,
                r#"{"path":"notes/b.md","path":"notes/z.md","before""#,
            ),
        ),
        (
            "a file state",
            spliced(
                &bare,
                r#""after":{"state":"absent"}"#,
                r#""after":{"state":"absent","state":"absent"}"#,
            ),
        ),
        (
            "the provenance",
            spliced(
                &full,
                r#""finding_generation":7"#,
                r#""finding_generation":7,"finding_generation":8"#,
            ),
        ),
    ];
    for (place, json) in constructions {
        for (reading, refusal) in every_reading_of_a_resolved_plan(&json) {
            let refusal = refusal.unwrap_or_else(|| {
                panic!("{reading} kept one of two values written for a key of {place}: {json}")
            });
            assert!(
                refusal.contains("duplicate field"),
                "{reading} refused a key of {place} written twice for another reason: {refusal}"
            );
        }
    }

    let authored = spliced(
        &wire(&an_authored_plan()),
        r#"{"plan":"operations","#,
        r#"{"plan":"operations","footnote":"a","footnote":"b","#,
    );
    for refusal in [
        serde_json::from_str::<AuthoredPlan>(&authored).err(),
        serde_json::from_str::<PlanDocument>(&authored).err(),
        serde_json::from_str::<ApplyParams>(&format!(r#"{{"mode":"apply","plan":{authored}}}"#))
            .err(),
    ] {
        let refusal = refusal.expect("an authored plan writing its footnote twice read back");
        assert!(refusal.to_string().contains("duplicate field"), "{refusal}");
    }
}

/// **An operation's fields are read by the derive, not kept last-wins.** A
/// move naming where it stands twice is refused alone, inside either plan and
/// inside a report, rather than moving whichever it named last.
#[test]
fn an_operation_refuses_a_field_written_twice_wherever_it_is_read() {
    let doubled = r#"{"kind":"move_document","fields":{"from":"a.md","from":"z.md","to":"b.md"}}"#;
    let refusal = serde_json::from_str::<Operation>(doubled)
        .expect_err("an operation naming where it moves from twice");
    assert!(refusal.to_string().contains("duplicate field"), "{refusal}");

    let authored = spliced(
        &wire(&an_authored_plan()),
        r#""operations":["#,
        &format!(r#""operations":[{doubled},"#),
    );
    assert!(
        serde_json::from_str::<PlanDocument>(&authored).is_err(),
        "an authored plan kept the last of two sources for a move"
    );
    let resolved = spliced(
        &wire(&a_bare_resolved_plan()),
        r#"{"kind":"delete_document","fields":{"path":"notes/b.md"}}"#,
        doubled,
    );
    for (reading, refusal) in every_reading_of_a_resolved_plan(&resolved) {
        assert!(
            refusal.is_some_and(|refusal| refusal.contains("duplicate field")),
            "{reading} kept the last of two sources for a move"
        );
    }
}

/// A document reads the tag wherever it is written, and a field only the
/// other plan names is refused as the plan alone refuses it.
#[test]
fn a_plan_document_reads_as_the_plan_it_holds_reads_alone() {
    let resolved = wire(&a_bare_resolved_plan());
    let tag_last = format!(
        r#"{},"plan":"resolved"}}"#,
        spliced(&resolved, r#"{"plan":"resolved","#, "{").trim_end_matches('}')
    );
    assert_eq!(
        serde_json::from_str::<PlanDocument>(&tag_last).expect("a document tagged last"),
        PlanDocument::resolved(a_bare_resolved_plan())
    );
    let without_provenance = spliced(
        &resolved,
        r#""conditions":[]"#,
        r#""conditions":[],"provenance":null"#,
    );
    assert_eq!(
        serde_json::from_str::<PlanDocument>(&without_provenance).expect("a null provenance"),
        PlanDocument::resolved(
            serde_json::from_str::<ResolvedPlan>(&without_provenance).expect("a null provenance")
        )
    );

    let authored = wire(&an_authored_plan());
    for foreign in [
        r#""root":"00000000000103020000000000000002""#,
        r#""transitions":[]"#,
        r#""conditions":[]"#,
        r#""provenance":null"#,
    ] {
        let json = spliced(
            &authored,
            r#"{"plan":"operations","#,
            &format!(r#"{{"plan":"operations",{foreign},"#),
        );
        assert!(
            serde_json::from_str::<AuthoredPlan>(&json).is_err(),
            "an authored plan carrying {foreign} read back"
        );
        assert!(
            serde_json::from_str::<PlanDocument>(&json).is_err(),
            "a document holding an authored plan carrying {foreign} read back"
        );
    }
    for missing in [
        r#""root":"00000000000103020000000000000002","#,
        r#""conditions":[]"#,
    ] {
        let json = spliced(&resolved, missing, "").replace(",}", "}");
        assert!(
            serde_json::from_str::<ResolvedPlan>(&json).is_err()
                && serde_json::from_str::<PlanDocument>(&json).is_err(),
            "a resolved plan without {missing} read back"
        );
    }
}

/// **A plan reads the same in any format, alone or inside a request.** Read
/// outside JSON, a plan inside a request is read by the same derive as the
/// plan alone, so a value that format reads one way alone — a tagged scalar in
/// YAML — is read that way inside a request too, rather than passing through a
/// JSON buffer the format cannot fill.
#[test]
fn a_plan_reads_the_same_in_any_format_alone_and_inside_a_request() {
    fn readings(plan: &str) -> [Option<PlanDocument>; 3] {
        let request = format!(
            "mode: apply\nplan:\n{}",
            plan.lines()
                .map(|line| format!("  {line}\n"))
                .collect::<String>()
        );
        [
            serde_yaml::from_str::<ResolvedPlan>(plan)
                .ok()
                .map(PlanDocument::resolved),
            serde_yaml::from_str::<PlanDocument>(plan).ok(),
            serde_yaml::from_str::<ApplyParams>(&request)
                .ok()
                .map(|params| params.plan),
        ]
    }
    let plan = serde_yaml::to_string(&a_bare_resolved_plan()).expect("a plan as YAML");
    let [alone, document, request] = readings(&plan);
    assert_eq!(alone, Some(PlanDocument::resolved(a_bare_resolved_plan())));
    assert_eq!(document, alone);
    assert_eq!(request, alone);

    let tagged = format!("{plan}footnote: !x f\n");
    let [alone, document, request] = readings(&tagged);
    assert_eq!(
        document, alone,
        "a document read a tagged footnote otherwise"
    );
    assert_eq!(request, alone, "a request read a tagged footnote otherwise");
}

// ── What an apply answers with ───────────────────────────────────────────

fn folder(text: &str) -> FolderPath {
    FolderPath::new(text).expect("a legal folder path")
}

/// A forecast naming a drifted target, a folder the plan makes and one it
/// removes.
fn a_forecast() -> Forecast {
    Forecast::new(
        vec![path("notes/a.md")],
        vec![folder("archive")],
        vec![folder("notes/old")],
    )
}

/// Every check a refused apply names.
fn refused_checks() -> Vec<RefusedCheck> {
    vec![
        RefusedCheck::drifted(path("notes/a.md"), FileState::present(content_hash(0x0f))),
        RefusedCheck::drifted(path("notes/b.md"), FileState::absent()),
        RefusedCheck::condition_failed(PlanCondition::content_hash(
            path("notes/c.md"),
            content_hash(0xcd),
        )),
        RefusedCheck::condition_unrecorded(PlanCondition::link_resolution(
            a_link_key(),
            Resolves::one(path("notes/a.md")),
            Resolves::several(),
        )),
        RefusedCheck::schema_violation(
            path("notes/a.md"),
            FindingKind::UndeclaredTag,
            Some("draft".to_string()),
            "the tag `draft` is not declared",
        ),
        RefusedCheck::schema_violation(
            path("notes/a.md"),
            FindingKind::FrontmatterUnreadable,
            None,
            "the frontmatter does not parse",
        ),
        RefusedCheck::name_taken(path("notes/new.md")),
    ]
}

/// Every reason an operation is left unresolved.
fn unresolved_reasons() -> Vec<UnresolvedReason> {
    vec![
        UnresolvedReason::part_landed(),
        UnresolvedReason::no_longer_resolves("the text `draft` no longer occurs"),
        UnresolvedReason::requires_unresolved(operation_id("make-b")),
        UnresolvedReason::has_backlinks(vec![path("notes/c.md"), path("notes/d.md")], 3),
        UnresolvedReason::ambiguous_target(
            AmbiguousEnd::Old,
            CandidateHead::new(
                [
                    Candidate::new(path("notes/a.md"), "notes/a"),
                    Candidate::new(path("archive/a.md"), "archive/a"),
                ],
                2,
            )
            .expect("a head"),
        ),
        UnresolvedReason::ambiguous_target(
            AmbiguousEnd::Target,
            CandidateHead::new([Candidate::new(path("notes/b.md"), "notes/b")], 3).expect("a head"),
        ),
    ]
}

/// Every advice a forecast gives about one link.
fn link_advisories() -> Vec<LinkAdvisory> {
    vec![
        LinkAdvisory::skipped_ambiguous(a_link_key()),
        LinkAdvisory::skipped_unrepresentable(a_link_key()),
        LinkAdvisory::skipped_would_corrupt_frontmatter(a_link_key()),
        LinkAdvisory::skipped_not_rewritable(a_link_key()),
        LinkAdvisory::left_broken(a_link_key()),
        LinkAdvisory::made_ambiguous(a_link_key()),
        LinkAdvisory::retargeted(a_link_key()),
    ]
}

/// Every cause that stops a publication part-way.
fn interruption_causes() -> Vec<InterruptionCause> {
    vec![
        InterruptionCause::io_failure("the disk is full"),
        InterruptionCause::name_taken(path("notes/new.md")),
        InterruptionCause::foreign_edit(path("notes/a.md")),
    ]
}

/// Every fault a plan's own shape has.
fn plan_faults() -> Vec<PlanFault> {
    vec![
        PlanFault::duplicate_id(operation_id("edit-a"), vec![0, 3]),
        PlanFault::unknown_requirement(2, operation_id("make-z")),
        PlanFault::requires_cycle(vec![1, 2]),
        PlanFault::content_cycle(vec![0, 1]),
        PlanFault::transitions_disagree(vec![path("notes/a.md"), path("notes/b.md")]),
        PlanFault::unexpanded_target(vec![1]),
        PlanFault::unexpanded_rule(vec![3]),
        PlanFault::expanded_target_ordered(vec![2]),
        PlanFault::misplaced_cascade(vec![0]),
        PlanFault::control_file_beside_documents(vec![1]),
    ]
}

fn apply_modes() -> Vec<ApplyMode> {
    vec![ApplyMode::Preview, ApplyMode::Apply]
}

fn changeset_outcomes() -> Vec<ChangesetOutcome> {
    vec![ChangesetOutcome::Committed, ChangesetOutcome::Healing]
}

fn target_results() -> Vec<TargetResult> {
    vec![TargetResult::Wrote, TargetResult::Found]
}

/// Both outcomes an apply answers with.
fn apply_reports() -> Vec<ApplyReport> {
    vec![
        ApplyReport::previewed(a_resolved_plan(), a_forecast()),
        ApplyReport::applied(
            a_resolved_plan(),
            ChangesetOutcome::Committed,
            vec![
                AppliedTarget::new(path("notes/a.md"), TargetResult::Wrote),
                AppliedTarget::new(path("archive/new.md"), TargetResult::Found),
            ],
            vec![folder("archive")],
            vec![folder("notes/old")],
        ),
        ApplyReport::applied(
            a_bare_resolved_plan(),
            ChangesetOutcome::Healing,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ),
    ]
}

/// A lifecycle refusal an apply that did not run carries as its cause.
fn a_park() -> ErrorEnvelope {
    ErrorEnvelope::new(
        "another process maintains this vault",
        ErrorDetail::maintainer_contended(MaintainerIdentity::unknown()),
    )
}

/// Every detail an apply's outcome carries, over every payload it can carry.
fn apply_details() -> Vec<ErrorDetail> {
    let mut details = vec![
        ErrorDetail::plan_refused(
            a_bare_resolved_plan(),
            a_forecast(),
            refused_checks(),
            unresolved_reasons()
                .into_iter()
                .map(|reason| {
                    UnresolvedOperation::new(
                        Operation::new(OperationKind::str_replace(
                            path("notes/a.md"),
                            "draft",
                            "final",
                        )),
                        reason,
                    )
                })
                .collect(),
            vec![path("notes/completed.md")],
        ),
        ErrorDetail::root_changed(a_root(), RootIdentity::from_device_and_inode(66_307, 2)),
        ErrorDetail::write_failed(a_resolved_plan(), "the disk is full", Vec::new()),
        ErrorDetail::apply_not_run(a_park(), None),
        ErrorDetail::apply_not_run(a_park(), Some(a_resolved_plan())),
        ErrorDetail::apply_outcome_unknown(a_resolved_plan()),
    ];
    details.extend(interruption_causes().into_iter().map(|cause| {
        ErrorDetail::plan_interrupted(
            a_resolved_plan(),
            vec![path("notes/a.md")],
            cause,
            Vec::new(),
        )
    }));
    details.extend(plan_faults().into_iter().map(ErrorDetail::plan_invalid));
    details
}

/// **Every plan vector here holds the whole vocabulary, and the schema is
/// what says so**, as the vectors above hold theirs.
#[test]
fn every_plan_vector_here_holds_the_members_the_schema_advertises() {
    fn tags<T: Serialize>(values: &[T], tag: &str) -> BTreeSet<String> {
        values.iter().map(|value| tag_string(value, tag)).collect()
    }
    assert_eq!(
        tags(&operation_kinds(), "kind"),
        advertised::<OperationKind>(Some("kind"))
    );
    assert_eq!(
        tags(&author_conditions(), "condition"),
        advertised::<AuthorCondition>(Some("condition"))
    );
    assert_eq!(
        tags(&plan_conditions(), "condition"),
        advertised::<PlanCondition>(Some("condition"))
    );
    assert_eq!(
        tags(&file_states(), "state"),
        advertised::<FileState>(Some("state"))
    );
    let own_tag = |schema: serde_json::Value| -> String {
        schema["properties"]["plan"]["const"]
            .as_str()
            .unwrap_or_else(|| panic!("a plan advertises no `plan` tag: {schema}"))
            .to_owned()
    };
    assert_eq!(
        tags(&plan_documents(), "plan"),
        [
            own_tag(serde_json::to_value(schemars::schema_for!(AuthoredPlan)).expect("a schema")),
            own_tag(serde_json::to_value(schemars::schema_for!(ResolvedPlan)).expect("a schema")),
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(
        tags(&refused_checks(), "check"),
        advertised::<RefusedCheck>(Some("check"))
    );
    assert_eq!(
        tags(&unresolved_reasons(), "kind"),
        advertised::<UnresolvedReason>(Some("kind"))
    );
    assert_eq!(
        tags(&link_advisories(), "advisory"),
        advertised::<LinkAdvisory>(Some("advisory"))
    );
    assert_eq!(
        tags(&interruption_causes(), "kind"),
        advertised::<InterruptionCause>(Some("kind"))
    );
    assert_eq!(
        tags(&plan_faults(), "kind"),
        advertised::<PlanFault>(Some("kind"))
    );
    assert_eq!(
        tags(&apply_reports(), "outcome"),
        advertised::<ApplyReport>(Some("outcome"))
    );
    assert_eq!(
        apply_modes()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>(),
        advertised::<ApplyMode>(None)
    );
    assert_eq!(
        changeset_outcomes()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>(),
        advertised::<ChangesetOutcome>(None)
    );
    assert_eq!(
        target_results()
            .iter()
            .map(flat_string)
            .collect::<BTreeSet<_>>(),
        advertised::<TargetResult>(None)
    );
}

#[test]
fn every_apply_shape_survives_the_round_trip() {
    round_trip(&a_forecast());
    round_trip(&a_forecast_of_links());
    for advisory in link_advisories() {
        round_trip(&advisory);
    }
    for check in refused_checks() {
        round_trip(&check);
    }
    for reason in unresolved_reasons() {
        round_trip(&reason);
    }
    for cause in interruption_causes() {
        round_trip(&cause);
    }
    for fault in plan_faults() {
        round_trip(&fault);
    }
    for mode in apply_modes() {
        round_trip(&mode);
    }
    for report in apply_reports() {
        round_trip(&report);
    }
    for document in plan_documents() {
        for mode in apply_modes() {
            round_trip(&ApplyParams::new(mode, document.clone()));
        }
    }
    round_trip(&VaultAnswer::new(
        AnswerReading::new(TrustState::Ready, "epoch-1", 2),
        Vec::new(),
        ApplyReport::previewed(a_resolved_plan(), a_forecast()),
    ));
}

/// A folder path is a vault-relative path that names something.
#[test]
fn a_folder_path_is_the_string_it_renders_as_and_is_relative() {
    assert_eq!(wire(&folder("notes/old")), r#""notes/old""#);
    round_trip(&folder("archive"));
    for text in ["", "/", "/notes"] {
        assert!(
            FolderPath::new(text).is_err(),
            "`{text}` was built as a folder path"
        );
        assert!(
            serde_json::from_str::<FolderPath>(&format!("\"{text}\"")).is_err(),
            "`{text}` was read back as a folder path"
        );
    }
    assert_eq!(
        FolderPath::new("/notes")
            .expect_err("a rooted folder path")
            .what(),
        "folder path"
    );
    // A folder path names a place on disk, which the document grammar does
    // not govern: a trailing slash, or a name a document could not take.
    for text in ["notes/", "a\\b", "..b"] {
        assert_eq!(folder(text).as_str(), text);
    }
}

/// **A forecast carries only what the plan beside it does not.** Each
/// transition is the resolved plan's own, so a forecast names the targets that
/// drifted and the folders the plan makes and removes, and a vault-wide
/// preview carries each transition once.
#[test]
fn a_forecast_names_what_the_plan_beside_it_does_not_carry() {
    assert_eq!(
        wire(&a_forecast()),
        r#"{"drifted":["notes/a.md"],"folders_made":["archive"],"folders_removed":["notes/old"],"forced":[],"links":[],"left_behind":[]}"#
    );
    let previewed = wire(&ApplyReport::previewed(a_resolved_plan(), a_forecast()));
    for transition in &a_resolved_plan().transitions {
        let after = wire(&transition.after);
        assert_eq!(
            previewed.matches(&after).count(),
            1,
            "a preview carries the transition of {} more than once: {previewed}",
            transition.path
        );
    }
}

/// A forecast advising on every link it can and naming a file a folder move
/// leaves behind.
fn a_forecast_of_links() -> Forecast {
    a_forecast()
        .with_links(link_advisories())
        .with_left_behind(vec![file("notes/diagram.png")])
}

fn file(text: &str) -> FilePath {
    FilePath::new(text).expect("a legal file path")
}

/// NUL cannot name a place on disk. Other control characters can, so folder
/// and file paths retain them rather than adopting the document grammar.
#[test]
fn folder_and_file_paths_refuse_nul_before_they_reach_the_filesystem() {
    for text in ["\0", "notes/nu\0l", "é🦀\n\0"] {
        let encoded = serde_json::to_string(text).expect("a JSON string");
        assert!(FolderPath::new(text).is_err(), "a folder admitted {text:?}");
        assert!(FilePath::new(text).is_err(), "a file admitted {text:?}");
        assert!(serde_json::from_str::<FolderPath>(&encoded).is_err());
        assert!(serde_json::from_str::<FilePath>(&encoded).is_err());
    }
    for text in ["notes/tab\t", "notes/line\n", "notes/control\u{1}"] {
        let encoded = serde_json::to_string(text).expect("a JSON string");
        assert_eq!(FolderPath::new(text).unwrap().as_str(), text);
        assert_eq!(FilePath::new(text).unwrap().as_str(), text);
        assert_eq!(
            serde_json::from_str::<FolderPath>(&encoded)
                .unwrap()
                .as_str(),
            text
        );
        assert_eq!(
            serde_json::from_str::<FilePath>(&encoded).unwrap().as_str(),
            text
        );
    }
}

/// **A link advisory points into the change set by the link's key**, never
/// repeating a resolution, and a folder move's left-behind file is named by a
/// path of its own.
#[test]
fn a_forecast_advises_on_links_by_their_key_and_names_files_left_behind() {
    assert_eq!(
        wire(&LinkAdvisory::left_broken(a_link_key())),
        r#"{"advisory":"left_broken","link":{"holder":"notes/c.md","syntax":"wikilink","address":"a"}}"#
    );
    let json = wire(&a_forecast_of_links());
    assert!(!json.contains("resolves"), "{json}");
    assert!(
        json.ends_with(r#""left_behind":["notes/diagram.png"]}"#),
        "{json}"
    );
    assert_eq!(
        wire(&UnresolvedReason::has_backlinks(
            vec![path("notes/c.md")],
            2
        )),
        r#"{"kind":"has_backlinks","holders":["notes/c.md"],"total":2}"#
    );
    assert_eq!(
        wire(&UnresolvedReason::ambiguous_target(
            AmbiguousEnd::Old,
            CandidateHead::new([Candidate::new(path("a.md"), "a")], 2).expect("a head")
        )),
        r#"{"kind":"ambiguous_target","end":"old","candidates":{"candidates":[{"path":"a.md","suffix":"a"}],"total":2}}"#
    );
    assert_eq!(
        wire(&UnresolvedReason::ambiguous_target(
            AmbiguousEnd::Target,
            CandidateHead::new([Candidate::new(path("a.md"), "a")], 2).expect("a head")
        )),
        r#"{"kind":"ambiguous_target","end":"target","candidates":{"candidates":[{"path":"a.md","suffix":"a"}],"total":2}}"#
    );
    for text in ["", "/", "/notes/a.png"] {
        assert!(FilePath::new(text).is_err(), "`{text}` was built");
        assert!(
            serde_json::from_str::<FilePath>(&format!("\"{text}\"")).is_err(),
            "`{text}` was read back as a file path"
        );
    }
    assert_eq!(FilePath::new("/a").expect_err("rooted").what(), "file path");
    // A file path names a file the vault does not read as a document, at the
    // spelling the tree lists it, which the document grammar does not govern.
    for text in ["a\\b.png", "a\u{1}.png", "..md"] {
        assert_eq!(file(text).as_str(), text);
    }
}

/// A forecast is an answer, so it drops a field it does not know where a plan
/// refuses one.
#[test]
fn a_forecast_drops_a_field_it_does_not_know() {
    let json = serde_json::to_value(a_forecast()).expect("a forecast as JSON");
    let read: Forecast = serde_json::from_str(&with_surprise(&json, ""))
        .unwrap_or_else(|error| panic!("a forecast with a field it does not know: {error}"));
    assert_eq!(read, a_forecast());
}

/// An apply names its mode, and there is no default: a request without one
/// does not read.
#[test]
fn an_apply_request_states_its_mode() {
    let plan = PlanDocument::resolved(a_bare_resolved_plan());
    let json = wire(&ApplyParams::new(ApplyMode::Preview, plan.clone()));
    assert_eq!(
        json,
        format!(r#"{{"mode":"preview","plan":{}}}"#, wire(&plan))
    );
    assert_eq!(flat_string(&ApplyMode::Apply), "apply");
    assert!(
        serde_json::from_str::<ApplyParams>(&format!(r#"{{"plan":{}}}"#, wire(&plan))).is_err(),
        "an apply naming no mode read back as one"
    );
    assert!(
        serde_json::from_str::<ApplyParams>(&format!(
            r#"{{"mode":"dry_run","plan":{}}}"#,
            wire(&plan)
        ))
        .is_err(),
        "a mode nobody minted read back as one"
    );
    assert_eq!(
        ApplyParams::new(ApplyMode::Apply, plan.clone())
            .plan
            .vault(),
        &VaultAddress::name(name("notes"))
    );
}

/// A preview answers with the resolved plan and its forecast; an applied plan
/// answers with the plan, whether its changeset committed, what each target
/// came to and the folders it made and removed.
#[test]
fn an_apply_report_is_an_object_tagged_outcome() {
    let [previewed, applied, _]: [ApplyReport; 3] =
        apply_reports().try_into().expect("three reports");
    assert_eq!(
        wire(&previewed),
        format!(
            r#"{{"outcome":"previewed","plan":{},"forecast":{}}}"#,
            resolved_plan_json(),
            wire(&a_forecast())
        )
    );
    assert_eq!(
        wire(&applied),
        format!(
            concat!(
                r#"{{"outcome":"applied","plan":{},"changeset":"committed","#,
                r#""targets":[{{"path":"notes/a.md","result":"wrote"}},{{"path":"archive/new.md","result":"found"}}],"#,
                r#""folders_made":["archive"],"folders_removed":["notes/old"],"forced":[]}}"#
            ),
            resolved_plan_json()
        )
    );
    assert_eq!(flat_string(&ChangesetOutcome::Healing), "healing");
}

/// A report is an answer and drops a field it does not know; the plan inside
/// it is a plan and still refuses one.
#[test]
fn an_apply_report_drops_a_field_it_does_not_know_and_its_plan_does_not() {
    let previewed = apply_reports().remove(0);
    let json = serde_json::to_value(&previewed).expect("a report as JSON");
    let read: ApplyReport =
        serde_json::from_str(&with_surprise(&json, "")).expect("a report with a field it drops");
    assert_eq!(read, previewed);
    assert!(
        serde_json::from_str::<ApplyReport>(&with_surprise(&json, "/plan")).is_err(),
        "the plan inside a report dropped a field it does not know"
    );
}

#[test]
fn every_apply_detail_survives_the_round_trip() {
    for detail in apply_details() {
        round_trip(&detail);
        round_trip(&ErrorEnvelope::new("the apply did not finish", detail));
    }
}

/// A refused plan carries the fresh plan, its forecast, each check that
/// refused and each operation the fresh plan could not resolve.
#[test]
fn a_refused_plan_carries_the_fresh_plan_and_why() {
    let detail = ErrorDetail::plan_refused(
        a_bare_resolved_plan(),
        Forecast::new(Vec::new(), Vec::new(), Vec::new()),
        vec![RefusedCheck::drifted(
            path("notes/a.md"),
            FileState::present(content_hash(0x0f)),
        )],
        vec![UnresolvedOperation::new(
            Operation::new(OperationKind::str_replace(
                path("notes/a.md"),
                "draft",
                "final",
            )),
            UnresolvedReason::no_longer_resolves("the text `draft` no longer occurs"),
        )],
        vec![path("notes/completed.md")],
    );
    assert_eq!(
        wire(&ErrorEnvelope::new("the plan drifted", detail)),
        format!(
            concat!(
                r#"{{"code":"vault/plan-refused","message":"the plan drifted","detail":{{"code":"vault/plan-refused","#,
                r#""plan":{plan},"forecast":{{"drifted":[],"folders_made":[],"folders_removed":[],"forced":[],"links":[],"left_behind":[]}},"#,
                r#""checks":[{{"check":"drifted","path":"notes/a.md","holds":{{"state":"present","hash":"{f}"}}}}],"#,
                r#""unresolved":[{{"operation":{{"kind":"str_replace","fields":{{"path":"notes/a.md","old_str":"draft","new_str":"final"}}}},"#,
                r#""reason":{{"kind":"no_longer_resolves","detail":"the text `draft` no longer occurs"}}}}],"landed":["notes/completed.md"]}}}}"#
            ),
            plan = wire(&a_bare_resolved_plan()),
            f = hash_text(0x0f),
        )
    );
}

/// Each check, reason, cause and fault is an object under its own tag.
#[test]
fn the_apply_reasons_are_tagged_objects() {
    assert_eq!(
        wire(&RefusedCheck::name_taken(path("notes/new.md"))),
        r#"{"check":"name_taken","path":"notes/new.md"}"#
    );
    assert_eq!(
        wire(&RefusedCheck::condition_failed(
            PlanCondition::content_hash(path("notes/c.md"), content_hash(0xcd))
        )),
        format!(
            r#"{{"check":"condition_failed","condition":{{"condition":"content_hash","path":"notes/c.md","hash":"{}"}}}}"#,
            hash_text(0xcd)
        )
    );
    assert_eq!(
        wire(&RefusedCheck::schema_violation(
            path("notes/a.md"),
            FindingKind::UndeclaredTag,
            Some("draft".to_string()),
            "the tag `draft` is not declared"
        )),
        concat!(
            r#"{"check":"schema_violation","path":"notes/a.md","kind":"document/undeclared-tag","#,
            r#""target":"draft","message":"the tag `draft` is not declared"}"#
        )
    );
    assert_eq!(
        wire(&UnresolvedReason::part_landed()),
        r#"{"kind":"part_landed"}"#
    );
    assert_eq!(
        wire(&UnresolvedReason::requires_unresolved(operation_id(
            "make-b"
        ))),
        r#"{"kind":"requires_unresolved","requires":"make-b"}"#
    );
    assert_eq!(
        wire(&InterruptionCause::io_failure("the disk is full")),
        r#"{"kind":"io_failure","detail":"the disk is full"}"#
    );
    assert_eq!(
        wire(&InterruptionCause::foreign_edit(path("notes/a.md"))),
        r#"{"kind":"foreign_edit","path":"notes/a.md"}"#
    );
    assert_eq!(
        wire(&PlanFault::duplicate_id(operation_id("edit-a"), vec![0, 3])),
        r#"{"kind":"duplicate_id","id":"edit-a","positions":[0,3]}"#
    );
    assert_eq!(
        wire(&PlanFault::unknown_requirement(2, operation_id("make-z"))),
        r#"{"kind":"unknown_requirement","position":2,"requires":"make-z"}"#
    );
    assert_eq!(
        wire(&PlanFault::content_cycle(vec![0, 1])),
        r#"{"kind":"content_cycle","positions":[0,1]}"#
    );
    assert_eq!(
        wire(&PlanFault::transitions_disagree(vec![path("notes/a.md")])),
        r#"{"kind":"transitions_disagree","paths":["notes/a.md"]}"#
    );
    assert_eq!(
        PlanFault::transitions_disagree(vec![
            path("notes/b.md"),
            path("notes/a.md"),
            path("notes/b.md"),
        ]),
        PlanFault::transitions_disagree(vec![path("notes/a.md"), path("notes/b.md")]),
        "the constructor names each file once, in order"
    );
}

/// The outcomes that are not a refusal and not an answer carry the plan they
/// were given, so a caller can finish or retry by sending it again.
#[test]
fn the_apply_outcomes_carry_what_a_caller_sends_again() {
    let plan = wire(&a_bare_resolved_plan());
    assert_eq!(
        wire(&ErrorDetail::plan_interrupted(
            a_bare_resolved_plan(),
            vec![path("notes/b.md")],
            InterruptionCause::name_taken(path("notes/new.md")),
            Vec::new(),
        )),
        format!(
            concat!(
                r#"{{"code":"vault/plan-interrupted","plan":{plan},"landed":["notes/b.md"],"#,
                r#""cause":{{"kind":"name_taken","path":"notes/new.md"}},"forced":[]}}"#
            ),
            plan = plan
        )
    );
    assert_eq!(
        wire(&ErrorDetail::root_changed(
            a_root(),
            RootIdentity::from_device_and_inode(66_307, 2)
        )),
        concat!(
            r#"{"code":"vault/root-changed","expected":"00000000000103020000000000000002","#,
            r#""found":"00000000000103030000000000000002"}"#
        )
    );
    assert_eq!(
        wire(&ErrorDetail::write_failed(
            a_bare_resolved_plan(),
            "the disk is full",
            vec![path("notes/completed.md")]
        )),
        format!(
            r#"{{"code":"vault/write-failed","plan":{plan},"detail":"the disk is full","landed":["notes/completed.md"]}}"#
        )
    );
    assert_eq!(
        wire(&ErrorDetail::apply_outcome_unknown(a_bare_resolved_plan())),
        format!(r#"{{"code":"host/apply-outcome-unknown","plan":{plan}}}"#)
    );
    assert_eq!(
        wire(&ErrorDetail::apply_not_run(a_park(), None)),
        format!(
            r#"{{"code":"host/apply-not-run","cause":{},"plan":null}}"#,
            wire(&a_park())
        )
    );
    assert_eq!(
        wire(&ErrorDetail::plan_invalid(PlanFault::requires_cycle(vec![
            1, 2
        ]))),
        r#"{"code":"request/plan-invalid","fault":{"kind":"requires_cycle","positions":[1,2]}}"#
    );
}

/// A detail is an answer and drops a field it does not know, and an envelope
/// whose code is not its detail's refuses the read, as every envelope does.
#[test]
fn an_apply_envelope_is_read_as_every_envelope_is() {
    let envelope = ErrorEnvelope::new(
        "the root changed",
        ErrorDetail::root_changed(a_root(), RootIdentity::from_device_and_inode(66_307, 2)),
    );
    let json = serde_json::to_value(&envelope).expect("an envelope as JSON");
    let read: ErrorEnvelope = serde_json::from_str(&with_surprise(&json, "/detail"))
        .expect("a detail with a field it drops");
    assert_eq!(read, envelope);

    let mut mismatched = json.clone();
    mismatched["code"] = serde_json::Value::String("vault/plan-refused".to_string());
    assert!(
        serde_json::from_str::<ErrorEnvelope>(&mismatched.to_string()).is_err(),
        "an envelope whose code is not its detail's read back"
    );
}

/// Each plan carries its own `plan` tag wherever it is written, so the plan
/// alone and the document carrying it are one set of bytes.
#[test]
fn each_plan_carries_its_own_tag() {
    let authored = an_authored_plan();
    assert!(wire(&authored).starts_with(r#"{"plan":"operations","#));
    assert_eq!(wire(&authored), wire(&PlanDocument::operations(authored)));
    assert!(wire(&a_bare_resolved_plan()).starts_with(r#"{"plan":"resolved","#));
}

/// A plan read alone refuses a missing tag and the other plan's tag, as the
/// document carrying it does.
#[test]
fn a_plan_refuses_a_missing_or_mismatched_tag() {
    let resolved = serde_json::to_value(a_resolved_plan()).expect("a resolved plan as JSON");
    let authored = serde_json::to_value(an_authored_plan()).expect("an authored plan as JSON");
    let retagged = |plan: &serde_json::Value, tag: Option<&str>| -> String {
        let mut plan = plan.clone();
        let members = plan.as_object_mut().expect("a plan is an object");
        match tag {
            Some(tag) => members.insert("plan".to_string(), tag.into()),
            None => members.remove("plan"),
        };
        plan.to_string()
    };
    for tag in [None, Some("operations"), Some("draft")] {
        let json = retagged(&resolved, tag);
        assert!(
            serde_json::from_str::<ResolvedPlan>(&json).is_err(),
            "a resolved plan tagged {tag:?} read back"
        );
        assert!(
            serde_json::from_str::<PlanDocument>(&json).is_err(),
            "a document holding a resolved plan tagged {tag:?} read back"
        );
    }
    for tag in [None, Some("resolved"), Some("draft")] {
        let json = retagged(&authored, tag);
        assert!(
            serde_json::from_str::<AuthoredPlan>(&json).is_err(),
            "an authored plan tagged {tag:?} read back"
        );
        assert!(
            serde_json::from_str::<PlanDocument>(&json).is_err(),
            "a document holding an authored plan tagged {tag:?} read back"
        );
    }
    assert!(
        serde_json::from_str::<PlanDocument>(
            &retagged(&resolved, None).replace(r#"{"#, r#"{"plan":7,"#)
        )
        .is_err(),
        "a document whose tag is no string read back"
    );
}

/// **A resolved plan is its own retry token.** Every answer that carries one
/// carries bytes a caller sends back unchanged: the plan taken out of the
/// answer reads as the plan an apply request carries, equal to the plan that
/// went in.
#[test]
fn a_resolved_plan_in_any_answer_is_sent_back_verbatim() {
    let plan = a_resolved_plan();
    let mut carriers: Vec<(String, serde_json::Value)> = apply_reports()
        .into_iter()
        .filter(|report| {
            serde_json::to_value(report).expect("a report as JSON")["plan"]
                == serde_json::to_value(&plan).expect("a plan as JSON")
        })
        .map(|report| {
            let json = serde_json::to_value(&report).expect("a report as JSON");
            (tag_string(&report, "outcome"), json["plan"].clone())
        })
        .collect();
    let details = [
        ErrorDetail::plan_refused(
            plan.clone(),
            a_forecast(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ),
        ErrorDetail::plan_interrupted(
            plan.clone(),
            vec![path("notes/a.md")],
            InterruptionCause::io_failure("the disk is full"),
            Vec::new(),
        ),
        ErrorDetail::write_failed(plan.clone(), "the disk is full", Vec::new()),
        ErrorDetail::apply_not_run(a_park(), Some(plan.clone())),
        ErrorDetail::apply_outcome_unknown(plan.clone()),
    ];
    for detail in details {
        let envelope = ErrorEnvelope::new("the apply did not apply", detail);
        let json = serde_json::to_value(&envelope).expect("an envelope as JSON");
        carriers.push((
            json["code"].as_str().expect("a code").to_owned(),
            json["detail"]["plan"].clone(),
        ));
    }
    let carried: BTreeSet<&str> = carriers.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        carried,
        [
            "previewed",
            "applied",
            "vault/plan-refused",
            "vault/plan-interrupted",
            "vault/write-failed",
            "host/apply-not-run",
            "host/apply-outcome-unknown",
        ]
        .into_iter()
        .collect(),
        "an answer carrying a resolved plan is left out of this check"
    );
    for (carrier, carried) in carriers {
        let request = format!(r#"{{"mode":"apply","plan":{carried}}}"#);
        let params: ApplyParams = serde_json::from_str(&request).unwrap_or_else(|error| {
            panic!("the plan {carrier} carries does not read as a request's plan: {error}")
        });
        assert_eq!(
            params.plan,
            PlanDocument::resolved(plan.clone()),
            "the plan {carrier} carries read back as another plan"
        );
        assert_eq!(
            serde_json::to_value(&params.plan).expect("a plan as JSON"),
            carried
        );
    }
}

/// What the one applier decides for each kind, state and condition, written
/// the way it must be: a match with no wildcard arm and a destructuring with
/// no `..`, outside this crate. The four enums are plain and their variants
/// and the operation hold no hidden field, so a member minted or a field added
/// without a decision here fails to compile rather than falling into a
/// default or being ignored.
fn applier_decision(operation: &Operation) -> String {
    let Operation {
        kind,
        id,
        requires,
        footnote,
        conditions,
        cascade,
    } = operation;
    let writes = match kind {
        OperationKind::CreateDocument { path, content } => {
            format!("create {path} with {} bytes", content.len())
        }
        OperationKind::StrReplace {
            path,
            old_str,
            new_str,
        } => format!("edit {path}: {old_str} to {new_str}"),
        OperationKind::MoveDocument { from, to } => format!("move {from} to {to}"),
        OperationKind::DeleteDocument { path, backlinks } => match backlinks {
            Backlinks::Forbidden => format!("delete {path}"),
            Backlinks::RewrittenTo(rewrite_to) => {
                format!("delete {path}, its links to {rewrite_to}")
            }
            Backlinks::LeftBroken => format!("delete {path}, its links broken"),
        },
        OperationKind::MoveFolder { from, to } => format!("move folder {from} to {to}"),
        OperationKind::RewriteLink {
            path,
            syntax,
            from,
            to,
        } => format!("in {path}, {syntax:?} {from} to {to}"),
        OperationKind::RewriteWikilink { old, new } => format!("wikilinks to {old} to {new}"),
        OperationKind::CreateByRule { rule, .. } => format!("create by rule {rule:?}"),
        OperationKind::SetFrontmatter {
            target,
            field,
            value,
        } => format!(
            "set {field} of {} to {}",
            target_decision(target),
            value_decision(value)
        ),
        OperationKind::RemoveFrontmatter { target, field } => {
            format!("remove {field} of {}", target_decision(target))
        }
        OperationKind::PushFrontmatter {
            target,
            field,
            value,
        } => format!(
            "push {} onto {field} of {}",
            value_decision(value),
            target_decision(target)
        ),
        OperationKind::PopFrontmatter {
            target,
            field,
            value,
        } => format!(
            "pop {} from {field} of {}",
            value_decision(value),
            target_decision(target)
        ),
        OperationKind::ReplaceBody { path, content } => {
            format!("replace the body of {path} with {} bytes", content.len())
        }
        OperationKind::ReplaceSection {
            path,
            heading,
            content,
        } => format!("replace {heading} of {path} with {} bytes", content.len()),
        OperationKind::AppendToSection {
            path,
            heading,
            content,
        } => format!("append {} bytes to {heading} of {path}", content.len()),
        OperationKind::DeleteSection { path, heading } => format!("delete {heading} of {path}"),
        OperationKind::InsertBeforeHeading {
            path,
            heading,
            content,
        } => format!("insert {} bytes before {heading} of {path}", content.len()),
        OperationKind::InsertAfterHeading {
            path,
            heading,
            content,
        } => format!("insert {} bytes after {heading} of {path}", content.len()),
        OperationKind::WriteControlFile { file, content } => match file {
            ControlFile::Schema => format!("write the schema, {} bytes", content.len()),
            ControlFile::Config => format!("write the config, {} bytes", content.len()),
        },
    };
    let observed: Vec<String> = conditions
        .iter()
        .map(|condition| match condition {
            AuthorCondition::ContentHash { path, hash } => format!("{path} at {hash}"),
            AuthorCondition::ExpectedValue {
                path,
                field,
                expect,
            } => match expect {
                ExpectedField::Absent {} => format!("{path} without {field}"),
                ExpectedField::Present { value } => {
                    format!("{path} with {field} {}", value_decision(value))
                }
            },
        })
        .collect();
    let requires: Vec<&str> = requires.iter().map(OperationId::as_str).collect();
    let cascade: Vec<String> = cascade
        .iter()
        .map(
            |LinkRewrite {
                 path,
                 syntax,
                 from,
                 to,
             }| format!("{path}: {syntax:?} {from} to {to}"),
        )
        .collect();
    let decided = format!(
        "{writes} as {}, after [{}], noting {}; {}",
        id.as_ref().map_or("-", OperationId::as_str),
        requires.join(", "),
        footnote.as_deref().unwrap_or("-"),
        observed.join(", ")
    );
    if cascade.is_empty() {
        decided
    } else {
        format!("{decided}; cascading {}", cascade.join(", "))
    }
}

/// Which documents a frontmatter kind writes, decided with no wildcard arm.
fn target_decision(target: &WriteTarget) -> String {
    match target {
        WriteTarget::Path(path) => path.to_string(),
        WriteTarget::Where(predicates) => format!("{} predicates' matches", predicates.len()),
    }
}

/// What a written value is, decided with no wildcard arm: a shape minted
/// without a decision here fails to compile.
fn value_decision(value: &AuthoredValue) -> String {
    match value {
        AuthoredValue::Null => "null".to_string(),
        AuthoredValue::Bool(value) => format!("bool {value}"),
        AuthoredValue::Integer(value) => format!("integer {value}"),
        AuthoredValue::Float(value) => format!("float {value}"),
        AuthoredValue::String(value) => format!("string {value}"),
        AuthoredValue::List(items) => format!(
            "list [{}]",
            items
                .iter()
                .map(value_decision)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        AuthoredValue::Map(map) => format!(
            "map {{{}}}",
            map.entries()
                .iter()
                .map(|(key, value)| format!("{key}: {}", value_decision(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn plan_check(condition: &PlanCondition) -> String {
    match condition {
        PlanCondition::ContentHash { path, hash } => format!("{path} at {hash}"),
        PlanCondition::LinkResolution {
            link:
                LinkKey {
                    holder,
                    syntax,
                    address,
                },
            before,
            after,
        } => format!(
            "{syntax:?} {address} in {holder}: {} to {}",
            resolution_check(before),
            resolution_check(after)
        ),
    }
}

fn resolution_check(resolves: &Resolves) -> String {
    match resolves {
        Resolves::One { path } => format!("one {path}"),
        Resolves::None {} => "none".to_string(),
        Resolves::Several {} => "several".to_string(),
    }
}

fn state_check(state: &FileState) -> String {
    match state {
        FileState::Absent {} => "absent".to_string(),
        FileState::Present {
            hash,
            quarantined: false,
        } => format!("present at {hash}"),
        FileState::Present {
            hash,
            quarantined: true,
        } => format!("quarantined at {hash}"),
    }
}

/// Everything the one applier reads of a plan document, destructured with no
/// `..`: a field added to either plan, a transition or the provenance fails
/// to compile here until the applier decides what it means.
fn applier_reading(document: &PlanDocument) -> Vec<String> {
    match document {
        PlanDocument::Operations(AuthoredPlan {
            plan: OperationsTag,
            vault,
            operations,
            force,
            footnote,
        }) => {
            let mut read = vec![format!(
                "operations for {vault:?}, forced {force}, noting {footnote:?}"
            )];
            read.extend(operations.iter().map(applier_decision));
            read
        }
        PlanDocument::Resolved(ResolvedPlan {
            plan: ResolvedTag,
            vault,
            root,
            operations,
            transitions,
            conditions,
            provenance,
            force,
            footnote,
        }) => {
            let mut read = vec![format!(
                "resolved for {vault:?} at {root:?}, forced {force}, noting {footnote:?}"
            )];
            read.extend(operations.iter().map(applier_decision));
            read.extend(transitions.iter().map(
                |Transition {
                     path,
                     before,
                     after,
                 }| {
                    format!("{path}: {} to {}", state_check(before), state_check(after))
                },
            ));
            read.extend(conditions.iter().map(plan_check));
            if let Some(Provenance {
                finding_generation,
                skipped,
            }) = provenance
            {
                read.push(format!("planned from generation {finding_generation}"));
                read.extend(skipped.iter().map(|SkippedFinding { finding, reason }| {
                    format!("skipped {finding}: {reason}")
                }));
            }
            read
        }
    }
}

#[test]
fn the_applier_decides_every_kind_state_and_condition_without_a_default() {
    let decisions: Vec<String> = operations().iter().map(applier_decision).collect();
    assert_eq!(decisions.len(), operations().len());
    assert_eq!(
        decisions[decisions.len() - 2],
        "move notes/a.md to archive/a.md as -, after [], noting -; ; cascading notes/c.md: Wikilink a to archive/a, notes/d.md: Markdown a.md to ../archive/a.md"
    );
    assert_eq!(
        decisions.last().map(String::as_str),
        Some(
            format!(
                "edit notes/a.md: draft to final as edit-a, after [make-b], noting marks it final; notes/a.md at {}, notes/a.md with status string draft, notes/a.md without owner",
                hash_text(0xab)
            )
            .as_str()
        )
    );
    assert_eq!(
        decisions[1],
        "create by rule Some(\"meeting\") as -, after [], noting -; "
    );
    assert_eq!(
        decisions[5..8],
        [
            "move folder notes to archive/notes as -, after [], noting -; ",
            "in notes/c.md, Wikilink a to archive/a as -, after [], noting -; ",
            "wikilinks to a to archive/a as -, after [], noting -; ",
        ]
    );
    assert_eq!(
        decisions[8..12],
        [
            "set status of notes/a.md to string done as -, after [], noting -; ",
            "remove due of 1 predicates' matches as -, after [], noting -; ",
            "push string project onto tags of notes/a.md as -, after [], noting -; ",
            "pop string stale from tags of 1 predicates' matches as -, after [], noting -; ",
        ]
    );
    assert_eq!(
        plan_conditions().iter().map(plan_check).collect::<Vec<_>>(),
        [
            format!("notes/c.md at {}", hash_text(0xcd)),
            "Wikilink a in notes/c.md: one notes/a.md to none".to_string(),
            "Markdown vault://notes/a in notes/d.md: several to one archive/a.md".to_string(),
        ]
    );
    assert_eq!(
        file_states().iter().map(state_check).collect::<Vec<_>>(),
        [
            "absent".to_string(),
            format!("present at {}", hash_text(0x01)),
            format!("quarantined at {}", hash_text(0x02)),
        ]
    );
}

/// The applier reads every field of both plans, and a plan outside this crate
/// is written as a literal as well as through its constructor.
#[test]
fn the_applier_reads_every_field_of_a_plan() {
    let read = applier_reading(&PlanDocument::resolved(a_resolved_plan()));
    assert_eq!(
        read.last().map(String::as_str),
        Some("skipped 42: the target names two documents")
    );
    assert_eq!(
        applier_reading(&PlanDocument::operations(an_authored_plan())).len(),
        operations().len() + 1
    );
    let literal = ResolvedPlan {
        plan: ResolvedTag,
        vault: VaultAddress::name(name("notes")),
        root: a_root(),
        operations: Vec::new(),
        transitions: Vec::new(),
        conditions: Vec::new(),
        provenance: None,
        force: false,
        footnote: None,
    };
    assert_eq!(
        wire(&literal),
        r#"{"plan":"resolved","vault":{"by":"name","name":"notes"},"root":"00000000000103020000000000000002","operations":[],"transitions":[],"conditions":[]}"#
    );
    assert_eq!(
        wire(&AuthoredPlan {
            plan: OperationsTag,
            vault: VaultAddress::name(name("notes")),
            operations: Vec::new(),
            force: false,
            footnote: None,
        }),
        r#"{"plan":"operations","vault":{"by":"name","name":"notes"},"operations":[]}"#
    );
}

/// What the one applier does with a request, written with no wildcard arm:
/// a mode or a document minted without a decision here fails to compile
/// rather than falling into "otherwise apply".
fn applier_route(params: &ApplyParams) -> String {
    let mode = match params.mode {
        ApplyMode::Preview => "preview",
        ApplyMode::Apply => "apply",
    };
    let plan = match &params.plan {
        PlanDocument::Operations(_) => "the operations",
        PlanDocument::Resolved(_) => "the resolved plan",
    };
    format!("{mode} {plan}")
}

#[test]
fn the_applier_decides_every_mode_and_document_without_a_default() {
    let routes: Vec<String> = plan_documents()
        .into_iter()
        .take(3)
        .flat_map(|document| {
            apply_modes()
                .into_iter()
                .map(move |mode| applier_route(&ApplyParams::new(mode, document.clone())))
        })
        .collect();
    assert_eq!(
        routes,
        [
            "preview the operations",
            "apply the operations",
            "preview the operations",
            "apply the operations",
            "preview the resolved plan",
            "apply the resolved plan",
        ]
    );
}

// ── Document-local writes ────────────────────────────────────────────────

/// A value that reads from `json`, or the refusal it reads as.
fn authored_value(json: &str) -> Result<AuthoredValue, String> {
    serde_json::from_str::<AuthoredValue>(json).map_err(|error| error.to_string())
}

/// **A written value is the plain JSON value of its shape.** Null, a
/// boolean, an integer, a float, a string, a list and a map are written as
/// themselves, with no tag, and each reads back as the shape it was written
/// as — an integer never as a float, nor a float as an integer.
#[test]
fn a_written_value_is_the_plain_value_of_its_shape() {
    let map = AuthoredValue::map([
        ("zeta".to_string(), AuthoredValue::Integer(1)),
        ("alpha".to_string(), AuthoredValue::Null),
    ])
    .expect("distinct keys");
    let pinned = [
        (AuthoredValue::Null, "null"),
        (AuthoredValue::Bool(true), "true"),
        (AuthoredValue::Integer(-3), "-3"),
        (AuthoredValue::float(2.5).expect("a finite float"), "2.5"),
        (AuthoredValue::float(1.0).expect("a finite float"), "1.0"),
        (AuthoredValue::string("done"), r#""done""#),
        (
            AuthoredValue::list([AuthoredValue::string("a"), AuthoredValue::Integer(2)]),
            r#"["a",2]"#,
        ),
        (map, r#"{"zeta":1,"alpha":null}"#),
        (AuthoredValue::Integer(i64::MAX), "9223372036854775807"),
    ];
    for (value, json) in pinned {
        assert_eq!(wire(&value), json);
        assert_eq!(authored_value(json), Ok(value.clone()), "{json}");
        round_trip(&value);
    }
    assert_ne!(authored_value("1"), authored_value("1.0"));
}

/// **A map keeps the order its keys are written in**, which is the order the
/// document writes them, rather than sorting them.
#[test]
fn a_written_map_keeps_the_order_its_keys_are_written_in() {
    let read = authored_value(r#"{"b":1,"a":2,"c":{"z":1,"y":2}}"#).expect("a map");
    assert_eq!(wire(&read), r#"{"b":1,"a":2,"c":{"z":1,"y":2}}"#);
    let AuthoredValue::Map(map) = read else {
        panic!("a map reads as a map");
    };
    let keys: Vec<&str> = map.entries().iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(keys, ["b", "a", "c"]);
}

/// **A written value refuses what no frontmatter field can hold**: a float
/// that is `NaN` or an infinity, in any format that can spell one, an
/// integer past the signed 64-bit range, and a map key written twice at any
/// depth — which is refused rather than keeping either value.
#[test]
fn a_written_value_refuses_a_non_finite_float_a_wide_integer_and_a_repeated_key() {
    for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(AuthoredValue::float(number).is_err(), "{number} was built");
        let deserializer: F64Deserializer<ValueError> = number.into_deserializer();
        assert!(
            AuthoredValue::deserialize(deserializer).is_err(),
            "{number} was read as a written value"
        );
        let deserializer: F64Deserializer<ValueError> = number.into_deserializer();
        assert!(
            norn_wire::FiniteFloat::deserialize(deserializer).is_err(),
            "{number} was read as a finite float"
        );
    }
    for yaml in [".nan", ".inf", "-.inf", "[1, .nan]"] {
        assert!(
            serde_yaml::from_str::<AuthoredValue>(yaml).is_err(),
            "`{yaml}` was read as a written value"
        );
    }
    assert!(authored_value("9223372036854775808").is_err());
    for json in [
        r#"{"a":1,"a":2}"#,
        r#"{"a":1,"b":{"c":1,"c":1}}"#,
        r#"[{"a":1,"a":1}]"#,
    ] {
        let refusal = authored_value(json).expect_err(json);
        assert!(refusal.contains("twice"), "{json}: {refusal}");
    }
    assert_eq!(
        ValueMap::new([
            ("a".to_string(), AuthoredValue::Null),
            ("a".to_string(), AuthoredValue::Null),
        ]),
        Err(norn_wire::DuplicateKey("a".to_string()))
    );
}

/// **Only a `u64` above `i64::MAX` is refused as an integer.** Numbers JSON
/// delivers as floats, which includes an integer beyond `u64` or below
/// `i64::MIN`, read as floats that no longer hold the integer exactly.
#[test]
fn an_integer_beyond_the_signed_range_is_refused_or_read_as_a_float() {
    assert!(authored_value("9223372036854775808").is_err());
    assert!(authored_value("18446744073709551615").is_err());
    for (json, float) in [
        ("18446744073709551616", 18_446_744_073_709_551_616.0),
        ("-9223372036854775809", -9_223_372_036_854_775_809.0),
    ] {
        assert_eq!(
            authored_value(json).expect(json),
            AuthoredValue::float(float).unwrap(),
            "{json}"
        );
    }
}

/// **An empty `where` list is refused wherever a target is read.** A
/// conjunction of no predicates matches every document, so `set --where []`
/// would write the whole vault.
#[test]
fn an_empty_where_list_is_refused() {
    let alone = r#"{"where":[]}"#;
    assert!(
        serde_json::from_str::<WriteTarget>(alone).is_err(),
        "{alone} read as a target"
    );
    let operation =
        r#"{"kind":"set_frontmatter","fields":{"where":[],"field":"status","value":"done"}}"#;
    let refusal = serde_json::from_str::<Operation>(operation)
        .expect_err("an empty where read as an operation");
    assert!(
        refusal.to_string().contains("at least one predicate"),
        "{refusal}"
    );
    let request = r#"{"vault":{"by":"name","name":"notes"},"mode":"preview","where":[],"changes":[{"change":"remove","field":"due"}]}"#;
    assert!(serde_json::from_str::<SetParams>(request).is_err());
}

/// **A frontmatter kind names its documents by exactly one of `path` and
/// `where`.** Both, or neither, is refused wherever a target is read: alone,
/// among an operation's fields, and among a `set` request's keys.
#[test]
fn a_target_is_exactly_one_of_path_and_where() {
    let where_list = r#"[{"op":"eq","key":"status","value":"draft"}]"#;
    assert_eq!(
        serde_json::from_str::<WriteTarget>(r#"{"path":"notes/a.md"}"#).ok(),
        Some(WriteTarget::path(path("notes/a.md")))
    );
    assert_eq!(
        serde_json::from_str::<WriteTarget>(&format!(r#"{{"where":{where_list}}}"#)).ok(),
        Some(WriteTarget::matching(drafts()))
    );
    for target in [
        format!(r#""path":"notes/a.md","where":{where_list},"#),
        String::new(),
    ] {
        let alone = format!("{{{}}}", target.trim_end_matches(','));
        assert!(
            serde_json::from_str::<WriteTarget>(&alone).is_err(),
            "{alone} read as a target"
        );
        let operation = format!(
            r#"{{"kind":"set_frontmatter","fields":{{{target}"field":"status","value":"done"}}}}"#
        );
        let refusal = serde_json::from_str::<Operation>(&operation)
            .expect_err(&format!("{operation} read as an operation"));
        assert!(
            refusal.to_string().contains("`path` or `where`"),
            "{refusal}"
        );
        assert!(serde_json::from_str::<OperationKind>(&operation).is_err());
        let request = format!(
            r#"{{"vault":{{"by":"name","name":"notes"}},"mode":"preview",{target}"changes":[{{"change":"remove","field":"due"}}]}}"#
        );
        assert!(
            serde_json::from_str::<SetParams>(&request).is_err(),
            "{request} read as a request"
        );
    }
}

/// **A kind's fields are its own**: a document-local kind carrying another
/// kind's field, or lacking one of its own, is refused — a `where` target on
/// a kind that names its document by path included.
#[test]
fn a_document_local_kind_refuses_fields_that_are_not_its_own() {
    for json in [
        r#"{"kind":"set_frontmatter","fields":{"path":"a.md","field":"k","value":1,"heading":"H"}}"#,
        r#"{"kind":"set_frontmatter","fields":{"path":"a.md","field":"k"}}"#,
        r#"{"kind":"remove_frontmatter","fields":{"path":"a.md","field":"k","value":1}}"#,
        r#"{"kind":"push_frontmatter","fields":{"path":"a.md","value":1}}"#,
        r#"{"kind":"pop_frontmatter","fields":{"path":"a.md","field":"k","value":1,"content":"x"}}"#,
        r#"{"kind":"replace_body","fields":{"path":"a.md","content":"x","heading":"H"}}"#,
        r#"{"kind":"replace_section","fields":{"path":"a.md","heading":"H","content":"x","value":1}}"#,
        r#"{"kind":"replace_section","fields":{"path":"a.md","content":"x"}}"#,
        r#"{"kind":"append_to_section","fields":{"path":"a.md","heading":"H"}}"#,
        r#"{"kind":"delete_section","fields":{"path":"a.md","heading":"H","content":"x"}}"#,
        r#"{"kind":"insert_before_heading","fields":{"where":[],"heading":"H","content":"x"}}"#,
        r#"{"kind":"insert_after_heading","fields":{"path":"a.md","heading":"H","old_str":"x"}}"#,
        r#"{"kind":"delete_document","fields":{"where":[]}}"#,
        r#"{"kind":"str_replace","fields":{"path":"a.md","old_str":"a","new_str":"b","field":"k"}}"#,
        r#"{"kind":"set_frontmatter","fields":{"path":"a.md","field":"k","value":1,"value":2}}"#,
    ] {
        assert!(
            serde_json::from_str::<Operation>(json).is_err(),
            "reading {json} produced an operation"
        );
    }
    let null_value: Operation = serde_json::from_str(
        r#"{"kind":"set_frontmatter","fields":{"path":"a.md","field":"k","value":null}}"#,
    )
    .expect("a field set to null");
    assert_eq!(
        null_value.kind,
        OperationKind::set_frontmatter(WriteTarget::path(path("a.md")), "k", AuthoredValue::Null)
    );
}

/// **An expected value is absent, or present with exactly a value**, tagged
/// `state` as a file state is, so a field holding null is present with the
/// value `null` and never read as absent.
#[test]
fn an_expected_value_is_absent_or_present_with_its_value() {
    let pinned = [
        (
            ExpectedField::absent(),
            r#"{"condition":"expected_value","path":"notes/a.md","field":"owner","expect":{"state":"absent"}}"#,
        ),
        (
            ExpectedField::present(AuthoredValue::Null),
            r#"{"condition":"expected_value","path":"notes/a.md","field":"owner","expect":{"state":"present","value":null}}"#,
        ),
        (
            ExpectedField::present(AuthoredValue::list([AuthoredValue::string("x")])),
            r#"{"condition":"expected_value","path":"notes/a.md","field":"owner","expect":{"state":"present","value":["x"]}}"#,
        ),
    ];
    for (expect, json) in pinned {
        let condition = AuthorCondition::expected_value(path("notes/a.md"), "owner", expect);
        assert_eq!(wire(&condition), json);
        round_trip(&condition);
    }
    assert_ne!(
        ExpectedField::absent(),
        ExpectedField::present(AuthoredValue::Null)
    );
    for expect in [
        r#"{"state":"absent","value":null}"#,
        r#"{"state":"present"}"#,
        r#"{"state":"missing"}"#,
        r#"{"value":1}"#,
    ] {
        let json = format!(
            r#"{{"condition":"expected_value","path":"notes/a.md","field":"owner","expect":{expect}}}"#
        );
        assert!(
            serde_json::from_str::<AuthorCondition>(&json).is_err(),
            "{json} read as a condition"
        );
    }
    assert!(
        serde_json::from_str::<AuthorCondition>(
            r#"{"condition":"expected_value","path":"notes/a.md","field":"owner"}"#
        )
        .is_err(),
        "an expected value observing nothing read as a condition"
    );
}

/// **A plan's force is `false` unless it is written `true`.** A plan written
/// without it reads as not forced and is written back without it, so every
/// plan made before the flag existed reads and writes as it did; a forced
/// plan writes `"force":true` and reads back forced, alone and as a
/// document; and `null` is no flag.
#[test]
fn a_plan_is_forced_only_where_it_says_so() {
    let unforced = wire(&a_bare_resolved_plan());
    assert!(!unforced.contains("force"), "{unforced}");
    let forced = a_bare_resolved_plan().with_force(true);
    let json = wire(&forced);
    assert_eq!(
        json,
        spliced(
            &unforced,
            r#""conditions":[]"#,
            r#""conditions":[],"force":true"#
        )
    );
    round_trip(&forced);
    assert_eq!(
        serde_json::from_str::<PlanDocument>(&json).ok(),
        Some(PlanDocument::resolved(forced))
    );
    let authored = an_authored_plan().with_force(true);
    let json = wire(&PlanDocument::operations(authored.clone()));
    assert!(json.ends_with(r#""force":true}"#), "{json}");
    assert_eq!(
        serde_json::from_str::<PlanDocument>(&json).ok(),
        Some(PlanDocument::operations(authored))
    );
    assert_eq!(
        serde_json::from_str::<AuthoredPlan>(
            r#"{"plan":"operations","vault":{"by":"name","name":"notes"},"operations":[]}"#
        )
        .map(|plan| plan.force)
        .ok(),
        Some(false)
    );
    for plan in [
        r#"{"plan":"operations","vault":{"by":"name","name":"notes"},"operations":[],"force":null}"#,
        r#"{"plan":"operations","vault":{"by":"name","name":"notes"},"operations":[],"force":"yes"}"#,
        r#"{"plan":"operations","vault":{"by":"name","name":"notes"},"operations":[],"force":true,"force":false}"#,
    ] {
        assert!(
            serde_json::from_str::<PlanDocument>(plan).is_err(),
            "{plan} read as a plan"
        );
    }
}

/// **A resolved plan carries only path targets.** Planning expands a `where`
/// target, so a resolved plan still carrying one names each such operation
/// by its position in the fault it answers with; one naming only paths has
/// no such fault.
#[test]
fn a_resolved_plan_with_a_where_target_is_a_fault() {
    let set = |target| {
        Operation::new(OperationKind::set_frontmatter(
            target,
            "status",
            AuthoredValue::string("done"),
        ))
    };
    let mut plan = a_bare_resolved_plan();
    plan.operations
        .push(set(WriteTarget::path(path("notes/a.md"))));
    assert_eq!(plan.unexpanded_targets(), None);
    plan.operations.push(set(WriteTarget::matching(drafts())));
    plan.operations
        .push(Operation::new(OperationKind::remove_frontmatter(
            WriteTarget::matching(Vec::new()),
            "due",
        )));
    assert_eq!(
        plan.unexpanded_targets(),
        Some(PlanFault::unexpanded_target(vec![2, 3]))
    );
    assert_eq!(
        wire(&PlanFault::unexpanded_target(vec![2, 3])),
        r#"{"kind":"unexpanded_target","positions":[2,3]}"#
    );
}

/// **A cascade travels on the operation that caused it**, after every part
/// an author writes, as one rewrite per holding document in a
/// `rewrite_link`'s own four fields; it is left out where there is none.
#[test]
fn a_cascade_travels_on_the_operation_that_caused_it() {
    let moved = Operation::new(OperationKind::move_document(
        path("notes/a.md"),
        path("archive/a.md"),
    ))
    .with_id(operation_id("move-a"))
    .with_cascade(a_cascade());
    let json = concat!(
        r#"{"kind":"move_document","fields":{"from":"notes/a.md","to":"archive/a.md"},"id":"move-a","#,
        r#""cascade":[{"path":"notes/c.md","syntax":"wikilink","from":"a","to":"archive/a"},"#,
        r#"{"path":"notes/d.md","syntax":"markdown","from":"a.md","to":"../archive/a.md"}]}"#
    );
    assert_eq!(wire(&moved), json);
    round_trip(&moved);
    for rewrite in a_cascade() {
        round_trip(&rewrite);
        let LinkRewrite {
            path,
            syntax,
            from,
            to,
        } = rewrite.clone();
        let as_kind = serde_json::to_value(OperationKind::rewrite_link(path, syntax, from, to))
            .expect("a kind as JSON");
        assert_eq!(as_kind["kind"], "rewrite_link");
        assert_eq!(
            as_kind["fields"],
            serde_json::to_value(&rewrite).expect("a rewrite as JSON"),
            "a cascade's rewrite is not a `rewrite_link`'s fields"
        );
    }
    for refused in [
        json.replace(r#""syntax":"wikilink""#, r#""syntax":"embed""#),
        json.replace(r#""to":"archive/a"}"#, r#""to":"archive/a","anchor":"x"}"#),
        json.replace(r#""path":"notes/c.md","#, ""),
        json.replace(r#""cascade":["#, r#""cascade":null,"x":["#),
    ] {
        assert!(
            serde_json::from_str::<Operation>(&refused).is_err(),
            "{refused} read as an operation"
        );
    }
}

/// **Planning writes a cascade; an author does not.** An authored plan
/// carrying one names each operation that does, and a resolved plan names
/// each operation carrying one on a kind that does not cascade — a document
/// move, a document removal rewriting the links naming its document and a
/// wikilink rewrite may. A removal forbidding those links, or leaving them
/// broken, rewrites none, so it carries no cascade either.
#[test]
fn a_cascade_where_planning_writes_none_is_a_fault() {
    let cascading = |kind| Operation::new(kind).with_cascade(a_cascade());
    let target = |text: &str| ResolutionTarget::new(text).expect("a target");
    let mut authored = AuthoredPlan::new(
        VaultAddress::name(name("notes")),
        vec![Operation::new(OperationKind::move_document(
            path("notes/a.md"),
            path("archive/a.md"),
        ))],
    );
    assert_eq!(authored.misplaced_cascades(), None);
    authored
        .operations
        .push(cascading(OperationKind::move_document(
            path("notes/b.md"),
            path("archive/b.md"),
        )));
    assert_eq!(
        authored.misplaced_cascades(),
        Some(PlanFault::misplaced_cascade(vec![1]))
    );

    let mut resolved = a_bare_resolved_plan();
    resolved.operations = vec![
        cascading(OperationKind::move_document(
            path("notes/a.md"),
            path("archive/a.md"),
        )),
        cascading(OperationKind::delete_document_rewriting(
            path("notes/b.md"),
            target("c"),
        )),
        cascading(OperationKind::rewrite_wikilink(target("a"), target("b"))),
    ];
    assert_eq!(resolved.misplaced_cascades(), None);
    resolved.operations.extend([
        cascading(OperationKind::str_replace(path("notes/a.md"), "x", "y")),
        cascading(OperationKind::rewrite_link(
            path("notes/c.md"),
            LinkFamily::Wikilink,
            "a",
            "b",
        )),
        cascading(OperationKind::delete_document(path("notes/d.md"))),
        cascading(OperationKind::delete_document_breaking_links(path(
            "notes/e.md",
        ))),
    ]);
    assert_eq!(
        resolved.misplaced_cascades(),
        Some(PlanFault::misplaced_cascade(vec![3, 4, 5, 6]))
    );
    assert_eq!(
        wire(&PlanFault::misplaced_cascade(vec![3, 4])),
        r#"{"kind":"misplaced_cascade","positions":[3,4]}"#
    );
}

/// **A plan that changes a vault control file changes nothing else** (ADR
/// 0032). An authored plan and a resolved plan alike name each control-file
/// write that stands beside an operation on documents; a plan of control-file
/// writes alone, and a plan of document operations alone, carry no fault.
#[test]
fn a_control_file_write_beside_a_document_operation_is_a_fault() {
    let schema = || {
        Operation::new(OperationKind::write_control_file(
            ControlFile::Schema,
            "version: 1\n",
        ))
    };
    let config = || Operation::new(OperationKind::write_control_file(ControlFile::Config, ""));
    let document = || Operation::new(OperationKind::delete_document(path("notes/a.md")));
    let authored = |operations| AuthoredPlan::new(VaultAddress::name(name("notes")), operations);
    assert_eq!(
        authored(vec![schema(), config()]).control_files_beside_documents(),
        None
    );
    assert_eq!(
        authored(vec![document(), document()]).control_files_beside_documents(),
        None
    );
    assert_eq!(
        authored(vec![document(), schema(), config()]).control_files_beside_documents(),
        Some(PlanFault::control_file_beside_documents(vec![1, 2]))
    );

    let mut resolved = a_bare_resolved_plan();
    resolved.operations = vec![config()];
    assert_eq!(resolved.control_files_beside_documents(), None);
    resolved.operations.insert(0, document());
    assert_eq!(
        resolved.control_files_beside_documents(),
        Some(PlanFault::control_file_beside_documents(vec![1]))
    );
    assert_eq!(
        wire(&PlanFault::control_file_beside_documents(vec![1])),
        r#"{"kind":"control_file_beside_documents","positions":[1]}"#
    );
}

/// **A resolved plan carries no folder move.** Planning expands one into a
/// document move per document the folder holds, as it expands a `where`
/// target, so a resolved plan still carrying one names it in the same fault.
#[test]
fn a_resolved_plan_with_a_folder_move_is_a_fault() {
    let mut plan = a_bare_resolved_plan();
    plan.operations
        .push(Operation::new(OperationKind::move_folder(
            folder("notes"),
            folder("archive"),
        )));
    assert_eq!(
        plan.unexpanded_targets(),
        Some(PlanFault::unexpanded_target(vec![1]))
    );
}

/// **An operation planning expands carries no identifier and requires
/// nothing.** A `where` target expands into one operation per matched
/// document, so it names no one operation another could require, and what it
/// would require could not change what it matches; a folder move expands
/// into one document move per document it holds, so it is held to the same
/// rule. An authored plan names each such operation
/// by its position in the fault it answers with, and one whose expanded
/// operations carry neither has no such fault.
#[test]
fn an_authored_expanded_operation_carrying_an_id_or_a_requirement_is_a_fault() {
    let set = |target| {
        Operation::new(OperationKind::set_frontmatter(
            target,
            "status",
            AuthoredValue::string("done"),
        ))
    };
    let mut plan = AuthoredPlan::new(
        VaultAddress::name(name("notes")),
        vec![
            set(WriteTarget::path(path("notes/a.md"))).with_id(operation_id("first")),
            set(WriteTarget::matching(drafts())),
        ],
    );
    assert_eq!(plan.ordered_expanded_targets(), None);
    plan.operations
        .push(set(WriteTarget::matching(drafts())).with_id(operation_id("bulk")));
    plan.operations
        .push(set(WriteTarget::matching(drafts())).with_requires(vec![operation_id("first")]));
    plan.operations.push(
        Operation::new(OperationKind::move_folder(
            FolderPath::new("notes").expect("a folder path"),
            FolderPath::new("archive").expect("a folder path"),
        ))
        .with_id(operation_id("folder")),
    );
    assert_eq!(
        plan.ordered_expanded_targets(),
        Some(PlanFault::expanded_target_ordered(vec![2, 3, 4]))
    );
    assert_eq!(
        wire(&PlanFault::expanded_target_ordered(vec![2, 3])),
        r#"{"kind":"expanded_target_ordered","positions":[2,3]}"#
    );
}

/// **A force is loud, in the shape a refusal carries.** The forecast, the
/// applied report and an interruption list every violation a force let
/// through, each exactly the object a schema-violation check is without its
/// `check` tag.
#[test]
fn a_forced_violation_is_listed_in_the_shape_a_refusal_carries() {
    let violation = SchemaViolation::new(
        path("notes/a.md"),
        FindingKind::UndeclaredTag,
        Some("draft".to_string()),
        "the tag `draft` is not declared",
    );
    let refused = serde_json::to_value(RefusedCheck::schema_violation(
        path("notes/a.md"),
        FindingKind::UndeclaredTag,
        Some("draft".to_string()),
        "the tag `draft` is not declared",
    ))
    .expect("a check as JSON");
    let mut without_tag = refused.as_object().expect("an object").clone();
    without_tag.remove("check");
    assert_eq!(
        serde_json::to_value(&violation).expect("a violation as JSON"),
        serde_json::Value::Object(without_tag)
    );
    let forecast = a_forecast().with_forced(vec![violation.clone()]);
    round_trip(&forecast);
    let forced_json = wire(&vec![violation.clone()]);
    assert!(
        wire(&forecast).ends_with(&format!(
            r#""forced":{forced_json},"links":[],"left_behind":[]}}"#
        )),
        "{}",
        wire(&forecast)
    );
    let applied = ApplyReport::applied(
        a_bare_resolved_plan().with_force(true),
        ChangesetOutcome::Committed,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .with_forced(vec![violation.clone()]);
    round_trip(&applied);
    assert!(
        wire(&applied).ends_with(&format!(r#""forced":{forced_json}}}"#)),
        "{}",
        wire(&applied)
    );
    let interrupted = ErrorDetail::plan_interrupted(
        a_bare_resolved_plan().with_force(true),
        vec![path("notes/a.md")],
        InterruptionCause::io_failure("the disk is full"),
        vec![violation.clone()],
    );
    round_trip(&interrupted);
    assert!(
        wire(&interrupted).ends_with(&format!(r#""forced":{forced_json}}}"#)),
        "{}",
        wire(&interrupted)
    );
    let previewed =
        ApplyReport::previewed(a_bare_resolved_plan(), a_forecast()).with_forced(vec![violation]);
    assert_eq!(
        previewed,
        ApplyReport::previewed(a_bare_resolved_plan(), forecast)
    );
}

// ── The write verbs' requests ────────────────────────────────────────────

fn notes() -> VaultAddress {
    VaultAddress::name(name("notes"))
}

/// **A `set` compiles to one frontmatter operation per change**, in order,
/// each with the request's target and conditions, forced as the request is.
#[test]
fn a_set_request_compiles_to_one_operation_per_change() {
    let condition = AuthorCondition::expected_value(
        path("notes/a.md"),
        "status",
        ExpectedField::present(AuthoredValue::string("draft")),
    );
    let target = WriteTarget::matching(drafts());
    let request = SetParams::new(
        notes(),
        ApplyMode::Preview,
        target.clone(),
        vec![
            FieldChange::set("status", AuthoredValue::string("done")),
            FieldChange::remove("due"),
            FieldChange::push("tags", AuthoredValue::string("closed")),
            FieldChange::pop("tags", AuthoredValue::string("open")),
        ],
    )
    .with_conditions(vec![condition.clone()])
    .with_force(true);
    round_trip(&request);
    let operation = |kind| Operation::new(kind).with_conditions(vec![condition.clone()]);
    assert_eq!(
        request.plan(),
        AuthoredPlan::new(
            notes(),
            vec![
                operation(OperationKind::set_frontmatter(
                    target.clone(),
                    "status",
                    AuthoredValue::string("done")
                )),
                operation(OperationKind::remove_frontmatter(target.clone(), "due")),
                operation(OperationKind::push_frontmatter(
                    target.clone(),
                    "tags",
                    AuthoredValue::string("closed")
                )),
                operation(OperationKind::pop_frontmatter(
                    target,
                    "tags",
                    AuthoredValue::string("open")
                )),
            ],
        )
        .with_force(true)
    );
}

/// A `set` request names its documents by the keys an operation does, and is
/// read by hand: pinned bytes read back, a path request compiles to path
/// operations, and a request without its mode, with an unknown key, or with
/// no change is refused.
#[test]
fn a_set_request_is_its_target_keys_beside_its_own() {
    let json = r#"{"vault":{"by":"name","name":"notes"},"mode":"apply","path":"notes/a.md","changes":[{"change":"set","field":"rank","value":2}]}"#;
    let request: SetParams = serde_json::from_str(json).expect("a set request");
    assert_eq!(wire(&request), json);
    assert_eq!(request.mode, ApplyMode::Apply);
    assert_eq!(
        request.plan(),
        AuthoredPlan::new(
            notes(),
            vec![Operation::new(OperationKind::set_frontmatter(
                WriteTarget::path(path("notes/a.md")),
                "rank",
                AuthoredValue::Integer(2),
            ))],
        )
    );
    for refused in [
        json.replace(r#""mode":"apply","#, ""),
        json.replace(r#""changes":"#, r#""surprise":1,"changes":"#),
        json.replace(r#"[{"change":"set","field":"rank","value":2}]"#, "[]"),
        json.replace(r#""field":"rank""#, r#""field":"rank","heading":"H""#),
        json.replace(r#""change":"set""#, r#""change":"add""#),
    ] {
        assert!(
            serde_json::from_str::<SetParams>(&refused).is_err(),
            "{refused} read as a set request"
        );
    }
}

/// **An `edit` compiles to one operation per edit on its document**, in
/// order, each carrying the request's conditions.
#[test]
fn an_edit_request_compiles_to_one_operation_per_edit() {
    let condition = AuthorCondition::content_hash(path("notes/a.md"), content_hash(0xab));
    let request = EditParams::new(
        notes(),
        ApplyMode::Apply,
        path("notes/a.md"),
        vec![
            DocumentEdit::str_replace("draft", "final"),
            DocumentEdit::replace_section("Notes", "New.\n"),
            DocumentEdit::append_to_section("Log", "- done\n"),
            DocumentEdit::delete_section("Scratch"),
            DocumentEdit::insert_before_heading("Notes", "Intro.\n"),
            DocumentEdit::insert_after_heading("Notes", "First.\n"),
            DocumentEdit::replace_body("Body.\n"),
        ],
    )
    .with_conditions(vec![condition.clone()]);
    round_trip(&request);
    let at = || path("notes/a.md");
    let expected: Vec<Operation> = [
        OperationKind::str_replace(at(), "draft", "final"),
        OperationKind::replace_section(at(), "Notes", "New.\n"),
        OperationKind::append_to_section(at(), "Log", "- done\n"),
        OperationKind::delete_section(at(), "Scratch"),
        OperationKind::insert_before_heading(at(), "Notes", "Intro.\n"),
        OperationKind::insert_after_heading(at(), "Notes", "First.\n"),
        OperationKind::replace_body(at(), "Body.\n"),
    ]
    .into_iter()
    .map(|kind| Operation::new(kind).with_conditions(vec![condition.clone()]))
    .collect();
    assert_eq!(request.plan(), AuthoredPlan::new(notes(), expected));
    let json = r#"{"vault":{"by":"name","name":"notes"},"mode":"preview","path":"notes/a.md","edits":[{"edit":"delete_section","heading":"Scratch"}]}"#;
    let read: EditParams = serde_json::from_str(json).expect("an edit request");
    assert_eq!(wire(&read), json);
    for refused in [
        json.replace(r#""mode":"preview","#, ""),
        json.replace(r#"[{"edit":"delete_section","heading":"Scratch"}]"#, "[]"),
        json.replace(
            r#""heading":"Scratch""#,
            r#""heading":"Scratch","content":"x""#,
        ),
        json.replace(r#""edits":"#, r#""force":true,"surprise":1,"edits":"#),
    ] {
        assert!(
            serde_json::from_str::<EditParams>(&refused).is_err(),
            "{refused} read as an edit request"
        );
    }
}

/// **A `new` compiles to one `create_document`** carrying the request's
/// conditions, forced as the request is.
#[test]
fn a_new_request_compiles_to_one_create() {
    let request = NewParams::new(notes(), ApplyMode::Preview, path("inbox/new.md"), "# New\n")
        .with_force(true);
    round_trip(&request);
    assert_eq!(
        wire(&request),
        r##"{"vault":{"by":"name","name":"notes"},"mode":"preview","path":"inbox/new.md","content":"# New\n","force":true}"##
    );
    assert_eq!(
        request.plan(),
        AuthoredPlan::new(
            notes(),
            vec![Operation::new(OperationKind::create_document(
                path("inbox/new.md"),
                "# New\n"
            ))],
        )
        .with_force(true)
    );
    for refused in [
        r#"{"vault":{"by":"name","name":"notes"},"path":"a.md","content":""}"#,
        r#"{"vault":{"by":"name","name":"notes"},"mode":"apply","path":"a.md","content":"","title":"A"}"#,
    ] {
        assert!(
            serde_json::from_str::<NewParams>(refused).is_err(),
            "{refused} read as a new request"
        );
    }
}

/// A `new` request body for `notes`, in preview, carrying `rest` after
/// `mode`.
fn new_request(rest: &str) -> String {
    format!(r#"{{"vault":{{"by":"name","name":"notes"}},"mode":"preview",{rest}}}"#)
}

/// **A `new` is exactly one of three forms**, and each compiles to its own
/// operation: a path with its content to one `create_document`, as it always
/// has; a rule name with optional variables, fields and body, and the inbox
/// capture naming no rule, each to one `create_by_rule`. The rule crosses as
/// `as`, and the parts a form does not carry are left out of the bytes.
#[test]
fn a_new_request_compiles_each_of_its_three_forms() {
    let document = NewParams::new(notes(), ApplyMode::Preview, path("inbox/new.md"), "# New\n");
    round_trip(&document);
    assert_eq!(
        document.plan().operations[0].kind,
        OperationKind::create_document(path("inbox/new.md"), "# New\n")
    );

    let by_rule = NewParams::for_subject(
        notes(),
        ApplyMode::Apply,
        NewSubject::by_rule(
            "meeting",
            variables(&[("project", "norn")]),
            a_value_map(),
            Some("Agenda.\n".to_string()),
        ),
    )
    .with_force(true);
    round_trip(&by_rule);
    assert_eq!(
        wire(&by_rule),
        r#"{"vault":{"by":"name","name":"notes"},"mode":"apply","as":"meeting","variables":{"project":"norn"},"fields":{"status":"draft","owner":{"name":"drew","tags":["a","b"]}},"body":"Agenda.\n","force":true}"#
    );
    assert_eq!(
        by_rule.plan(),
        AuthoredPlan::new(
            notes(),
            vec![Operation::new(OperationKind::create_by_rule(
                Some("meeting".to_string()),
                variables(&[("project", "norn")]),
                a_value_map(),
                Some("Agenda.\n".to_string()),
            ))],
        )
        .with_force(true)
    );

    let bare_rule = NewParams::for_subject(
        notes(),
        ApplyMode::Preview,
        NewSubject::by_rule("meeting", Variables::default(), ValueMap::default(), None),
    );
    round_trip(&bare_rule);
    assert_eq!(
        wire(&bare_rule),
        r#"{"vault":{"by":"name","name":"notes"},"mode":"preview","as":"meeting"}"#
    );

    let inbox = NewParams::for_subject(
        notes(),
        ApplyMode::Preview,
        NewSubject::inbox(a_value_map(), Some("Call Sam.\n".to_string())),
    );
    round_trip(&inbox);
    assert_eq!(
        wire(&inbox),
        r#"{"vault":{"by":"name","name":"notes"},"mode":"preview","fields":{"status":"draft","owner":{"name":"drew","tags":["a","b"]}},"body":"Call Sam.\n"}"#
    );
    assert_eq!(
        inbox.plan().operations[0].kind,
        OperationKind::create_by_rule(
            None,
            Variables::default(),
            a_value_map(),
            Some("Call Sam.\n".to_string())
        )
    );

    let capture = NewParams::for_subject(
        notes(),
        ApplyMode::Preview,
        NewSubject::inbox(ValueMap::default(), None),
    );
    round_trip(&capture);
    assert_eq!(
        wire(&capture),
        r#"{"vault":{"by":"name","name":"notes"},"mode":"preview"}"#
    );
}

/// **A `new` that mixes its forms is refused, naming the rule.** A path with
/// a rule, content with no path, a path with no content, variables with no
/// rule, and a path carrying fields or a body each mix two forms or half of
/// one; none is read as another.
#[test]
fn a_new_request_mixing_its_forms_is_refused() {
    for rest in [
        r#""path":"a.md","content":"","as":"meeting""#,
        r#""content":"text""#,
        r#""path":"a.md""#,
        r#""variables":{"project":"norn"}"#,
        r#""path":"a.md","content":"","fields":{"status":"draft"}"#,
        r#""path":"a.md","content":"","body":"text""#,
        r#""path":"a.md","content":"","variables":{"project":"norn"}"#,
        r#""as":"meeting","content":"text""#,
        r#""path":"a.md","as":"meeting""#,
    ] {
        let refused = new_request(rest);
        let error = serde_json::from_str::<NewParams>(&refused)
            .expect_err(&format!("{refused} read as a new request"))
            .to_string();
        assert!(
            error.contains("exactly one of three forms"),
            "{refused} was refused without naming the rule: {error}"
        );
    }
}

/// **A `new` by rule refuses what the vocabulary refuses elsewhere**: a key
/// it does not name, a variable named twice, a field written twice, and a
/// variable that is not text.
#[test]
fn a_new_request_by_rule_refuses_unknown_and_repeated_keys() {
    for rest in [
        r#""as":"meeting","title":"A""#,
        r#""as":"meeting","variables":{"project":"a","project":"b"}"#,
        r#""as":"meeting","variables":{"year":2026}"#,
        r#""as":"meeting","fields":{"status":"a","status":"b"}"#,
        r#""as":"meeting","as":"other""#,
        r#""as":null"#,
        r#""as":"""#,
    ] {
        let refused = new_request(rest);
        assert!(
            serde_json::from_str::<NewParams>(&refused).is_err(),
            "{refused} read as a new request"
        );
    }
}

/// **A `create_by_rule` names its rule or leaves it out for the inbox**, and
/// takes its variables, fields and body each optionally; it reads back
/// exactly the kind that wrote it, nested field values included.
#[test]
fn a_create_by_rule_reads_with_and_without_a_rule() {
    let inbox: OperationKind =
        serde_json::from_str(r#"{"kind":"create_by_rule","fields":{"body":"Call Sam.\n"}}"#)
            .expect("an inbox capture");
    assert_eq!(
        inbox,
        OperationKind::create_by_rule(
            None,
            Variables::default(),
            ValueMap::default(),
            Some("Call Sam.\n".to_string())
        )
    );
    assert_eq!(
        wire(&inbox),
        r#"{"kind":"create_by_rule","fields":{"body":"Call Sam.\n"}}"#
    );
    let bare: OperationKind = serde_json::from_str(r#"{"kind":"create_by_rule","fields":{}}"#)
        .expect("a creation naming nothing");
    assert_eq!(
        bare,
        OperationKind::create_by_rule(None, Variables::default(), ValueMap::default(), None)
    );
    round_trip(&bare);
    for refused in [
        r#"{"kind":"create_by_rule","fields":{"rule":"a","path":"a.md"}}"#,
        r#"{"kind":"create_by_rule","fields":{"rule":"a","variables":{"k":"1","k":"2"}}}"#,
        r#"{"kind":"create_by_rule","fields":{"rule":"a","variables":{"k":1}}}"#,
        r#"{"kind":"create_by_rule","fields":{"rule":"a","fields":{"k":1,"k":2}}}"#,
        r#"{"kind":"create_by_rule","fields":{"rule":null}}"#,
        r#"{"kind":"create_by_rule","fields":{"rule":""}}"#,
        r#"{"kind":"create_document","fields":{"path":"a.md","content":"","rule":"a"}}"#,
        r#"{"kind":"set_frontmatter","fields":{"path":"a.md","field":"f","value":1,"body":"a"}}"#,
    ] {
        assert!(
            serde_json::from_str::<Operation>(refused).is_err(),
            "reading {refused} produced an operation"
        );
    }
}

/// **A `write_control_file` names its file by role**, `schema` or `config`,
/// and carries the file's whole content; it names no path, since where each
/// role lives is the planner's to say, and it reads back exactly the kind that
/// wrote it. A role the vocabulary does not name, a missing part and a key of
/// another kind are refused at the read.
#[test]
fn a_write_control_file_names_its_file_by_role_and_carries_its_content() {
    for (file, role) in [
        (ControlFile::Schema, "schema"),
        (ControlFile::Config, "config"),
    ] {
        let json = format!(
            r#"{{"kind":"write_control_file","fields":{{"file":"{role}","content":"x = 1\n"}}}}"#
        );
        let read: OperationKind = serde_json::from_str(&json).expect("a control-file write");
        assert_eq!(read, OperationKind::write_control_file(file, "x = 1\n"));
        assert_eq!(wire(&read), json);
    }
    for refused in [
        r#"{"kind":"write_control_file","fields":{"file":"gitignore","content":""}}"#,
        r#"{"kind":"write_control_file","fields":{"file":"schema"}}"#,
        r#"{"kind":"write_control_file","fields":{"content":"version: 1\n"}}"#,
        r#"{"kind":"write_control_file","fields":{"file":null,"content":""}}"#,
        r#"{"kind":"write_control_file","fields":{"file":"schema","content":"","path":".norn/schema.yaml"}}"#,
        r#"{"kind":"create_document","fields":{"path":"a.md","content":"","file":"schema"}}"#,
    ] {
        assert!(
            serde_json::from_str::<Operation>(refused).is_err(),
            "reading {refused} produced an operation"
        );
    }
}

/// **An `init` request names its vault and states its mode**, and nothing
/// else: there is no default mode, and a key it does not name is refused.
#[test]
fn an_init_request_names_its_vault_and_states_its_mode() {
    let request: InitParams =
        serde_json::from_str(r#"{"vault":{"by":"name","name":"notes"},"mode":"preview"}"#)
            .expect("an init request");
    assert_eq!(
        request,
        InitParams::new(VaultAddress::name(name("notes")), ApplyMode::Preview)
    );
    round_trip(&request);
    for refused in [
        r#"{"vault":{"by":"name","name":"notes"}}"#,
        r#"{"vault":{"by":"name","name":"notes"},"mode":"apply","force":true}"#,
        r#"{"mode":"apply"}"#,
    ] {
        assert!(
            serde_json::from_str::<InitParams>(refused).is_err(),
            "{refused} read as an init request"
        );
    }
}

/// **An `init` answers one of three outcomes, each an object tagged
/// `outcome`**: the starter schema scaffolded — the apply's own report, a
/// preview's plan or an apply's landing, with the refusal of the reload after
/// a landing beside it, or `null` — the vault already set up, naming
/// the schema that stands, or the schema living elsewhere, naming the source
/// the registration reads it from. Each reads back as itself.
#[test]
fn an_init_report_is_one_of_three_outcomes() {
    let schema = path(".norn/schema.yaml");
    let source = SchemaSource::new("/home/person/shared/schema.yaml").expect("a schema source");
    let reports = [
        InitReport::scaffolded(ApplyReport::previewed(a_bare_resolved_plan(), a_forecast())),
        InitReport::already_set_up(schema.clone()),
        InitReport::schema_elsewhere(source.clone()),
        InitReport::scaffolded_reload_refused(
            ApplyReport::previewed(a_bare_resolved_plan(), a_forecast()),
            ErrorEnvelope::new(
                "the vault config cannot be read",
                ErrorDetail::reload_failed(ReloadFailure::unsupported()),
            ),
        ),
    ];
    for report in &reports {
        round_trip(report);
    }
    assert!(
        wire(&reports[0])
            .starts_with(r#"{"outcome":"scaffolded","report":{"outcome":"previewed","#)
    );
    assert!(
        wire(&reports[0]).ends_with(r#","reload_refused":null}"#),
        "{}",
        wire(&reports[0])
    );
    assert!(
        wire(&reports[3]).contains(r#","reload_refused":{"code":"vault/reload-failed","#),
        "{}",
        wire(&reports[3])
    );
    assert_eq!(
        wire(&reports[1]),
        r#"{"outcome":"already_set_up","schema":".norn/schema.yaml"}"#
    );
    assert_eq!(
        wire(&reports[2]),
        r#"{"outcome":"schema_elsewhere","source":"/home/person/shared/schema.yaml"}"#
    );
}

/// **A `vault migrate` request names its vault and states its mode**, and
/// nothing else: there is no default mode, and a key it does not name is
/// refused.
#[test]
fn a_migrate_request_names_its_vault_and_states_its_mode() {
    let request: MigrateParams =
        serde_json::from_str(r#"{"vault":{"by":"name","name":"notes"},"mode":"apply"}"#)
            .expect("a migrate request");
    assert_eq!(
        request,
        MigrateParams::new(VaultAddress::name(name("notes")), ApplyMode::Apply)
    );
    round_trip(&request);
    for refused in [
        r#"{"vault":{"by":"name","name":"notes"}}"#,
        r#"{"vault":{"by":"name","name":"notes"},"mode":"preview","to":2}"#,
        r#"{"mode":"preview"}"#,
    ] {
        assert!(
            serde_json::from_str::<MigrateParams>(refused).is_err(),
            "{refused} read as a migrate request"
        );
    }
}

/// **A `vault migrate` answers one of two outcomes, each an object tagged
/// `outcome`**: the control files migrated — the apply's own report, a
/// preview's plan or an apply's landing, with the refusal of the reload after
/// a landing beside it, or `null` — or every control file already current,
/// which plans nothing. Each reads back as itself.
#[test]
fn a_migrate_report_is_migrated_or_already_current() {
    let reports = [
        MigrateReport::migrated(ApplyReport::previewed(a_bare_resolved_plan(), a_forecast())),
        MigrateReport::migrated_reload_refused(
            ApplyReport::previewed(a_bare_resolved_plan(), a_forecast()),
            ErrorEnvelope::new(
                "the vault config cannot be read",
                ErrorDetail::reload_failed(ReloadFailure::unsupported()),
            ),
        ),
        MigrateReport::already_current(),
    ];
    for report in &reports {
        round_trip(report);
    }
    assert!(
        wire(&reports[0]).starts_with(r#"{"outcome":"migrated","report":{"outcome":"previewed","#)
    );
    assert!(
        wire(&reports[0]).ends_with(r#","reload_refused":null}"#),
        "{}",
        wire(&reports[0])
    );
    assert!(
        wire(&reports[1]).contains(r#""reload_refused":{"code":"vault/reload-failed","#),
        "{}",
        wire(&reports[1])
    );
    assert_eq!(wire(&reports[2]), r#"{"outcome":"already_current"}"#);
}

/// **A migration refused names the control file and why**, the reason an
/// object tagged `reason`: a file whose version cannot be read, one at a
/// version ahead of this build or one no step migrates from, a rewrite that
/// would lose a comment, and a file another writer changed while the
/// migration ran. Each reads back as itself, under `vault/migration-refused`.
#[test]
fn a_migration_refused_names_the_file_and_why() {
    let cases = [
        (
            MigrationRefusal::unreadable("the file is not YAML"),
            r#"{"reason":"unreadable","detail":"the file is not YAML"}"#,
        ),
        (
            MigrationRefusal::version_ahead(3, 1),
            r#"{"reason":"version_ahead","found":3,"current":1}"#,
        ),
        (
            MigrationRefusal::no_step(0, 1),
            r#"{"reason":"no_step","found":0,"current":1}"#,
        ),
        (
            MigrationRefusal::comment_lost("# keep me"),
            r##"{"reason":"comment_lost","comment":"# keep me"}"##,
        ),
        (MigrationRefusal::changed(), r#"{"reason":"changed"}"#),
    ];
    for (reason, json) in cases {
        assert_eq!(wire(&reason), json);
        round_trip(&reason);
    }
    let envelope = ErrorEnvelope::new(
        "the schema's rewrite loses a comment",
        ErrorDetail::migration_refused(
            ControlFile::Schema,
            MigrationRefusal::comment_lost("# keep me"),
        ),
    );
    assert_eq!(envelope.code(), &ReasonCode::VaultMigrationRefused);
    assert_eq!(
        wire(&envelope),
        r##"{"code":"vault/migration-refused","message":"the schema's rewrite loses a comment","detail":{"code":"vault/migration-refused","file":"schema","reason":{"reason":"comment_lost","comment":"# keep me"}}}"##
    );
    round_trip(&envelope);
}

/// **A `create_by_rule` takes the generic envelope.** It expands one for one
/// into a `create_document`, so an identifier it carries and the operations it
/// requires are the expanded create's, and it reads and writes them as any
/// kind does.
#[test]
fn a_create_by_rule_takes_an_identifier_and_requirements() {
    let json = r#"{"kind":"create_by_rule","fields":{"rule":"meeting"},"id":"make-meeting","requires":["make-folder"]}"#;
    let read: Operation = serde_json::from_str(json).expect("a create_by_rule with an envelope");
    assert_eq!(
        read,
        Operation::new(OperationKind::create_by_rule(
            Some("meeting".to_string()),
            Variables::default(),
            ValueMap::default(),
            None,
        ))
        .with_id(operation_id("make-meeting"))
        .with_requires(vec![operation_id("make-folder")])
    );
    assert_eq!(wire(&read), json);
}

/// **A resolved plan carries no creation by rule.** Planning expands one into
/// a `create_document` with a concrete path and content, so a resolved plan
/// still carrying one names it by position in a fault of its own.
#[test]
fn a_resolved_plan_with_a_creation_by_rule_is_a_fault() {
    let mut plan = a_bare_resolved_plan();
    assert_eq!(plan.unexpanded_rules(), None);
    plan.operations
        .push(Operation::new(OperationKind::create_document(
            path("notes/new.md"),
            "# New\n",
        )));
    plan.operations
        .push(Operation::new(OperationKind::create_by_rule(
            None,
            Variables::default(),
            ValueMap::default(),
            None,
        )));
    assert_eq!(
        plan.unexpanded_rules(),
        Some(PlanFault::unexpanded_rule(vec![2]))
    );
    assert_eq!(
        wire(&PlanFault::unexpanded_rule(vec![2])),
        r#"{"kind":"unexpanded_rule","positions":[2]}"#
    );
}

/// **A `move` reads what it moves from its source.** A `from` carrying the
/// document extension, in any case, compiles to one `move_document`, and any
/// other `from` to one `move_folder`; each carries the request's conditions,
/// forced as the request is, and the ends cross as the paths they are.
#[test]
fn a_move_request_compiles_to_the_move_its_source_names() {
    let condition = AuthorCondition::content_hash(path("notes/a.md"), content_hash(0xab));
    let document = MoveParams::new(
        notes(),
        ApplyMode::Preview,
        MoveSubject::new("notes/a.md", "archive/A.MD").expect("a document move"),
    )
    .with_conditions(vec![condition.clone()])
    .with_force(true);
    round_trip(&document);
    assert_eq!(
        document.plan(),
        AuthoredPlan::new(
            notes(),
            vec![
                Operation::new(OperationKind::move_document(
                    path("notes/a.md"),
                    path("archive/A.MD")
                ))
                .with_conditions(vec![condition])
            ],
        )
        .with_force(true)
    );
    let json = r#"{"vault":{"by":"name","name":"notes"},"mode":"apply","from":"notes","to":"archive/notes"}"#;
    let folder_move: MoveParams = serde_json::from_str(json).expect("a folder move");
    assert_eq!(wire(&folder_move), json);
    assert_eq!(
        folder_move.subject,
        MoveSubject::folder(folder("notes"), folder("archive/notes"))
    );
    assert_eq!(
        folder_move.plan(),
        AuthoredPlan::new(
            notes(),
            vec![Operation::new(OperationKind::move_folder(
                folder("notes"),
                folder("archive/notes")
            ))],
        )
    );
    for (from, to) in [
        ("notes/a.md", "archive"),
        ("notes", "archive/a.md"),
        ("", "archive"),
        ("notes/a.md", "/archive/a.md"),
        ("notes/.md", "archive/a.md"),
    ] {
        assert!(
            MoveSubject::new(from, to).is_err(),
            "moving `{from}` to `{to}` read as a move"
        );
    }
    assert_eq!(
        MoveSubject::new(".md", "x").ok(),
        Some(MoveSubject::folder(folder(".md"), folder("x"))),
        "a leaf that is only an extension names no document"
    );
    for refused in [
        json.replace(r#""mode":"apply","#, ""),
        json.replace(r#""to":"archive/notes""#, r#""to":"archive/notes.md""#),
        json.replace(
            r#""to":"archive/notes""#,
            r#""to":"archive/notes","parents":true"#,
        ),
        json.replace(
            r#""to":"archive/notes""#,
            r#""to":"archive/notes","force":null"#,
        ),
    ] {
        assert!(
            serde_json::from_str::<MoveParams>(&refused).is_err(),
            "{refused} read as a move request"
        );
    }
}

/// **A trailing slash does not change what a `move` moves.** Its leaf is
/// judged with the slash removed, so a source whose last segment carries the
/// document extension names a document, slash or not: moved to a folder's
/// name it is refused, as it is without the slash, rather than read as a
/// move of a folder named `a.md`. A folder's own trailing slash leaves it a
/// folder.
#[test]
fn a_trailing_slash_does_not_turn_a_document_into_a_folder() {
    for (from, to) in [("a.md/", "b/"), ("notes/a.md/", "archive"), ("a/", "b.md/")] {
        assert!(
            MoveSubject::new(from, to).is_err(),
            "moving `{from}` to `{to}` read as a move"
        );
    }
    assert!(
        serde_json::from_str::<MoveParams>(
            r#"{"vault":{"by":"name","name":"notes"},"mode":"preview","from":"a.md/","to":"b/"}"#
        )
        .is_err(),
        "a document's name with a trailing slash read as a folder move"
    );
    assert_eq!(
        MoveSubject::new("notes/", "archive/").ok(),
        Some(MoveSubject::folder(folder("notes/"), folder("archive/")))
    );
}

/// **A `delete` compiles to one `delete_document` saying what its request
/// says of the links naming its document**: rewritten to `rewrite_to`, left
/// broken, or — saying neither — neither, and saying both is refused.
#[test]
fn a_delete_request_compiles_to_one_delete_saying_what_becomes_of_its_links() {
    let plain = DeleteParams::new(notes(), ApplyMode::Apply, path("notes/b.md"));
    assert_eq!(
        wire(&plain),
        r#"{"vault":{"by":"name","name":"notes"},"mode":"apply","path":"notes/b.md"}"#
    );
    assert_eq!(
        plain.clone().plan(),
        AuthoredPlan::new(
            notes(),
            vec![Operation::new(OperationKind::delete_document(path(
                "notes/b.md"
            )))]
        )
    );
    let rewriting = plain.clone().rewriting_to(target("notes/c"));
    let breaking = plain.clone().breaking_links().with_force(true);
    for (request, json, kind) in [
        (
            &rewriting,
            r#"{"vault":{"by":"name","name":"notes"},"mode":"apply","path":"notes/b.md","rewrite_to":"notes/c"}"#,
            OperationKind::delete_document_rewriting(path("notes/b.md"), target("notes/c")),
        ),
        (
            &breaking,
            r#"{"vault":{"by":"name","name":"notes"},"mode":"apply","path":"notes/b.md","allow_broken_links":true,"force":true}"#,
            OperationKind::delete_document_breaking_links(path("notes/b.md")),
        ),
    ] {
        assert_eq!(wire(request), json);
        round_trip(request);
        assert_eq!(
            request.clone().plan().operations,
            vec![Operation::new(kind)]
        );
    }
    assert_eq!(rewriting.breaking_links(), plain.clone().breaking_links());
    let json = wire(&plain);
    for refused in [
        json.replace(
            r#""path":"notes/b.md""#,
            r#""path":"notes/b.md","rewrite_to":"c","allow_broken_links":true"#,
        ),
        json.replace(
            r#""path":"notes/b.md""#,
            r##""path":"notes/b.md","rewrite_to":"c#Notes""##,
        ),
        json.replace(
            r#""path":"notes/b.md""#,
            r#""path":"notes/b.md","rewrite_to":null"#,
        ),
        json.replace(
            r#""path":"notes/b.md""#,
            r#""path":"notes/b.md","recursive":true"#,
        ),
        json.replace(r#""mode":"apply","#, ""),
    ] {
        assert!(
            serde_json::from_str::<DeleteParams>(&refused).is_err(),
            "{refused} read as a delete request"
        );
    }
}

/// **A `rewrite_wikilink` compiles to one `rewrite_wikilink`**, its `old`
/// free to name a document no vault holds, and neither end carrying an
/// anchor. A request naming no mode is refused.
#[test]
fn a_rewrite_wikilink_request_compiles_to_one_rewrite() {
    let json = r#"{"vault":{"by":"name","name":"notes"},"mode":"preview","old":"gone/never-was","new":"plans/2026"}"#;
    let request: RewriteWikilinkParams = serde_json::from_str(json).expect("a rewrite request");
    assert_eq!(wire(&request), json);
    assert_eq!(
        request,
        RewriteWikilinkParams::new(
            notes(),
            ApplyMode::Preview,
            target("gone/never-was"),
            target("plans/2026")
        )
    );
    let condition = AuthorCondition::content_hash(path("notes/a.md"), content_hash(0xab));
    let conditioned = request.with_conditions(vec![condition.clone()]);
    round_trip(&conditioned);
    assert_eq!(
        conditioned.plan(),
        AuthoredPlan::new(
            notes(),
            vec![
                Operation::new(OperationKind::rewrite_wikilink(
                    target("gone/never-was"),
                    target("plans/2026")
                ))
                .with_conditions(vec![condition])
            ],
        )
    );
    for refused in [
        json.replace(r#""mode":"preview","#, ""),
        json.replace(r#""old":"gone/never-was""#, r##""old":"gone#Heading""##),
        json.replace(r#""new":"plans/2026""#, r##""new":"plans/2026#^a1""##),
        json.replace(r#""old":"gone/never-was","#, ""),
        json.replace(
            r#""new":"plans/2026""#,
            r#""new":"plans/2026","path":"a.md""#,
        ),
    ] {
        assert!(
            serde_json::from_str::<RewriteWikilinkParams>(&refused).is_err(),
            "{refused} read as a rewrite request"
        );
    }
}
