//! Heading-delimited sections — the byte ranges a heading owns.
//!
//! A section is addressed by a heading anchor and runs from its heading line to
//! the next same-or-higher-level heading, or to the end of the body. This is
//! the single section resolver: a read and a write consume the same span, the
//! same matching rule and the same failure modes, so they cannot disagree
//! about where a section is. What a caller chooses is only what several
//! matching headings mean ([`Duplicates`]): a read takes the first, and a write
//! refuses unless it names an occurrence. [`resolve_section`] resolves over
//! heading facts a caller already holds — a store's rows as well as a scan's
//! headings — so a caller that never parses the body resolves through it too.
//!
//! # How an anchor matches a heading
//!
//! Three readings, each tried only where the one before matched no heading:
//!
//! 1. **The heading's text**, both sides trimmed, each whitespace run
//!    collapsed to one space, and ASCII case folded. This is what a wikilink
//!    `#anchor` addresses, and what a person types.
//! 2. **The text of an ATX-shaped anchor** (`## State`), read by the same
//!    CommonMark pass that produced the document's headings, then matched as
//!    the first reading matches.
//! 3. **The heading's slug, exactly**, dedupe suffix included. This is what an
//!    inline Markdown `#fragment` addresses.
//!
//! The text readings come first, so an anchor that names a heading by its
//! text is never reinterpreted as another heading's slug.
//!
//! # Separator-aware ranges
//!
//! [`SectionSpan`] carries four boundaries rather than two, because the blank
//! lines around a heading are separators rather than content. `body_start`
//! begins just past the whole heading construct and `end` stops at the next
//! heading, so `body_start..end` is everything the section owns; the narrower
//! `content_start..content_end` excludes the blank lines at each end. A
//! replace addressed at the content leaves the separators standing, which is
//! what keeps an edit from collapsing `## Alpha\n\nbody\n\n## Beta` into
//! `## Alpha\nnew\n## Beta` and rewriting the document's blank structure every
//! time it is touched.

use std::fmt;

use crate::body::BodyScan;
use crate::heading::Heading;
use crate::span::split_lines_inclusive;

/// What several headings an anchor matches mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Duplicates {
    /// Refuse: an ambiguous address is a question, not an edit. What a write
    /// takes by default.
    Refuse,
    /// The first matching heading in document order. What a read takes.
    First,
    /// The 1-based nth matching heading in document order — the escape hatch
    /// that lets a document which has grown two headings with the same text
    /// be repaired by the same tool that reads it.
    Occurrence(usize),
}

/// Which section a caller means: the anchor, and what several headings it
/// matches mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionAddress<'a> {
    pub heading: &'a str,
    pub duplicates: Duplicates,
}

impl<'a> SectionAddress<'a> {
    /// The 1-based nth heading the anchor matches.
    pub fn occurrence(heading: &'a str, occurrence: usize) -> Self {
        SectionAddress {
            heading,
            duplicates: Duplicates::Occurrence(occurrence),
        }
    }

    /// The first heading the anchor matches, in document order.
    pub fn first(heading: &'a str) -> Self {
        SectionAddress {
            heading,
            duplicates: Duplicates::First,
        }
    }
}

impl<'a> From<&'a str> for SectionAddress<'a> {
    /// The anchor, refusing where it matches several headings.
    fn from(heading: &'a str) -> Self {
        SectionAddress {
            heading,
            duplicates: Duplicates::Refuse,
        }
    }
}

/// The byte ranges of a resolved section, relative to the body it was resolved
/// against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionSpan {
    /// Start of the heading construct.
    pub heading_start: usize,
    /// Start of everything below the heading, blank separators included.
    pub body_start: usize,
    /// Start of the first non-blank line below the heading.
    pub content_start: usize,
    /// End of the last non-blank line below the heading, its newline
    /// included. Equal to `content_start` when the section has no content.
    pub content_end: usize,
    /// Start of the next same-or-higher-level heading, or the end of the body.
    pub end: usize,
}

/// Why a section did not resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionError {
    HeadingNotFound {
        heading: String,
    },
    HeadingAmbiguous {
        heading: String,
        count: usize,
    },
    OccurrenceOutOfRange {
        heading: String,
        occurrence: usize,
        count: usize,
    },
}

impl fmt::Display for SectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SectionError::HeadingNotFound { heading } => {
                write!(f, "heading not found: {heading:?}")
            }
            SectionError::HeadingAmbiguous { heading, count } => write!(
                f,
                "{count} headings named {heading:?}; address an occurrence or make the heading \
                 unique"
            ),
            SectionError::OccurrenceOutOfRange {
                heading,
                occurrence,
                count,
            } => write!(
                f,
                "occurrence {occurrence} of {heading:?} was asked for and there are {count}"
            ),
        }
    }
}

impl std::error::Error for SectionError {}

/// The section `address` names among `headings`, in `body`: the one section
/// resolver.
///
/// `headings` are the body's headings in document order, as
/// [`BodyScan::headings`] reads them or as a caller holding them as facts
/// recorded them: each one's level, text, slug, where it starts and where its
/// construct ends, as offsets into `body`.
pub fn resolve_section(
    headings: &[Heading],
    body: &str,
    address: SectionAddress<'_>,
) -> Result<SectionSpan, SectionError> {
    let matches = matching_indices(headings, address.heading);
    let index = match (matches.len(), address.duplicates) {
        (0, _) => {
            return Err(SectionError::HeadingNotFound {
                heading: address.heading.to_string(),
            });
        }
        (_, Duplicates::First) => matches[0],
        (_, Duplicates::Occurrence(occurrence)) => {
            let count = matches.len();
            if occurrence == 0 || occurrence > count {
                return Err(SectionError::OccurrenceOutOfRange {
                    heading: address.heading.to_string(),
                    occurrence,
                    count,
                });
            }
            matches[occurrence - 1]
        }
        (1, Duplicates::Refuse) => matches[0],
        (count, Duplicates::Refuse) => {
            return Err(SectionError::HeadingAmbiguous {
                heading: address.heading.to_string(),
                count,
            });
        }
    };

    let level = headings[index].level;
    let heading_start = headings[index].span.byte_offset;
    let body_start = headings[index].body_offset.min(body.len());
    let end = headings[index + 1..]
        .iter()
        .find(|heading| heading.level <= level)
        .map(|heading| heading.span.byte_offset)
        .unwrap_or(body.len())
        .max(body_start);
    let (content_start, content_end) = content_bounds(body, body_start, end);

    Ok(SectionSpan {
        heading_start,
        body_start,
        content_start,
        content_end,
        end,
    })
}

/// The section body with its leading and trailing blank lines excluded.
///
/// A section whose body is entirely blank collapses to a single point at
/// `end`, so an insert lands below the separators the heading already has
/// rather than crowding the heading.
///
/// Lines are cut on [`crate::span`]'s break rule, so a `\r`-separated blank
/// line is blank and the separators it makes stay outside the content an edit
/// writes over.
fn content_bounds(body: &str, body_start: usize, end: usize) -> (usize, usize) {
    let slice = &body[body_start..end];

    let mut start = body_start;
    for line in split_lines_inclusive(slice) {
        if !line.trim().is_empty() {
            break;
        }
        start += line.len();
    }

    let mut stop = end;
    for line in split_lines_inclusive(slice).rev() {
        if !line.trim().is_empty() {
            break;
        }
        stop -= line.len();
    }

    (start, stop.max(start))
}

/// Every heading `anchor` matches, in document order, under the first of the
/// three readings the module states that matches any.
fn matching_indices(headings: &[Heading], anchor: &str) -> Vec<usize> {
    let by_text = |text: &str| {
        let wanted = normalized(text);
        indices(headings, |heading| normalized(&heading.text) == wanted)
    };
    let mut matches = by_text(anchor);
    if matches.is_empty()
        && let Some(text) = atx_anchor_text(anchor)
    {
        matches = by_text(&text);
    }
    if matches.is_empty() {
        matches = indices(headings, |heading| heading.slug == anchor);
    }
    matches
}

/// The headings `matches` holds, by index, in document order.
fn indices(headings: &[Heading], matches: impl Fn(&Heading) -> bool) -> Vec<usize> {
    headings
        .iter()
        .enumerate()
        .filter(|(_, heading)| matches(heading))
        .map(|(index, _)| index)
        .collect()
}

/// Heading text as an anchor compares it: trimmed, each whitespace run one
/// space, and ASCII case folded. A letter outside ASCII keeps its case.
fn normalized(text: &str) -> String {
    text.split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<String>>()
        .join(" ")
}

/// The heading text of an ATX-shaped anchor, or `None` when the anchor is not
/// one.
///
/// The anchor must open with one to six `#` followed by a space or tab —
/// CommonMark's ATX opening — and its text is read by the same CommonMark pass
/// that produced the document's headings, so a trailing closer (`## X ##`) and
/// inline markup are handled identically to a real heading and a `#` that is
/// part of the text (`## C#`) is preserved.
///
/// An anchor carrying a line break is not one heading and is refused. Reading
/// the first heading out of it would answer a question about `## a` when the
/// caller asked about `## a\n## b`, and address the wrong section by a margin
/// nothing in the result reports.
fn atx_anchor_text(anchor: &str) -> Option<String> {
    if anchor.contains(['\n', '\r']) {
        return None;
    }
    let hashes = anchor.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    if !matches!(anchor.as_bytes().get(hashes), Some(b' ') | Some(b'\t')) {
        return None;
    }
    BodyScan::new(anchor)
        .headings()
        .first()
        .map(|heading| heading.text.clone())
}
