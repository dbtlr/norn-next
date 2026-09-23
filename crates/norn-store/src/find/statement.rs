//! The statements the find builder emits, named, and the one composer that
//! spells each of them with its parameters.
//!
//! **One function writes a statement's text and binds its parameters.**
//! [`compose_page`] numbers each parameter as it writes the placeholder for
//! it, so the text and the values cannot drift apart, and the plan the
//! instrument takes and the page the builder reads are the same call over the
//! same inputs.

use norn_db::rusqlite::types::Value;

use super::FieldOrder;
use crate::request::range_predicate_from;

/// Every statement shape the find builder runs, named.
///
/// The same discipline as [`crate::ExplainedStatement`]: [`FindStatement::all`]
/// holds each shape once, [`FindStatement::slot`] is exhaustive over the enum,
/// and [`FIND_STATEMENTS`] is the count a census is checked against. A page's
/// filters are spelled inside the page statement they narrow, and are named by
/// [`FindFilter`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FindStatement {
    /// The pinned vault-schema fingerprint, read on the snapshot: what a
    /// finding filter is judged under.
    ActiveFingerprint,
    /// A page in path order: ASCII case folded, with the bytewise path as the
    /// tie-break, on `documents_path_nocase`.
    PathPage(PageDirection),
    /// The documents a field sort orders by value: one marker row per document,
    /// its least value under the order, in `(value, path)` order on that
    /// order's marker index.
    FieldValuePage(FieldOrder, PageDirection),
    /// The documents a field sort holds no value for under the order, in path
    /// order on `documents_path`. They stand before every valued document
    /// ascending and after every one descending.
    FieldMissingPage(FieldOrder, PageDirection),
    /// Whether any document carries a key the declaration does not name: one
    /// existence seek of `document_fields_presence`.
    KnownKey,
    /// Every key a document carries, each once, in key order: a walk of
    /// `document_fields_presence` that seeks past each key to the next, so it
    /// costs the distinct keys rather than the rows that carry them. Run only
    /// where some key a request named is unknown.
    FieldUniverse,
    /// Whether a path part with no wildcard names a directory documents stand
    /// under and no document: one seek of `documents_path` at the path, and
    /// one of the range beneath it.
    BareDirectory,
    /// The document rows a page found, by row id, carrying only the columns
    /// the projection names: the frontmatter projection, the head of the body
    /// and the body's length.
    HydrateDocuments,
    /// The head of one nested collection for each document a page found: the
    /// rows below the per-row ceiling, one range seek of the collection's
    /// `(document, ordinal)` index per document.
    NestedHead(Nested),
    /// How many rows one nested collection holds for each document whose head
    /// the ceiling cut: an index-only count over the same index.
    NestedTotal(Nested),
}

/// How many statement shapes [`FindStatement::all`] holds.
pub const FIND_STATEMENTS: usize = 10;

impl FindStatement {
    /// Every statement shape, in slot order.
    ///
    /// The order and the direction a page carries, and the collection a nested
    /// read reads, are not part of its slot: a bar that cares about those axes
    /// ranges over them itself.
    pub fn all() -> [Self; FIND_STATEMENTS] {
        [
            Self::ActiveFingerprint,
            Self::PathPage(PageDirection::Ascending),
            Self::FieldValuePage(FieldOrder::Raw, PageDirection::Ascending),
            Self::FieldMissingPage(FieldOrder::Raw, PageDirection::Ascending),
            Self::KnownKey,
            Self::FieldUniverse,
            Self::BareDirectory,
            Self::HydrateDocuments,
            Self::NestedHead(Nested::Tags),
            Self::NestedTotal(Nested::Tags),
        ]
    }

    /// Where this statement stands in [`Self::all`]. Exhaustive, so a shape
    /// added to the enum has to take a slot.
    pub fn slot(self) -> usize {
        let slot = match self {
            Self::ActiveFingerprint => 0,
            Self::PathPage(_) => 1,
            Self::FieldValuePage(..) => 2,
            Self::FieldMissingPage(..) => 3,
            Self::KnownKey => 4,
            Self::FieldUniverse => 5,
            Self::BareDirectory => 6,
            Self::HydrateDocuments => 7,
            Self::NestedHead(_) => 8,
            Self::NestedTotal(_) => 9,
        };
        assert!(
            slot < FIND_STATEMENTS,
            "slot {slot} is outside the enumeration: grow `all` and `FIND_STATEMENTS` with the \
             statement that took it"
        );
        slot
    }
}

/// A nested collection a row can project.
///
/// Each is a table whose rows a document owns, keyed by the document and the
/// row's position in it, so the head of one is a range of that key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Nested {
    /// The tags the document carries, in `document_tags`.
    Tags,
    /// The headings the document carries, in `headings`.
    Headings,
    /// The block identifiers the document defines, in `blocks`.
    Blocks,
}

impl Nested {
    /// Every collection, in the order a row's columns are hydrated in.
    pub const ALL: [Nested; 3] = [Nested::Tags, Nested::Headings, Nested::Blocks];

    /// The table the collection's rows live in.
    pub const fn table(self) -> &'static str {
        match self {
            Nested::Tags => "document_tags",
            Nested::Headings => "headings",
            Nested::Blocks => "blocks",
        }
    }

    /// The columns an item is read from, in the order the stored-fact reader
    /// of the same table reads them.
    const fn columns(self) -> &'static str {
        match self {
            Nested::Tags => "n.name, n.source, n.span_line, n.span_column, n.span_offset",
            Nested::Headings => {
                "n.text, n.slug, n.level, n.span_line, n.span_column, n.span_offset, \
                 n.body_offset, n.inside_container"
            }
            Nested::Blocks => "n.block_id, n.span_line, n.span_column, n.span_offset",
        }
    }

    /// How many columns [`Nested::columns`] names, which is where the document
    /// id and the ordinal stand after them.
    pub(crate) const fn width(self) -> usize {
        match self {
            Nested::Tags => 5,
            Nested::Headings => 8,
            Nested::Blocks => 4,
        }
    }
}

/// Which way a page runs.
///
/// The store's own reading of the wire's direction, so the statements it
/// spells are named by a closed set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageDirection {
    /// Smallest first.
    Ascending,
    /// Largest first.
    Descending,
}

/// Every filter shape a page statement narrows by, named.
///
/// Each is a membership test of the page's document against rows one index
/// seek reaches, so a filter costs the rows it matches rather than the vault.
/// [`FindFilter::all`] and [`FindFilter::slot`] keep the same discipline as
/// [`FindStatement`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FindFilter {
    /// A value row under the key equals the value under the order: `(key,
    /// raw)` on `document_fields_raw`, or `(key, typed)` on
    /// `document_fields_typed` against the value's typed sort key.
    Equal(FieldOrder),
    /// No value row under the key equals the value under the order, a
    /// document without the key included.
    NotEqual(FieldOrder),
    /// A value row under the key equals one of the values under the order.
    Member(FieldOrder),
    /// The document carries the key: its presence row, on
    /// `document_fields_presence`.
    Present,
    /// The document does not carry the key.
    Absent,
    /// A value row under the key sorts before the bound under the order.
    Before(FieldOrder),
    /// A value row under the key sorts after the bound under the order.
    After(FieldOrder),
    /// The body matches a full-text query, through `documents_fts`.
    FullText,
    /// The path matches a glob: the glob's literal-prefix range on
    /// `documents_path`, and the glob function over the paths it reaches.
    PathGlob,
    /// The document is in the class a resolution target opens: the target's
    /// suffix ranges on `documents_suffix_key`.
    Resolves,
    /// The document carries the tag, on `document_tags_name`.
    Tag,
    /// A finding of the kind stands over the document under the active
    /// fingerprint, on `findings_vault_schema_fingerprint`.
    Finding,
}

/// How many filter shapes [`FindFilter::all`] holds.
pub const FIND_FILTERS: usize = 12;

impl FindFilter {
    /// Every filter shape, in slot order. A filter that compares values is
    /// named under the raw order; the typed order is the same slot.
    pub fn all() -> [Self; FIND_FILTERS] {
        [
            Self::Equal(FieldOrder::Raw),
            Self::NotEqual(FieldOrder::Raw),
            Self::Member(FieldOrder::Raw),
            Self::Present,
            Self::Absent,
            Self::Before(FieldOrder::Raw),
            Self::After(FieldOrder::Raw),
            Self::FullText,
            Self::PathGlob,
            Self::Resolves,
            Self::Tag,
            Self::Finding,
        ]
    }

    /// Where this filter stands in [`Self::all`]. Exhaustive, so a shape added
    /// to the enum has to take a slot.
    pub fn slot(self) -> usize {
        let slot = match self {
            Self::Equal(_) => 0,
            Self::NotEqual(_) => 1,
            Self::Member(_) => 2,
            Self::Present => 3,
            Self::Absent => 4,
            Self::Before(_) => 5,
            Self::After(_) => 6,
            Self::FullText => 7,
            Self::PathGlob => 8,
            Self::Resolves => 9,
            Self::Tag => 10,
            Self::Finding => 11,
        };
        assert!(
            slot < FIND_FILTERS,
            "slot {slot} is outside the enumeration: grow `all` and `FIND_FILTERS` with the \
             filter that took it"
        );
        slot
    }
}

/// One filter as a statement spells it: its shape, and the values it binds in
/// the order its fragment numbers them.
#[derive(Clone, Debug)]
pub(crate) struct Filter {
    pub(crate) shape: FindFilter,
    /// The values the fragment binds, in the order [`Filter::spell`] writes
    /// their placeholders. A resolution filter binds two per suffix range, so
    /// the count is also how many ranges it opens.
    pub(crate) values: Vec<Value>,
}

/// The columns a filter tests: the page's document id, and its path.
#[derive(Clone, Copy)]
struct Subject {
    id: &'static str,
    path: &'static str,
}

impl Filter {
    /// This filter's fragment over `subject`, its placeholders numbered by
    /// `binder`.
    fn spell(&self, subject: Subject, binder: &mut Binder) -> String {
        let Subject { id, path } = subject;
        // The number this fragment's first value takes: a fragment's values
        // are bound in the order it names them, so they number from here.
        let first = binder.next_number();
        let mut values = self.values.iter().cloned();
        let mut next = || {
            binder.bind(
                values
                    .next()
                    .expect("a filter binds every value its fragment names"),
            )
        };
        let bounded = |order: FieldOrder, comparison: &str, next: &mut dyn FnMut() -> String| {
            let column = order.column();
            let (key, bound) = (next(), next());
            format!(
                "{id} IN (SELECT fb.document FROM document_fields AS fb
                     WHERE fb.key = {key} AND fb.{column} {comparison} {bound})"
            )
        };
        match self.shape {
            FindFilter::Equal(order) | FindFilter::NotEqual(order) => {
                let column = order.column();
                let (key, value) = (next(), next());
                let membership = if matches!(self.shape, FindFilter::Equal(_)) {
                    "IN"
                } else {
                    "NOT IN"
                };
                format!(
                    "{id} {membership} (SELECT fv.document FROM document_fields AS fv
                     WHERE fv.key = {key} AND fv.{column} = {value})"
                )
            }
            FindFilter::Member(order) => {
                let column = order.column();
                let (key, values) = (next(), next());
                format!(
                    "{id} IN (SELECT fv.document FROM document_fields AS fv
                     WHERE fv.key = {key}
                       AND fv.{column} IN (SELECT value FROM json_each({values})))"
                )
            }
            FindFilter::Present | FindFilter::Absent => {
                let key = next();
                let membership = if self.shape == FindFilter::Present {
                    "IN"
                } else {
                    "NOT IN"
                };
                format!(
                    "{id} {membership} (SELECT fp.document FROM document_fields AS fp
                     WHERE fp.key = {key} AND fp.ordinal = 0)"
                )
            }
            FindFilter::Before(order) => bounded(order, "<", &mut next),
            FindFilter::After(order) => bounded(order, ">", &mut next),
            FindFilter::FullText => {
                let query = next();
                format!(
                    "{id} IN (SELECT rowid FROM documents_fts WHERE documents_fts MATCH {query})"
                )
            }
            FindFilter::PathGlob => {
                let (lower, upper, pattern) = (next(), next(), next());
                format!(
                    "{id} IN (SELECT dg.id FROM documents AS dg
                     WHERE dg.path >= {lower} AND dg.path < {upper}
                       AND {function}({pattern}, dg.path))",
                    function = super::glob::GLOB_FUNCTION
                )
            }
            FindFilter::Resolves => {
                // Each bound takes its number in turn, and the range predicate
                // spells the ranges over those numbers from `first`.
                for _ in &self.values {
                    next();
                }
                let ranges = range_predicate_from("dr.suffix_key", self.values.len() / 2, first);
                format!("{id} IN (SELECT dr.id FROM documents AS dr WHERE {ranges})")
            }
            FindFilter::Tag => {
                let name = next();
                format!(
                    "{id} IN (SELECT tg.document FROM document_tags AS tg WHERE tg.name = {name})"
                )
            }
            FindFilter::Finding => {
                let (fingerprint, kind) = (next(), next());
                format!(
                    "{path} IN (SELECT fg.path FROM findings AS fg
                     WHERE fg.vault_schema_fingerprint = {fingerprint} AND fg.kind = {kind})"
                )
            }
        }
    }
}

/// Numbers placeholders as they are written, holding the values in that order.
#[derive(Default)]
struct Binder {
    values: Vec<Value>,
}

impl Binder {
    /// The number the next placeholder takes.
    fn next_number(&self) -> usize {
        self.values.len() + 1
    }

    /// The placeholder for `value`, which takes the next number.
    fn bind(&mut self, value: Value) -> String {
        self.values.push(value);
        format!("?{}", self.values.len())
    }
}

/// Where a page section resumes: the sort value and the path of the row it
/// stopped after. `None` in both starts the section at its first row.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SectionStart<'a> {
    pub(crate) sort: Option<&'a str>,
    pub(crate) path: Option<&'a str>,
}

/// What one page section reads: the statement, the key a field section is
/// ordered by, where it resumes, the filters it narrows by, and how many rows.
pub(crate) struct Section<'a> {
    pub(crate) statement: FindStatement,
    pub(crate) key: Option<&'a str>,
    pub(crate) start: SectionStart<'a>,
    pub(crate) filters: &'a [Filter],
    pub(crate) rows: usize,
}

/// One page section's statement and its parameters, in the numbering the text
/// states.
///
/// Every section selects the document id, its path and its sort value — null
/// outside a field sort's valued section — in the order the page runs.
///
/// **Where a section resumes is its lower bound**, not a test applied to the
/// rows it read: an unset position coalesces to a bound below every row
/// ascending — the empty text — and above every row descending — an empty
/// blob, which SQLite orders after every text. So the text does not branch on
/// whether the section resumes, and the plan is the same either way.
pub(crate) fn compose_page(section: &Section<'_>) -> (String, Vec<Value>) {
    let mut binder = Binder::default();
    let text = |value: Option<&str>| value.map_or(Value::Null, |value| Value::Text(value.into()));
    let (head, ordering, subject) = match section.statement {
        FindStatement::ActiveFingerprint
        | FindStatement::KnownKey
        | FindStatement::FieldUniverse
        | FindStatement::BareDirectory
        | FindStatement::HydrateDocuments
        | FindStatement::NestedHead(_)
        | FindStatement::NestedTotal(_) => {
            unreachable!("{:?} is not a page section", section.statement)
        }
        FindStatement::PathPage(direction) => {
            let after = binder.bind(text(section.start.path));
            let (comparison, beyond, descending) = spelled(direction);
            (
                format!(
                    "SELECT d.id, d.path, NULL FROM documents AS d
                     WHERE d.path {comparison}= COALESCE({after}, {beyond}) COLLATE NOCASE
                       AND ({after} IS NULL OR d.path {comparison} {after} COLLATE NOCASE
                            OR d.path {comparison} {after})"
                ),
                format!("d.path COLLATE NOCASE{descending}, d.path{descending}"),
                Subject {
                    id: "d.id",
                    path: "d.path",
                },
            )
        }
        FindStatement::FieldValuePage(order, direction) => {
            let key = binder.bind(text(section.key));
            let sort = binder.bind(text(section.start.sort));
            let after = binder.bind(text(section.start.path));
            let (comparison, beyond, descending) = spelled(direction);
            let (column, marker) = (order.column(), order.marker());
            (
                format!(
                    "SELECT f.document, f.path, f.{column} FROM document_fields AS f
                     WHERE f.key = {key} AND f.{marker} = 1
                       AND (f.{column}, f.path) {comparison}
                           (COALESCE({sort}, {beyond}), COALESCE({after}, {beyond}))"
                ),
                format!("f.{column}{descending}, f.path{descending}"),
                Subject {
                    id: "f.document",
                    path: "f.path",
                },
            )
        }
        FindStatement::FieldMissingPage(order, direction) => {
            let after = binder.bind(text(section.start.path));
            let key = binder.bind(text(section.key));
            let (comparison, beyond, descending) = spelled(direction);
            let marker = order.marker();
            (
                format!(
                    "SELECT d.id, d.path, NULL FROM documents AS d
                     WHERE d.path {comparison} COALESCE({after}, {beyond})
                       AND NOT EXISTS (SELECT 1 FROM document_fields AS m
                           WHERE m.document = d.id AND m.key = {key} AND m.{marker} = 1)"
                ),
                format!("d.path{descending}"),
                Subject {
                    id: "d.id",
                    path: "d.path",
                },
            )
        }
    };
    let filters: String = section
        .filters
        .iter()
        .map(|filter| {
            format!(
                "\n                       AND {}",
                filter.spell(subject, &mut binder)
            )
        })
        .collect();
    let limit = binder.bind(Value::Integer(
        i64::try_from(section.rows).expect("a page's row count fits i64"),
    ));
    let sql = format!(
        "{head}{filters}
                     ORDER BY {ordering}
                     LIMIT {limit}"
    );
    (sql, binder.values)
}

/// A direction's comparison, the bound an unset position coalesces to, and the
/// `ORDER BY` suffix.
fn spelled(direction: PageDirection) -> (&'static str, &'static str, &'static str) {
    match direction {
        PageDirection::Ascending => (">", "''", ""),
        PageDirection::Descending => ("<", "x''", " DESC"),
    }
}

/// The ids a hydration statement reads, as the JSON array `json_each` walks.
fn id_list(ids: &[i64]) -> Value {
    let listed: Vec<String> = ids.iter().map(i64::to_string).collect();
    Value::Text(format!("[{}]", listed.join(",")))
}

/// [`FindStatement::KnownKey`]: whether any document carries `key`.
pub(crate) fn compose_known_key(key: &str) -> (String, Vec<Value>) {
    (
        "SELECT EXISTS (SELECT 1 FROM document_fields AS fk
                         WHERE fk.key = ?1 AND fk.ordinal = 0)"
            .to_string(),
        vec![Value::Text(key.to_string())],
    )
}

/// [`FindStatement::FieldUniverse`]: every key a document carries, once each,
/// in key order.
///
/// Each step seeks the presence index for the least key after the one before
/// it, so the walk reads one index entry per distinct key rather than one per
/// document that carries it. Every key is text, and every text sorts at or
/// after the empty one, so the first step's bound excludes none.
pub(crate) fn compose_universe() -> (String, Vec<Value>) {
    (
        "WITH RECURSIVE universe(key) AS (
             SELECT (SELECT MIN(fu.key) FROM document_fields AS fu
                      WHERE fu.ordinal = 0 AND fu.key >= '')
             UNION ALL
             SELECT (SELECT MIN(fx.key) FROM document_fields AS fx
                      WHERE fx.ordinal = 0 AND fx.key > universe.key)
               FROM universe WHERE universe.key IS NOT NULL
         )
         SELECT key FROM universe WHERE key IS NOT NULL"
            .to_string(),
        Vec::new(),
    )
}

/// [`FindStatement::BareDirectory`]: true where no document stands at `path`
/// and some document stands in `[lower, upper)`, the range of paths beneath
/// it.
pub(crate) fn compose_bare_directory(path: &str, lower: &str, upper: &str) -> (String, Vec<Value>) {
    (
        "SELECT NOT EXISTS (SELECT 1 FROM documents AS da WHERE da.path = ?1)
            AND EXISTS (SELECT 1 FROM documents AS du WHERE du.path >= ?2 AND du.path < ?3)"
            .to_string(),
        vec![
            Value::Text(path.to_string()),
            Value::Text(lower.to_string()),
            Value::Text(upper.to_string()),
        ],
    )
}

/// Which of a document row's own columns a hydration reads.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DocumentColumns {
    /// The frontmatter projection, which a field is read off.
    pub(crate) frontmatter: bool,
    /// The head of the body, and the whole body's length in bytes.
    pub(crate) body: bool,
}

/// [`FindStatement::HydrateDocuments`]: the rows `ids` name, each as its id,
/// its frontmatter projection, at most `body_ceiling` bytes of its body, and
/// the whole body's length in bytes — a column the projection does not name
/// selected as `NULL`, so its value is never handed back.
///
/// The body's head is cut in bytes and may end inside a character; the reader
/// cuts it back to the last whole one.
pub(crate) fn compose_documents(
    ids: &[i64],
    columns: DocumentColumns,
    body_ceiling: usize,
) -> (String, Vec<Value>) {
    let mut binder = Binder::default();
    let listed = binder.bind(id_list(ids));
    let frontmatter = if columns.frontmatter {
        "d.frontmatter".to_string()
    } else {
        "NULL".to_string()
    };
    let (head, length) = if columns.body {
        let ceiling = binder.bind(Value::Integer(
            i64::try_from(body_ceiling).expect("a body ceiling fits i64"),
        ));
        (
            format!("substr(CAST(d.body AS BLOB), 1, {ceiling})"),
            "octet_length(d.body)".to_string(),
        )
    } else {
        ("NULL".to_string(), "NULL".to_string())
    };
    (
        format!(
            "SELECT d.id, {frontmatter}, {head}, {length} FROM documents AS d
             WHERE d.id IN (SELECT value FROM json_each({listed}))"
        ),
        binder.values,
    )
}

/// [`FindStatement::NestedHead`]: the rows of `nested` below `ceiling` for
/// each document `ids` names, each row's item columns followed by its
/// document id and its ordinal.
pub(crate) fn compose_nested_head(
    nested: Nested,
    ids: &[i64],
    ceiling: usize,
) -> (String, Vec<Value>) {
    (
        format!(
            "SELECT {columns}, n.document, n.ordinal FROM {table} AS n
             WHERE n.document IN (SELECT value FROM json_each(?1)) AND n.ordinal < ?2",
            columns = nested.columns(),
            table = nested.table(),
        ),
        vec![
            id_list(ids),
            Value::Integer(i64::try_from(ceiling).expect("a row ceiling fits i64")),
        ],
    )
}

/// [`FindStatement::NestedTotal`]: how many rows of `nested` each document
/// `ids` names holds, as the id and the count.
pub(crate) fn compose_nested_total(nested: Nested, ids: &[i64]) -> (String, Vec<Value>) {
    (
        format!(
            "SELECT j.value, (SELECT COUNT(*) FROM {table} AS n WHERE n.document = j.value)
             FROM json_each(?1) AS j",
            table = nested.table(),
        ),
        vec![id_list(ids)],
    )
}
