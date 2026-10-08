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
//! not as a resolved edge, and not as an addressing mode either.
//!
//! **There is no addressing-mode column.** How a target resolves derives from
//! the fact protocol-first and family-second: the wire's `LinkAddress` is the
//! one selector, which [`crate::link`] reads the family, protocol and target
//! columns through, and `norn-text`'s syntax-only `Link::resolution` agrees
//! with it. For the same reason the store never re-derives emission order
//! from spans: link ranges of the two families may overlap and nest, so a
//! span comparison is not a total order and `ordinal` is.
//!
//! **`address` is what judging a link reads of that selector**, and nothing
//! more: `elsewhere`, where the link addresses no document of the vault;
//! `attachment`, where its target names one, so resolving to no document
//! leaves it unjudged; and `document` otherwise. It is the wire's
//! `LinkAddress::kind`, computed at the write, which is the one classification
//! a link's read-time health (`LinkHealth::of_link`) is judged by too, so the
//! two cannot disagree, and a predicate over links reads it as a column rather
//! than re-running the selector per row.
//!
//! **A frontmatter link may have no span.** A wikilink written in a
//! frontmatter value whose bytes are not its text — a flow sequence item, an
//! escaped or folded scalar, a nested value — or in a block whose fields the
//! text layer cannot tell apart is a link like any other, and the text layer
//! reports it with no position, so its span columns are `NULL`. Nothing
//! about resolving or judging a link reads its span.
//!
//! **A link names at most one place, and "no place" has one stored form.**
//! `anchor` is a heading anchor as the text layer records it and `block_ref` a
//! block reference; at most one of them is set, and neither is ever empty: a
//! link written with an empty fragment, `note#` or `note#^`, names no place
//! and stores `NULL` in both, as a link written with no fragment does.
//! Whether a link carries an anchor is therefore whether either is set, which
//! is the store's one spelling of that test (`carries_anchor`).
//!
//! **`anchor_text` and `anchor_marked` are the readings a heading anchor is
//! matched by** ([`crate::AnchorReadings`]): its text and the heading text
//! past its `#` markers, each compared with a heading's `reading`; `anchor`
//! itself is compared with a heading's `slug`. The host takes the readings
//! from the text layer's one section resolver — this crate reads no text —
//! and hands them over inside the anchor ([`crate::LinkAnchor`]). The readings
//! stand exactly where a heading anchor does, so none sits beside a block
//! reference, and `anchor_marked` is `NULL` too where the anchor has no `#`
//! markers. The table's `CHECK`s hold every link row to those shapes.
//!
//! **`target` is stored raw**, and compared under `BINARY`: no normalization,
//! no percent-decoding, no case folding. What a link's target names is
//! decided at read time against the vault as it stands, and nothing here
//! stores that answer.
//!
//! # `link_keys` is the link index
//!
//! A links-to part asks which documents hold a link that could name one
//! document, and a scan of `links` would answer it by reading every link in the
//! vault. So each link that can name a document is held under the keys a seek
//! finds it by, derived at the write from the link and the path of the document
//! holding it ([`crate::link`]):
//!
//! - A wikilink's target is a suffix address, and its keys are its probe's
//!   prefixes in the segment-reversed form `documents.suffix_key` takes — one
//!   key, or two for a leaf carrying a dot, which reduces both ways — beside
//!   the number of segments the target spells, which the ambiguity-ignore test
//!   reads.
//! - A path — a Markdown target, `vault://` or not — has one key: the vault
//!   path it names, read from the holding document's directory or from the
//!   vault root by URL rules; a same-document anchor's key is the holding
//!   document's own path. A `vault://` wikilink is a rooted name, and its keys
//!   are the root paths its reductions spell — one, or two for a leaf
//!   carrying an extension. `segments` is `NULL` beside a path's key.
//!
//! A target naming an attachment is keyed like any other, since a document
//! may carry the attachment's name. A link addressed elsewhere — a protocol
//! other than `vault`, a Markdown target opening with a URI scheme — and a
//! target that names no vault path are held under no key.
//!
//! **A document's keys are a handful, so a links-to seek is a handful of
//! equality seeks.** The documents a link could name share a key with it: a
//! suffix key is a segment-aligned prefix of the named document's own suffix
//! key, and a path key is the document's path. A seek for the links that could
//! name one document therefore reads `link_keys_key` — or
//! `link_keys_folded_key`, where the root folds ASCII case — at each of that
//! document's prefixes and its path, and reaches the holding document off the
//! index entry. A suffix key always ends in the separator and a path never
//! does, so the two kinds share one column, and the `CHECK` holds `segments`
//! to the kind its key is.
//!
//! The keys are the link's own derived columns and never a resolution: which
//! documents a key reaches is read against `documents` at the instant of the
//! read. Their rows are the link row's: `link_keys_link` leads with the link,
//! so a re-derivation's delete of a document's link rows cascades to their keys
//! through it, and a document's death reaches them the same way. `document`
//! is the link row's own document, copied beside the key so a seek reads the
//! holding document off the index without visiting the link, and the foreign
//! key is the pair — the link and its document, which `links_id_document`
//! makes a key of `links` — so a key held beside any other document is refused
//! at rest.
//!
//! **A class's range reads suffix keys alone.** A class is a prefix range, and
//! a path key spelled under a folder named like the class's stem —
//! `hub/0001.md` against the class `hub/` — sorts inside it. So the ranges a
//! class is walked by seek `link_keys_suffix_key` or
//! `link_keys_folded_suffix_key`, the same columns held only where `segments`
//! is set: a path key is not in them, and a folder note's class costs its own
//! links however many links into the folder beside it stand.

//! # `headings` is addressed two ways
//!
//! A heading anchor matches a heading by its **text**, which `reading` holds
//! as an anchor's text compares it: ASCII case and ASCII whitespace folded, as
//! the text layer's section resolver reads it. It also matches by its
//! **slug**, dedupe suffix included, as `norn-text` issued it in document
//! order. One resolver reads a wikilink's anchor and a Markdown link's
//! fragment alike, by all three of its readings: its text and its marked text
//! against `reading`, and the anchor as written against `slug`. Every lookup
//! is inside one document, so each index leads with `document`:
//! `headings_document_reading` and `headings_document_slug`, the two a link's
//! anchor readings are sought through.
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
//! points at, matched by its identifier exactly and sought through
//! `blocks_document_block_id`. `norn-text` names each definition's `^`-marker
//! span, so a writer reading the text layer has a position to record; the
//! columns are nullable because this table takes what a writer knows rather
//! than forcing a position on one that lacks it. Nothing enforces uniqueness
//! of `block_id` within a document — two lines defining the same id is a
//! vault defect, and judging it is the findings pillar's job, not a constraint
//! that would refuse to record what the file says.
//!
//! # `document_tags` records the tag as written, beside its fold
//!
//! `name` is the tag as written, case included, because a report shows the
//! spelling an author used. `folded_name` is the same name under the tag fold
//! (`norn_wire::fold_tag`: each character's Unicode lowercase, accents kept,
//! over the whole nested name), computed at the write, and it is the column
//! every comparison reads: `#Work` and `#work` are two rows of one tag.
//! SQLite's `NOCASE` folds ASCII alone, so the fold is computed in Rust rather
//! than left to a collation. `document_tags_folded_name` is the index a find's
//! tag part seeks, read off the index without touching the rows, and the one
//! a count's tag grouping walks in group order. A count's tag label reads
//! past the index: each row's `ordinal` and `name`, and the holding
//! document's path, which is where the label's first occurrence is judged.
//! `ordinal` is the tag's position in the order the writer hands the tags,
//! which the host's derivation makes the file's: frontmatter entries first,
//! then body tokens in body order. `source` says which home the tag came
//! from — a body token or the frontmatter `tags` field — because the two are
//! read by different grammars and a consumer may care which one an author
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
    let mut all = nullable_span_tables();
    all.extend(super::fixed(STATEMENTS));
    all
}

/// The `CHECK` that keeps a nullable span triple whole. A partial triple reads
/// back as no span at all, so refusing one at the write is what makes that
/// reading lossless.
pub(crate) const WHOLE_SPAN: &str = "CHECK ((span_line IS NULL) = (span_column IS NULL)
        AND (span_line IS NULL) = (span_offset IS NULL))";

const STATEMENTS: &[&str] = &[
    "CREATE TABLE link_keys (
    id         INTEGER PRIMARY KEY,
    link       INTEGER NOT NULL,
    document   INTEGER NOT NULL,
    key        TEXT    NOT NULL,
    folded_key TEXT    NOT NULL,
    segments   INTEGER,
    FOREIGN KEY (link, document) REFERENCES links(id, document) ON DELETE CASCADE,
    CHECK ((segments IS NULL) = (substr(key, -1) <> '/'))
)",
    "CREATE UNIQUE INDEX link_keys_link ON link_keys(link, key)",
    "CREATE INDEX link_keys_key ON link_keys(key, document, link)",
    "CREATE INDEX link_keys_folded_key ON link_keys(folded_key, document, link)",
    "CREATE INDEX link_keys_suffix_key ON link_keys(key, document, link)
    WHERE segments IS NOT NULL",
    "CREATE INDEX link_keys_folded_suffix_key ON link_keys(folded_key, document, link)
    WHERE segments IS NOT NULL",
    "CREATE TABLE headings (
    id               INTEGER PRIMARY KEY,
    document         INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    ordinal          INTEGER NOT NULL,
    text             TEXT    NOT NULL,
    reading          TEXT    NOT NULL,
    slug             TEXT    NOT NULL,
    level            INTEGER NOT NULL,
    span_line        INTEGER NOT NULL,
    span_column      INTEGER NOT NULL,
    span_offset      INTEGER NOT NULL,
    body_offset      INTEGER NOT NULL,
    inside_container INTEGER NOT NULL
)",
    "CREATE UNIQUE INDEX headings_document_ordinal ON headings(document, ordinal)",
    "CREATE INDEX headings_document_reading ON headings(document, reading)",
    "CREATE INDEX headings_document_slug ON headings(document, slug)",
];

/// The three tables whose span triple is nullable, with the `CHECK` appended
/// so the constraint is stated once. `links` comes first, since `link_keys`
/// names it.
fn nullable_span_tables() -> Vec<String> {
    vec![
        format!(
            "CREATE TABLE links (
    id            INTEGER PRIMARY KEY,
    document      INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    ordinal       INTEGER NOT NULL,
    family        TEXT    NOT NULL,
    embed         INTEGER NOT NULL,
    protocol      TEXT,
    target        TEXT    NOT NULL,
    title         TEXT,
    anchor        TEXT,
    anchor_text   TEXT,
    anchor_marked TEXT,
    block_ref     TEXT,
    address       TEXT    NOT NULL,
    span_line     INTEGER,
    span_column   INTEGER,
    span_offset   INTEGER,
    CHECK (anchor IS NULL OR block_ref IS NULL),
    CHECK (anchor <> ''),
    CHECK (block_ref <> ''),
    CHECK ((anchor IS NULL) = (anchor_text IS NULL)),
    CHECK (anchor_text IS NOT NULL OR anchor_marked IS NULL),
    {WHOLE_SPAN}
)"
        ),
        "CREATE UNIQUE INDEX links_document_ordinal ON links(document, ordinal)".to_string(),
        "CREATE UNIQUE INDEX links_id_document ON links(id, document)".to_string(),
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
        "CREATE INDEX blocks_document_block_id ON blocks(document, block_id)".to_string(),
        format!(
            "CREATE TABLE document_tags (
    id          INTEGER PRIMARY KEY,
    document    INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    ordinal     INTEGER NOT NULL,
    name        TEXT    NOT NULL,
    folded_name TEXT    NOT NULL,
    source      TEXT    NOT NULL,
    span_line   INTEGER,
    span_column INTEGER,
    span_offset INTEGER,
    {WHOLE_SPAN}
)"
        ),
        "CREATE UNIQUE INDEX document_tags_document_ordinal ON document_tags(document, ordinal)"
            .to_string(),
        "CREATE INDEX document_tags_folded_name ON document_tags(folded_name, document)"
            .to_string(),
    ]
}
