#![forbid(unsafe_code)]
//! An SDK for talking to SQL.
//!
//! This crate is the first effect seam: it owns the store schema, the four
//! pillars — full text, findings, migrations and the field pillar — what the
//! database-side heal rung means for derived state, and the derivation
//! counters. **No other crate reaches the derived database**,
//! harness included — what a test or a gate needs from the substrate, it gets
//! through this API.
//!
//! It is `norn-db`'s first client. The connection, the pinned-scalar
//! mechanics, the DDL fingerprint, the store epoch and the database file's
//! lifecycle are that crate's; what is here is what the statements and the
//! rows mean.
//!
//! Its verbs translate cleanly to SQL and carry no business logic beyond how a
//! query is composed, save one rule [ADR 0027] rules into it: link health,
//! judged in SQL and filed inside every changeset
//! ([`Request::apply_increment`]). It takes
//! **typed facts rather than documents**: parsing is orchestration's job, so
//! nothing here reads document text.
//!
//! [ADR 0027]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0027-link-health-rides-the-changeset.md
//!
//! # Where to start
//!
//! - [`Store`] — open, create or rebuild one derived store, and own its file.
//! - [`Store::begin_request`] — everything the store does happens inside a
//!   [`Request`], which is what makes derivation attributable. A request is an
//!   **attribution scope**: what it groups is the accounting, never the writes.
//! - [`Request::apply_increment`] — the write-through increment, and the store's
//!   one way in for document facts. **A changeset is the unit of atomicity**: it
//!   lands whole or not at all, and that entry point states the contract in
//!   full.
//! - [`Request::changed_documents_after`] and
//!   [`Request::changed_tombstones_after`] — the change feed, which is a
//!   generation-ordered query over current state rather than a retained log. A
//!   consumer pages it at its own rate, triages on the fingerprints it projects,
//!   and keeps [`Store::epoch`] beside its cursor: a position is meaningless in
//!   a database that was discarded and built again.
//! - [`Snapshot::find`] — the find builder: a request's conjunction and order
//!   compiled into index seeks on a read snapshot, answering a page of rows
//!   projected onto the columns it names, the cursor the next page continues,
//!   and the parts it could not apply. [`Snapshot::find_plans`] runs the same
//!   find and hands out the plan of every statement it ran, taken of the text
//!   and values it ran with, which is what its `EXPLAIN` bars are asserted
//!   through.
//! - [`Snapshot::count`] — the count builder: a request's conjunction,
//!   compiled as a find compiles it, and its grouping, answering a page of
//!   tallies, the cursor the next page continues, and the parts it could not
//!   apply. [`Snapshot::count_plans`] explains what it ran, as
//!   [`Snapshot::find_plans`] does for a find.
//! - [`Snapshot::validate`] — the validate builder: the findings standing
//!   under the active fingerprint, narrowed by kind, severity and a
//!   conjunction compiled as a find compiles it, answering a page of finding
//!   rows in `(kind, path, position, id)` order or one tally per kind and severity.
//!   [`Snapshot::validate_plans`] explains what it ran.
//! - [`Snapshot::describe`] — the describe builder: the vault's content model
//!   as a page of facets in `(kind, key)` order, the declared facets read off
//!   the pinned declaration and the observed field keys off the field
//!   pillar's presence index. [`Snapshot::describe_plans`] explains what it
//!   ran.
//! - [`Snapshot::search`] — the search builder's lexical floor: a plain-text
//!   query ranked by BM25 over the full-text pillar, narrowed by a
//!   conjunction compiled as a find compiles it, answering a page of hits in
//!   score order with the path breaking a tie, each carrying the row its
//!   columns name. [`Snapshot::search_plans`] explains what it ran.
//! - [`Snapshot::get`] — the get builder: one document named by a
//!   resolution target through the one resolver, answered as a find's row, as
//!   the section or block its anchor names — read through the document reader
//!   its caller hands it, since nothing here parses — or as one page of one
//!   nested collection, and refused where the target names several documents
//!   or none. [`Snapshot::get_plans`] explains what it ran.
//! - [`Snapshot::held_links`] — the links one document holds, as derived,
//!   beside the content hash they were derived from, read by its path
//!   without its body. [`Snapshot::held_links_plans`] explains what it ran.
//! - [`ddl`] — the store schema, designed whole, and its fingerprint.
//! - [`DocumentPath`] — the segment-aware path representation the suffix
//!   resolution ladder is indexed by.
//! - [`TargetClass`] — the one resolver: a target compiled for its root, over
//!   the suffix key the root's proven case behaviour selects and less the
//!   places its schema's ambiguity-ignore set names.
//!
//! # The shapes the API is built out of
//!
//! **The fact types are the store's own, not the wire's.** The store↔host seam
//! is inside one process; a wire type belongs to the client/host seam, and
//! putting one here would make the vocabulary every surface renders answerable
//! to what a column happens to need. The host maps the text layer's output onto
//! these types, which is the same mapping it already performs to compose them.
//!
//! **Counters are per request and never process-global.** A request carries its
//! own set, so what one request derived is readable without subtracting two
//! readings of a shared number — which is what makes a zero-on-warm bar mean
//! anything once a second request exists.
//!
//! # What is deliberately not here yet
//!
//! - **Which documents a change reaches.** An increment applies the changeset it
//!   is handed; deciding what belongs in one — what the watcher's facts imply,
//!   what a plan's blast radius is — is orchestration's, and so is re-recording
//!   the findings the increment discarded by class.
//! - **Tombstone retention.** When a death has outlived the disorder it was
//!   recorded to survive is a policy over generations, and nothing here decides
//!   it: a tombstone is kept until something says otherwise.
//! - **Anything that reads a document.** One parser, and it is not this crate.

pub mod ddl;

mod anchor;
mod count;
mod counters;
mod describe;
mod error;
mod facts;
#[cfg(feature = "induced-failure")]
mod faults;
mod feed;
mod fields;
mod find;
mod get;
mod hash;
mod health;
mod held_links;
mod increment;
mod json;
mod link;
mod path;
mod read;
mod request;
mod resolve;
mod rule_set;
mod search;
mod store;
mod validate;
mod write_path;

pub use count::{COUNT_STATEMENTS, CountPlan, CountStatement, CountWork, Counted, GroupMember};
pub use counters::{DerivationCounters, SnapshotCounters};
pub use describe::{DESCRIBE_STATEMENTS, DescribePlan, DescribeStatement, DescribeWork, Described};
pub use error::StoreError;
pub use facts::{
    AnchorReadings, BlockFact, CANDIDATE_HEAD, CandidateFact, DerivationVersion, DocumentFacts,
    FeedDocument, FeedTombstone, FindingFacts, HeadingFact, IndexedTerm, Invalidation, LinkAnchor,
    LinkFact, LinkFamily, PillarReport, Provenance, SchemaPin, Span, StoredDocument, StoredFacts,
    StoredFinding, StoredLink, StoredLinkKey, StoredPathOrder, StoredRuleSet, StoredSuffixKeys,
    StoredTag, StoredTombstone, TagFact, TagSource, VaultSchemaPin,
};
#[cfg(feature = "induced-failure")]
pub use faults::induced_failure;
pub use feed::FeedRead;
pub use fields::{
    ContentModel, FieldContainer, FieldDeclaration, FieldRow, FieldRows, OffsetSpelling,
    TAG_CARRIER, TypedOrder,
};
pub use find::{
    BODY_ROW_CEILING, FIND_STATEMENTS, FindPlan, FindStatement, FindWork, Found,
    NESTED_ROW_CEILING, Nested, NestedRows, PageDirection,
};
pub use get::{DocumentText, GET_STATEMENTS, GetPlan, GetStatement, GetWork, Gotten, SectionAt};
pub use health::resolution::{
    LinkChange, PathOverlay, PlanSide, ProbedLink, ResolutionWork, TargetNaming,
};
pub use health::run::ResolutionStatement;
pub use health::{KeySummaries, LinkSelection};
pub use held_links::{HELD_LINKS_STATEMENTS, HeldLinks, HeldLinksPlan, HeldLinksStatement};
pub use increment::{Change, DerivedFinding, IncrementOutcome, IncrementProvenance};
pub use json::{FrontmatterValue, MAX_FRONTMATTER_DEPTH, canonical_json};
pub use link::{named_paths, relative_spelling, rooted_spelling, spelled_place};
pub use read::{
    DEFAULT_PAGE, FieldMatch, FieldOrder, IN_VALUES_CEILING, MatchedColumn, PageRefusal,
    READ_FILTERS, ReadBound, ReadFilter, ReadStatement, TargetAmbiguity, page_limit,
};
// The open ceremony's own vocabulary, which is this crate's too: a store is
// one client of that ceremony, and the rung a disagreement names is the same
// rung whichever database met it.
pub use norn_db::{ColumnRead, EmittedPlan, OpenOutcome, PlanStep, RebuildReason};
pub use path::{
    ClassKey, DirectoryPrefix, DocumentPath, PathKey, RENDERED_MARKER, SuffixKey, SuffixProbe,
    suffix_probe,
};
pub use request::{
    DiscardScope, ExplainedStatement, FINDING_ID_CHUNK, FeedCursor, FindingCursor, MAX_PAGE,
    POINT_READS, Request, RuleSetCursor, STATEMENTS, SubjectScope,
};
pub use resolve::{AmbiguityIgnore, TargetClass};
pub use search::{
    Candidate, Candidates, FeedRows, Held, HitRows, LexicalQuery, SEARCH_STATEMENTS, SearchPlan,
    SearchStatement, SearchWork, Searched,
};
pub use store::{
    ConnectionTurn, ReaderMint, RecordedStoreSchema, SNAPSHOT_ESTABLISHMENT_STATEMENTS, Snapshot,
    SnapshotReader, Store, StoreMode, StoreReading,
};
pub use validate::{
    VALIDATE_STATEMENTS, ValidatePlan, ValidateStatement, ValidateWork, Validated, Validation,
};
pub use write_path::{WRITE_STATEMENTS, WriteStatement};
