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
//! key the root selects, and the exclusion — and this module holds the one
//! spelling of each class predicate in SQL — a range of the probed key, the
//! exclusion, and a link key's class or path — so no two surfaces can disagree
//! about a class.
//!
//! **The class keys are a dormant carrier.** [`TargetClass::class_keys`] is the
//! set a finding about a target is filed under, and its consumer is the
//! link-health findings the link-health unit of Layer 3 files: a producer that
//! reads a link's target, reads its class here, and files what it found under
//! these keys. The current call graph does not reach it, because no producer
//! files a link-health finding yet — every finding the host files is about its
//! own subject and belongs to no class.
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
//! keeps its case in a glob as it does in a key. The path parts of a find, a
//! count and a validate, and a tag facet's patterns, match the same grammar
//! bytewise on every root.

use std::collections::BTreeSet;

use norn_db::rusqlite::functions::FunctionFlags;
use norn_db::rusqlite::types::{Value, ValueRef};
use norn_db::rusqlite::{self, Connection};
use norn_wire::{CaseFold, Pattern};

use crate::error::{self, StoreError};
use crate::facts::StoredPathOrder;
use crate::path::{ClassKey, SuffixKey, SuffixProbe, prefix_successor, suffix_probe};
use crate::request::range_predicate_from;

/// The name a statement calls the exclusion by:
/// `norn_ambiguity_admits(ignored, target_segments, path_order, path)`.
pub(crate) const ADMITS_FUNCTION: &str = "norn_ambiguity_admits";

/// The name a statement calls the prefix step by: `norn_key_successor(key)`,
/// the least text after every text a separator-terminated suffix key opens,
/// which is [`crate::path::prefix_successor`] itself.
pub(crate) const SUCCESSOR_FUNCTION: &str = "norn_key_successor";

/// How many values [`TargetClass::parameters`] numbers after the ranges'
/// bounds: the ignore set, the target's segment count, and the path order.
pub(crate) const EXCLUSION_PARAMETERS: usize = 3;

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
        let Some(ignored) = self.ignored_place(path, glob_case(order)) else {
            return true;
        };
        let depth = path.split(SEPARATOR).count();
        let from_the_place_to_the_leaf = depth - ignored + 1;
        target_segments >= from_the_place_to_the_leaf && (target_segments > 1 || depth == 1)
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

    /// The set as one text value a statement binds: each glob's byte length,
    /// a colon, and the glob. Length-prefixed, so a glob may hold any character
    /// and the encoding still reads back one way.
    pub(crate) fn encoded(&self) -> String {
        self.patterns
            .iter()
            .map(|pattern| format!("{}:{}", pattern.as_str().len(), pattern.as_str()))
            .collect()
    }

    /// Read back what [`AmbiguityIgnore::encoded`] wrote.
    fn decoded(mut text: &str) -> Result<Self, String> {
        let mut patterns = Vec::new();
        while !text.is_empty() {
            let (length, rest) = text
                .split_once(':')
                .ok_or_else(|| format!("an ignore set lacks a length before `{text}`"))?;
            let length: usize = length
                .parse()
                .map_err(|problem| format!("an ignore set's length `{length}`: {problem}"))?;
            let source = rest
                .get(..length)
                .ok_or_else(|| format!("an ignore set's glob is shorter than {length}"))?;
            patterns.push(Pattern::parse(source).map_err(|problem| problem.to_string())?);
            text = &rest[length..];
        }
        Ok(AmbiguityIgnore { patterns })
    }
}

/// The case an ignore glob matches under on a root whose path order is `order`.
fn glob_case(order: StoredPathOrder) -> CaseFold {
    match order {
        StoredPathOrder::Sensitive => CaseFold::Exact,
        StoredPathOrder::AsciiCaseInsensitive => CaseFold::Ascii,
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

    /// The values [`predicate`] numbers, in its order: each range's bounds,
    /// then the ignore set, the target's segment count, and the recorded
    /// spelling of the path order the ignore set is matched under.
    pub(crate) fn parameters(&self) -> Vec<Value> {
        self.probe
            .ranges()
            .flat_map(|(lower, upper)| {
                [
                    Value::Text(lower.to_string()),
                    Value::Text(upper.to_string()),
                ]
            })
            .chain([
                Value::Text(self.ignore.encoded()),
                Value::Integer(self.target_segments as i64),
                Value::Text(self.order.as_str().to_string()),
            ])
            .collect()
    }
}

/// The predicate a resolution of `ranges` ranges over `key` spells against the
/// `documents` rows a statement calls `alias`, its values numbered from
/// `first` in [`TargetClass::parameters`]'s order.
///
/// Each range is a seek of the key's own index; the ignore set is a test of
/// the rows those seeks reached, so it narrows a class without widening what
/// is read.
pub(crate) fn predicate(alias: &str, key: SuffixKey, ranges: usize, first: usize) -> String {
    let column = format!("{alias}.{}", key.column());
    let seeks = range_predicate_from(&column, ranges, first);
    let ignored = first + ranges * 2;
    let segments = ignored + 1;
    let order = segments + 1;
    format!(
        "({seeks}) AND {}",
        admits(
            &format!("?{ignored}"),
            &format!("?{segments}"),
            &format!("?{order}"),
            &format!("{alias}.path")
        )
    )
}

/// The exclusion, spelled over its four arguments: whether the document at
/// `path` stays in the class of a target of `segments` segments under the
/// ignore set `ignored` and the path order `order`.
pub(crate) fn admits(ignored: &str, segments: &str, order: &str, path: &str) -> String {
    format!("{ADMITS_FUNCTION}({ignored}, {segments}, {order}, {path})")
}

/// The rows of `documents` a statement calls `alias` in the one range from
/// `lower` to `upper` of the key the root probes, less the places the ignore
/// set `ignored` excludes from a class of a target of `segments` segments
/// under `order`: one class range, each bound an expression the caller names.
pub(crate) fn range_class(
    alias: &str,
    key: SuffixKey,
    (lower, upper): (&str, &str),
    segments: &str,
    ignored: &str,
    order: &str,
) -> String {
    let column = format!("{alias}.{}", key.column());
    format!(
        "{column} >= {lower} AND {column} < {upper} AND {}",
        admits(ignored, segments, order, &format!("{alias}.path"))
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

/// The rows of `documents` a statement calls `alias` in the class a link's
/// suffix key `link_key` — an expression holding the key in the space `key`
/// selects — opens, less the places the ignore set `ignored` excludes for a
/// target of `segments` segments under `order`. The range's upper bound is the
/// key's [`SUCCESSOR_FUNCTION`], so the range is the one the link's own probe
/// opens.
pub(crate) fn link_key_class(
    alias: &str,
    key: SuffixKey,
    link_key: &str,
    segments: &str,
    ignored: &str,
    order: &str,
) -> String {
    range_class(
        alias,
        key,
        (link_key, &format!("{SUCCESSOR_FUNCTION}({link_key})")),
        segments,
        ignored,
        order,
    )
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
        "FROM documents AS dr WHERE {}",
        predicate("dr", class.probe().key(), class.probe().range_count(), 1)
    )
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
pub(crate) fn ladder(key: &str, path: &str) -> String {
    format!("{key}, {path}")
}

/// Register the exclusion and the prefix step on `connection`, so every
/// statement a resolution spells can call them: the writer's and every read
/// snapshot's alike.
///
/// **Deterministic**, both, and registered as such: the exclusion's answer is
/// a function of its four arguments, the path order among them, so the case
/// the globs match under is the statement's input rather than the
/// connection's state, and the step's is a function of its one key. The
/// ignore set is decoded once per statement and kept as the call's auxiliary
/// data for the rows after the first.
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
    connection
        .create_scalar_function(
            ADMITS_FUNCTION,
            4,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            |context| {
                let ignore = context.get_or_create_aux(0, |encoded: ValueRef<'_>| {
                    let encoded = encoded
                        .as_str()
                        .map_err(|problem| rusqlite::Error::UserFunctionError(Box::new(problem)))?;
                    AmbiguityIgnore::decoded(encoded)
                        .map_err(|problem| rusqlite::Error::UserFunctionError(problem.into()))
                })?;
                let segments: i64 = context.get(1)?;
                let recorded = context
                    .get_raw(2)
                    .as_str()
                    .map_err(|problem| rusqlite::Error::UserFunctionError(Box::new(problem)))?;
                let order = StoredPathOrder::from_recorded(recorded).ok_or_else(|| {
                    rusqlite::Error::UserFunctionError(
                        format!("`{recorded}` is no path order").into(),
                    )
                })?;
                let path = context
                    .get_raw(3)
                    .as_str()
                    .map_err(|problem| rusqlite::Error::UserFunctionError(Box::new(problem)))?;
                Ok(ignore.admits(path, usize::try_from(segments).unwrap_or(0), order))
            },
        )
        .map_err(|problem| error::sql("registering the ambiguity-ignore function", problem))
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

    #[test]
    fn an_encoded_set_reads_back_whole() {
        let set = ignoring(&["archive/**", "a:b/*", "7:x"]);
        assert_eq!(AmbiguityIgnore::decoded(&set.encoded()), Ok(set));
        // A glob whose bytes outnumber its characters: the prefix counts
        // bytes, which is what the decoder slices by.
        let wide = ignoring(&["archivé/**", "日記/*", "x"]);
        assert_eq!(AmbiguityIgnore::decoded(&wide.encoded()), Ok(wide));
        assert_eq!(
            AmbiguityIgnore::decoded(""),
            Ok(AmbiguityIgnore::none()),
            "the empty set encodes as nothing"
        );
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
