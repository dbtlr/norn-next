//! The field pillar — one row per key a document's frontmatter carries, and one
//! per scalar value under it.
//!
//! What the rows are, and how they are derived, is [`crate::FieldRows`]'s to
//! state. This area is the table they are written to.
//!
//! # Two lane-1 index projections share the table
//!
//! The presence and value rows:
//!
//! - **Inputs.** The document's canonical frontmatter projection.
//! - **Determinism.** Deterministic: the rows are a function of the frontmatter
//!   value, computed by one pure function.
//! - **Maintenance.** Inside the document's changeset. The rows are written by
//!   the increment that writes the document row, replaced wholesale with the
//!   other fact rows on a re-derivation, and taken by the cascade when the
//!   document dies.
//! - **Invalidation key.** The document's content hash: the rows are written
//!   with the document's changeset and rewritten whenever the document is.
//!
//! The `typed` column and its `least_typed` marker:
//!
//! - **Inputs.** The value rows and the schema content model's declared field
//!   types.
//! - **Determinism.** Deterministic: a typed value is a function of the raw
//!   value and its key's declared type.
//! - **Maintenance.** Inside the document's changeset, written by the same
//!   statement as the value it types.
//! - **Invalidation key.** The standing schema pin, held in `meta`: the pin's
//!   own transaction clears every typed value, and the walk that follows
//!   refills them.
//!
//! # Why clearing the typed column at the pin is safe
//!
//! A pin clears `typed` and `least_typed` everywhere, and the heal that follows
//! a schema reload refills them for every document derived before the pin,
//! whether or not its bytes moved. Between the two, a reader could meet a column
//! that is half refilled. None does: a schema reload closes the reader and
//! publishes the vault as warming while the heal runs, and hands a reader back
//! only once the heal has converged — so no read observes the column between the
//! pin and the walk that refills it. A pin of a schema declaring no typed field
//! clears the column and owes no refill, which is the answer an undeclared key
//! has anyway.
//!
//! The rows carry no fingerprint column of their own, unlike `findings`: the
//! typed column's invalidation key is the one pin standing in `meta`, and the
//! pin clears what the previous one derived, so every typed value in the store
//! is derived under it and a per-row key would say the same thing on every row.
//!
//! # Shape
//!
//! `(document, key, ordinal)` is the primary key, and the table is `WITHOUT
//! ROWID`, so a document's rows are one contiguous run of the key's b-tree:
//! the wholesale replacement, the cascade and the stored read each seek it by
//! `document`. Ordinal zero is the presence row, the only one that carries a
//! container and the only one that carries no value; the `CHECK`s hold that
//! split, and hold that a marker stands only on a row with the value it marks.
//!
//! `path` is the document's own path, copied beside its id, so a row can be
//! ordered and compared by the path it stands at without a join to
//! `documents`. The copy cannot go stale: the document upsert conflicts on
//! `path` and never rewrites it, so a path never moves under an id, and a
//! document that moves is a death at one path and a birth at another, whose
//! rows the cascade takes and the birth writes again.
//!
//! # Indexes, each the seek one read makes
//!
//! Every index leads with `key`, because every read of the pillar is about one
//! key, and ends with `path`, so rows under one value come off the index in path
//! order. The table is `WITHOUT ROWID`, so each index also carries the primary
//! key: a read that wants the document id reads it off the index.
//!
//! - `document_fields_raw` is every row by `(key, raw)`: an equality, a
//!   membership and a `before`/`after` bound on a key with no typed order are
//!   one seek on it.
//! - `document_fields_typed` holds the rows that carry a typed value, by
//!   `(key, typed)`: the same parts on a key with a typed order seek it, and it
//!   is the set a pin clears, so the clear reads that index and never the rest
//!   of the table.
//! - `document_fields_least_raw` and `document_fields_least_typed` hold the
//!   marker rows alone, one per document and key, by `(key, value, path)`: a
//!   field sort pages one of them from a `(value, path)` position with no sort
//!   step, and a document whose field holds a set is read once.
//! - `document_fields_presence` holds the presence rows alone, by
//!   `(key, path)`: `has` and `missing` are one seek on it, and the keys the
//!   vault holds are its distinct leading column.

use crate::fields::FieldContainer;

pub(crate) fn statements() -> Vec<String> {
    let containers = FieldContainer::ALL
        .iter()
        .map(|container| format!("'{}'", container.as_str()))
        .collect::<Vec<String>>()
        .join(", ");
    vec![
        format!(
            "CREATE TABLE document_fields (
    document    INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    key         TEXT    NOT NULL,
    ordinal     INTEGER NOT NULL CHECK (ordinal >= 0),
    path        TEXT    NOT NULL,
    container   TEXT    CHECK (container IN ({containers})),
    raw         TEXT,
    typed       TEXT,
    least_raw   INTEGER NOT NULL DEFAULT 0 CHECK (least_raw IN (0, 1)),
    least_typed INTEGER NOT NULL DEFAULT 0 CHECK (least_typed IN (0, 1)),
    PRIMARY KEY (document, key, ordinal),
    CHECK ((ordinal = 0) = (container IS NOT NULL)),
    CHECK (ordinal > 0 OR (raw IS NULL AND typed IS NULL AND least_raw = 0 AND least_typed = 0)),
    CHECK (least_raw = 0 OR raw IS NOT NULL),
    CHECK (least_typed = 0 OR typed IS NOT NULL)
) WITHOUT ROWID"
        ),
        "CREATE INDEX document_fields_raw ON document_fields(key, raw, path)".to_string(),
        "CREATE INDEX document_fields_typed ON document_fields(key, typed, path)
    WHERE typed IS NOT NULL"
            .to_string(),
        "CREATE INDEX document_fields_least_raw ON document_fields(key, raw, path)
    WHERE least_raw = 1"
            .to_string(),
        "CREATE INDEX document_fields_least_typed ON document_fields(key, typed, path)
    WHERE least_typed = 1"
            .to_string(),
        "CREATE INDEX document_fields_presence ON document_fields(key, path)
    WHERE ordinal = 0"
            .to_string(),
    ]
}
