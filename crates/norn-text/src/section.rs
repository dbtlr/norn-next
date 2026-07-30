//! Heading-delimited sections — the byte ranges a heading owns.
//!
//! A section is addressed by heading text and runs from its heading line to
//! the next same-or-higher-level heading, or to the end of the body. This is
//! the single section resolver: a read and a write consume the same span and
//! the same failure modes, so they cannot disagree about where a section is.
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

/// Which section a caller means.
///
/// The default requires the heading text to be unique and refuses otherwise:
/// an ambiguous address is a question, not an edit. `occurrence` is the escape
/// hatch — a document that has already grown two headings with the same text
/// can still be repaired by the same tool that reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionAddress<'a> {
    pub heading: &'a str,
    /// `None` requires a unique heading. `Some(n)` selects the 1-based nth
    /// heading whose text matches.
    pub occurrence: Option<usize>,
}

impl<'a> SectionAddress<'a> {
    /// The heading must be unique in the body.
    pub fn unique(heading: &'a str) -> Self {
        SectionAddress {
            heading,
            occurrence: None,
        }
    }

    /// The 1-based nth heading whose text matches.
    pub fn occurrence(heading: &'a str, occurrence: usize) -> Self {
        SectionAddress {
            heading,
            occurrence: Some(occurrence),
        }
    }
}

impl<'a> From<&'a str> for SectionAddress<'a> {
    fn from(heading: &'a str) -> Self {
        SectionAddress::unique(heading)
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

/// Resolve a section against `body`, walking it once.
///
/// Fence-safe: a `#` inside a fenced code block is never a heading, so it
/// never addresses a section.
pub fn resolve_section<'a>(
    body: &str,
    address: impl Into<SectionAddress<'a>>,
) -> Result<SectionSpan, SectionError> {
    BodyScan::new(body).resolve_section(address.into())
}

/// The resolution core, over headings already parsed.
pub(crate) fn resolve_section_in(
    headings: &[Heading],
    body: &str,
    address: SectionAddress<'_>,
) -> Result<SectionSpan, SectionError> {
    // Exact text first, so a heading whose text legitimately begins with `#`
    // is addressed verbatim and no working anchor is ever reinterpreted.
    let mut matches = matching_indices(headings, address.heading);
    // Only on a total miss, and only for an ATX-shaped anchor (`## State`,
    // `### X ##`), retry on the heading text with the marker stripped. Level
    // is syntax noise: a `## X` anchor resolves a `### X` heading.
    if matches.is_empty()
        && let Some(text) = atx_anchor_text(address.heading)
    {
        matches = matching_indices(headings, &text);
    }

    let index = match (matches.len(), address.occurrence) {
        (0, _) => {
            return Err(SectionError::HeadingNotFound {
                heading: address.heading.to_string(),
            });
        }
        (_, Some(occurrence)) => {
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
        (1, None) => matches[0],
        (count, None) => {
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
fn content_bounds(body: &str, body_start: usize, end: usize) -> (usize, usize) {
    let slice = &body[body_start..end];

    let mut start = body_start;
    for line in slice.split_inclusive('\n') {
        if !line.trim().is_empty() {
            break;
        }
        start += line.len();
    }

    let mut stop = end;
    for line in slice.split_inclusive('\n').rev() {
        if !line.trim().is_empty() {
            break;
        }
        stop -= line.len();
    }

    (start, stop.max(start))
}

fn matching_indices(headings: &[Heading], text: &str) -> Vec<usize> {
    headings
        .iter()
        .enumerate()
        .filter(|(_, heading)| heading.text == text)
        .map(|(index, _)| index)
        .collect()
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
