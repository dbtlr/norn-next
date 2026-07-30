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
use crate::span::SourceSpan;
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
        let mut code_ranges = Vec::new();
        let mut active_heading: Option<(u8, String, usize)> = None;
        let mut active_code_block: Option<usize> = None;

        for (event, range) in Parser::new(body).into_offset_iter() {
            match event {
                Event::Start(Tag::Heading { level, .. }) => {
                    active_heading = Some((heading_level(level), String::new(), range.start));
                }
                Event::End(TagEnd::Heading(_)) => {
                    if let Some((level, text, start)) = active_heading.take() {
                        let text = text.trim().to_string();
                        headings.push(Heading {
                            level,
                            slug: slugify(&text),
                            text,
                            span: SourceSpan::at(body, start),
                            // `range.end` covers the whole heading construct:
                            // the ATX line through its newline, or the setext
                            // underline line.
                            body_offset: range.end.min(body.len()),
                        });
                    }
                }
                Event::Text(text) => {
                    if let Some((_, heading_text, _)) = active_heading.as_mut() {
                        heading_text.push_str(&text);
                    }
                }
                Event::Code(text) => {
                    if let Some((_, heading_text, _)) = active_heading.as_mut() {
                        heading_text.push_str(&text);
                    }
                    code_ranges.push(range);
                }
                Event::Start(Tag::CodeBlock(_)) => active_code_block = Some(range.start),
                Event::End(TagEnd::CodeBlock) => {
                    if let Some(start) = active_code_block.take() {
                        code_ranges.push(start..range.end);
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

    /// The byte ranges where code makes text opaque: inline code spans and
    /// whole fenced or indented code blocks.
    pub fn code_ranges(&self) -> &[Range<usize>] {
        &self.code_ranges
    }

    /// Whether `range` overlaps any opaque code range.
    pub fn is_in_code(&self, range: &Range<usize>) -> bool {
        self.code_ranges.iter().any(|code| overlap(code, range))
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

pub(crate) fn overlap(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
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
