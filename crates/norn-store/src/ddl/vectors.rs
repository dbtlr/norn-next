//! The vector pillar — a DDL carve, and deliberately no more than one.
//!
//! `document_vectors` holds derived embeddings. It is here because the shape
//! the rest of the schema has to live with is decided now: a vector is
//! identified by `(document, model id, model version)` and carries the content
//! hash it was computed over.
//!
//! # A vector is a pure function of (model id, version, content)
//!
//! Which is why all three are columns rather than assumptions. `content_hash`
//! is the staleness test: a vector whose hash differs from its document's
//! current hash describes text that is no longer there, and finding those is a
//! join rather than a bookkeeping table. A model upgrade is a migration of
//! derived state over `(model_id, model_version)`, which is what that index is
//! for.
//!
//! Embeddings are **eventually consistent** — they arrive from async workers
//! rather than inside the increment that changed the document — so a document
//! with no row here is ordinary, not broken. `generation` says which derivation
//! the vector caught up to.
//!
//! # What is deliberately not decided here
//!
//! One row per document per model is the floor, not the ceiling. Whether a long
//! document is embedded as one vector or several chunks, whether the blob is
//! float32 or quantized, and what index makes nearest-neighbour search fast are
//! **storage mechanics behind the pillar contract**, and each of them arrives
//! with the pillar's implementation. `dimensions` is stored beside the blob so
//! that a reader can tell what it is holding without consulting a table of
//! model facts that lives in another crate.
//!
//! The embedding runtime does not exist yet, and this table's emptiness is the
//! honest state of that. What the carve guarantees is that adding it is not a
//! change to `documents`.

pub(crate) const STATEMENTS: &[&str] = &[
    "CREATE TABLE document_vectors (
    document      INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    model_id      TEXT    NOT NULL,
    model_version TEXT    NOT NULL,
    content_hash  TEXT    NOT NULL,
    dimensions    INTEGER NOT NULL,
    embedding     BLOB    NOT NULL,
    generation    INTEGER NOT NULL,
    PRIMARY KEY (document, model_id, model_version)
) WITHOUT ROWID",
    "CREATE INDEX document_vectors_model ON document_vectors(model_id, model_version)",
];
