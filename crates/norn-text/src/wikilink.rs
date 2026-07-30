//! Wikilink syntax — token-level recognition of `[[…]]` references.
//!
//! This is the lexical layer only. It recognizes a wikilink and decomposes it
//! into target, title, heading anchor and block reference, plus whether it is
//! an embed (`![[…]]`), and it reads block-id definitions (`^block-id`). The
//! grammar it recognizes is:
//!
//! ```text
//! [[ target [#anchor | #^block-ref] [| title] ]]
//! ```
//!
//! Matching a target to a document — path resolution, ambiguity, titles as an
//! alternate address — is **resolution**, not syntax, and lives outside this
//! crate. A [`Wikilink`] carries no vault knowledge.

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

use crate::body::overlaps_any;
use crate::span::{LineCursor, SourceSpan};

static WIKILINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(!?)\[\[([^\]]+)\]\]").expect("valid wikilink regex"));

static BLOCK_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|\s)\^([A-Za-z0-9_-]+)\s*$").expect("valid block id regex"));

/// A recognized wikilink token, decomposed and unresolved.
///
/// For `![[Note#Heading|Shown]]`: `embed = true`, `target = "Note"`,
/// `anchor = Some("Heading")`, `title = Some("Shown")`, `block_ref = None`.
/// For a block reference `[[Note#^blk]]`: `block_ref = Some("blk")` and
/// `anchor = None`. A same-note reference like `[[#Heading]]` has an empty
/// target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wikilink {
    /// The exact matched text, delimiters and any leading `!` included.
    pub raw: String,
    /// True for an embed (`![[…]]`).
    pub embed: bool,
    /// The link target. Empty for a same-note anchor or block reference.
    pub target: String,
    /// The display title after `|`, trimmed.
    pub title: Option<String>,
    /// A heading anchor after `#`. Mutually exclusive with `block_ref`.
    pub anchor: Option<String>,
    /// A block reference after `#^`.
    pub block_ref: Option<String>,
    /// Where the whole token begins.
    pub span: SourceSpan,
}

impl Wikilink {
    /// The token's byte range in the text it was parsed from.
    pub fn range(&self) -> Range<usize> {
        self.span.byte_offset..self.span.byte_offset + self.raw.len()
    }
}

/// Parse `[[…]]` tokens in arbitrary text with no code exclusion — for text
/// that is not a Markdown body, such as a frontmatter string value. A Markdown
/// body goes through [`crate::BodyScan::wikilinks`], where code is opaque.
pub fn parse_wikilinks_in_text(text: &str) -> Vec<Wikilink> {
    parse_tokens(text, &[])
}

/// Rewrite selected `[[…]]` tokens in arbitrary text with no code exclusion.
/// The Markdown-body counterpart is [`crate::BodyScan::splice_wikilinks`].
pub fn splice_wikilinks_in_text(
    text: &str,
    replace: impl FnMut(&Wikilink) -> Option<String>,
) -> String {
    splice_tokens(text, &parse_wikilinks_in_text(text), replace)
}

pub(crate) fn parse_tokens(text: &str, ignored: &[Range<usize>]) -> Vec<Wikilink> {
    // Matches arrive in ascending order, so their positions are counted once
    // across the text rather than once per token.
    let mut cursor = LineCursor::new(text);
    WIKILINK_RE
        .captures_iter(text)
        .filter_map(|captures| {
            let full_match = captures.get(0)?;
            let match_range = full_match.start()..full_match.end();
            if overlaps_any(ignored, &match_range) {
                return None;
            }

            let raw = full_match.as_str().to_string();
            let embed = captures.get(1).is_some_and(|m| m.as_str() == "!");
            let inner = captures.get(2)?.as_str();
            let (target_part, title) = inner
                .split_once('|')
                .map_or((inner, None), |(target, title)| {
                    (target, Some(title.trim().to_string()))
                });
            let (target, anchor, block_ref) = split_wikilink_target(target_part.trim());

            Some(Wikilink {
                raw,
                embed,
                target,
                title,
                anchor,
                block_ref,
                span: cursor.span_at(full_match.start()),
            })
        })
        .collect()
}

pub(crate) fn splice_tokens(
    text: &str,
    links: &[Wikilink],
    mut replace: impl FnMut(&Wikilink) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for link in links {
        let range = link.range();
        let Some(replacement) = replace(link) else {
            continue;
        };
        // Matches are non-overlapping and left-to-right, so spans ascend and
        // the cursor advances monotonically.
        out.push_str(&text[cursor..range.start]);
        out.push_str(&replacement);
        cursor = range.end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// Split a reference target into `(target, anchor, block_ref)`.
///
/// The single splitter. Only the first `#` splits, so extra hashes stay inside
/// the anchor. A `#^id` fragment is a block reference; any other `#frag` is a
/// heading anchor. **A bare `^` is an ordinary target character** — a caret is
/// a block sigil only after a hash — so `a^b` is a target named `a^b` and not
/// a block reference to `b`.
pub fn split_wikilink_target(raw: &str) -> (String, Option<String>, Option<String>) {
    match raw.split_once('#') {
        Some((target, reference)) if reference.starts_with('^') => {
            (target.to_string(), None, Some(reference[1..].to_string()))
        }
        Some((target, anchor)) => (target.to_string(), Some(anchor.to_string()), None),
        None => (raw.to_string(), None, None),
    }
}

/// Whether `target` can be written as a wikilink target — reconstructing a
/// link with it and re-parsing yields the same target.
///
/// The delimiter bytes a target must not contain are `|` (begins the title),
/// `#` (begins the anchor or block reference) and `[` / `]` (the fences). A
/// target carrying one would re-parse as a different link shape — `a|b` reads
/// as target `a` with title `b`.
///
/// A target must also carry something, and carry it on one line. `[[]]` and
/// `[[   ]]` are not tokens this grammar recognizes at all, so a link written
/// with one vanishes; a target holding `\n` or `\r` splices a line break into
/// whatever the link sat in, which ends a table row and leaves a blockquote
/// mid-paragraph. Every other byte, a bare `^` included, round-trips.
pub fn wikilink_target_is_representable(target: &str) -> bool {
    !target.trim().is_empty() && !target.contains(['|', '#', '[', ']', '\n', '\r'])
}

/// Reconstruct a wikilink's text with `new_target` in place of its target,
/// preserving the embed marker, the anchor or block reference, and the title.
///
/// Returns `None` when `new_target` is not representable
/// ([`wikilink_target_is_representable`]): emitting it would corrupt the link
/// into a different shape, so the caller refuses or skips instead.
///
/// The title and anchor are re-emitted from their parsed, trimmed forms, so a
/// padded link (`[[ Target | Shown ]]`) canonicalizes. A tight link is
/// unchanged.
pub fn reconstruct_wikilink(link: &Wikilink, new_target: &str) -> Option<String> {
    if !wikilink_target_is_representable(new_target) {
        return None;
    }
    let mut out = String::new();
    if link.embed {
        out.push('!');
    }
    out.push_str("[[");
    out.push_str(new_target);
    if let Some(block_ref) = &link.block_ref {
        out.push_str("#^");
        out.push_str(block_ref);
    } else if let Some(anchor) = &link.anchor {
        out.push('#');
        out.push_str(anchor);
    }
    if let Some(title) = &link.title {
        out.push('|');
        out.push_str(title);
    }
    out.push_str("]]");
    Some(out)
}

pub(crate) fn parse_block_ids_in(body: &str, ignored: &[Range<usize>]) -> Vec<String> {
    let mut block_ids = Vec::new();
    let mut line_start = 0;
    for line in body.split_inclusive('\n') {
        if let Some(block_id) = BLOCK_ID_RE.captures(line).and_then(|c| c.get(1)) {
            let id_range = (line_start + block_id.start())..(line_start + block_id.end());
            if !overlaps_any(ignored, &id_range) {
                block_ids.push(block_id.as_str().to_string());
            }
        }
        line_start += line.len();
    }
    block_ids
}
