//! The sidecar schema: what the engine's database is made of.
//!
//! One table beside the substrate's `meta`. A sidecar is a projection of
//! lane-1 records, so its whole shape is the projection's rows plus the pinned
//! scalars that say which build wrote them, how far the feed was consumed, and
//! how many committed mutations the sidecar has seen — its revision, which is
//! the substrate's count of committed writes (`norn_db::meta::WRITE_GENERATION`),
//! seeded at create and taken by every committing transaction. The sidecar
//! reads the count alone and stamps no row with it.
//!
//! # A row is keyed by content and model, never by a main-database rowid
//!
//! `(path, model_id, model_version)` is the identity, and `input_hash` — the
//! store's `body_hash` of the text that was embedded — is the staleness test:
//! a row whose hash equals the feed's current one needs no recompute, which is
//! what lets expensive derived state survive the cheap state's rebuild (ADR
//! 0052). Nothing here references the main database's rows; the two databases
//! share no ids and no transactions, and this one outlives each store epoch,
//! recording only which epoch its progress was taken in.
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

/// The engine schema version, pinned at 1 through the pre-release build. The
/// open compares this version first and the DDL fingerprint — its digest of
/// [`statements`] — second, so with the version pinned a DDL change is
/// detected as a fingerprint mismatch and resolved by rebuilding from zero,
/// which consumes no version numbers.
pub(crate) const ENGINE_SCHEMA_VERSION: i64 = 1;

/// Every statement the sidecar schema is made of, in execution order. `meta`
/// is first: an open reads it to decide whether the rest is trustworthy.
pub(crate) fn statements() -> Vec<String> {
    let mut statements = norn_db::meta::statements();
    statements.extend(STATEMENTS.iter().map(|statement| (*statement).to_string()));
    statements
}

const STATEMENTS: &[&str] = &[
    "CREATE TABLE document_vectors (
    path          TEXT    NOT NULL,
    model_id      TEXT    NOT NULL,
    model_version TEXT    NOT NULL,
    input_hash    TEXT    NOT NULL,
    dimensions    INTEGER NOT NULL,
    embedding     BLOB    NOT NULL,
    PRIMARY KEY (path, model_id, model_version)
)",
    // The census (`crate::census`): one row per document the store holds
    // readable, with the body hash it holds. Narrow and keyed by path alone,
    // so it is stored in its key's order.
    "CREATE TABLE census (
    path      TEXT NOT NULL PRIMARY KEY,
    body_hash TEXT NOT NULL
) WITHOUT ROWID",
];

/// The engine's own pinned scalars, beside the substrate's
/// (`norn_db::meta`). Together they are the whole of the engine's progress:
/// the store epoch the cursors are valid in, and the position each feed
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
    /// recompute (ADR 0052). A finer migration over `(model id, version)` is
    /// a carved evolution this key leaves room for.
    pub(crate) const ENGINE_MODEL_ID: &str = "engine_model_id";
    /// The version half, beside the id.
    pub(crate) const ENGINE_MODEL_VERSION: &str = "engine_model_version";
    /// The lane-1 store epoch the recorded cursors are valid within. A store
    /// whose epoch differs from this value is a different database, and the
    /// recorded positions name nothing in it — the answer is a
    /// reconcile and a rescan from the start of the feed, never a seek.
    pub(crate) const OBSERVED_STORE_EPOCH: &str = "observed_store_epoch";
    /// The document feed position.
    pub(crate) const DOCUMENT_CURSOR: &str = "document_cursor";
    /// The tombstone feed position.
    pub(crate) const TOMBSTONE_CURSOR: &str = "tombstone_cursor";
    /// How far the document feed was drained: `{generation}:{store epoch}`,
    /// the store's write generation observed immediately before the page
    /// that last completed the feed, with the store epoch it is a position
    /// in. Absent until the feed first completes. It carries its
    /// own epoch rather than leaning on [`OBSERVED_STORE_EPOCH`], so a
    /// reconcile that moved the observed epoch and a drain that then failed
    /// leave a watermark that still says which epoch it was taken in.
    pub(crate) const DOCUMENT_WATERMARK: &str = "document_watermark";
    /// How far the tombstone feed was drained, spelled as
    /// [`DOCUMENT_WATERMARK`] is.
    pub(crate) const TOMBSTONE_WATERMARK: &str = "tombstone_watermark";
    /// The store epoch a paged reconcile is reconciling toward, absent where
    /// no reconcile is part way through. A pass recorded for another epoch
    /// than the store's names a database that no longer exists, and starts
    /// over rather than resuming: paths it checked as standing there may be
    /// gone here.
    pub(crate) const RECONCILE_STORE_EPOCH: &str = "reconcile_store_epoch";
    /// The last held path the paged reconcile checked, beside
    /// [`RECONCILE_STORE_EPOCH`] and moved in the same transaction as the
    /// page that checked it.
    pub(crate) const RECONCILE_CURSOR: &str = "reconcile_cursor";
    /// The write generation a paged reconcile's first page read, its **G0**,
    /// beside [`RECONCILE_STORE_EPOCH`] and present exactly while a reconcile
    /// is part way through. A pass recorded without it is not resumed: it
    /// starts over, so its first page reads a G0 of its own.
    pub(crate) const RECONCILE_G0: &str = "reconcile_g0";
    /// The engine's read-through generation as its records stood when the
    /// paged reconcile began, `{generation}:{store epoch}`, or the empty
    /// string where they recorded none. Present with [`RECONCILE_G0`]: while
    /// a reconcile runs, its death cursor raises no read-through generation,
    /// and a restart resumes with this value.
    pub(crate) const RECONCILE_READ_THROUGH: &str = "reconcile_read_through";
    /// The G0 the engine's last completed reconcile read on its first page,
    /// `{generation}:{store epoch}`: every death committed at or below it is
    /// accounted for. Absent until a reconcile completes.
    pub(crate) const RECONCILED_THROUGH: &str = "reconciled_through";
    /// The store epoch the census was taken in, absent until its first step.
    /// A store whose epoch differs empties the census and reads both feeds
    /// again from their starts.
    pub(crate) const CENSUS_STORE_EPOCH: &str = "census_store_epoch";
    /// The census's document feed position, spelled as [`DOCUMENT_CURSOR`] is
    /// and moved only by the census.
    pub(crate) const CENSUS_DOCUMENT_CURSOR: &str = "census_document_cursor";
    /// The census's tombstone feed position.
    pub(crate) const CENSUS_TOMBSTONE_CURSOR: &str = "census_tombstone_cursor";
    /// How many census rows there are: the readable documents counted.
    /// Seeded at zero at create and moved in every transaction that moves a
    /// census row.
    pub(crate) const CENSUS_TOTAL: &str = "census_total";
    /// How many census rows this engine's model holds a vector row over with
    /// the same hash. Seeded at zero and moved in every transaction that moves
    /// a census row or a vector row.
    pub(crate) const CENSUS_EMBEDDED: &str = "census_embedded";
    /// `1` where the census step that last moved the census read both feeds
    /// to their ends, so the two counts are whole; `0` otherwise. Seeded at
    /// `0`, and set to `0` by every census step that empties the census or
    /// stops short of a feed's end.
    pub(crate) const CENSUS_WHOLE: &str = "census_whole";
    /// The `total` count as it stood when the counts last stopped being
    /// whole, which a status answers while [`CENSUS_WHOLE`] is `0`. Absent,
    /// with [`CENSUS_LAST_WHOLE_EMBEDDED`], until the counts first stop being
    /// whole in the census's store epoch, and removed by the census step that
    /// empties the census for a new one.
    pub(crate) const CENSUS_LAST_WHOLE_TOTAL: &str = "census_last_whole_total";
    /// The `embedded` count beside [`CENSUS_LAST_WHOLE_TOTAL`], recorded and
    /// removed with it.
    pub(crate) const CENSUS_LAST_WHOLE_EMBEDDED: &str = "census_last_whole_embedded";
    /// The write generation a census recount's first page read, its G0,
    /// present exactly while a recount below the death floor runs. The
    /// recount is in [`CENSUS_STORE_EPOCH`]: a new store epoch ends it.
    pub(crate) const CENSUS_RECOUNT_G0: &str = "census_recount_g0";
    /// The census's read-through generation as its records stood when the
    /// recount began, spelled as [`RECONCILE_READ_THROUGH`] is and present
    /// with [`CENSUS_RECOUNT_G0`].
    pub(crate) const CENSUS_RECOUNT_READ_THROUGH: &str = "census_recount_read_through";
    /// The census's **completion**: the write generation it read before the
    /// final page of its last count that reached the end of both feeds,
    /// `{generation}:{store epoch}`. Absent until a count first completes.
    pub(crate) const CENSUS_COMPLETION: &str = "census_completion";
    /// The G0 of the census's last recount that reached the end of both
    /// feeds, `{generation}:{store epoch}`, recorded whether or not the
    /// recount's final page read a floor at or below it.
    pub(crate) const CENSUS_RECOUNTED: &str = "census_recounted";
}
