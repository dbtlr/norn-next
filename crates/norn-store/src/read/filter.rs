//! The filters a conjunction's parts spell, named, and the binder every
//! read builder's composer numbers its parameters with.

use norn_db::rusqlite::types::Value;

use super::FieldOrder;
use crate::request::range_predicate_from;

/// Every filter shape a page statement narrows by, named.
///
/// Each is a membership test of the page's document against rows one index
/// seek reaches, so a filter costs the rows it matches rather than the vault.
/// [`ReadFilter::all`] and [`ReadFilter::slot`] keep the same discipline as
/// [`crate::FindStatement`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadFilter {
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
    /// fingerprint, on `findings_vault_schema_fingerprint`, each finding's
    /// path read back to its document on `documents_path`.
    Finding,
}

/// How many filter shapes [`ReadFilter::all`] holds.
pub const READ_FILTERS: usize = 12;

impl ReadFilter {
    /// Every filter shape, in slot order. A filter that compares values is
    /// named under the raw order; the typed order is the same slot.
    pub fn all() -> [Self; READ_FILTERS] {
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

    /// Whether this filter excludes: its seek reaches the documents a page
    /// must not hold rather than the ones it keeps, so a page is never driven
    /// from it. Inequality and absence exclude; every other filter keeps what
    /// its seek reaches.
    pub fn excludes(self) -> bool {
        matches!(self, Self::NotEqual(_) | Self::Absent)
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
            slot < READ_FILTERS,
            "slot {slot} is outside the enumeration: grow `all` and `READ_FILTERS` with the \
             filter that took it"
        );
        slot
    }
}

/// One filter as a statement spells it: its shape, and the values it binds in
/// the order its fragment numbers them.
#[derive(Clone, Debug)]
pub(crate) struct Filter {
    pub(crate) shape: ReadFilter,
    /// The values the fragment binds, in the order [`Filter::spell`] writes
    /// their placeholders. A resolution filter binds two per suffix range, so
    /// the count is also how many ranges it opens.
    pub(crate) values: Vec<Value>,
}

impl Filter {
    /// This filter's fragment: a membership test of `id`, the column holding
    /// the page's document id, its placeholders numbered by `binder`.
    pub(crate) fn spell(&self, id: &str, binder: &mut Binder) -> String {
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
            ReadFilter::Equal(order) | ReadFilter::NotEqual(order) => {
                let column = order.column();
                let (key, value) = (next(), next());
                let membership = if matches!(self.shape, ReadFilter::Equal(_)) {
                    "IN"
                } else {
                    "NOT IN"
                };
                format!(
                    "{id} {membership} (SELECT fv.document FROM document_fields AS fv
                     WHERE fv.key = {key} AND fv.{column} = {value})"
                )
            }
            ReadFilter::Member(order) => {
                let column = order.column();
                let (key, values) = (next(), next());
                format!(
                    "{id} IN (SELECT fv.document FROM document_fields AS fv
                     WHERE fv.key = {key}
                       AND fv.{column} IN (SELECT value FROM json_each({values})))"
                )
            }
            ReadFilter::Present | ReadFilter::Absent => {
                let key = next();
                let membership = if self.shape == ReadFilter::Present {
                    "IN"
                } else {
                    "NOT IN"
                };
                format!(
                    "{id} {membership} (SELECT fp.document FROM document_fields AS fp
                     WHERE fp.key = {key} AND fp.ordinal = 0)"
                )
            }
            ReadFilter::Before(order) => bounded(order, "<", &mut next),
            ReadFilter::After(order) => bounded(order, ">", &mut next),
            ReadFilter::FullText => {
                let query = next();
                format!(
                    "{id} IN (SELECT rowid FROM documents_fts WHERE documents_fts MATCH {query})"
                )
            }
            ReadFilter::PathGlob => {
                let (lower, upper, pattern) = (next(), next(), next());
                format!(
                    "{id} IN (SELECT dg.id FROM documents AS dg
                     WHERE dg.path >= {lower} AND dg.path < {upper}
                       AND {function}({pattern}, dg.path))",
                    function = super::glob::GLOB_FUNCTION
                )
            }
            ReadFilter::Resolves => {
                // Each bound takes its number in turn, and the range predicate
                // spells the ranges over those numbers from `first`.
                for _ in &self.values {
                    next();
                }
                let ranges = range_predicate_from("dr.suffix_key", self.values.len() / 2, first);
                format!("{id} IN (SELECT dr.id FROM documents AS dr WHERE {ranges})")
            }
            ReadFilter::Tag => {
                let name = next();
                format!(
                    "{id} IN (SELECT tg.document FROM document_tags AS tg WHERE tg.name = {name})"
                )
            }
            ReadFilter::Finding => {
                let (fingerprint, kind) = (next(), next());
                format!(
                    "{id} IN (SELECT df.id FROM documents AS df
                     WHERE df.path IN (SELECT fg.path FROM findings AS fg
                         WHERE fg.vault_schema_fingerprint = {fingerprint}
                           AND fg.kind = {kind}))"
                )
            }
        }
    }
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
