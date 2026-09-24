//! The glob grammar: the one pattern language a `/`-separated name is matched
//! by.
//!
//! One grammar, three uses. A vault schema names path sets — the
//! ambiguity-ignore set — and tag sets — the patterns a declared tag facet
//! admits beyond its literal names — and a request's path part carries a glob
//! a document's path must match. All three are `/`-separated hierarchies
//! written by the same people, so all three are read by one grammar rather
//! than several that drift apart. The grammar lives here because the path
//! part is spelled here: a request's glob crosses the seam as its text, and
//! [`Pattern`] is how that text is read wherever it is matched.
//!
//! [`Pattern`] is a reading of text, not a value that crosses: the path part
//! carries the glob as a string, so the pattern carries none of the wire
//! derives.
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
//! # Case
//!
//! Whether a vault folds case is a filesystem fact, so a pattern never decides
//! it: the caller names a [`CaseFold`] at every match. Under
//! [`CaseFold::Exact`] every character compares as itself. Under
//! [`CaseFold::Ascii`] a literal ASCII letter matches either case of itself —
//! `A`–`Z` with `a`–`z` — and every other character, a letter outside ASCII
//! included, still compares as itself; `?`, `*` and `**` mean the same under
//! both, because none of them compares a character. The pattern stays the text
//! it was written as, so one grammar and one matcher answer both.
//!
//! The store's ambiguity-ignore set matches under [`CaseFold::Ascii`] where
//! the store's recorded path order folds ASCII case, and under
//! [`CaseFold::Exact`] where it does not. The path parts of a find, a count
//! and a validate, and a tag facet's patterns, match under [`CaseFold::Exact`]
//! on every root.
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
    ///
    /// `case` says how a literal letter compares with a subject's — see
    /// [`CaseFold`].
    pub fn matches(&self, subject: &str, case: CaseFold) -> bool {
        let subject: Vec<&str> = subject.split('/').collect();
        matches_units(
            &self.segments,
            &subject,
            |segment| matches!(segment, Segment::AnyDepth),
            |segment, subject| match segment {
                // Taken by the predicate above, which is read first.
                Segment::AnyDepth => false,
                Segment::Within(shape) => matches_within(shape, subject, case),
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
/// separator because neither side holds one at this level. Every other pattern
/// character is a literal, compared under `case`.
fn matches_within(shape: &str, subject: &str, case: CaseFold) -> bool {
    let shape: Vec<char> = shape.chars().collect();
    let subject: Vec<char> = subject.chars().collect();
    matches_units(
        &shape,
        &subject,
        |character| *character == '*',
        |shape, subject| *shape == '?' || case.equal(*shape, *subject),
    )
}

/// How a pattern's literal characters compare with a subject's.
///
/// The caller names it at every match, because whether two spellings are one
/// name is the vault root's fact, not the pattern's.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CaseFold {
    /// Every character compares as itself.
    Exact,
    /// An ASCII letter compares with its other ASCII case — `A`–`Z` with
    /// `a`–`z` — and every other character, a letter outside ASCII included,
    /// compares as itself. It is the filesystem seam's fold: the store's
    /// suite pins it to the fold contract sample the seam's fold is pinned to.
    Ascii,
}

impl CaseFold {
    /// Whether a literal pattern character and a subject character are one
    /// under this case.
    fn equal(self, literal: char, subject: char) -> bool {
        match self {
            CaseFold::Exact => literal == subject,
            CaseFold::Ascii => literal.eq_ignore_ascii_case(&subject),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, subject: &str, case: CaseFold) -> bool {
        Pattern::parse(pattern)
            .expect("a pattern")
            .matches(subject, case)
    }

    /// **Under the fold, a literal letter matches either ASCII case of
    /// itself; without it, only itself.** Every other literal — a digit, a
    /// dot, a separator — compares as itself either way.
    #[test]
    fn a_literal_letter_matches_its_other_ascii_case_only_under_the_fold() {
        for (pattern, subject) in [
            ("archive/**", "Archive/norn/glossary.md"),
            ("Archive/**", "archive/deep/term.md"),
            ("NOTES.MD", "notes.md"),
            ("v1.md", "V1.MD"),
        ] {
            assert!(
                matches(pattern, subject, CaseFold::Ascii),
                "{pattern} vs {subject}"
            );
            assert!(
                !matches(pattern, subject, CaseFold::Exact),
                "{pattern} vs {subject}"
            );
        }
        // The fold is a case fold and nothing wider: a different letter, digit
        // or separator still refuses.
        assert!(!matches("v1.md", "v2.md", CaseFold::Ascii));
        assert!(!matches("a-b", "a_b", CaseFold::Ascii));
        assert!(!matches("a/b", "a\\b", CaseFold::Ascii));
        // `@` and `` ` `` sit one byte beside `A` and `a`, and are not letters.
        assert!(!matches("@", "`", CaseFold::Ascii));
        assert!(!matches("[", "{", CaseFold::Ascii));
    }

    /// **A letter outside ASCII keeps its case under the fold.** `É` and `é`
    /// are two characters to a glob on every root.
    #[test]
    fn a_letter_outside_ascii_never_folds() {
        for case in [CaseFold::Exact, CaseFold::Ascii] {
            assert!(!matches("Été/**", "été/x.md", case), "{case:?}");
            assert!(!matches("ÉTÉ", "été", case), "{case:?}");
            assert!(matches("Été/**", "Été/x.md", case), "{case:?}");
        }
        // The ASCII letters beside them still fold.
        assert!(matches("Été/**", "ÉTé/x.md", CaseFold::Ascii));
        assert!(!matches("Été/**", "ÉTé/x.md", CaseFold::Exact));
    }

    /// **The wildcards mean the same under either case.** `?` takes one
    /// character that is not `/`, `*` a run within a segment, and `**` a run
    /// of segments; the fold changes only how the literals around them
    /// compare.
    #[test]
    fn the_wildcards_mean_the_same_under_either_case() {
        let cases: &[(&str, &str, bool, bool)] = &[
            // (pattern, subject, exact, folded)
            ("note?.md", "NoteX.MD", false, true),
            ("note?.md", "note/.md", false, false),
            ("note?.md", "Note.md", false, false),
            ("*.md", "Notes.MD", false, true),
            ("*.md", "a/Notes.md", false, false),
            ("A*Z", "abcz", false, true),
            ("A*Z", "abc/z", false, false),
            ("**/Drafts/**", "notes/drafts/x.md", false, true),
            ("**/Drafts/**", "Drafts", true, true),
            ("**/drafts/**", "notes/draftsx/x.md", false, false),
            ("Archive/*", "ARCHIVE/x.md", false, true),
            ("Archive/*", "ARCHIVE/deep/x.md", false, false),
        ];
        for (pattern, subject, exact, folded) in cases {
            assert_eq!(
                matches(pattern, subject, CaseFold::Exact),
                *exact,
                "{pattern} vs {subject}, exact"
            );
            assert_eq!(
                matches(pattern, subject, CaseFold::Ascii),
                *folded,
                "{pattern} vs {subject}, folded"
            );
        }
    }

    /// **The fold keeps the matching bound.** A pattern whose stars would each
    /// be an independent choice answers at once under the fold as without it.
    #[test]
    fn the_fold_keeps_the_matching_bound() {
        let pattern = Pattern::parse(&format!("{}B", "A*".repeat(12))).expect("a pattern");
        let subject = "a".repeat(64);
        let started = std::time::Instant::now();
        assert!(!pattern.matches(&subject, CaseFold::Ascii));
        assert!(pattern.matches(&format!("{subject}b"), CaseFold::Ascii));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }
}
