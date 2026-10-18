//! The one resolver: a written target, the case behaviour its vault root
//! proved, and the places the vault's schema keeps out of ambiguity classes,
//! compiled into the class of documents the target names.
//!
//! Every surface that asks which documents a target names asks here: a find's
//! `resolves` part, a read of a class's candidates — a get's target, a
//! links-to part's, and each candidate's minimal disambiguating suffix — and
//! every read of what a stored link names, which reads the class or the path
//! each of the link's keys opens: a row's links, and a links-to part's
//! confirmation that a link names one document alone. What a [`TargetClass`]
//! carries is what each of them needs and nothing more — the probe, over the
//! key the root selects, the target's segment count and the ignore set for
//! in-memory admission — and this module holds the one spelling of each class
//! predicate in SQL: admitted-count runs over the probed key's ranges, and a
//! link key's class or path, so no two surfaces can disagree
//! about a class.
//!
//! [`TargetClass::class_keys`] spells the class keys a target is read
//! through. The link-health judgment ([`crate::health`]) keys a link's
//! findings by the link's stored `link_keys` rows, not by this set, and
//! nothing outside this module's tests calls [`TargetClass::class_keys`].
//!
//! # Case is the root's
//!
//! A root that tells spellings apart reads the raw suffix key and never
//! consults the folded one. A root that folds ASCII case reads the folded key,
//! and there every document whose folded key the target's folded reduction
//! prefixes is one class: an exact-case match takes no precedence, because the
//! root itself does not tell the two spellings apart. The fold is ASCII alone,
//! so a non-ASCII letter keeps its case under either — see
//! [`crate::path::SuffixKey`].
//!
//! # Ignored places
//!
//! A vault's schema may name globs whose places stay out of ambiguity classes
//! ([`AmbiguityIgnore`]). A place is **under** an ignore glob when the glob
//! matches the place or one of its segment-aligned ancestors; the shallowest
//! such spelling is the **ignored place**. A candidate under one is excluded
//! from a target's class unless the target names the ignored place: the
//! segments it spells reach the ignored place's last segment. A one-segment
//! target spells a stem alone and names no place below the root, while a
//! document at the root is its whole place and its own name names it. So with
//! `archive/**` ignored, `glossary` and `norn/glossary` both pass over
//! `archive/norn/glossary.md`, and `archive/norn/glossary` resolves to it; with
//! `glossary.md` ignored, `glossary` still resolves to `glossary.md`.
//!
//! The globs match under the order the class is read under, the store's
//! recorded path order: where it folds ASCII case a glob matches with ASCII
//! case folded, so `archive/**` ignores `Archive/notes.md` there as the root
//! itself does not tell the two apart; where it tells spellings apart a glob
//! matches bytes. The fold is [`CaseFold::Ascii`], so a letter outside ASCII
//! keeps its case in a glob as it does in a key. It is the one fold every glob
//! takes from the order ([`StoredPathOrder::glob_case`]), so an ignore glob and
//! a find's path part written alike name the same places on any root.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use norn_db::rusqlite::functions::FunctionFlags;
use norn_db::rusqlite::types::Value;
use norn_db::rusqlite::{self, Connection};
use norn_wire::{CaseFold, Pattern};

use crate::error::{self, StoreError};
use crate::facts::StoredPathOrder;
use crate::path::{ClassKey, SuffixKey, SuffixProbe, prefix_successor, suffix_probe};
use crate::request::range_predicate_from;

/// The name a statement calls the prefix step by: `norn_key_successor(key)`,
/// the least text after every text a separator-terminated suffix key opens,
/// which is [`crate::path::prefix_successor`] itself.
pub(crate) const SUCCESSOR_FUNCTION: &str = "norn_key_successor";

/// The separator between segments, in a target and in a path alike.
const SEPARATOR: char = '/';

/// The globs a vault's schema keeps out of ambiguity classes.
///
/// The store reads no schema: the host declares each glob on the
/// [`crate::ContentModel`] it derives from the schema it pinned, which holds
/// the set once, so the set a read applies is the set of the schema the
/// snapshot pins, and the set `describe` reports as path rules is that same
/// set.
///
/// A glob is held once, by its text, and the set is kept in the byte order of
/// that text: a glob written twice is the same text twice and ignores nothing
/// more.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AmbiguityIgnore {
    patterns: Vec<Pattern>,
}

impl AmbiguityIgnore {
    /// The set that ignores nothing.
    pub fn none() -> Self {
        Self::default()
    }

    /// The set these globs name.
    pub fn new(patterns: impl IntoIterator<Item = Pattern>) -> Self {
        patterns
            .into_iter()
            .fold(Self::none(), AmbiguityIgnore::with)
    }

    /// The same set, also ignoring the places `pattern` names. A glob already
    /// held is the glob already held.
    pub fn with(mut self, pattern: Pattern) -> Self {
        if let Err(at) = self
            .patterns
            .binary_search_by(|held| held.as_str().cmp(pattern.as_str()))
        {
            self.patterns.insert(at, pattern);
        }
        self
    }

    /// The globs, each once, in the byte order of their text.
    pub fn patterns(&self) -> &[Pattern] {
        &self.patterns
    }

    /// The globs whose text is after `after`, or every glob where it is
    /// `None`, in the byte order of their text.
    pub(crate) fn after<'a>(&'a self, after: Option<&str>) -> impl Iterator<Item = &'a Pattern> {
        let from = after.map_or(0, |after| {
            self.patterns
                .partition_point(|pattern| pattern.as_str() <= after)
        });
        self.patterns[from..].iter()
    }

    /// Whether `path` stays in the class of a target of `target_segments`
    /// segments, on a root whose path order is `order`.
    ///
    /// A path under no glob always stays. A path under one stays only where the
    /// target names its ignored place: the segments the target spells — the
    /// last `target_segments` of the path — run from the ignored place's last
    /// segment down to the leaf, so the target spells at least as many
    /// segments as there are from that place to the leaf. A one-segment target
    /// spells the stem alone, which names no place below the root; a document
    /// at the root is its whole place, so its own name names it.
    ///
    /// The globs match with ASCII case folded where `order` folds it, and
    /// bytewise where it does not.
    pub fn admits(&self, path: &str, target_segments: usize, order: StoredPathOrder) -> bool {
        target_segments >= self.admitting_segments(path, order)
    }

    /// The fewest segments a target spells that keep `path` in its class, on
    /// a root whose path order is `order`: [`AmbiguityIgnore::admits`] holds
    /// exactly for a target of at least this many segments.
    ///
    /// One for a path under no glob, and for a document at the root, whose
    /// own name names its whole place. Otherwise the segments from the ignored
    /// place down to the leaf, and never fewer than two, because a
    /// one-segment target spells a stem alone and names no place below the
    /// root.
    ///
    /// A store writes it beside each document it derives
    /// (`documents.admitting_segments`), so every class read seeks the
    /// members its class admits rather than stepping past the ones it keeps
    /// out.
    pub fn admitting_segments(&self, path: &str, order: StoredPathOrder) -> usize {
        let Some(ignored) = self.ignored_place(path, order.glob_case()) else {
            return 1;
        };
        let depth = path.split(SEPARATOR).count();
        if depth == 1 {
            return 1;
        }
        let from_the_place_to_the_leaf = depth - ignored + 1;
        from_the_place_to_the_leaf.max(2)
    }

    /// How many segments the shallowest spelling of `path` or one of its
    /// ancestors an ignore glob matches has, or `None` where no glob matches
    /// any of them, the globs matching under `case`.
    fn ignored_place(&self, path: &str, case: CaseFold) -> Option<usize> {
        if self.patterns.is_empty() {
            return None;
        }
        let ends = path
            .match_indices(SEPARATOR)
            .map(|(at, _)| at)
            .chain(std::iter::once(path.len()));
        ends.enumerate()
            .find(|(_, end)| {
                let place = &path[..*end];
                self.patterns
                    .iter()
                    .any(|pattern| pattern.matches(place, case))
            })
            .map(|(index, _)| index + 1)
    }
}

/// A target compiled for one root: the probe over the key the root probes, how
/// many segments the target spells, the places the root's schema ignores, and
/// the path order those places are matched under.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetClass {
    probe: SuffixProbe,
    target_segments: usize,
    ignore: AmbiguityIgnore,
    order: StoredPathOrder,
}

impl TargetClass {
    /// Compile `target` — a suffix address, with any `#` anchor already split
    /// off — for a root whose proven case behaviour is `order`, excluding the
    /// places `ignore` names.
    ///
    /// Crate-private, because the order is the store's: a class is compiled
    /// by [`crate::Request::target_class`] under the order the store records,
    /// or by a find under the order its snapshot reads, and never under an
    /// order a caller names.
    ///
    /// The refusals are [`suffix_probe`]'s: a spelling that is not a suffix
    /// address names no class under any root.
    pub(crate) fn compile(
        target: &str,
        order: StoredPathOrder,
        ignore: &AmbiguityIgnore,
    ) -> Result<Self, StoreError> {
        Ok(TargetClass {
            probe: suffix_probe(target)?.in_space(SuffixKey::under(order)),
            target_segments: target.split(SEPARATOR).count(),
            ignore: ignore.clone(),
            order,
        })
    }

    /// The path order the class was compiled under, which is the order of the
    /// store it was compiled for.
    pub(crate) fn order(&self) -> StoredPathOrder {
        self.order
    }

    /// The probe the class is read through.
    pub fn probe(&self) -> &SuffixProbe {
        &self.probe
    }

    /// The ambiguity classes a finding about this target belongs to, in the
    /// key space the root probes.
    pub fn class_keys(&self) -> BTreeSet<ClassKey> {
        self.probe.class_keys()
    }

    /// Whether a document at `path` whose key this probe's ranges hold is in
    /// the class: the ignore set is the one test the ranges do not make.
    pub fn admits(&self, path: &str) -> bool {
        self.ignore.admits(path, self.target_segments, self.order)
    }

    /// Each range's bounds, then the target's segment count.
    pub(crate) fn parameters(&self) -> Vec<Value> {
        self.probe
            .ranges()
            .flat_map(|(lower, upper)| {
                [
                    Value::Text(lower.to_string()),
                    Value::Text(upper.to_string()),
                ]
            })
            .chain([Value::Integer(self.target_segments as i64)])
            .collect()
    }
}

/// Splitting a separator-terminated suffix key yields one row per segment
/// plus one trailing empty element. Every probe lower bound and stored link
/// key spells exactly its target's segments, with `/` after each segment.
/// The driver's `n.key` is zero-based, so the admitting count is `n.key + 1`;
/// callers keep `n.key < segments`. JSON quoting keeps arbitrary key
/// characters intact.
pub(crate) fn admitting_counts(key: &str) -> String {
    format!(r#"json_each('[' || replace(json_quote({key}), '/', '","') || ']')"#)
}

/// A row from [`admitting_counts`] supplies its one-based admitting count.
pub(crate) fn admitting_count(alias: &str) -> String {
    format!("(CAST({alias}.key AS INTEGER) + 1)")
}

/// A driver's row is in range when its count is at most the target's segments.
pub(crate) fn admitting_count_within(alias: &str, segments: &str) -> String {
    format!("CAST({alias}.key AS INTEGER) < {segments}")
}

/// Each count's rows stand together in one run of the admitted index. The
/// count equality and key bounds seek that run in ladder order.
pub(crate) fn range_admitted(
    alias: &str,
    key: SuffixKey,
    (lower, upper): (&str, &str),
    admitting: &str,
) -> String {
    let column = format!("{alias}.{}", key.column());
    format!(
        "{alias}.admitting_segments = {admitting} AND {column} >= {lower} AND {column} < {upper}"
    )
}

/// The admitted rows of one range. A head cuts each run in ladder order before
/// merging, so neither the merge nor its sort ever reads a whole class.
pub(crate) fn range_rows(
    key: SuffixKey,
    bounds: (&str, &str),
    segments: &str,
    head: Option<&str>,
) -> String {
    let counts = admitting_counts(bounds.0);
    let admitted = range_admitted("dl", key, bounds, &admitting_count("n"));
    let within = admitting_count_within("n", segments);
    let rung = key.column();
    let rows = if let Some(head) = head {
        let order = ladder(&format!("dl.{rung}"), "dl.path");
        format!(
            "da.id IN (SELECT dl.id FROM documents AS dl WHERE {admitted}
                           ORDER BY {order} LIMIT {head})"
        )
    } else {
        range_admitted("da", key, bounds, &admitting_count("n"))
    };
    let cut = head
        .map(|head| {
            format!(
                " ORDER BY {} LIMIT {head}",
                ladder(&format!("da.{rung}"), "da.path")
            )
        })
        .unwrap_or_default();
    format!(
        "SELECT da.id, da.path, da.{rung} FROM {counts} AS n
             CROSS JOIN documents AS da WHERE {within} AND {rows}{cut}"
    )
}

/// A class range's count is the sum of its disjoint admitted-index run counts.
pub(crate) fn range_total(key: SuffixKey, bounds: (&str, &str), segments: &str) -> String {
    let counts = admitting_counts(bounds.0);
    let admitted = range_admitted("dl", key, bounds, &admitting_count("n"));
    let within = admitting_count_within("n", segments);
    format!(
        "SELECT COALESCE(SUM((SELECT COUNT(*) FROM documents AS dl WHERE {admitted})), 0)
             FROM {counts} AS n WHERE {within}"
    )
}

/// Membership tested at one document's row key: its stored count and the
/// target's ranges, numbered in [`TargetClass::parameters`]'s order.
pub(crate) fn predicate(alias: &str, key: SuffixKey, ranges: usize, first: usize) -> String {
    let column = format!("{alias}.{}", key.column());
    let ranges_sql = range_predicate_from(&column, ranges, first);
    let segments = first + ranges * 2;
    format!("({ranges_sql}) AND {alias}.admitting_segments <= ?{segments}")
}

/// Each range's admitted runs, streamed so a caller's ceiling can stop them.
pub(crate) fn target_rows(
    key: SuffixKey,
    ranges: usize,
    first: usize,
    head: Option<&str>,
) -> String {
    let segments = format!("?{}", first + ranges * 2);
    (0..ranges)
        .map(|range| {
            let lower = format!("?{}", first + range * 2);
            let upper = format!("?{}", first + range * 2 + 1);
            format!(
                "SELECT * FROM ({})",
                range_rows(key, (&lower, &upper), &segments, head)
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ")
}

/// The admitted rows of a link key, with the same per-run head bound as a
/// compiled target's ranges where `head` is given.
pub(crate) fn link_key_rows(
    key: SuffixKey,
    link_key: &str,
    segments: &str,
    head: Option<&str>,
) -> String {
    range_rows(
        key,
        (link_key, &format!("{SUCCESSOR_FUNCTION}({link_key})")),
        segments,
        head,
    )
}

/// The sum of the disjoint admitted-index run counts of a link key.
pub(crate) fn link_key_total(key: SuffixKey, link_key: &str, segments: &str) -> String {
    range_total(
        key,
        (link_key, &format!("{SUCCESSOR_FUNCTION}({link_key})")),
        segments,
    )
}

/// Whether the named document's stored count admits it to a link's class.
pub(crate) fn named_admitted(document: &str, segments: &str) -> String {
    format!(
        "(SELECT dn.admitting_segments FROM documents AS dn WHERE dn.id = {document}) <= {segments}"
    )
}

/// The `link_keys` column a read under `key` seeks: the raw key where the root
/// tells spellings apart, and the folded key where it folds ASCII case.
pub(crate) fn link_key_column(key: SuffixKey) -> &'static str {
    match key {
        SuffixKey::Raw => "key",
        SuffixKey::Folded => "folded_key",
    }
}

/// The rows of `documents` a statement calls `alias` standing at a link's path
/// key `link_key` — an expression holding the key in the space `key` selects —
/// under the root's order: bytewise where it tells spellings apart, and with
/// ASCII case folded where it folds, which `documents_path_nocase` seeks.
pub(crate) fn link_key_path(alias: &str, key: SuffixKey, link_key: &str) -> String {
    match key {
        SuffixKey::Raw => format!("{alias}.path = {link_key}"),
        SuffixKey::Folded => format!("{alias}.path = {link_key} COLLATE NOCASE"),
    }
}

/// The rows of `documents` a statement calls `dr` that are in `class`: the
/// `FROM` and `WHERE` clauses every read of a class spells, its values numbered
/// from `?1` in [`TargetClass::parameters`]'s order.
pub(crate) fn class_rows(class: &TargetClass) -> String {
    format!(
        "FROM ({}) AS dr",
        target_rows(class.probe().key(), class.probe().range_count(), 1, None)
    )
}

/// A head bounds each range's count runs before the caller merges and cuts them.
pub(crate) fn class_head_rows(class: &TargetClass, head: &str) -> String {
    format!(
        "FROM ({}) AS dr",
        target_rows(
            class.probe().key(),
            class.probe().range_count(),
            1,
            Some(head)
        )
    )
}

/// The sum of the counts of a target's disjoint ranges and admitting runs.
pub(crate) fn class_total(class: &TargetClass) -> String {
    let segments = format!("?{}", class.probe().range_count() * 2 + 1);
    let totals = (0..class.probe().range_count())
        .map(|range| {
            let lower = format!("?{}", range * 2 + 1);
            let upper = format!("?{}", range * 2 + 2);
            format!(
                "({})",
                range_total(class.probe().key(), (&lower, &upper), &segments)
            )
        })
        .collect::<Vec<_>>()
        .join(" + ");
    format!("SELECT {totals}")
}

/// The resolution ladder's order over the rows [`class_rows`] reads: the
/// probed key, then the path. Total, because equal suffix keys are exactly
/// what an ambiguity class is made of, and a ladder whose ties fell out in
/// row-insertion order would reorder itself when a document is re-derived.
pub(crate) fn ladder_order(class: &TargetClass) -> String {
    format!(
        "ORDER BY {}",
        ladder(&format!("dr.{}", class.probe().key().column()), "dr.path")
    )
}

/// The resolution ladder's order over rows whose probed key is `key` and whose
/// path is `path`, each an expression: the key, then the path.
///
/// [`ladder_cmp`] is the same order over rows read into memory, and the two
/// must agree: a head a statement cuts in this order is merged and re-cut in
/// that one.
pub(crate) fn ladder(key: &str, path: &str) -> String {
    format!("{key}, {path}")
}

/// The resolution ladder's order over two rows read into memory, each its
/// probed key — its rung — and its path: the key, then the path, each
/// compared byte by byte.
///
/// It is the order [`ladder`] spells in SQL. The two agree because every
/// column a rung or a path is read from — `documents.suffix_key`,
/// `documents.folded_suffix_key`, `documents.path` and `link_keys.key` or
/// `link_keys.folded_key` — takes the `BINARY` collation, which compares
/// bytes as Rust compares `str`. A column given another collation, or a
/// ladder over another expression, changes both or neither.
pub(crate) fn ladder_cmp(left: (&str, &str), right: (&str, &str)) -> Ordering {
    left.cmp(&right)
}

/// Register the deterministic prefix step on the writer and every reader.
pub(crate) fn register_functions(connection: &Connection) -> Result<(), StoreError> {
    connection
        .create_scalar_function(
            SUCCESSOR_FUNCTION,
            1,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            |context| {
                let key = context
                    .get_raw(0)
                    .as_str()
                    .map_err(|problem| rusqlite::Error::UserFunctionError(Box::new(problem)))?;
                Ok(prefix_successor(key))
            },
        )
        .map_err(|problem| error::sql("registering the key successor function", problem))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use StoredPathOrder::Sensitive;

    fn ignoring(globs: &[&str]) -> AmbiguityIgnore {
        AmbiguityIgnore::new(
            globs
                .iter()
                .map(|glob| Pattern::parse(glob).expect("a glob")),
        )
    }

    /// A target reaches an ignored place when the segments it spells run up to
    /// the place's last segment, and not one segment short of it; a
    /// one-segment target spells the stem alone and reaches no ignored place
    /// below the root.
    #[test]
    fn a_target_reaches_an_ignored_place_from_its_last_segment_down() {
        let nested = ignoring(&["**/drafts/**"]);
        assert!(nested.admits("notes/drafts/x.md", 2, Sensitive));
        assert!(!nested.admits("notes/drafts/x.md", 1, Sensitive));
        let deeper = ignoring(&["archive/**"]);
        assert!(deeper.admits("archive/norn/glossary.md", 3, Sensitive));
        assert!(!deeper.admits("archive/norn/glossary.md", 2, Sensitive));
        let leaf = ignoring(&["attachments/*"]);
        assert!(leaf.admits("attachments/image.md", 2, Sensitive));
        assert!(!leaf.admits("attachments/image.md", 1, Sensitive));
    }

    /// **A document at the root that a glob ignores is reached by its own
    /// name.** The place is the document, and a one-segment target naming it
    /// names the whole place, so it resolves.
    #[test]
    fn a_root_level_ignored_document_is_reached_by_its_own_name() {
        let root = ignoring(&["glossary.md"]);
        assert!(root.admits("glossary.md", 1, Sensitive));
        assert!(
            root.admits("docs/glossary.md", 1, Sensitive),
            "a path no glob matches"
        );
    }

    /// **The admitting count is the shortest target that keeps a place in
    /// its class**: one for a place under no glob and for an ignored document
    /// at the root, the segments from the ignored place to the leaf below it,
    /// and two at least for an ignored place below the root.
    #[test]
    fn a_place_is_admitted_from_the_fewest_segments_that_name_it() {
        let set = ignoring(&["archive/**", "attachments/*", "glossary.md"]);
        for (path, segments) in [
            ("notes/glossary.md", 1),
            ("glossary.md", 1),
            ("archive/norn/glossary.md", 3),
            ("archive/glossary.md", 2),
            ("attachments/image.md", 2),
        ] {
            assert_eq!(set.admitting_segments(path, Sensitive), segments, "{path}");
            assert!(set.admits(path, segments, Sensitive), "{path}");
            assert!(!set.admits(path, segments - 1, Sensitive), "{path}");
        }
    }

    #[test]
    fn a_place_is_under_the_shallowest_spelling_a_glob_matches() {
        let set = ignoring(&["archive/**", "attachments/*", "**/drafts/**"]);
        for case in [CaseFold::Exact, CaseFold::Ascii] {
            assert_eq!(set.ignored_place("archive/deep/glossary.md", case), Some(1));
            assert_eq!(set.ignored_place("attachments/image.md", case), Some(2));
            assert_eq!(set.ignored_place("notes/drafts/x.md", case), Some(2));
            assert_eq!(set.ignored_place("notes/glossary.md", case), None);
        }
        assert_eq!(
            set.ignored_place("Archive/glossary.md", CaseFold::Exact),
            None
        );
        assert_eq!(
            set.ignored_place("Archive/glossary.md", CaseFold::Ascii),
            Some(1)
        );
        assert_eq!(
            set.ignored_place("notes/Drafts/x.md", CaseFold::Ascii),
            Some(2)
        );
    }
}
