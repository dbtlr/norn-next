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
//! query is composed. It takes **typed facts rather than documents**: parsing
//! is orchestration's job, so nothing here reads document text.
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
//!   and the parts it could not apply. [`Snapshot::find_keys`] is its paging
//!   half alone, and [`Snapshot::find_plans`] hands out the plan of every
//!   statement it runs, which is what its `EXPLAIN` bars are asserted through.
//! - [`ddl`] — the store schema, designed whole, and its fingerprint.
//! - [`DocumentPath`] — the segment-aware path representation the suffix
//!   resolution ladder is indexed by.
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
//! - **The read shapes no builder emits.** The find builder is the one read
//!   builder: nothing here counts by field. Nothing indexes a link's target,
//!   so a find refuses a `links_to` part by name, and a find's row projects no
//!   link or finding column, so it refuses those columns by name. Both
//!   refusals are dormant carriers whose consumer is NORN-229, the task that
//!   builds the link index.

pub mod ddl;

mod counters;
mod error;
mod facts;
#[cfg(feature = "induced-failure")]
mod faults;
mod feed;
mod fields;
mod find;
mod hash;
mod increment;
mod json;
mod path;
mod request;
mod store;

pub use counters::{DerivationCounters, SnapshotCounters};
pub use error::StoreError;
pub use facts::{
    BlockFact, CANDIDATE_HEAD, CandidateFact, DocumentFacts, FeedDocument, FeedTombstone,
    FindingFacts, HeadingFact, IndexedTerm, Invalidation, LinkFact, LinkFamily, PillarReport,
    Provenance, SchemaPin, Span, StoredDocument, StoredFacts, StoredFinding, StoredPathOrder,
    StoredTombstone, TagFact, TagSource, VaultSchemaPin,
};
#[cfg(feature = "induced-failure")]
pub use faults::induced_failure;
pub use feed::FeedRead;
pub use fields::{DeclaredFields, FieldContainer, FieldRow, FieldRows, TypedOrder};
pub use find::{
    BODY_ROW_CEILING, DEFAULT_PAGE, FIND_FILTERS, FIND_STATEMENTS, FieldOrder, FindFilter,
    FindPlan, FindPosition, FindRefusal, FindStatement, FindWork, Found, FoundKey, KeyPage,
    NESTED_ROW_CEILING, Nested, NestedRows, PageDirection, Resume,
};
pub use increment::{Change, DerivedFinding, IncrementOutcome, IncrementProvenance};
pub use json::{FrontmatterValue, MAX_FRONTMATTER_DEPTH, canonical_json};
// The open ceremony's own vocabulary, which is this crate's too: a store is
// one client of that ceremony, and the rung a disagreement names is the same
// rung whichever database met it.
pub use norn_db::{EmittedPlan, OpenOutcome, PlanStep, RebuildReason};
pub use path::{
    ClassKey, DirectoryPrefix, DocumentPath, RENDERED_MARKER, SuffixProbe, class_probe,
    suffix_probe,
};
pub use request::{
    DiscardScope, ExplainedStatement, FINDING_ID_CHUNK, FeedCursor, FindingCursor, MAX_PAGE,
    POINT_READS, Request, STATEMENTS, SubjectScope,
};
pub use store::{
    ConnectionTurn, ReaderMint, RecordedStoreSchema, Snapshot, SnapshotAttempt, SnapshotReader,
    Store, StoreMode, StoreReading,
};
