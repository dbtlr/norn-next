//! The sidecar schema: what the engine's database is made of.
//!
//! One table beside the substrate's `meta`. A sidecar is a projection of
//! lane-1 records, so its whole shape is the projection's rows plus the pinned
//! scalars that say which build wrote them and how far the feed was consumed.
//!
//! # A row is keyed by content and model, never by a main-database rowid
//!
//! `(path, model_id, model_version)` is the identity, and `input_hash` — the
//! store's `body_hash` of the text that was embedded — is the staleness test:
//! a row whose hash equals the feed's current one needs no recompute, which is
//! what lets expensive derived state survive the cheap state's rebuild (ADR
//! 0021). Nothing here references the main database's rows; the two databases
//! share no ids, no transactions and no lifetime.
//!
//! # A rowid table, because the rows are wide
//!
//! The primary key is a real uniqueness constraint and nothing more. `WITHOUT
//! ROWID` would make it the storage order too, which puts the embedding blob
//! inside the index B-tree: at realistic embedding sizes the row overflows a
//! page, so every key comparison walks overflow chains and the table costs
//! materially more on disk than the same rows behind a rowid.
//!
//! # What is deliberately not decided here
//!
//! One row per document per model is the floor, not the ceiling. Whether a
//! long document is embedded as one vector or several chunks, and what index
//! makes nearest-neighbour search fast, are storage mechanics that arrive with
//! the need that proves them. `dimensions` is stored beside the blob because
//! the blob's own bytes do not say how many values they hold.

/// The engine schema version, pinned at 1 through the pre-release build. A
/// DDL change is detected as a DDL fingerprint mismatch — the open takes that
/// fingerprint over [`statements`] — and resolved by rebuilding from zero,
/// which consumes no version numbers.
pub(crate) const ENGINE_SCHEMA_VERSION: i64 = 1;

/// Every statement the sidecar schema is made of, in execution order. `meta`
/// is first: an open reads it to decide whether the rest is trustworthy.
pub(crate) fn statements() -> Vec<String> {
    let mut statements = norn_db::meta::statements();
    statements.extend(STATEMENTS.iter().map(|statement| (*statement).to_string()));
    statements
}

const STATEMENTS: &[&str] = &["CREATE TABLE document_vectors (
    path          TEXT    NOT NULL,
    model_id      TEXT    NOT NULL,
    model_version TEXT    NOT NULL,
    input_hash    TEXT    NOT NULL,
    dimensions    INTEGER NOT NULL,
    embedding     BLOB    NOT NULL,
    PRIMARY KEY (path, model_id, model_version)
)"];

/// The engine's own pinned scalars, beside the substrate's
/// (`norn_db::meta`). Together they are the whole of the engine's progress:
/// the store lifetime the cursors are valid in, and the position each feed
/// was consumed to.
///
/// A cursor is one key holding `{generation}:{path}`, with the empty string
/// meaning the start of the feed. One key rather than a pair because the
/// position must move atomically with the page that produced it, and must
/// reset atomically with the epoch that invalidated it — two keys would let a
/// crash leave half a position.
pub(crate) mod meta {
    /// The id half of the model this sidecar's rows and cursors belong to.
    ///
    /// The cursors are model-blind — a drained feed is drained for whichever
    /// model was embedding — so a sidecar adopted under a different model
    /// would skip everything the old model already consumed. Recording the
    /// model makes the mismatch inspectable, and the open resolves it by the
    /// wholesale-rebuild floor: a moved model is a fresh sidecar and a full
    /// recompute (ADR 0021). A finer migration over `(model id, version)` is
    /// a carved evolution this key leaves room for.
    pub(crate) const ENGINE_MODEL_ID: &str = "engine_model_id";
    /// The version half, beside the id.
    pub(crate) const ENGINE_MODEL_VERSION: &str = "engine_model_version";
    /// The lane-1 store epoch the recorded cursors are valid within. A store
    /// whose epoch differs from this value is a different database lifetime,
    /// and the recorded positions name nothing in it — the answer is a
    /// reconcile and a rescan from the start of the feed, never a seek.
    pub(crate) const OBSERVED_STORE_EPOCH: &str = "observed_store_epoch";
    /// The document feed position.
    pub(crate) const DOCUMENT_CURSOR: &str = "document_cursor";
    /// The tombstone feed position.
    pub(crate) const TOMBSTONE_CURSOR: &str = "tombstone_cursor";
}
