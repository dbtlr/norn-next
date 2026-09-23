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
}

/// How many statement shapes [`FindStatement::all`] holds.
pub const FIND_STATEMENTS: usize = 4;

impl FindStatement {
    /// Every statement shape, in slot order.
    ///
    /// The order and the direction a page carries are not part of its slot: a
    /// bar that cares about those axes ranges over them itself.
    pub fn all() -> [Self; FIND_STATEMENTS] {
        [
            Self::ActiveFingerprint,
            Self::PathPage(PageDirection::Ascending),
            Self::FieldValuePage(FieldOrder::Raw, PageDirection::Ascending),
            Self::FieldMissingPage(FieldOrder::Raw, PageDirection::Ascending),
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
        };
        assert!(
            slot < FIND_STATEMENTS,
            "slot {slot} is outside the enumeration: grow `all` and `FIND_STATEMENTS` with the \
             statement that took it"
        );
        slot
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
    /// A value row under the key equals the value: `(key, raw)` on
    /// `document_fields_raw`.
    Equal,
    /// No value row under the key equals the value, a document without the key
    /// included.
    NotEqual,
    /// A value row under the key equals one of the values.
    Member,
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
    /// Every filter shape, in slot order. A bounded filter is named under the
    /// raw order; the typed order is the same slot.
    pub fn all() -> [Self; FIND_FILTERS] {
        [
            Self::Equal,
            Self::NotEqual,
            Self::Member,
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
            Self::Equal => 0,
            Self::NotEqual => 1,
            Self::Member => 2,
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
            FindFilter::Equal | FindFilter::NotEqual => {
                let (key, value) = (next(), next());
                let membership = if self.shape == FindFilter::Equal {
                    "IN"
                } else {
                    "NOT IN"
                };
                format!(
                    "{id} {membership} (SELECT fv.document FROM document_fields AS fv
                     WHERE fv.key = {key} AND fv.raw = {value})"
                )
            }
            FindFilter::Member => {
                let (key, values) = (next(), next());
                format!(
                    "{id} IN (SELECT fv.document FROM document_fields AS fv
                     WHERE fv.key = {key} AND fv.raw IN (SELECT value FROM json_each({values})))"
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
        FindStatement::ActiveFingerprint => {
            unreachable!("the fingerprint read is a point read, not a page section")
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
