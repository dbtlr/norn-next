//! The document body's one CommonMark pass.
//!
//! Every body-level fact this crate reports — headings, section boundaries,
//! link tokens of both families, `#tag` tokens, block-id anchors — is derived
//! from a single `pulldown-cmark` traversal held by [`BodyScan`], or from the
//! byte masks that traversal produces: the `#tag` scanner and the `[[…]]`
//! token parser walk the body themselves, over ranges the one pass decided
//! were opaque. One pass is both the cost story (a document is walked once,
//! not once per question) and the correctness story: three parses are three
//! chances for two readings to disagree about where a code fence is.
//!
//! # Code is opaque
//!
//! Fenced code blocks, indented code blocks and inline code spans are a
//! different document. No token this crate extracts — a heading, a link, a
//! tag, a block id — may match inside one. What a reader sees as a literal
//! code sample, norn reads as literal text and never as vault structure.
//!
//! The one nuance: a `^block-id` on the line *after* a closing fence
//! references the code block itself and stays valid. The exclusion covers what
//! is inside the fences, not the anchor line trailing them.
//!
//! # The link scope fence
//!
//! Two written forms produce link facts: `[[…]]` and the inline Markdown link
//! `[title](target)`. Four other forms are recognized by the CommonMark parse
//! and deliberately produce nothing, because their target lives somewhere
//! other than the token and emitting a fact with no rewrite story behind it
//! would be a half-supported family:
//!
//! - **Images** (`![alt](x.png)`) are a different element to the parser, and a
//!   transclusion in this vocabulary is `![[…]]`. The fence is about which
//!   written form produces a fact rather than about where in the document it
//!   was written, so a link inside an image's *alt text* —
//!   `![see [here](a.md)](i.png)` — is a link of an implemented family and **is
//!   reported**, while the image around it still is not.
//! - **Autolinks** (`<https://example.com>`) and bare email autolinks.
//! - **Reference-style links** — `[text][label]`, collapsed `[label][]`, and
//!   shortcut `[label]` — whose destination sits in a definition line
//!   elsewhere in the document. The definition line itself
//!   (`[label]: target.md`) produces no parser event at all and is invisible
//!   here; an *undefined* reference is not a link to the parser either and
//!   arrives as ordinary text.
//!
//! Producing no link fact is not the same as being unrecognized. Every one of
//! these forms is opaque to the `#tag` scan — raw HTML and definition lines
//! included, wherever a definition line sits, block quotes and list items
//! included — so a URL fragment written in one is that construct's syntax
//! rather than a tag, and a `#tag` commented out in `<!-- … -->` stays
//! commented out. That is the same rule the two implemented families follow. A
//! bare URL in prose is the stated exception: nothing recognizes one, so
//! `see https://x/#setup` carries the tag `setup`.

use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, LinkType, Parser, Tag, TagEnd};

use crate::heading::{Heading, SlugCounter};
use crate::link::{
    BlockId, Link, markdown_link, parse_block_ids_in, parse_tokens, splice_tokens, wikilink_ranges,
};
use crate::section::{SectionAddress, SectionError, SectionSpan, is_ascii_space, resolve_section};
use crate::span::{LineCursor, lf_normalized, split_lines_inclusive};
use crate::tag::{Tag as TagFact, scan_tags};

/// One CommonMark reading of a document body: its headings, the inline
/// Markdown links it carries, and the byte ranges where code makes text
/// opaque.
///
/// Build it once and ask it everything; each accessor is a view over the same
/// traversal.
#[derive(Debug, Clone)]
pub struct BodyScan<'a> {
    body: &'a str,
    headings: Vec<Heading>,
    markdown_links: Vec<MarkdownToken>,
    code_ranges: Vec<Range<usize>>,
    construct_ranges: Vec<Range<usize>>,
    /// The leaf blocks a block-id definition can name: each paragraph,
    /// heading and code block, and each list item's own text where the item
    /// holds no paragraph, in document order.
    leaves: Vec<Range<usize>>,
    /// The fenced code blocks, fences included, in document order.
    fenced: Vec<Range<usize>>,
}

impl<'a> BodyScan<'a> {
    /// Walk `body` once.
    pub fn new(body: &'a str) -> Self {
        let mut headings = Vec::new();
        let mut markdown_links = Vec::new();
        let mut code_ranges: Vec<Range<usize>> = Vec::new();
        let mut construct_ranges: Vec<Range<usize>> = Vec::new();
        let mut leaves: Vec<Range<usize>> = Vec::new();
        let mut fenced: Vec<Range<usize>> = Vec::new();
        // How many leaf blocks are open around the event being read, and, for
        // each open list item, the run of its own text read so far outside
        // any leaf: a tight item holds its text directly, with no paragraph.
        let mut open_leaves: usize = 0;
        let mut item_runs: Vec<Option<Range<usize>>> = Vec::new();
        let mut active_heading: Option<ActiveHeading> = None;
        let mut active_link: Option<ActiveLink> = None;
        let mut active_code_block: Option<usize> = None;
        // Headings arrive in ascending order, so their positions are counted
        // once across the whole walk rather than once per heading. Link spans
        // are not taken here: a link inside a heading would ask for a position
        // ahead of the one the heading asks for when it ends, and a cursor
        // only walks forward.
        let mut cursor = LineCursor::new(body);
        let mut slugs = SlugCounter::default();
        let mut container_depth: usize = 0;

        // The parse reads a narrower line rule than the rest of the crate: it
        // opens a backtick fence on `\n` and on `\r\n` and not on a lone
        // `\r`, so a CR-only document's code is not code and every construct
        // this scan extracts leaks out of it. Normalizing lone `\r` to `\n`
        // gives the parse the break rule CommonMark states and the cursor
        // already counts. A document carrying no lone `\r` is parsed from the
        // caller's own bytes.
        //
        // Two things come out of the parse, and the normalization is sound for
        // both. **Offsets**: the rewrite is byte-length preserving, so every
        // range the parse reports — heading starts, link ranges, code ranges —
        // indexes the original `body` unchanged. **Decoded text**: the heading
        // text, the link text and `dest_url` are built from the copy's bytes,
        // and no rewritten byte can reach one. A line ending inside inline
        // content arrives as `SoftBreak` or `HardBreak`, which this loop
        // flattens to a space without reading the break's bytes at all; and
        // CommonMark forbids a line ending inside a link destination, in the
        // angle-bracket form and outside it, so `dest_url` spans no break in
        // the first place. What is left is a code span's text, where a `\r`
        // the copy holds as `\n` is a line ending either way.
        let source = lf_normalized(body);

        for (event, range) in Parser::new(&source).into_offset_iter() {
            // Every link, image and span of raw HTML the parse recognizes is
            // opaque to the tag scan, whichever family it belongs to and
            // whether or not it produces a link fact: the `#` in
            // `<https://x/#install>`, `![alt](./pics/#frag)`,
            // `<a href="https://x/#frag">` or `<!-- #tag -->` is that
            // construct's own syntax rather than a marker. The ranges come free
            // with the events, so the mask covers exactly what the one pass
            // already saw.
            if matches!(
                event,
                Event::Start(Tag::Link { .. } | Tag::Image { .. } | Tag::HtmlBlock)
                    | Event::Html(_)
                    | Event::InlineHtml(_)
            ) {
                construct_ranges.push(range.clone());
            }
            // Where an inline link's bracket text ends is the parse's answer,
            // and the only one needed to find the destination's own bytes
            // afterwards: the closing `]` is the next one past it, however
            // many brackets the text itself carried. Every event between the
            // link's start and its end is inside that text.
            if let Some(active) = active_link.as_mut()
                && !matches!(event, Event::End(TagEnd::Link))
            {
                active.text_end = active.text_end.max(range.end.min(active.range.end));
            }

            match &event {
                Event::Start(Tag::Paragraph | Tag::Heading { .. } | Tag::CodeBlock(_)) => {
                    open_leaves += 1;
                    leaves.push(range.clone());
                    if matches!(
                        event,
                        Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_)))
                    ) {
                        fenced.push(range.clone());
                    }
                }
                Event::End(TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::CodeBlock) => {
                    open_leaves = open_leaves.saturating_sub(1);
                }
                Event::Start(Tag::Item) => item_runs.push(None),
                Event::End(TagEnd::Item) => leaves.extend(item_runs.pop().flatten()),
                // A nested list ends the text its item holds before it.
                Event::Start(Tag::List(_)) => {
                    leaves.extend(item_runs.last_mut().and_then(Option::take));
                }
                Event::End(_) | Event::Start(_) if open_leaves == 0 => {}
                _ if open_leaves == 0 => {
                    if let Some(run) = item_runs.last_mut() {
                        *run = Some(match run.take() {
                            Some(held) => held.start..range.end.max(held.end),
                            None => range.clone(),
                        });
                    }
                }
                _ => {}
            }

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
                        let text = active.text.trim_matches(is_ascii_space).to_string();
                        headings.push(Heading {
                            level: active.level,
                            slug: slugs.issue(&text),
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
                // Only the inline form is a link family here; every other
                // `LinkType` is the documented scope fence.
                Event::Start(Tag::Link {
                    link_type: LinkType::Inline,
                    dest_url,
                    ..
                }) => {
                    active_link = Some(ActiveLink {
                        text_end: range.start + 1,
                        range: range.clone(),
                        destination: dest_url.into_string(),
                        text: String::new(),
                    });
                }
                Event::End(TagEnd::Link) => {
                    if let Some(active) = active_link.take() {
                        markdown_links.push(MarkdownToken {
                            text_end: active.text_end - active.range.start,
                            range: active.range,
                            destination: active.destination,
                            text: active.text,
                        });
                    }
                }
                Event::Text(text) => {
                    if let Some(active) = active_heading.as_mut() {
                        active.text.push_str(&text);
                    }
                    if let Some(active) = active_link.as_mut() {
                        active.text.push_str(&text);
                    }
                }
                // A setext heading's title may run across several lines. The
                // break between them separates two words, so it contributes
                // one, and the text — and the slug and the address derived
                // from it — reads as the heading a human sees. A link's
                // bracket text is flattened the same way.
                Event::SoftBreak | Event::HardBreak => {
                    if let Some(active) = active_heading.as_mut() {
                        active.text.push(' ');
                    }
                    if let Some(active) = active_link.as_mut() {
                        active.text.push(' ');
                    }
                }
                Event::Code(text) => {
                    if let Some(active) = active_heading.as_mut() {
                        active.text.push_str(&text);
                    }
                    if let Some(active) = active_link.as_mut() {
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

        leaves.sort_by_key(|leaf| leaf.start);
        BodyScan {
            body,
            headings,
            markdown_links,
            code_ranges,
            construct_ranges,
            leaves,
            fenced,
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

    /// Every link token in the body, both families, in document order, code
    /// excluded. Filter on [`Link::family`] for one family alone.
    ///
    /// **The order is total and contracted**, because a caller storing these
    /// as rows needs the same link to be the same row twice: ascending by
    /// where the token starts, and a wikilink before a Markdown link that
    /// starts on the same byte. The tie is not hypothetical — `[[a]](b)`
    /// satisfies both grammars at once, and the ranges of two link facts may
    /// overlap and nest across the families.
    pub fn links(&self) -> Vec<Link> {
        let mut links = self.wikilinks();
        links.extend(self.markdown_link_facts());
        // A stable sort, so the wikilinks that went in first stay in front of
        // the Markdown links they tie with.
        links.sort_by_key(|link| link.span.byte_offset);
        links
    }

    /// Every `[[…]]` token in the body, code excluded.
    pub fn wikilinks(&self) -> Vec<Link> {
        parse_tokens(self.body, &self.code_ranges)
    }

    /// Every `#tag` in the body, in document order.
    ///
    /// Code is opaque to tags, and so is every construct the parse recognizes
    /// as addressing something: the `#` in `[[Note#Heading]]`,
    /// `[text](#frag)`, `<https://x/#install>`, `![alt](./pics/#frag)`,
    /// `<a href="https://x/#frag">` or a `[label]: url#faq` definition line is
    /// that construct's fragment syntax, not a marker. Raw HTML is opaque
    /// whole, so a `#tag` inside an HTML comment is commented out. A bare URL
    /// in prose is recognized by nothing and is the stated exception.
    pub fn tags(&self) -> Vec<TagFact> {
        scan_tags(self.body, &self.opaque_to_tags())
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
    ///
    /// Inline Markdown links are never touched: their targets are relative to
    /// the document they sit in, so rewriting one is a per-document
    /// computation rather than a token substitution.
    pub fn splice_wikilinks(&self, replace: impl FnMut(&Link) -> Option<String>) -> String {
        splice_tokens(self.body, &self.wikilinks(), replace)
    }

    /// Trailing block-id definitions (`… ^block-id`), one per line that
    /// carries one, in document order. These are the anchors a
    /// `[[Note#^block-id]]` reference points at; each carries the span of its
    /// `^` marker, body-relative like every other span this scan reports.
    pub fn block_ids(&self) -> Vec<BlockId> {
        parse_block_ids_in(self.body, &self.code_ranges)
    }

    /// The block a block-id definition names, given where its `^` marker
    /// stands: the byte range of the leaf block the marker trails, its
    /// trailing break left out.
    ///
    /// That is the paragraph it ends, every line of it; the heading it ends;
    /// or, in a list item, the item's own text — never its list marker, a
    /// sibling, or a nested list. A marker opening the line after a closing
    /// fence names the fenced block, fences included. A marker no leaf block
    /// holds — a table row — names the line it stands on.
    pub fn block_extent(&self, marker: usize) -> Range<usize> {
        let marker = marker.min(self.body.len());
        let holding = self
            .leaves
            .iter()
            .filter(|leaf| leaf.start <= marker && marker < leaf.end)
            .min_by_key(|leaf| leaf.len());
        let Some(leaf) = holding else {
            return line_around(self.body, marker);
        };
        let opens_its_block = self.body[leaf.start..marker].trim().is_empty();
        let after_a_fence = self.fenced.iter().find(|fence| {
            fence.end <= leaf.start
                && matches!(&self.body[fence.end..leaf.start], "" | "\n" | "\r\n" | "\r")
        });
        let range = match after_a_fence {
            Some(fence) if opens_its_block => fence.clone(),
            _ => leaf.clone(),
        };
        let kept = self.body[range.clone()]
            .trim_end_matches(['\n', '\r'])
            .len();
        range.start..range.start + kept
    }

    /// Resolve a heading-addressed section to the byte ranges it owns,
    /// through [`crate::resolve_section`] over this scan's headings.
    pub fn resolve_section(
        &self,
        address: SectionAddress<'_>,
    ) -> Result<SectionSpan, SectionError> {
        resolve_section(&self.headings, self.body, address)
    }

    /// The inline Markdown links as facts, positions counted once across the
    /// body.
    fn markdown_link_facts(&self) -> Vec<Link> {
        let mut cursor = LineCursor::new(self.body);
        self.markdown_links
            .iter()
            .map(|token| {
                markdown_link(
                    &self.body[token.range.clone()],
                    &token.destination,
                    &token.text,
                    token.text_end,
                    cursor.span_at(token.range.start),
                )
            })
            .collect()
    }

    /// The ranges a tag may not be read out of: code, every `[[…]]` token, and
    /// every link or image construct the CommonMark parse recognized —
    /// reference-style links and their definition lines included.
    fn opaque_to_tags(&self) -> Vec<Range<usize>> {
        let mut ranges = self.code_ranges.clone();
        ranges.extend(wikilink_ranges(self.body, &self.code_ranges));
        ranges.extend(self.construct_ranges.iter().cloned());
        ranges.extend(definition_lines(self.body, &self.code_ranges));
        merge_ranges(ranges)
    }
}

/// The line holding `at`, its break left out: from the break before it, or
/// the start of the body, to the break after it, or the end of the body.
fn line_around(body: &str, at: usize) -> Range<usize> {
    let is_break = |byte: &u8| matches!(byte, b'\n' | b'\r');
    let start = body.as_bytes()[..at]
        .iter()
        .rposition(is_break)
        .map_or(0, |found| found + 1);
    let end = body.as_bytes()[at..]
        .iter()
        .position(is_break)
        .map_or(body.len(), |found| at + found);
    start..end
}

/// The byte range of every link reference definition line (`[label]: target`),
/// code excluded.
///
/// A definition line produces no parser event, so it is the one recognized
/// construct whose bytes the one pass cannot hand over. The rule is therefore
/// the line shape CommonMark gives a definition its own: up to three spaces of
/// indent, `[`, a label, then `]:`. Masking the line is deliberate over-reach
/// in the safe direction — the alternative is minting a tag out of the
/// fragment of every URL a document defines. A footnote definition
/// (`[^fn]: prose`) has that shape too and is masked with them.
///
/// The shape test reads a whole line and the mask covers a whole line, so lines
/// arrive on the break rule the rest of the crate counts — a chunk spanning a
/// break both over-masks (the prose sharing it loses its tags) and under-masks
/// (a definition behind prose is never tested, and its URL fragment is minted
/// as a tag).
fn definition_lines(body: &str, code_ranges: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut line_start = 0;
    for line in split_lines_inclusive(body) {
        if is_definition_line(line) {
            let range = line_start..line_start + line.len();
            if !overlaps_any(code_ranges, &range) {
                lines.push(range);
            }
        }
        line_start += line.len();
    }
    lines
}

/// Whether `line` carries a link reference definition.
///
/// A definition is a leaf block, so it sits wherever a leaf block sits: inside
/// a block quote, inside a list item, inside both at once. Every container
/// marker opening the line is stripped before the definition's own shape is
/// tested, because `> [label]: ./target/#faq` defines a label exactly as the
/// unprefixed line does and a shape test anchored at column zero reads its
/// fragment as a tag.
fn is_definition_line(line: &str) -> bool {
    let mut rest = line;
    loop {
        let indent = rest.len() - rest.trim_start_matches(' ').len();
        if indent > 3 {
            return false;
        }
        rest = &rest[indent..];
        if let Some(after) = rest.strip_prefix('>').or_else(|| strip_list_marker(rest)) {
            rest = after;
            continue;
        }
        return rest.starts_with('[')
            && matches!(rest.find(']'), Some(close) if rest[close + 1..].starts_with(':'));
    }
}

/// Strip one list marker — a `-`, `*` or `+` bullet, or a `1.` / `1)` ordinal —
/// leaving the space that follows it to be counted as the content's indent.
///
/// The trailing whitespace is what makes a marker a marker: `-[label]: x` opens
/// no list item and is ordinary paragraph text.
fn strip_list_marker(rest: &str) -> Option<&str> {
    let after = match rest.as_bytes().first()? {
        b'-' | b'*' | b'+' => &rest[1..],
        b'0'..=b'9' => {
            let digits = rest.len()
                - rest
                    .trim_start_matches(|ch: char| ch.is_ascii_digit())
                    .len();
            if !matches!(rest.as_bytes().get(digits), Some(b'.' | b')')) {
                return None;
            }
            &rest[digits + 1..]
        }
        _ => return None,
    };
    after.starts_with(' ').then_some(after)
}

/// A heading being accumulated across the events that make it up.
struct ActiveHeading {
    level: u8,
    text: String,
    start: usize,
    inside_container: bool,
}

/// An inline Markdown link being accumulated across the events that make it
/// up.
struct ActiveLink {
    range: Range<usize>,
    /// The body offset just past the furthest event seen inside the link,
    /// which is where its bracket text ends.
    text_end: usize,
    destination: String,
    text: String,
}

/// What the one pass saw of an inline Markdown link, before it is priced into
/// a fact with a span.
#[derive(Debug, Clone)]
struct MarkdownToken {
    range: Range<usize>,
    /// Where the bracket text ends, relative to the token's first byte.
    text_end: usize,
    destination: String,
    text: String,
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

/// Sort ranges and coalesce every overlap, producing the ascending,
/// non-overlapping, non-empty form [`overlaps_any`] requires.
///
/// Link tokens of the two families can overlap each other — `[[a]](b)` is one
/// wikilink and one Markdown link sharing bytes — so a mask built from both
/// is unioned rather than concatenated.
fn merge_ranges(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|range| (range.start, range.end));
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if range.start >= range.end {
            continue;
        }
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// Whether `query` overlaps any of `ranges`, which are ascending,
/// non-overlapping and non-empty.
///
/// A linear scan makes every masked-construct question cost the document's
/// code, so a document that is mostly code pays that once per link, per tag,
/// per block id and per heading. Binary search over the ordering the one pass
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
