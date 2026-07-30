#![forbid(unsafe_code)]
//! An SDK for talking to SQL.
//!
//! This crate is the first effect seam: it owns the store schema, the DDL
//! fingerprint, the four pillars, the database-side heal rung, the derivation
//! counters, and the file lifecycle of the database itself. **No other crate
//! opens a SQLite connection**, harness included — what a test or a gate needs
//! from the substrate, it gets through this API.
//!
//! Its verbs translate cleanly to SQL and carry no business logic beyond how a
//! query is composed. It links one workspace crate, `norn-wire`, and it takes
//! **typed facts rather than documents**: parsing is orchestration's job, so
//! nothing here reads document text.
//!
//! # Where to start
//!
//! - [`Store`] — open, create or rebuild a per-vault store, and own its file.
//! - [`Store::begin_request`] — everything the store does happens inside a
//!   [`Request`], which is what makes derivation attributable. A request is an
//!   **attribution scope and not an atomicity scope**: each operation commits
//!   its own transaction, so what a request groups is the accounting rather than
//!   the writes.
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
//! - **The increment**, and with it the transaction that spans more than one
//!   operation. Which documents a change reaches, how a changeset is ordered,
//!   how a torn write is detected, and when a tombstone has outlived its
//!   purpose: all of it composes [`Request`]'s operations rather than replacing
//!   them, and the store schema is shaped for it — tombstones, provenance, and
//!   wholesale row replacement in one transaction.
//! - **The read builders.** Compiling request parameters into SQL, with the
//!   `EXPLAIN` bars that judge the SQL a builder actually emitted, is Layer 3.
//!   The probe readers here are the range primitives those builders compose;
//!   they take index bounds, never parameters. What is here already is the seam
//!   those bars are asserted through — [`Request::probe_reader_plan`] — because
//!   a plan cannot be taken by a crate that may not open a connection.
//! - **Anything that reads a document.** One parser, and it is not this crate.

pub mod ddl;

mod counters;
mod error;
mod facts;
mod json;
mod path;
mod request;
mod store;

pub use counters::DerivationCounters;
pub use error::StoreError;
pub use facts::{
    BlockFact, CANDIDATE_HEAD, CandidateFact, Deletion, DocumentFacts, FindingFacts, HeadingFact,
    Invalidation, LinkFact, LinkFamily, PillarReport, Provenance, SchemaPin, Span, StoredDocument,
    StoredFacts, StoredFinding, StoredTombstone, TagFact, TagSource, VaultSchemaPin, VectorFacts,
};
pub use json::{FrontmatterValue, MAX_FRONTMATTER_DEPTH, canonical_json};
pub use path::{ClassKey, DocumentPath, SuffixProbe, class_probe, suffix_probe};
pub use request::{EmittedPlan, PlanStep, ProbeReader, Request};
pub use store::{
    OpenOutcome, RebuildReason, RecordedStoreSchema, Store, StoreMode, induced_failure,
};
