//! The parse-fact tables — what one document says, one row per token.
//!
//! Four tables, all shaped the same way: a `document` the rows belong to, an
//! `ordinal` fixing their order within that document, and the token's own
//! fields. They are **schema-independent** — the vault schema shapes none of
//! what a document says — so none of them carries a vault-schema fingerprint,
//! and a schema edit re-derives none of them.
//!
//! # Ordinal is the emission order, and it is the row's identity
//!
//! `norn-text` contracts a total order over the tokens it reports, and
//! `ordinal` persists exactly that order. `UNIQUE(document, ordinal)` is the
//! guard on the crate's own write loop: the ordinals come from a slice index,
//! so the constraint refuses the shapes a defect in that loop would produce —
//! a second row claiming one position, or a replacement that did not discard
//! what it replaced. It says nothing about the text layer's emission, which
//! this schema never sees twice.
//!
//! Row identity is meaningful **within one document snapshot only**. A
//! re-derivation replaces a document's fact rows wholesale, so ordinal 3 after
//! a write is not the row ordinal 3 named before it, and nothing outside a
//! single snapshot may hold onto one. That is why nothing references these
//! tables: a finding cites a path and a target, never a link row.
//!
//! # Spans are body-relative
//!
//! Every `span_*` triple here is the position `norn-text` reported, which is
//! relative to the document **body**. `documents.body_offset` is the frame;
//! see [`crate::ddl::documents`].
//!
//! # `links` stores syntax and never resolution
//!
//! A link row is the token as it was written: its family, the `protocol://`
//! prefix if it carried one, the raw target text, the title, and the fragment
//! split into an anchor or a block reference. Resolution is **not** here —
//! not as a resolved edge, and not as a mode column either.
//!
//! **There is deliberately no addressing-mode column.** How a target resolves
//! derives from the fact protocol-first and family-second, and it derives in
//! exactly one place: `norn-text`'s own `Link::resolution`. A column beside
//! `family` and `protocol` would be a second answer to a question those two
//! already settle, and a stored answer that disagreed with them would be
//! believed. For the same reason the store never re-derives emission order
//! from spans: link ranges of the two families may overlap and nest, so a
//! span comparison is not a total order and `ordinal` is.
//!
//! **`target` is stored raw, and it is not indexed yet.** The links-to join
//! probes it — a document's own path yields the handful of segment-suffixes
//! that can address it, and the join looks each of them up — but that builder
//! is Layer 3's, and the crate's own rule is that index support arrives with
//! the statement whose `EXPLAIN` bar can judge it. The same rule is why no
//! other lookup column here carries an index today.
//!
//! Raw also means **unfolded**: `links.target` is compared under `BINARY`, and
//! `documents` folds nothing either, so the two sides agree about what a name
//! is by both leaving it alone. If case or normalization folding ever enters
//! the resolution grammar, the shape is a derived `target_key` beside `target`
//! and a folded `suffix_key` beside the raw one — a DDL edit, which pre-release
//! costs a rebuild and nothing else.
//!
//! # `headings` is addressed two ways
//!
//! A wikilink `#anchor` addresses a heading's **text**; an inline Markdown
//! `#fragment` addresses its **slug**, dedupe suffix included, as `norn-text`
//! issued it in document order. Both lookups are inside one document, so an
//! index for either leads with `document` — and both arrive with the builder
//! that emits them.
//!
//! `body_offset` is where the heading construct ends and the section's body
//! begins, and `inside_container` says the heading sits inside a blockquote or
//! a list item. They are the two heading facts a section-addressed mutation
//! cannot recompute without re-reading the file, so storing them is what makes
//! this table a lossless mirror of the fact rather than a summary of it.
//!
//! # `blocks` is the target side of `links.block_ref`
//!
//! A block-id definition trailing a line is what a `[[Note#^id]]` reference
//! points at. `norn-text` names each definition's `^`-marker span, so a writer
//! reading the text layer has a position to record; the columns are nullable
//! because this table takes what a writer knows rather than forcing a
//! position on one that lacks it. Nothing enforces uniqueness of `block_id`
//! within a document — two lines defining the same id is a vault defect, and
//! judging it is the findings pillar's job, not a constraint that would refuse
//! to record what the file says.
//!
//! # `document_tags` records the tag as written
//!
//! Case is preserved, because deciding that `#Work` and `#work` are the same
//! tag is a matching question and matching happens at the query venue. The
//! comparison is therefore `BINARY`; a case-folded index is the query venue's
//! to add along with the query that needs it. `source` says which home the tag
//! came from — a body token or the frontmatter `tags` field — because the two
//! are read by different grammars and a consumer may care which one an author
//! used. Frontmatter tags may have no locatable span, so the span columns are
//! nullable here too.
//!
//! # A nullable span triple is all three or none
//!
//! Wherever a span is optional, a `CHECK` says the three columns agree about
//! it. The reader turns a partial triple into `None`, which would silently
//! degrade a position the writer half-recorded; the constraint means the writer
//! cannot produce one, so the reader's rule is a total function over rows that
//! exist rather than a repair.

pub(crate) fn statements() -> Vec<String> {
    let mut all = super::fixed(STATEMENTS);
    all.extend(nullable_span_tables());
    all
}

/// The `CHECK` that keeps a nullable span triple whole. A partial triple reads
/// back as no span at all, so refusing one at the write is what makes that
/// reading lossless.
pub(crate) const WHOLE_SPAN: &str = "CHECK ((span_line IS NULL) = (span_column IS NULL)
        AND (span_line IS NULL) = (span_offset IS NULL))";

const STATEMENTS: &[&str] = &[
    "CREATE TABLE links (
    id          INTEGER PRIMARY KEY,
    document    INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    ordinal     INTEGER NOT NULL,
    family      TEXT    NOT NULL,
    embed       INTEGER NOT NULL,
    protocol    TEXT,
    target      TEXT    NOT NULL,
    title       TEXT,
    anchor      TEXT,
    block_ref   TEXT,
    span_line   INTEGER NOT NULL,
    span_column INTEGER NOT NULL,
    span_offset INTEGER NOT NULL
)",
    "CREATE UNIQUE INDEX links_document_ordinal ON links(document, ordinal)",
    "CREATE TABLE headings (
    id               INTEGER PRIMARY KEY,
    document         INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    ordinal          INTEGER NOT NULL,
    text             TEXT    NOT NULL,
    slug             TEXT    NOT NULL,
    level            INTEGER NOT NULL,
    span_line        INTEGER NOT NULL,
    span_column      INTEGER NOT NULL,
    span_offset      INTEGER NOT NULL,
    body_offset      INTEGER NOT NULL,
    inside_container INTEGER NOT NULL
)",
    "CREATE UNIQUE INDEX headings_document_ordinal ON headings(document, ordinal)",
];

/// The two tables whose span triple is nullable, with the `CHECK` appended so
/// the constraint is stated once.
fn nullable_span_tables() -> Vec<String> {
    vec![
        format!(
            "CREATE TABLE blocks (
    id          INTEGER PRIMARY KEY,
    document    INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    ordinal     INTEGER NOT NULL,
    block_id    TEXT    NOT NULL,
    span_line   INTEGER,
    span_column INTEGER,
    span_offset INTEGER,
    {WHOLE_SPAN}
)"
        ),
        "CREATE UNIQUE INDEX blocks_document_ordinal ON blocks(document, ordinal)".to_string(),
        format!(
            "CREATE TABLE document_tags (
    id          INTEGER PRIMARY KEY,
    document    INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    ordinal     INTEGER NOT NULL,
    name        TEXT    NOT NULL,
    source      TEXT    NOT NULL,
    span_line   INTEGER,
    span_column INTEGER,
    span_offset INTEGER,
    {WHOLE_SPAN}
)"
        ),
        "CREATE UNIQUE INDEX document_tags_document_ordinal ON document_tags(document, ordinal)"
            .to_string(),
    ]
}
