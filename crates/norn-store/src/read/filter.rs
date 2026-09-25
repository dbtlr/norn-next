//! The filters a conjunction's parts spell, named, and the binder every
//! read builder's composer numbers its parameters with.

use norn_db::rusqlite::types::Value;

use super::FieldOrder;
use crate::path::SuffixKey;
use crate::resolve;

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
    /// The document is in the class a resolution target opens on the root:
    /// the target's suffix ranges on `documents_suffix_key` where the root
    /// tells spellings apart, and on `documents_folded_suffix_key` where it
    /// folds ASCII case, less the places the schema ignores.
    Resolves(SuffixKey),
    /// The document carries the tag, on `document_tags_name`.
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
    /// fingerprint: one covering seek of the findings at `(fingerprint,
    /// kind)`, which `findings_fingerprint_kind_severity` and
    /// `findings_vault_schema_fingerprint` both lead with and both carry the
    /// path in, each finding's path read back to its document on
    /// `documents_path`.
    Finding,
}

/// How many filter shapes [`ReadFilter::all`] holds.
pub const READ_FILTERS: usize = 13;

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

/// One filter as a statement spells it: its shape, and the values it binds in
/// the order its fragment numbers them.
#[derive(Clone, Debug)]
pub(crate) struct Filter {
    pub(crate) shape: ReadFilter,
    /// The values the fragment binds, in the order [`Filter::spell`] writes
    /// their placeholders. A resolution filter binds two per suffix range and
    /// [`crate::resolve::EXCLUSION_PARAMETERS`] more — the ignore set, the
    /// target's segment count and the path order — so the count also says how
    /// many ranges it opens. A links-to filter binds each key it seeks and
    /// [`LINKS_TO_PARAMETERS`] more.
    pub(crate) values: Vec<Value>,
}

impl Filter {
    /// A path part's range and glob, as the values it binds — the range's
    /// lower bound, its upper bound, and the pattern — and `None` for any
    /// other part.
    pub(crate) fn path_glob(&self) -> Option<(&Value, &Value, &Value)> {
        match (self.shape, self.values.as_slice()) {
            (ReadFilter::PathGlob, [lower, upper, pattern]) => Some((lower, upper, pattern)),
            _ => None,
        }
    }

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
                       AND {})",
                    glob_test(&pattern, "dg.path")
                )
            }
            ReadFilter::Resolves(key) => {
                // Each value takes its number in turn, and the resolution's
                // predicate spells its ranges and its exclusion over those
                // numbers from `first`: two bounds per range, then the ignore
                // set, the target's segment count and the path order.
                for _ in &self.values {
                    next();
                }
                let ranges = (self.values.len() - resolve::EXCLUSION_PARAMETERS) / 2;
                format!(
                    "{id} IN (SELECT dr.id FROM documents AS dr WHERE {})",
                    resolve::predicate("dr", key, ranges, first)
                )
            }
            ReadFilter::Tag => {
                let name = next();
                format!(
                    "{id} IN (SELECT tg.document FROM document_tags AS tg WHERE tg.name = {name})"
                )
            }
            ReadFilter::LinksTo(key) => {
                let listed: Vec<String> = (0..self.values.len() - LINKS_TO_PARAMETERS)
                    .map(|_| next())
                    .collect();
                let (ignored, order, path, document) = (next(), next(), next(), next());
                let link_key = resolve::link_key_column(key);
                format!(
                    "{id} IN (SELECT lk.document FROM link_keys AS lk
                     WHERE lk.{link_key} IN ({listed})
                       AND (lk.segments IS NULL OR {named_admitted})
                       AND NOT EXISTS (SELECT 1 FROM link_keys AS lo, documents AS dl
                           WHERE lo.link = lk.link AND lo.segments IS NOT NULL
                             AND {other_in_class}
                             AND dl.id <> {document})
                       AND NOT EXISTS (SELECT 1 FROM link_keys AS lp, documents AS dp
                           WHERE lp.link = lk.link AND lp.segments IS NULL
                             AND {other_at_path} AND dp.id <> {document}))",
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

/// How many values a links-to filter binds after the keys it seeks: the
/// ignore set, the path order it is matched under, and the named document's
/// path and row id.
pub(crate) const LINKS_TO_PARAMETERS: usize = 4;

/// The test that `path`, a column holding a path, matches the glob bound at
/// `pattern`: the one spelling of a glob match every statement runs.
pub(crate) fn glob_test(pattern: &str, path: &str) -> String {
    format!("{}({pattern}, {path})", super::glob::GLOB_FUNCTION)
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
