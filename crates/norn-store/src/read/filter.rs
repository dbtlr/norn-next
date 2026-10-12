//! The filters a conjunction's parts spell, named, and the binder every
//! read builder's composer numbers its parameters with.

use norn_db::rusqlite::types::Value;
use norn_wire::Pattern;

use super::{FieldMatch, FieldOrder, MatchedColumn};
use crate::facts::StoredPathOrder;
use crate::fields::FieldContainer;
use crate::path::SuffixKey;
use crate::resolve;

/// Every filter shape a page statement narrows by, named.
///
/// Each reads rows one index seek reaches, so a filter costs the rows it
/// matches rather than the vault, three ways: as the set of documents it keeps,
/// as a test of one document at its key, and — where it keeps its documents in
/// the answer's path order ([`ReadFilter::is_path_ordered`]) — as those
/// documents from a place in that order, a seek of the same index past its
/// key and value, or one such seek per value or container, merged.
/// [`ReadFilter::all`] and [`ReadFilter::slot`] keep the same discipline as
/// [`crate::FindStatement`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadFilter {
    /// A value row under the key equals the value under the reading: `(key,
    /// raw)` on `document_fields_raw`, `(key, typed)` on
    /// `document_fields_typed` against the value's typed sort key, or `(key,
    /// folded)` on `document_fields_folded` against the tag or link the value
    /// names. Each index holds the path after them, so the documents at one
    /// value stand in the answer's path order.
    /// A shaped reading counts a row only where its key's presence row, one
    /// primary-key seek, names the container the declared shape holds.
    Equal(FieldMatch),
    /// No value row under the key equals the value under the reading, a
    /// document without the key included.
    NotEqual(FieldMatch),
    /// A value row under the key equals one of the values under the reading.
    Member(FieldMatch),
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
    /// The path matches a glob under the root's path order: the glob's
    /// literal-prefix range on `documents_path` where the root tells
    /// spellings apart, and the folded prefix's `NOCASE` range on
    /// `documents_path_nocase` where it folds ASCII case, and the glob
    /// function, under the same order, over the paths the range reaches. A
    /// validate spells the part over a finding's own path instead
    /// ([`crate::ValidateStatement`]).
    PathGlob(StoredPathOrder),
    /// The document is in the class a resolution target opens on the root:
    /// the target's suffix ranges on `documents_suffix_key` where the root
    /// tells spellings apart, and on `documents_folded_suffix_key` where it
    /// folds ASCII case, less the places the schema ignores.
    Resolves(SuffixKey),
    /// The document carries the tag: its folded name, bound folded, on
    /// `document_tags_folded_name`.
    Tag,
    /// The document holds a link that resolves to exactly the one document a
    /// links-to part's target names: an equality seek of `link_keys_key`
    /// where the root tells spellings apart, and of `link_keys_folded_key`
    /// where it folds ASCII case, at each key that document is named by, then
    /// for each link reached, a seek of `link_keys_link` for the link's keys
    /// and of the documents each of them reaches — a range of the suffix key
    /// the root probes, or the path — to confirm no other document is in the
    /// link's resolution. A path key is confirmed as a suffix key is: a
    /// rooted wikilink holds a path key per reduction, so a link reached at
    /// the named document's path may name another document at its other one.
    LinksTo(SuffixKey),
    /// A finding of the kind stands over the document under the active
    /// fingerprint: one covering seek of `findings_fingerprint_kind_nocase` at
    /// `(fingerprint, kind)`, which carries the path after them, each
    /// finding's path read back to its document on `documents_path`.
    Finding,
}

/// How many filter shapes [`ReadFilter::all`] holds.
pub const READ_FILTERS: usize = 13;

impl ReadFilter {
    /// Every filter shape, in slot order. A filter that compares values is
    /// named under the raw order, and a filter a root's case behaviour selects
    /// under the order that tells spellings apart; the other form is the same
    /// slot.
    pub fn all() -> [Self; READ_FILTERS] {
        [
            Self::Equal(FieldMatch::RAW),
            Self::NotEqual(FieldMatch::RAW),
            Self::Member(FieldMatch::RAW),
            Self::Present,
            Self::Absent,
            Self::Before(FieldOrder::Raw),
            Self::After(FieldOrder::Raw),
            Self::FullText,
            Self::PathGlob(StoredPathOrder::Sensitive),
            Self::Resolves(SuffixKey::Raw),
            Self::Tag,
            Self::Finding,
            Self::LinksTo(SuffixKey::Raw),
        ]
    }

    /// Whether this filter excludes: its seek reaches the documents a page
    /// must not hold rather than the ones it keeps, so a page is never driven
    /// from it. Inequality and absence exclude; every other filter keeps what
    /// its seek reaches.
    pub fn excludes(self) -> bool {
        matches!(self, Self::NotEqual(_) | Self::Absent)
    }

    /// Whether this filter hands the documents it keeps back in the answer's
    /// path order, running `descending` or not, from any place in it: an
    /// equality, a tag, a finding and a path part either way, and a
    /// membership and a presence ascending, each merging several seeks. Such
    /// a filter drives a page from the place it resumes at, so a continuation
    /// costs its page, not what the filter keeps. A range, a full-text match,
    /// a resolution and a links-to part reach their documents in another
    /// order, and an excluding filter keeps no documents of its own.
    pub fn is_path_ordered(self, descending: bool) -> bool {
        match self {
            Self::Equal(_) | Self::Tag | Self::Finding | Self::PathGlob(_) => true,
            Self::Member(_) | Self::Present => !descending,
            Self::NotEqual(_)
            | Self::Absent
            | Self::Before(_)
            | Self::After(_)
            | Self::FullText
            | Self::Resolves(_)
            | Self::LinksTo(_) => false,
        }
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
            Self::PathGlob(_) => 8,
            Self::Resolves(_) => 9,
            Self::Tag => 10,
            Self::Finding => 11,
            Self::LinksTo(_) => 12,
        };
        assert!(
            slot < READ_FILTERS,
            "slot {slot} is outside the enumeration: grow `all` and `READ_FILTERS` with the \
             filter that took it"
        );
        slot
    }
}

/// One filter as a statement spells it.
#[derive(Clone, Debug)]
pub(crate) enum Filter {
    /// A path part, typed, so each read derives the range its own order
    /// seeks from the glob: a find and a count the root's
    /// ([`super::glob::path_range`]), a validate the answer's
    /// ([`super::answer_order::answer_range`]).
    Path(PathPart),
    /// Every other part: its shape, and the values it binds.
    Bound {
        shape: ReadFilter,
        /// The values the fragment binds, in the order [`Filter::spell`]
        /// writes their placeholders. A resolution filter binds two per
        /// suffix range and [`crate::resolve::EXCLUSION_PARAMETERS`] more —
        /// the ignore set, the target's segment count and the path order — so
        /// the count also says how many ranges it opens. A links-to filter
        /// binds each key it seeks and [`LINKS_TO_PARAMETERS`] more.
        values: Vec<Value>,
        /// The values a membership names, one by one, which its source seeks
        /// each of apart ([`Filter::source`]); `values` binds them as one list.
        /// Empty for every other shape.
        members: Vec<Value>,
    },
}

/// A path part as compiled: its glob, parsed, and the root's path order the
/// glob matches under.
#[derive(Clone, Debug)]
pub(crate) struct PathPart {
    pub(crate) pattern: Pattern,
    pub(crate) order: StoredPathOrder,
}

impl PathPart {
    /// The test that `path`, a column holding a path, matches the glob under
    /// the root's fold, its two values bound by `binder`.
    pub(crate) fn glob_test(&self, path: &str, binder: &mut Binder) -> String {
        let pattern = binder.bind(Value::Text(self.pattern.as_str().to_string()));
        let order = binder.bind(Value::Text(self.order.as_str().to_string()));
        glob_test(&pattern, path, &order)
    }
}

impl Filter {
    /// The shape this filter narrows by.
    pub(crate) fn shape(&self) -> ReadFilter {
        match self {
            Filter::Path(part) => ReadFilter::PathGlob(part.order),
            Filter::Bound { shape, .. } => *shape,
        }
    }

    /// A path part's glob and the order it matches under, and `None` for any
    /// other part.
    pub(crate) fn path_part(&self) -> Option<&PathPart> {
        match self {
            Filter::Path(part) => Some(part),
            Filter::Bound { .. } => None,
        }
    }

    /// This filter's fragment: a membership test of `id`, the column holding
    /// the page's document id, against the set of documents the filter keeps,
    /// its placeholders numbered by `binder`.
    pub(crate) fn spell(&self, id: &str, binder: &mut Binder) -> String {
        let Rows {
            table,
            document,
            condition,
            ..
        } = self.kept_rows(binder);
        let membership = if self.shape().excludes() {
            "NOT IN"
        } else {
            "IN"
        };
        format!(
            "{id} {membership} (SELECT {document} FROM {table}
                     WHERE {condition})"
        )
    }

    /// Whether the documents this filter keeps are at most `ceiling` rows of
    /// its seek, as one statement answers it: the rows counted, stopping one
    /// past the ceiling, so the count costs at most the ceiling whatever the
    /// filter keeps. A document reached by several rows counts once per row,
    /// so a filter is never taken to keep fewer documents than it does.
    pub(crate) fn kept_within(&self, ceiling: usize) -> (String, Vec<Value>) {
        let mut binder = Binder::default();
        let Rows {
            table, condition, ..
        } = self.kept_rows(&mut binder);
        let ceiling = i64::try_from(ceiling).expect("a ceiling fits i64");
        let (within, past) = (
            binder.bind(Value::Integer(ceiling)),
            binder.bind(Value::Integer(ceiling + 1)),
        );
        (
            format!(
                "SELECT count(*) <= {within} FROM (SELECT 1 FROM {table}
                     WHERE {condition}
                     LIMIT {past})"
            ),
            binder.into_values(),
        )
    }

    /// The rows this filter's seek reads for the set of documents it keeps:
    /// a path part's glob range under the root's order, then the glob and the
    /// order; every other part its own rows.
    fn kept_rows(&self, binder: &mut Binder) -> Rows {
        match self {
            Filter::Path(part) => {
                let (lower, upper) = super::glob::path_range(&part.pattern, part.order);
                rows_of(
                    self.shape(),
                    &[
                        Value::Text(lower),
                        upper,
                        Value::Text(part.pattern.as_str().to_string()),
                        Value::Text(part.order.as_str().to_string()),
                    ],
                    Reached::Set,
                    binder,
                )
            }
            Filter::Bound { shape, values, .. } => rows_of(*shape, values, Reached::Set, binder),
        }
    }

    /// Whether the one document whose id `id` names passes this filter: a
    /// test correlated with that document, which reaches only its rows — a
    /// seek at the document's key, or at its path — so a page testing each
    /// document it reads costs those documents, never the rows the part
    /// matches across the vault ([`Filter::spell`] reads those once per
    /// statement). Its placeholders are numbered by `binder`.
    pub(crate) fn test(&self, id: &str, binder: &mut Binder) -> String {
        match self {
            Filter::Path(part) => format!(
                "EXISTS (SELECT 1 FROM documents AS dg
                     WHERE dg.id = {id} AND {})",
                part.glob_test("dg.path", binder)
            ),
            Filter::Bound { shape, values, .. } => test_bound(*shape, values, id, binder),
        }
    }

    /// Whether this filter hands the documents it keeps back in the answer's
    /// path order, running `descending` or not: its shape does
    /// ([`ReadFilter::is_path_ordered`]), and a membership names at most
    /// [`MERGED_SOURCE_VALUES`] values.
    pub(crate) fn is_path_ordered(&self, descending: bool) -> bool {
        let merges_few = match self {
            Filter::Bound { members, .. } => members.len() <= MERGED_SOURCE_VALUES,
            Filter::Path(_) => true,
        };
        self.shape().is_path_ordered(descending) && merges_few
    }

    /// The documents this filter keeps, in the answer's path order from
    /// `reach`'s place on, each once, as a statement's `FROM` names one: the
    /// columns `folded`, `path` and `id`, ordered by the first two and
    /// running `reach`'s way. `None` where the filter has no such order
    /// `reach`'s way ([`ReadFilter::is_path_ordered`]).
    ///
    /// **A source seeks past its place**, so a continuation driven by it
    /// reads the documents from the place it resumes at and stops where the
    /// statement does. Each is one seek of an index holding the part's key
    /// and then the path in the answer order — a key's value rows, a tag's
    /// rows, a key's presence rows in each container, a finding kind's rows,
    /// or the documents in a path part's folded range — grouped by the path
    /// where one document holds several of the rows. A membership merges one
    /// seek per value it names, and a presence one per container, which
    /// SQLite merges in either direction but hands back in order only
    /// ascending.
    pub(crate) fn source(&self, reach: &Reach, binder: &mut Binder) -> Option<String> {
        if !self.is_path_ordered(reach.descending) {
            return None;
        }
        match self {
            Filter::Path(part) => {
                // The answer order's folded range, on either root, rather than
                // the root's range a set reads: the source hands documents
                // back in the answer's order, and the glob decides each.
                let (lower, upper) = super::answer_order::answer_range(&part.pattern);
                let (lower, upper) = (binder.bind(Value::Text(lower)), binder.bind(upper));
                let glob = part.glob_test("dg.path", binder);
                let rows = Rows::new(
                    "documents AS dg",
                    "dg.id",
                    format!(
                        "dg.path >= {lower} COLLATE NOCASE AND dg.path < {upper} COLLATE NOCASE \
                         AND {glob}"
                    ),
                );
                Some(reach.arm(&rows, false, binder))
            }
            Filter::Bound {
                shape,
                values,
                members,
            } => Some(source_bound(*shape, values, members, reach, binder)),
        }
    }
}

/// Where a [`Filter::source`] starts and stops in the answer's path order,
/// and which way it runs.
pub(crate) struct Reach {
    /// Whether the source runs from the greatest path down.
    pub(crate) descending: bool,
    /// The place the source starts from, folded and bytewise: the documents
    /// standing past it the source's way, and the one at it where `inclusive`.
    /// A source starting at its first document starts past a place no path
    /// stands at — the empty text ascending, and the empty blob, which SQLite
    /// orders after every text, descending — so its text does not branch on
    /// whether it resumes.
    pub(crate) from: (Value, Value),
    /// Whether the document at `from` is read too.
    pub(crate) inclusive: bool,
    /// The folded path the source stops at, or `None`: an ascending source
    /// reads the paths folding below it, and a descending one the paths
    /// folding at or above it — a path part's range, from the end the source
    /// runs toward.
    pub(crate) until: Option<Value>,
}

impl Reach {
    /// One arm of a source: `rows`, bounded by this reach, ordered its way,
    /// and grouped by the path where `grouped`, which is where a document
    /// holds several of the rows.
    fn arm(&self, rows: &Rows, grouped: bool, binder: &mut Binder) -> String {
        let body = self.rows(rows, binder);
        let grouping = if grouped { " GROUP BY 1, 2" } else { "" };
        format!("{body}{grouping} ORDER BY {}", self.ordering())
    }

    /// `rows` bounded by this reach and unordered, each as its document's
    /// folded path, path and id: what a merge of several arms joins before
    /// ordering them together.
    fn rows(&self, rows: &Rows, binder: &mut Binder) -> String {
        let Rows {
            table,
            document,
            path,
            condition,
        } = rows;
        let mut conditions = vec![condition.clone()];
        let comparison = match (self.descending, self.inclusive) {
            (false, false) => ">",
            (false, true) => ">=",
            (true, false) => "<",
            (true, true) => "<=",
        };
        let (folded, bytewise) = (
            binder.bind(self.from.0.clone()),
            binder.bind(self.from.1.clone()),
        );
        conditions.push(format!(
            "({path}, {path}) {comparison} ({folded} COLLATE NOCASE, {bytewise})"
        ));
        if let Some(until) = &self.until {
            let until = binder.bind(until.clone());
            let comparison = if self.descending { ">=" } else { "<" };
            conditions.push(format!("{path} {comparison} {until} COLLATE NOCASE"));
        }
        format!(
            "SELECT {path} COLLATE NOCASE AS folded, {path} AS path, {document} AS id FROM {table}
                     WHERE {}",
            conditions.join(" AND ")
        )
    }

    /// The order a source hands its documents back in.
    fn ordering(&self) -> &'static str {
        if self.descending {
            "1 DESC, 2 DESC"
        } else {
            "1, 2"
        }
    }

    /// Several arms' rows merged into one order, each document once: what a
    /// membership's values and a presence's containers drive. SQLite merges
    /// the arms' own orders, which it reuses past the merge only ascending,
    /// so a descending reach has no merged source.
    fn merged(&self, arms: Vec<String>) -> String {
        debug_assert!(!self.descending, "a merged source runs ascending");
        format!(
            "{}\n                     ORDER BY {}",
            arms.join("\n                     UNION\n                     "),
            self.ordering()
        )
    }
}

/// The test of a filter of `shape` binding `values` on the one document `id`
/// names: whether any of the rows the filter reads stands for it, reached by
/// that document's key, its placeholders numbered by `binder`.
fn test_bound(shape: ReadFilter, values: &[Value], id: &str, binder: &mut Binder) -> String {
    let Rows {
        table,
        document,
        condition,
        ..
    } = rows_of(shape, values, Reached::OneDocument, binder);
    let existence = if shape.excludes() {
        "NOT EXISTS"
    } else {
        "EXISTS"
    };
    format!(
        "{existence} (SELECT 1 FROM {table}
                     WHERE {document} = {id} AND {condition})"
    )
}

/// What a filter's rows are read for: the set of every document they stand
/// for, or whether they stand for one document.
#[derive(Clone, Copy, PartialEq)]
enum Reached {
    Set,
    OneDocument,
}

/// The rows a filter reads: the table they stand in, the column naming the
/// document each one stands for, the column holding that document's path
/// where the table carries one, and the condition a row meets. An excluding
/// filter's rows are the documents it does not keep.
struct Rows {
    table: String,
    document: String,
    path: String,
    condition: String,
}

impl Rows {
    /// The rows of `table`, read under its alias, whose `path` column is the
    /// alias's: every table a source reads carries its document's path.
    fn new(table: &str, document: &str, condition: String) -> Self {
        let alias = table.rsplit(" AS ").next().unwrap_or(table);
        Rows {
            table: table.to_string(),
            document: document.to_string(),
            path: format!("{alias}.path"),
            condition,
        }
    }
}

/// The rows a filter of `shape` binding `values` reads, read for `reached`,
/// its values bound by `binder` in the order the condition names them.
///
/// Every shape reads the same rows either way but a finding's: the set of
/// documents a finding kind stands over is the documents at the paths it
/// stands at, and one document's question is whether one stands at its path,
/// which a seek at that path answers.
fn rows_of(shape: ReadFilter, values: &[Value], reached: Reached, binder: &mut Binder) -> Rows {
    // The number this fragment's first value takes: a fragment's values
    // are bound in the order it names them, so they number from here.
    let first = binder.next_number();
    // One document's rows are sought at its key, so the column a row is
    // compared in stands behind a unary `+`, which SQLite reads as the same
    // value and never as an index constraint: an index on the value would
    // otherwise reach every row holding it across the vault.
    let compared = match reached {
        Reached::Set => "",
        Reached::OneDocument => "+",
    };
    let mut bound = values.iter().cloned();
    let mut next = || {
        binder.bind(
            bound
                .next()
                .expect("a filter binds every value its fragment names"),
        )
    };
    let bounded = |order: FieldOrder, comparison: &str, next: &mut dyn FnMut() -> String| {
        let column = order.column();
        let (key, bound) = (next(), next());
        Rows::new(
            "document_fields AS fb",
            "fb.document",
            format!("fb.key = {key} AND {compared}fb.{column} {comparison} {bound}"),
        )
    };
    match shape {
        ReadFilter::Equal(matched) | ReadFilter::NotEqual(matched) => {
            let column = matched.column();
            let (key, value) = (next(), next());
            let conditions = reading_conditions(matched, &mut next);
            Rows::new(
                "document_fields AS fv",
                "fv.document",
                format!("fv.key = {key} AND {compared}fv.{column} = {value}{conditions}"),
            )
        }
        ReadFilter::Member(matched) => {
            let column = matched.column();
            let (key, values) = (next(), next());
            let conditions = reading_conditions(matched, &mut next);
            Rows::new(
                "document_fields AS fv",
                "fv.document",
                format!(
                    "fv.key = {key}
                       AND {compared}fv.{column} IN (SELECT value FROM json_each({values})){conditions}"
                ),
            )
        }
        ReadFilter::Present | ReadFilter::Absent => {
            let key = next();
            Rows::new(
                "document_fields AS fp",
                "fp.document",
                format!("fp.key = {key} AND fp.ordinal = 0"),
            )
        }
        ReadFilter::Before(order) => bounded(order, "<", &mut next),
        ReadFilter::After(order) => bounded(order, ">", &mut next),
        ReadFilter::FullText => {
            let query = next();
            Rows::new(
                "documents_fts",
                "rowid",
                format!("documents_fts MATCH {query}"),
            )
        }
        ReadFilter::PathGlob(order) => {
            let (lower, upper, pattern, recorded) = (next(), next(), next(), next());
            let collation = order.collation();
            Rows::new(
                "documents AS dg",
                "dg.id",
                format!(
                    "dg.path >= {lower}{collation} AND dg.path < {upper}{collation}
                       AND {}",
                    glob_test(&pattern, "dg.path", &recorded)
                ),
            )
        }
        ReadFilter::Resolves(key) => {
            // Each value takes its number in turn, and the resolution's
            // predicate spells its ranges and its exclusion over those
            // numbers from `first`: two bounds per range, then the ignore
            // set, the target's segment count and the path order.
            for _ in values {
                next();
            }
            let ranges = (values.len() - resolve::EXCLUSION_PARAMETERS) / 2;
            Rows::new(
                "documents AS dr",
                "dr.id",
                resolve::predicate("dr", key, ranges, first),
            )
        }
        ReadFilter::Tag => {
            let name = next();
            Rows::new(
                "document_tags AS tg",
                "tg.document",
                format!("{compared}tg.folded_name = {name}"),
            )
        }
        ReadFilter::LinksTo(key) => {
            let listed: Vec<String> = (0..values.len() - LINKS_TO_PARAMETERS)
                .map(|_| next())
                .collect();
            let (ignored, order, path, document) = (next(), next(), next(), next());
            let link_key = resolve::link_key_column(key);
            Rows::new(
                "link_keys AS lk",
                "lk.document",
                format!(
                    "lk.{link_key} IN ({listed})
                       AND (lk.segments IS NULL OR {named_admitted})
                       AND NOT EXISTS (SELECT 1 FROM link_keys AS lo, documents AS dl
                           WHERE lo.link = lk.link AND lo.segments IS NOT NULL
                             AND {other_in_class}
                             AND dl.id <> {document})
                       AND NOT EXISTS (SELECT 1 FROM link_keys AS lp, documents AS dp
                           WHERE lp.link = lk.link AND lp.segments IS NULL
                             AND {other_at_path} AND dp.id <> {document})",
                    listed = listed.join(", "),
                    named_admitted = resolve::admits(&ignored, "lk.segments", &order, &path),
                    other_in_class = resolve::link_key_class(
                        "dl",
                        key,
                        &format!("lo.{link_key}"),
                        "lo.segments",
                        &ignored,
                        &order,
                    ),
                    other_at_path = resolve::link_key_path("dp", key, &format!("lp.{link_key}")),
                ),
            )
        }
        ReadFilter::Finding => {
            let (fingerprint, kind) = (next(), next());
            let standing = match reached {
                Reached::Set => format!(
                    "df.path IN (SELECT fg.path FROM findings AS fg
                         WHERE fg.vault_schema_fingerprint = {fingerprint}
                           AND fg.kind = {kind})"
                ),
                Reached::OneDocument => format!(
                    "EXISTS (SELECT 1 FROM findings AS fg
                         WHERE fg.path = df.path AND fg.vault_schema_fingerprint = {fingerprint}
                           AND {compared}fg.kind = {kind})"
                ),
            };
            Rows::new("documents AS df", "df.id", standing)
        }
    }
}

/// The conditions a reading adds to a value row `fv` beside its comparison:
/// under the tag fold, the bound flag saying whether the key's typed order
/// must also read the value; under a declared shape, the container the key's
/// presence row must name. Their values are bound through `next`.
fn reading_conditions(reading: FieldMatch, next: &mut dyn FnMut() -> String) -> String {
    let mut conditions = String::new();
    if reading.column == MatchedColumn::Folded {
        let typed = next();
        conditions.push_str(&format!(" AND ({typed} = 0 OR fv.typed IS NOT NULL)"));
    }
    if reading.shaped {
        let container = next();
        conditions.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM document_fields AS fs
                         WHERE fs.document = fv.document AND fs.key = fv.key
                           AND fs.ordinal = 0 AND fs.container = {container})"
        ));
    }
    conditions
}

/// The source of a filter of `shape` binding `values`, whose documents stand
/// in the answer's path order `reach`'s way ([`Filter::source`]): the rows the
/// filter's set reads ([`rows_of`]), bounded by the reach. A membership is
/// one arm per member, each the equality its member is — a membership's values
/// are an equality's with its list where the equality's value stands — and a
/// presence one arm per container, merged.
fn source_bound(
    shape: ReadFilter,
    values: &[Value],
    members: &[Value],
    reach: &Reach,
    binder: &mut Binder,
) -> String {
    match shape {
        ReadFilter::Equal(_) | ReadFilter::Tag => {
            let rows = rows_of(shape, values, Reached::Set, binder);
            reach.arm(&rows, true, binder)
        }
        ReadFilter::Member(matched) => {
            let arms = members
                .iter()
                .map(|member| {
                    let mut equal = values.to_vec();
                    equal[1] = member.clone();
                    let rows = rows_of(ReadFilter::Equal(matched), &equal, Reached::Set, binder);
                    reach.rows(&rows, binder)
                })
                .collect();
            reach.merged(arms)
        }
        ReadFilter::Present => {
            let arms = FieldContainer::ALL
                .iter()
                .map(|container| {
                    let mut rows = rows_of(shape, values, Reached::Set, binder);
                    let container = binder.bind(Value::Text(container.as_str().to_string()));
                    rows.condition
                        .push_str(&format!(" AND fp.container = {container}"));
                    reach.rows(&rows, binder)
                })
                .collect();
            reach.merged(arms)
        }
        ReadFilter::Finding => {
            // The set reads each document's path among the kind's findings';
            // a source reads the findings themselves, off the kind's index in
            // path order, each joined to the document at its path.
            let (fingerprint, kind) = (
                binder.bind(values[0].clone()),
                binder.bind(values[1].clone()),
            );
            let rows = Rows {
                table: "findings AS fg CROSS JOIN documents AS df ON df.path = fg.path".to_string(),
                document: "df.id".to_string(),
                path: "fg.path".to_string(),
                condition: format!(
                    "fg.vault_schema_fingerprint = {fingerprint} AND fg.kind = {kind}"
                ),
            };
            reach.arm(&rows, true, binder)
        }
        ReadFilter::NotEqual(_)
        | ReadFilter::Absent
        | ReadFilter::Before(_)
        | ReadFilter::After(_)
        | ReadFilter::FullText
        | ReadFilter::PathGlob(_)
        | ReadFilter::Resolves(_)
        | ReadFilter::LinksTo(_) => {
            unreachable!("{shape:?} is no part a bound source spells")
        }
    }
}

/// The most values a membership drives a page from its source with, one seek
/// of each merged.
///
/// Every read prepares its statement afresh, and a merge of one arm per value
/// costs its preparation per arm: at 256 values a page spent about 9 ms
/// preparing a source whose rows took a fraction of that, so a wider
/// membership drives a page as a part in no page's order does.
pub const MERGED_SOURCE_VALUES: usize = 16;

/// How many values a links-to filter binds after the keys it seeks: the
/// ignore set, the path order it is matched under, and the named document's
/// path and row id.
pub(crate) const LINKS_TO_PARAMETERS: usize = 4;

/// The test that `path`, a column holding a path, matches the glob bound at
/// `pattern` under the path order whose recorded spelling is bound at
/// `order`: the one spelling of a glob match every statement runs.
fn glob_test(pattern: &str, path: &str, order: &str) -> String {
    format!("{}({pattern}, {path}, {order})", super::glob::GLOB_FUNCTION)
}

/// Numbers placeholders as they are written, holding the values in that order.
#[derive(Default)]
pub(crate) struct Binder {
    values: Vec<Value>,
}

impl Binder {
    /// The number the next placeholder takes.
    pub(crate) fn next_number(&self) -> usize {
        self.values.len() + 1
    }

    /// The placeholder for `value`, which takes the next number.
    pub(crate) fn bind(&mut self, value: Value) -> String {
        self.values.push(value);
        format!("?{}", self.values.len())
    }

    /// The values bound, in their placeholders' numbering.
    pub(crate) fn into_values(self) -> Vec<Value> {
        self.values
    }
}
