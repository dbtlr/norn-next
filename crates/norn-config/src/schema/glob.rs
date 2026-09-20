//! The pattern language the schema declares path and tag sets in.
//!
//! One matcher, two uses. A vault schema names path sets — the ambiguity-ignore
//! set — and tag sets — the patterns a declared tag facet admits beyond its
//! literal names. Both are `/`-separated hierarchies written the same way by
//! the same authors, so both are read by one grammar rather than two that drift
//! apart.
//!
//! The grammar is the familiar one, stated exactly:
//!
//! - `?` matches one character that is not `/`.
//! - `*` matches any run of characters, empty included, that holds no `/`.
//! - `**` is a whole segment, and it matches any run of segments including no
//!   segments at all. `a**b` is two `*` inside one segment, which is the same
//!   set one `*` matches.
//! - Every other character matches itself. There is no escape and no character
//!   class: a pattern is a name with holes in it, not a regular expression.
//!
//! A pattern is anchored at both ends. `archive` matches the subject `archive`
//! and nothing under it; `archive/*` matches exactly one level under it;
//! `archive/**` matches `archive` and everything under it, because `**` covers
//! the run of no segments as well as every longer one. One rule for `**` in
//! every position is what keeps `**/notes.md` matching `notes.md` at the root,
//! so the grammar states it once rather than special-casing the trailing
//! segment.
//!
//! Matching is byte-oriented over `char`s and folds no case. Whether a vault
//! folds case is a filesystem fact that the store and the walk carry as a
//! parameter, and a pattern that decided it here would answer differently from
//! every other path comparison in the system.

use std::fmt;

/// A validated pattern over a `/`-separated name.
///
/// Its identity is the text it was written as, so two schemas that spell one
/// set two ways stay two declarations — the model is a projection of the bytes
/// and never a normalization of them.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Pattern {
    source: String,
    segments: Vec<Segment>,
}

/// One `/`-separated part of a pattern.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Segment {
    /// A whole segment spelled `**`: it matches any run of segments, none
    /// included.
    AnyDepth,
    /// A segment matched against one subject segment, with `*` and `?` holes.
    Within(String),
}

impl Pattern {
    /// Reads `source` as a pattern, or says why it is not one.
    pub fn parse(source: &str) -> Result<Self, PatternError> {
        if source.is_empty() {
            return Err(PatternError::Empty);
        }
        let segments = source
            .split('/')
            .map(|segment| {
                if segment == "**" {
                    Segment::AnyDepth
                } else {
                    Segment::Within(segment.to_string())
                }
            })
            .collect();
        Ok(Pattern {
            source: source.to_string(),
            segments,
        })
    }

    /// The pattern as it was written.
    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// Whether `subject` is in the set this pattern names.
    ///
    /// The walk is over segments rather than characters, which is what makes
    /// `**` a segment quantifier instead of a wildcard that eats separators by
    /// accident. It backtracks on `**` alone, and a pattern holds few of those,
    /// so the cost is the subject's length in the shapes anyone writes.
    pub fn matches(&self, subject: &str) -> bool {
        let subject: Vec<&str> = subject.split('/').collect();
        matches_from(&self.segments, &subject)
    }
}

impl fmt::Display for Pattern {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.source)
    }
}

/// Whether the remaining pattern segments match the remaining subject segments.
fn matches_from(pattern: &[Segment], subject: &[&str]) -> bool {
    match pattern.split_first() {
        None => subject.is_empty(),
        Some((Segment::AnyDepth, rest)) => {
            (0..=subject.len()).any(|consumed| matches_from(rest, &subject[consumed..]))
        }
        Some((Segment::Within(shape), rest)) => match subject.split_first() {
            Some((head, tail)) => matches_within(shape, head) && matches_from(rest, tail),
            None => false,
        },
    }
}

/// Whether one subject segment matches one pattern segment.
///
/// `*` is the only backtracking point, and it never crosses a separator because
/// neither side holds one at this level.
fn matches_within(shape: &str, subject: &str) -> bool {
    let shape: Vec<char> = shape.chars().collect();
    let subject: Vec<char> = subject.chars().collect();
    within(&shape, &subject)
}

fn within(shape: &[char], subject: &[char]) -> bool {
    match shape.split_first() {
        None => subject.is_empty(),
        Some(('*', rest)) => (0..=subject.len()).any(|consumed| within(rest, &subject[consumed..])),
        Some(('?', rest)) => !subject.is_empty() && within(rest, &subject[1..]),
        Some((literal, rest)) => {
            matches!(subject.split_first(), Some((head, tail)) if head == literal && within(rest, tail))
        }
    }
}

/// Why a string is not a pattern.
///
/// One variant today, and an enum rather than a unit so that a grammar that
/// grows a second refusal does not change the shape every caller matches on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PatternError {
    /// The empty string names no set: it matches one empty segment, which is
    /// no name any subject has.
    Empty,
}

impl fmt::Display for PatternError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PatternError::Empty => formatter.write_str("a pattern cannot be empty"),
        }
    }
}

impl std::error::Error for PatternError {}
