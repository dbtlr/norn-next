//! The document body's one CommonMark pass.
//!
//! Every body-level fact this crate reports — headings, section boundaries,
//! wikilink tokens, block-id anchors — is derived from a single
//! `pulldown-cmark` traversal held by [`BodyScan`]. One pass is both the
//! cost story (a document is walked once, not once per question) and the
//! correctness story: three passes are three chances for two parsers to
//! disagree about where a code fence is.
//!
//! # Code is opaque
//!
//! Fenced code blocks, indented code blocks and inline code spans are a
//! different document. No token this crate extracts — a heading, a wikilink,
//! a block id — may match inside one. What a reader sees as a literal code
//! sample, norn reads as literal text and never as vault structure.
//!
//! The one nuance: a `^block-id` on the line *after* a closing fence
//! references the code block itself and stays valid. The exclusion covers what
//! is inside the fences, not the anchor line trailing them.

use std::ops::Range;

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

use crate::heading::{Heading, slugify};
use crate::section::{SectionAddress, SectionError, SectionSpan, resolve_section_in};
use crate::span::LineCursor;
use crate::wikilink::{Wikilink, parse_tokens, splice_tokens};

/// One CommonMark reading of a document body: its headings and the byte
/// ranges where code makes text opaque.
///
/// Build it once and ask it everything; each accessor is a view over the same
/// traversal.
#[derive(Debug, Clone)]
pub struct BodyScan<'a> {
    body: &'a str,
    headings: Vec<Heading>,
    code_ranges: Vec<Range<usize>>,
}

impl<'a> BodyScan<'a> {
    /// Walk `body` once.
    pub fn new(body: &'a str) -> Self {
        let mut headings = Vec::new();
        let mut code_ranges: Vec<Range<usize>> = Vec::new();
        let mut active_heading: Option<ActiveHeading> = None;
        let mut active_code_block: Option<usize> = None;
        // Headings arrive in ascending order, so their positions are counted
        // once across the whole walk rather than once per heading.
        let mut cursor = LineCursor::new(body);
        let mut container_depth: usize = 0;

        for (event, range) in Parser::new(body).into_offset_iter() {
            match event {
                Event::Start(Tag::BlockQuote(_) | Tag::List(_) | Tag::Item) => {
                    container_depth += 1;
                }
                Event::End(TagEnd::BlockQuote(_) | TagEnd::List(_) | TagEnd::Item) => {
                    container_depth = container_depth.saturating_sub(1);
                }
                Event::Start(Tag::Heading { level, .. }) => {
                    active_heading = Some(ActiveHeading {
                        level: heading_level(level),
                        text: String::new(),
                        start: range.start,
                        inside_container: container_depth > 0,
                    });
                }
                Event::End(TagEnd::Heading(_)) => {
                    if let Some(active) = active_heading.take() {
                        let text = active.text.trim().to_string();
                        headings.push(Heading {
                            level: active.level,
                            slug: slugify(&text),
                            text,
                            span: cursor.span_at(active.start),
                            // `range.end` covers the whole heading construct:
                            // the ATX line through its newline, or the setext
                            // underline line.
                            body_offset: range.end.min(body.len()),
                            inside_container: active.inside_container,
                        });
                    }
                }
                Event::Text(text) => {
                    if let Some(active) = active_heading.as_mut() {
                        active.text.push_str(&text);
                    }
                }
                // A setext heading's title may run across several lines. The
                // break between them separates two words, so it contributes
                // one, and the text — and the slug and the address derived
                // from it — reads as the heading a human sees.
                Event::SoftBreak | Event::HardBreak => {
                    if let Some(active) = active_heading.as_mut() {
                        active.text.push(' ');
                    }
                }
                Event::Code(text) => {
                    if let Some(active) = active_heading.as_mut() {
                        active.text.push_str(&text);
                    }
                    push_code_range(&mut code_ranges, range);
                }
                Event::Start(Tag::CodeBlock(_)) => active_code_block = Some(range.start),
                Event::End(TagEnd::CodeBlock) => {
                    if let Some(start) = active_code_block.take() {
                        push_code_range(&mut code_ranges, start..range.end);
                    }
                }
                _ => {}
            }
        }

        BodyScan {
            body,
            headings,
            code_ranges,
        }
    }

    /// The body this scan describes.
    pub fn body(&self) -> &'a str {
        self.body
    }

    /// Every heading, in document order.
    pub fn headings(&self) -> &[Heading] {
        &self.headings
    }

    /// Every `[[…]]` token in the body, code excluded.
    pub fn wikilinks(&self) -> Vec<Wikilink> {
        parse_tokens(self.body, &self.code_ranges)
    }

    /// Rewrite selected `[[…]]` tokens by splicing a replacement at each
    /// token's exact byte span.
    ///
    /// `replace` returns `Some(text)` to substitute for a token verbatim, or
    /// `None` to leave it untouched; it is `FnMut`, so a caller can rewrite
    /// only the first match. Every substitution lands on a parser-recognized
    /// span, so the embed marker, the title and the anchor survive by
    /// construction, and code-fenced samples are excluded structurally rather
    /// than by a rule somebody has to remember.
    pub fn splice_wikilinks(&self, replace: impl FnMut(&Wikilink) -> Option<String>) -> String {
        splice_tokens(self.body, &self.wikilinks(), replace)
    }

    /// Trailing block-id definitions (`… ^block-id`), one per line that
    /// carries one, in document order. These are the anchors a
    /// `[[Note#^block-id]]` reference points at.
    pub fn block_ids(&self) -> Vec<String> {
        crate::wikilink::parse_block_ids_in(self.body, &self.code_ranges)
    }

    /// Resolve a heading-addressed section to the byte ranges it owns.
    pub fn resolve_section(
        &self,
        address: SectionAddress<'_>,
    ) -> Result<SectionSpan, SectionError> {
        resolve_section_in(&self.headings, self.body, address)
    }
}

/// A heading being accumulated across the events that make it up.
struct ActiveHeading {
    level: u8,
    text: String,
    start: usize,
    inside_container: bool,
}

/// Record an opaque range, dropping an empty one.
///
/// An empty range masks nothing, and [`overlaps_any`] resolves a query against
/// one candidate — the last range that could reach it — so an empty range
/// standing in that position would hide a real one behind it.
fn push_code_range(ranges: &mut Vec<Range<usize>>, range: Range<usize>) {
    if range.start >= range.end {
        return;
    }
    debug_assert!(
        ranges.last().is_none_or(|last| last.end <= range.start),
        "opaque ranges arrive ascending and disjoint: {range:?} after {:?}",
        ranges.last()
    );
    ranges.push(range);
}

/// Whether `query` overlaps any of `ranges`, which are ascending,
/// non-overlapping and non-empty.
///
/// A linear scan makes every masked-construct question cost the document's
/// code, so a document that is mostly code pays that once per wikilink, per
/// block id and per heading. Binary search over the ordering the one pass
/// already produced answers it in a step: at most one range can start before
/// the query ends and still reach past where it begins.
pub(crate) fn overlaps_any(ranges: &[Range<usize>], query: &Range<usize>) -> bool {
    let index = ranges.partition_point(|range| range.start < query.end);
    index > 0 && ranges[index - 1].end > query.start
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}
