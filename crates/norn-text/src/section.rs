//! Heading-delimited sections — the byte ranges a heading owns.
//!
//! A section is addressed by a heading anchor and runs from its heading line to
//! the start of the line holding the next same-or-higher-level heading, or to
//! the end of the body. This is
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
//! An empty anchor is no anchor and matches no heading. Any other anchor is
//! read as written — a Markdown link's fragment arrives here already decoded,
//! because the link's parse decodes it ([`crate::Link::anchor`]), and a
//! wikilink's anchor and a get target's are literal — and it is read three
//! ways, each tried only where the one before matched no heading:
//!
//! 1. **Its text**, compared with each heading's [`heading_reading`]: both
//!    sides trimmed, each run of ASCII whitespace collapsed to one space, and
//!    ASCII case folded. This is what a wikilink `#anchor` addresses, and what
//!    a person types. The fold is ASCII's alone, case and whitespace alike: a
//!    no-break space or an ideographic space is text, as a letter outside
//!    ASCII keeps its case.
//! 2. **The heading text past its `#` markers**, compared as the first
//!    reading compares. An ATX-shaped anchor (`## State`) is read by the same
//!    CommonMark pass that produced the document's headings. A heading chain
//!    (`Top#Sub`, as `[[note#Top#Sub]]` writes it) is read as its last
//!    heading: the text after its last `#`, where text stands on both sides of
//!    the chain's `#`s.
//! 3. **Its slug reading, exactly**: the anchor against each heading's slug,
//!    dedupe suffix included. This is what an inline Markdown `#fragment`
//!    addresses.
//!
//! [`anchor_readings`] is the one place an anchor's first two readings are
//! made, and a stored link carries them beside the anchor, so a store's
//! predicate over stored readings and this resolver agree about which
//! headings an anchor matches.
//!
//! The text reading comes first, so an anchor that names a heading by its
//! whole text is never reinterpreted as another heading's marked text or slug.
//! A read and a write match through the same three readings, so headings the
//! first reading folds together are one anchor's matches for both: a write of
//! `dup` over `## Dup` and `## dup` refuses as ambiguous, and an occurrence
//! reaches either one.
//!
//! # Separator-aware ranges
//!
//! [`SectionSpan`] carries four boundaries rather than two, because the blank
//! lines around a heading are separators rather than content. `body_start`
//! begins just past the whole heading construct and `end` stops at the start
//! of the next heading's line, so `body_start..end` is everything the section
//! owns and never a container's prefix on that line; the narrower
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
    /// The index of the heading the address matched among the headings it
    /// was resolved over, in document order.
    pub heading: usize,
    /// Start of the heading construct.
    pub heading_start: usize,
    /// Start of everything below the heading, blank separators included.
    pub body_start: usize,
    /// Start of the first non-blank line below the heading.
    pub content_start: usize,
    /// End of the last non-blank line below the heading, its newline
    /// included. Equal to `content_start` when the section has no content.
    pub content_end: usize,
    /// Start of the line holding the next same-or-higher-level heading, or
    /// the end of the body. Where that heading sits inside a container, the
    /// container's prefix on its line is the next section's.
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
///
/// **It makes at most four passes over `headings`, each visiting a heading at
/// most once**: one for each of the three readings an anchor is matched by,
/// each run only where the one before matched no heading, and one from the
/// matched heading to the heading that ends its section. So what it compares
/// is at most four times the headings it is handed, and it reads no heading
/// it is not handed.
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
        .map(|heading| line_start(body, heading.span.byte_offset))
        .unwrap_or(body.len())
        .max(body_start);
    let (content_start, content_end) = content_bounds(body, body_start, end);

    Ok(SectionSpan {
        heading: index,
        heading_start,
        body_start,
        content_start,
        content_end,
        end,
    })
}

/// Where the line holding `at` starts: just past the break before it, on
/// [`crate::span`]'s break rule, or the start of the body.
///
/// A heading inside a container starts past the container's prefix on its
/// line — `> ## Q`, `- # L` — and that prefix is the container's, which the
/// next section holds, so the section the heading ends stops at the line.
fn line_start(body: &str, at: usize) -> usize {
    let at = at.min(body.len());
    body.as_bytes()[..at]
        .iter()
        .rposition(|byte| matches!(byte, b'\n' | b'\r'))
        .map_or(0, |found| found + 1)
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
    let Some(readings) = anchor_readings(anchor) else {
        return Vec::new();
    };
    let by_reading =
        |wanted: &str| indices(headings, |heading| heading_reading(&heading.text) == wanted);
    let mut matches = by_reading(&readings.text);
    if matches.is_empty()
        && let Some(marked) = &readings.marked
    {
        matches = by_reading(marked);
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

/// The two readings an anchor is matched by beside itself, as the module
/// states them: its third reading is the anchor as written, against a
/// heading's slug.
///
/// A heading's side of both is its [`heading_reading`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchorReadings {
    /// The anchor as a heading reading.
    pub text: String,
    /// The heading text past the anchor's `#` markers, as a heading reading:
    /// an ATX-shaped anchor's heading text, or a heading chain's last heading.
    /// `None` where the anchor has no such markers.
    pub marked: Option<String>,
}

/// The readings `anchor` is matched by, or `None` for an empty anchor, which
/// reads as no anchor.
pub fn anchor_readings(anchor: &str) -> Option<AnchorReadings> {
    if anchor.is_empty() {
        return None;
    }
    Some(AnchorReadings {
        text: heading_reading(anchor),
        marked: marked_text(anchor).map(|text| heading_reading(&text)),
    })
}

/// Heading text as an anchor compares it: trimmed, each run of ASCII space one
/// space, and ASCII case folded. A letter outside ASCII keeps its case, and a
/// space outside ASCII — a no-break space, an ideographic space — is text.
pub fn heading_reading(text: &str) -> String {
    text.split(is_ascii_space)
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<String>>()
        .join(" ")
}

/// The heading text past `anchor`'s `#` markers: an ATX-shaped anchor's
/// heading text, a heading chain's last heading, or `None` where it is
/// neither.
///
/// An anchor carrying a line break is not one heading and has no marked text.
/// Reading the first heading out of it would answer a question about `## a`
/// when the caller asked about `## a\n## b`, and address the wrong section by
/// a margin nothing in the result reports.
fn marked_text(anchor: &str) -> Option<String> {
    if anchor.contains(['\n', '\r']) {
        return None;
    }
    if opens_atx(anchor) {
        atx_anchor_text(anchor)
    } else {
        chain_leaf(anchor).map(str::to_string)
    }
}

/// Whether `anchor` opens with CommonMark's ATX opening: one to six `#`
/// followed by a space or tab.
fn opens_atx(anchor: &str) -> bool {
    let hashes = anchor.bytes().take_while(|byte| *byte == b'#').count();
    (1..=6).contains(&hashes) && matches!(anchor.as_bytes().get(hashes), Some(b' ' | b'\t'))
}

/// The last heading of a heading chain — the text after its last `#` — where
/// `anchor` is one: text that is not blank stands before its first `#` and
/// after its last. A hash run opening an anchor is no chain, and neither is an
/// anchor ending in a `#`.
fn chain_leaf(anchor: &str) -> Option<&str> {
    let (first, _) = anchor.split_once('#')?;
    let (_, last) = anchor.rsplit_once('#')?;
    let blank = |text: &str| text.trim_matches(is_ascii_space).is_empty();
    (!blank(first) && !blank(last)).then_some(last)
}

/// Whether `ch` is one of the six ASCII whitespace characters CommonMark
/// names: space, tab, line feed, carriage return, form feed and line
/// tabulation. The whitespace a heading's text is trimmed of and an anchor
/// folds.
pub(crate) fn is_ascii_space(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\n' | '\r' | '\u{c}' | '\u{b}')
}

/// The heading text of an anchor [`opens_atx`] recognizes.
///
/// The text is read by the same CommonMark pass that produced the document's
/// headings, so a trailing closer (`## X ##`) and inline markup are handled
/// identically to a real heading and a `#` that is part of the text (`## C#`)
/// is preserved.
fn atx_anchor_text(anchor: &str) -> Option<String> {
    BodyScan::new(anchor)
        .headings()
        .first()
        .map(|heading| heading.text.clone())
}
