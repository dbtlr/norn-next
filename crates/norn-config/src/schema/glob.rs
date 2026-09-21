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
//!
//! # What matching costs
//!
//! **At most the pattern's length times the subject's length, for every
//! pattern and every subject.** A schema is authored by a person and read at
//! every attach and every re-pin, so the bound has to hold for the pattern
//! someone writes by accident as well as the ones they write on purpose: a
//! matcher that explored each wildcard's choices independently would cost
//! `2^stars` on a subject that nearly matches, and twelve stars against a
//! thirty-character tag is already seconds of a vault's attach. Both levels of
//! the walk therefore use the standard wildcard match, which remembers where
//! the last wildcard was taken and resumes one unit past it instead of
//! re-entering every earlier choice — see [`matches_units`].

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
    /// accident. The segment walk and the walk inside one segment are the same
    /// linear match over two alphabets, so the whole answer costs at most this
    /// pattern's length times the subject's.
    pub fn matches(&self, subject: &str) -> bool {
        let subject: Vec<&str> = subject.split('/').collect();
        matches_units(
            &self.segments,
            &subject,
            |segment| matches!(segment, Segment::AnyDepth),
            |segment, subject| match segment {
                // Taken by the predicate above, which is read first.
                Segment::AnyDepth => false,
                Segment::Within(shape) => matches_within(shape, subject),
            },
        )
    }
}

impl fmt::Display for Pattern {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.source)
    }
}

/// The standard wildcard match, over whatever a level's units are.
///
/// `star` says which pattern units match any run of subject units; `unit` says
/// whether one pattern unit matches one subject unit. The walk is greedy and
/// remembers where the last star was taken: a mismatch after it resumes with
/// that star consuming one more unit, rather than re-entering the choices of
/// every star before it. Each `(pattern index, subject index)` pair is
/// therefore reached at most once, which is the `pattern × subject` bound this
/// module states — and the one thing that makes it hold is that a star can
/// consume anything, so giving one more unit to the *last* star is never worse
/// than revisiting an earlier one.
fn matches_units<P, S>(
    pattern: &[P],
    subject: &[S],
    star: impl Fn(&P) -> bool,
    unit: impl Fn(&P, &S) -> bool,
) -> bool {
    let (mut at_pattern, mut at_subject) = (0usize, 0usize);
    // Where the last star stands, and how much of the subject it has taken.
    let mut last_star: Option<(usize, usize)> = None;
    while at_subject < subject.len() {
        if at_pattern < pattern.len() && star(&pattern[at_pattern]) {
            last_star = Some((at_pattern, at_subject));
            at_pattern += 1;
        } else if at_pattern < pattern.len() && unit(&pattern[at_pattern], &subject[at_subject]) {
            at_pattern += 1;
            at_subject += 1;
        } else if let Some((star_at, taken)) = last_star {
            last_star = Some((star_at, taken + 1));
            at_pattern = star_at + 1;
            at_subject = taken + 1;
        } else {
            return false;
        }
    }
    // The subject is spent, so what is left of the pattern matches only if it
    // is stars: each of them takes the empty run.
    pattern[at_pattern..].iter().all(star)
}

/// Whether one subject segment matches one pattern segment.
///
/// `*` matches any run of characters and `?` exactly one, and neither crosses a
/// separator because neither side holds one at this level.
fn matches_within(shape: &str, subject: &str) -> bool {
    let shape: Vec<char> = shape.chars().collect();
    let subject: Vec<char> = subject.chars().collect();
    matches_units(
        &shape,
        &subject,
        |character| *character == '*',
        |shape, subject| *shape == '?' || shape == subject,
    )
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
