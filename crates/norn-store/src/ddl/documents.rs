//! `documents` — one row per **live** vault document.
//!
//! A deleted document has no row here. Its death is recorded in `tombstones`
//! instead, so every query over this table reads the vault as it stands and no
//! predicate has to remember to exclude the dead.
//!
//! A re-derivation **updates** the row in place and keeps its `id`. That is
//! what lets a finding or a vector reference a document across a
//! re-derivation, and it is why identity is a rowid with a unique `path`
//! rather than the path itself: the path is the document's name, and a name is
//! not an identity.
//!
//! # The path is stored three ways, all derived from one
//!
//! - `path` — the vault-root-relative path, already normalized. Case,
//!   dot-prefix and separator normalization belong to the filesystem seam, so
//!   the store compares bytes under the default `BINARY` collation and never
//!   folds anything itself. This is the document's name and its unique key.
//! - `suffix_key` — the segment-reversed form the suffix-resolution ladder
//!   probes, described in [`crate::path`]. It exists so that
//!   right-to-left, segment-aligned suffix resolution is a range scan over an
//!   index rather than a scan over every path in the vault.
//! - `stem` — the leaf segment with its final extension removed, which is the
//!   one-segment case of the same ladder and the key an **ambiguity class** is
//!   named by. Indexed in its own right so that enumerating every ambiguous
//!   stem in a vault is a grouped index read.
//!
//! All three are computed in one place, from one input, by
//! [`crate::path::DocumentPath`] — three columns that must agree, produced
//! together so they cannot disagree.
//!
//! `depth` is the number of segments the path has. Emitting a candidate as its
//! *minimal disambiguating suffix* needs to know how much of a path to take,
//! and asking the database rather than splitting strings back apart is what
//! keeps that a projection of the column that was indexed.
//!
//! # The rest of the row
//!
//! - `content_hash` — the hash the filesystem seam computed over the bytes it
//!   read. **Only a content hash concludes "unchanged"**, so this is what a
//!   full tree heal compares against, and it is the same value a
//!   compare-and-swap confirmation rides on when a plan is applied.
//! - `byte_length` — the size of the document that hash was taken over.
//! - `body` — the document's body, frontmatter block excluded. It is here
//!   because the full-text pillar is an external-content FTS5 index over this
//!   column; see [`crate::ddl::fts`] for what that buys and what it costs.
//! - `body_offset` — where `body` begins inside the document. **Every span
//!   this schema stores is body-relative**, because that is the frame
//!   `norn-text` reports them in; this column is the one place that frame is
//!   named, and adding it turns a body-relative span into a document offset.
//! - `frontmatter` — the canonical JSON projection of the frontmatter block.
//!   `NULL` means **no projection exists**: either the document carries no
//!   frontmatter block, or it carries one that did not parse. An empty block
//!   projects to `{}`, which is a different value from `NULL`, so a second
//!   presence column would be a redundant answer to a question this one already
//!   settles. See [`crate::json`], and `diagnostic_count` for which of the two
//!   `NULL` means.
//! - `diagnostic_count` — how many diagnostics the parse raised. It answers
//!   "did this document read cleanly" without a table of diagnostics; what a
//!   diagnostic *means* is a finding's question, not a parse fact.
//! - `generation` — the write generation this row was last derived at.
//! - `derived_at` — unix seconds, for a person reading a report. It orders
//!   nothing: `generation` does that.

pub(crate) const STATEMENTS: &[&str] = &[
    "CREATE TABLE documents (
    id               INTEGER PRIMARY KEY,
    path             TEXT    NOT NULL,
    suffix_key       TEXT    NOT NULL,
    stem             TEXT    NOT NULL,
    depth            INTEGER NOT NULL,
    content_hash     TEXT    NOT NULL,
    byte_length      INTEGER NOT NULL,
    body             TEXT    NOT NULL,
    body_offset      INTEGER NOT NULL,
    frontmatter      TEXT,
    diagnostic_count INTEGER NOT NULL,
    generation       INTEGER NOT NULL,
    derived_at       INTEGER NOT NULL
)",
    "CREATE UNIQUE INDEX documents_path ON documents(path)",
    "CREATE INDEX documents_suffix_key ON documents(suffix_key)",
    "CREATE INDEX documents_stem ON documents(stem)",
];
