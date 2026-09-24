//! The one resolver: a written target, the case behaviour its vault root
//! proved, and the places the vault's schema keeps out of ambiguity classes,
//! compiled into the class of documents the target names.
//!
//! Every surface that asks which documents a target names asks here: a find's
//! `resolves` part, a read of a class's candidates, and a finding's producer
//! reading the class it files a finding about. What a [`Resolution`] carries is
//! what each of them needs and nothing more — the probe, over the key the root
//! selects, and the exclusion — and one spelling of both in SQL, so no two
//! surfaces can disagree about a class.
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
//! from a target's class unless the target names the ignored place: a
//! one-segment target never does, and a longer one does when the segments it
//! spells reach the ignored place's last segment. So with `archive/**`
//! ignored, `glossary` and `norn/glossary` both pass over
//! `archive/norn/glossary.md`, and `archive/norn/glossary` resolves to it.
//!
//! The globs are matched as [`Pattern`] matches, bytewise, which is how a
//! find's path part matches the same grammar.

use std::collections::BTreeSet;

use norn_db::rusqlite::functions::FunctionFlags;
use norn_db::rusqlite::types::{Value, ValueRef};
use norn_db::rusqlite::{self, Connection};
use norn_wire::Pattern;

use crate::error::{self, StoreError};
use crate::facts::StoredPathOrder;
use crate::path::{ClassKey, SuffixKey, SuffixProbe, suffix_probe};
use crate::request::range_predicate_from;

/// The name a statement calls the exclusion by:
/// `norn_ambiguity_admits(ignored, target_segments, path)`.
pub(crate) const ADMITS_FUNCTION: &str = "norn_ambiguity_admits";

/// The separator between segments, in a target and in a path alike.
const SEPARATOR: char = '/';

/// The globs a vault's schema keeps out of ambiguity classes.
///
/// The store reads no schema: the host hands this over from the schema it
/// pinned, beside the declared fields, so the set a read applies is the set of
/// the schema the snapshot pins.
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
        AmbiguityIgnore {
            patterns: patterns.into_iter().collect(),
        }
    }

    /// The globs, in the order they were declared.
    pub fn patterns(&self) -> &[Pattern] {
        &self.patterns
    }

    /// Whether `path` stays in the class of a target of `target_segments`
    /// segments.
    ///
    /// A path under no glob always stays. A path under one stays only where the
    /// target names its ignored place: the target is longer than one segment,
    /// and the segments it spells — the last `target_segments` of the path —
    /// reach the ignored place's last segment.
    pub fn admits(&self, path: &str, target_segments: usize) -> bool {
        let Some(ignored) = self.ignored_place(path) else {
            return true;
        };
        let depth = path.split(SEPARATOR).count();
        target_segments > 1 && ignored + target_segments > depth
    }

    /// How many segments the shallowest spelling of `path` or one of its
    /// ancestors an ignore glob matches has, or `None` where no glob matches
    /// any of them.
    fn ignored_place(&self, path: &str) -> Option<usize> {
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
                self.patterns.iter().any(|pattern| pattern.matches(place))
            })
            .map(|(index, _)| index + 1)
    }

    /// The set as one text value a statement binds: each glob's byte length,
    /// a colon, and the glob. Length-prefixed, so a glob may hold any character
    /// and the encoding still reads back one way.
    fn encoded(&self) -> String {
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

/// A target compiled for one root: the probe over the key the root probes, how
/// many segments the target spells, and the places the root's schema ignores.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resolution {
    probe: SuffixProbe,
    target_segments: usize,
    ignore: AmbiguityIgnore,
}

impl Resolution {
    /// Compile `target` — a suffix address, with any `#` anchor already split
    /// off — for a root whose proven case behaviour is `order`, excluding the
    /// places `ignore` names.
    ///
    /// The refusals are [`suffix_probe`]'s: a spelling that is not a suffix
    /// address names no class under any root.
    pub fn new(
        target: &str,
        order: StoredPathOrder,
        ignore: &AmbiguityIgnore,
    ) -> Result<Self, StoreError> {
        let raw = suffix_probe(target)?;
        let probe = match SuffixKey::under(order) {
            SuffixKey::Raw => raw,
            SuffixKey::Folded => raw.folded(),
        };
        Ok(Resolution {
            probe,
            target_segments: target.split(SEPARATOR).count(),
            ignore: ignore.clone(),
        })
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
        self.ignore.admits(path, self.target_segments)
    }

    /// The values [`predicate`] numbers, in its order: each range's bounds,
    /// then the ignore set and the target's segment count.
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
            ])
            .collect()
    }
}

impl From<SuffixProbe> for Resolution {
    /// The class a probe opens, with no place ignored: the whole class a
    /// maintenance read ranges over.
    fn from(probe: SuffixProbe) -> Self {
        Resolution {
            probe,
            target_segments: 1,
            ignore: AmbiguityIgnore::none(),
        }
    }
}

/// The predicate a resolution of `ranges` ranges over `key` spells against the
/// `documents` rows a statement calls `alias`, its values numbered from
/// `first` in [`Resolution::parameters`]'s order.
///
/// Each range is a seek of the key's own index; the ignore set is a test of
/// the rows those seeks reached, so it narrows a class without widening what
/// is read.
pub(crate) fn predicate(alias: &str, key: SuffixKey, ranges: usize, first: usize) -> String {
    let column = format!("{alias}.{}", key.column());
    let seeks = range_predicate_from(&column, ranges, first);
    let ignored = first + ranges * 2;
    let segments = ignored + 1;
    format!("({seeks}) AND {ADMITS_FUNCTION}(?{ignored}, ?{segments}, {alias}.path)")
}

/// Register the exclusion on `connection`, so every statement a resolution
/// spells can call it: the writer's and every read snapshot's alike.
///
/// **Deterministic**, and registered as such: its answer is a function of its
/// three arguments. The ignore set is decoded once per statement and kept as
/// the call's auxiliary data for the rows after the first.
pub(crate) fn register_functions(connection: &Connection) -> Result<(), StoreError> {
    connection
        .create_scalar_function(
            ADMITS_FUNCTION,
            3,
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
                let path = context
                    .get_raw(2)
                    .as_str()
                    .map_err(|problem| rusqlite::Error::UserFunctionError(Box::new(problem)))?;
                Ok(ignore.admits(path, usize::try_from(segments).unwrap_or(0)))
            },
        )
        .map_err(|problem| error::sql("registering the ambiguity-ignore function", problem))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            AmbiguityIgnore::decoded(""),
            Ok(AmbiguityIgnore::none()),
            "the empty set encodes as nothing"
        );
    }

    #[test]
    fn a_place_is_under_the_shallowest_spelling_a_glob_matches() {
        let set = ignoring(&["archive/**", "attachments/*", "**/drafts/**"]);
        assert_eq!(set.ignored_place("archive/deep/glossary.md"), Some(1));
        assert_eq!(set.ignored_place("attachments/image.md"), Some(2));
        assert_eq!(set.ignored_place("notes/drafts/x.md"), Some(2));
        assert_eq!(set.ignored_place("notes/glossary.md"), None);
        assert_eq!(set.ignored_place("Archive/glossary.md"), None);
    }
}
