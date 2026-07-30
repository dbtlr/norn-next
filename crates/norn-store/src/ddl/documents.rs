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
//! # The path is stored two ways, both derived from one
//!
//! - `path` — the vault-root-relative path, already normalized. Case,
//!   dot-prefix and separator normalization belong to the filesystem seam, so
//!   the store compares bytes under the default `BINARY` collation and never
//!   folds anything itself. This is the document's name and its unique key.
//! - `suffix_key` — the segment-reversed form the suffix-resolution ladder
//!   probes, described in [`crate::path`]. It exists so that
//!   right-to-left, segment-aligned suffix resolution is a range scan over an
//!   index rather than a scan over every path in the vault.
//!
//! Both are computed in one place, from one input, by
//! [`crate::path::DocumentPath`] — two columns that must agree, produced
//! together so they cannot disagree.
//!
//! **One fact, one home.** The stem and the segment count are functions of
//! `path`, and [`crate::path::DocumentPath`] computes both from it on the way
//! back out of the table. A column for either would be a second spelling that
//! no statement reads and that a bad write could make disagree with the path
//! beside it.
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
//!   settles. See [`crate::json`], and `frontmatter_diagnostic_count` for which
//!   of the two `NULL` means.
//! - `frontmatter_diagnostic_count` — how many **frontmatter-scoped**
//!   diagnostics the parse raised, which is what discriminates the two things
//!   `frontmatter IS NULL` can mean: zero is "there was no block", and nonzero
//!   is "there was a block and it did not read". A count over every diagnostic
//!   a parse raised could not tell those apart the moment one diagnostic came
//!   from anywhere else, so the column holds the narrower fact. What a
//!   diagnostic *means* is a finding's question, not a parse fact.
//! - `generation` — the write generation this row was last derived at.
//! - `derived_at` — unix seconds, for a person reading a report. It orders
//!   nothing: `generation` does that.

pub(crate) fn statements() -> Vec<String> {
    super::fixed(STATEMENTS)
}

const STATEMENTS: &[&str] = &[
    "CREATE TABLE documents (
    id                           INTEGER PRIMARY KEY,
    path                         TEXT    NOT NULL,
    suffix_key                   TEXT    NOT NULL,
    content_hash                 TEXT    NOT NULL,
    byte_length                  INTEGER NOT NULL,
    body                         TEXT    NOT NULL,
    body_offset                  INTEGER NOT NULL,
    frontmatter                  TEXT,
    frontmatter_diagnostic_count INTEGER NOT NULL,
    generation                   INTEGER NOT NULL,
    derived_at                   INTEGER NOT NULL
)",
    "CREATE UNIQUE INDEX documents_path ON documents(path)",
    "CREATE INDEX documents_suffix_key ON documents(suffix_key)",
];
