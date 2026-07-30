//! Link syntax — token-level recognition of the two written link families.
//!
//! Both families decompose into one fact ([`Link`]): a family, an optional
//! protocol, a target stem, a title, a heading anchor or block reference, and
//! a span. The grammars are:
//!
//! ```text
//! [[ [protocol://] stem [#anchor | #^block-ref] [| title] ]]
//! [title]( [protocol://] stem [#anchor | #^block-ref] )
//! ```
//!
//! What differs between them is how a resolver is meant to read the stem, and
//! that difference is carried on the fact as [`AddressingMode`]: a wikilink
//! stem is a path *suffix*, an inline Markdown link's is a path *relative to
//! the containing document*. Neither reading happens here. Matching a target
//! to a document — suffix resolution, path joining, ambiguity, containment,
//! percent-decoding — is **resolution**, and a [`Link`] carries no vault
//! knowledge to do it with.
//!
//! # Recognition is lossless
//!
//! `[[x]]` and `[[vault://x]]` are two different facts and stay two different
//! facts: the protocol is recorded where it was written and never supplied,
//! never dropped, and never normalized away. The same holds of the raw bytes
//! and the span, which is what lets a rewrite put new bytes over the stem and
//! leave every other byte of the token alone.

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

use crate::body::overlaps_any;
use crate::span::{LineCursor, SourceSpan};

static WIKILINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(!?)\[\[([^\]]+)\]\]").expect("valid wikilink regex"));

static BLOCK_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|\s)\^([A-Za-z0-9_-]+)\s*$").expect("valid block id regex"));

/// The written form a link was recognized from.
///
/// Plain rather than `#[non_exhaustive]`: a third family arriving should break
/// every `match` that dispatches on this one, because a family whose
/// addressing nobody chose is the defect this enum exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkFamily {
    /// `[[…]]`, the vault idiom.
    Wikilink,
    /// `[title](target)`, an inline Markdown link. Reference-style links,
    /// autolinks and images are not this family and produce no link fact —
    /// see [`crate::BodyScan`].
    Markdown,
}

/// How a family's stems address a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddressingMode {
    /// A right-to-left, segment-aligned path suffix: `glossary` addresses any
    /// `**/glossary.md`, `norn/glossary` only `**/norn/glossary.md`. Never
    /// relative to the document it was written in.
    Suffix,
    /// A filesystem path resolved against the containing document's directory
    /// — `./`, `../`, or a bare segment — and against the vault root when the
    /// path is rooted. Exact, with no ambiguity class.
    RelativePath,
}

impl LinkFamily {
    /// The addressing mode this family's stems are read with.
    pub fn addressing(self) -> AddressingMode {
        match self {
            LinkFamily::Wikilink => AddressingMode::Suffix,
            LinkFamily::Markdown => AddressingMode::RelativePath,
        }
    }
}

/// A recognized link token, decomposed and unresolved.
///
/// For `![[Note#Heading|Shown]]`: `family = Wikilink`, `embed = true`,
/// `target = "Note"`, `anchor = Some("Heading")`, `title = Some("Shown")`,
/// `block_ref = None`, `protocol = None`. For `[Shown](./note.md#Heading)`:
/// `family = Markdown`, `target = "./note.md"`, and the rest reads the same
/// way. A block reference `[[Note#^blk]]` carries `block_ref = Some("blk")`
/// and no anchor; a same-note reference `[[#Heading]]` has an empty target.
///
/// Fields are plain and public. The crate is pre-1.0 and a leaf, so adding one
/// is a cheap breaking change a caller should see, and `#[non_exhaustive]`
/// would buy nothing a public field cannot already be written to: the one
/// relationship that has to hold — `stem_range` indexing `raw` — is checked
/// where it is used rather than by the type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// Which grammar recognized this token.
    pub family: LinkFamily,
    /// The exact matched text: for a wikilink the delimiters and any leading
    /// `!`, for a Markdown link `[` through the closing `)`.
    pub raw: String,
    /// True for a wikilink embed (`![[…]]`). Always false for a Markdown
    /// link: `![alt](x)` is an image, and images are not links here.
    pub embed: bool,
    /// The `protocol://` prefix, sentinel excluded, when the target opens with
    /// a recognized one. `None` means written-without-a-protocol and is a
    /// distinct fact from any protocol, `vault` included — nothing here
    /// supplies a default.
    pub protocol: Option<String>,
    /// The target stem: the address with the protocol prefix, the fragment and
    /// the title removed. Empty for a same-note anchor or block reference.
    pub target: String,
    /// Where [`Link::target`]'s bytes sit inside [`Link::raw`], which is what
    /// a rewrite writes over.
    ///
    /// `None` when the stem's bytes cannot be named as one span of `raw`: a
    /// Markdown destination may be written with backslash escapes or inside
    /// `<…>`, so the parsed target need not appear literally in the source,
    /// and a span that is not certainly right is absent rather than guessed.
    pub stem_range: Option<Range<usize>>,
    /// The display title, trimmed — after `|` for a wikilink, the bracket text
    /// for a Markdown link. A Markdown link always has one, `Some("")`
    /// included, because its brackets are always written.
    pub title: Option<String>,
    /// A heading anchor after `#`. Mutually exclusive with `block_ref`.
    pub anchor: Option<String>,
    /// A block reference after `#^`.
    pub block_ref: Option<String>,
    /// Where the whole token begins.
    pub span: SourceSpan,
}

impl Link {
    /// The token's byte range in the text it was parsed from.
    pub fn range(&self) -> Range<usize> {
        self.span.byte_offset..self.span.byte_offset + self.raw.len()
    }

    /// How this link's stem addresses a document, which is its family's.
    ///
    /// Derived rather than stored: two fields that must agree are two fields
    /// that can disagree, and the family is the one that was observed.
    pub fn addressing(&self) -> AddressingMode {
        self.family.addressing()
    }
}

/// Parse `[[…]]` tokens in arbitrary text with no code exclusion — for text
/// that is not a Markdown body, such as a frontmatter string value. A Markdown
/// body goes through [`crate::BodyScan::wikilinks`], where code is opaque.
///
/// Wikilinks are the only link family read out of a frontmatter value. A
/// `[title](target)` string in a property is inert text: the Markdown form is
/// body syntax, an editor's properties recognize the wikilink form only, and a
/// crate that disagreed with the editor about whether a property holds a link
/// would make link-graph membership an argument. Opting a property into the
/// link graph is what writing the wikilink form does.
pub fn parse_wikilinks_in_text(text: &str) -> Vec<Link> {
    parse_tokens(text, &[])
}

/// Rewrite selected `[[…]]` tokens in arbitrary text with no code exclusion.
/// The Markdown-body counterpart is [`crate::BodyScan::splice_wikilinks`].
pub fn splice_wikilinks_in_text(
    text: &str,
    replace: impl FnMut(&Link) -> Option<String>,
) -> String {
    splice_tokens(text, &parse_wikilinks_in_text(text), replace)
}

pub(crate) fn parse_tokens(text: &str, ignored: &[Range<usize>]) -> Vec<Link> {
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
            let inner = captures.get(2)?;
            let inner_text = inner.as_str();
            let (target_part, title) = inner_text
                .split_once('|')
                .map_or((inner_text, None), |(target, title)| {
                    (target, Some(title.trim().to_string()))
                });
            let padding = target_part.len() - target_part.trim_start().len();
            let (addressed, anchor, block_ref) = split_wikilink_target(target_part.trim());
            let (protocol, target) = split_protocol(&addressed);
            // The stem sits at a known offset inside the token: past the
            // fences and any embed marker, past the padding the author wrote,
            // past the protocol prefix. A rewrite writes over exactly that, so
            // padding, protocol, fragment and title bytes are never in the
            // edited range.
            let stem_start = inner.start() - full_match.start()
                + padding
                + protocol.as_ref().map_or(0, |scheme| scheme.len() + 3);

            Some(Link {
                family: LinkFamily::Wikilink,
                raw,
                embed,
                protocol,
                stem_range: Some(stem_start..stem_start + target.len()),
                target,
                title,
                anchor,
                block_ref,
                span: cursor.span_at(full_match.start()),
            })
        })
        .collect()
}

/// Build the fact for one inline Markdown link.
///
/// `destination` is the CommonMark parse's link destination and `text` its
/// bracket text, so backslash escapes and entity references are already
/// resolved — the document is read through one parser, and re-lexing what that
/// parser already answered is how two readings drift. Percent-encoding is
/// *not* resolved, here or anywhere: the fragment split runs over the
/// destination as written, so `note%23draft.md` names a file and carries no
/// anchor.
pub(crate) fn markdown_link(raw: &str, destination: &str, text: &str, span: SourceSpan) -> Link {
    let (addressed, anchor, block_ref) = split_wikilink_target(destination);
    let (protocol, target) = split_protocol(&addressed);
    Link {
        family: LinkFamily::Markdown,
        raw: raw.to_string(),
        embed: false,
        protocol,
        target,
        // The destination's source bytes are not separately nameable, so the
        // stem has no sub-span and a wikilink rewrite refuses this link.
        stem_range: None,
        title: Some(text.trim().to_string()),
        anchor,
        block_ref,
        span,
    }
}

pub(crate) fn splice_tokens(
    text: &str,
    links: &[Link],
    mut replace: impl FnMut(&Link) -> Option<String>,
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

/// Split a recognized protocol prefix off an addressed target, returning the
/// protocol and the stem that follows it.
///
/// The sentinel is the whole `://`. A protocol is an RFC 3986 scheme
/// identifier — `[a-z][a-z0-9+.-]*` — written in lowercase, followed by `://`
/// and a non-empty remainder, and nothing else is one: `note:draft`,
/// `todo: buy milk` and `HTTPS://x` are ordinary targets, colons and all.
/// Recognizing too little is recoverable — the target still reads as itself —
/// while recognizing too much hides a real target behind a protocol nobody
/// wrote, so the grammar is deliberately narrow.
///
/// The `//` is a recognition sentinel here rather than a claim about an
/// authority component: `vault://Note` names no host.
fn split_protocol(addressed: &str) -> (Option<String>, String) {
    let Some((scheme, stem)) = addressed.split_once("://") else {
        return (None, addressed.to_string());
    };
    if stem.is_empty() || !is_scheme_identifier(scheme) {
        return (None, addressed.to_string());
    }
    (Some(scheme.to_string()), stem.to_string())
}

/// Whether `candidate` is a lowercase RFC 3986 scheme identifier.
fn is_scheme_identifier(candidate: &str) -> bool {
    let mut chars = candidate.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '+' | '.' | '-')
        })
}

/// Split a reference target into `(target, anchor, block_ref)`.
///
/// The single splitter, and an inline Markdown destination is split by it too,
/// so the two families cannot disagree about what a fragment is. Only the
/// first `#` splits, so extra hashes stay inside the anchor. A `#^id` fragment
/// is a block reference; any other `#frag` is a heading anchor. **A bare `^`
/// is an ordinary target character** — a caret is a block sigil only after a
/// hash — so `a^b` is a target named `a^b` and not a block reference to `b`.
///
/// A protocol prefix stays with the returned target; [`Link::protocol`] is
/// where recognition happens.
///
/// What an anchor *addresses* is the family's business rather than this
/// function's: a wikilink anchor is heading text, a Markdown fragment is a
/// heading slug, and the raw fragment recorded here is what both readings
/// start from.
pub fn split_wikilink_target(raw: &str) -> (String, Option<String>, Option<String>) {
    match raw.split_once('#') {
        Some((target, reference)) if reference.starts_with('^') => {
            (target.to_string(), None, Some(reference[1..].to_string()))
        }
        Some((target, anchor)) => (target.to_string(), Some(anchor.to_string()), None),
        None => (raw.to_string(), None, None),
    }
}

/// Whether `target` can be written as a wikilink stem — reconstructing a link
/// with it and re-parsing yields the same target.
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
/// mid-paragraph.
///
/// A target opening with a recognized `protocol://` prefix is refused for the
/// same reason: written as a stem it re-parses as a protocol and a shorter
/// stem, which is a different fact. A rewrite preserves the protocol the
/// author wrote, and changing that protocol is not a target rewrite.
///
/// Every other byte, a bare `^` included, round-trips.
pub fn wikilink_target_is_representable(target: &str) -> bool {
    !target.trim().is_empty()
        && !target.contains(['|', '#', '[', ']', '\n', '\r'])
        && split_protocol(target).0.is_none()
}

/// Reconstruct a wikilink's text with `new_target` in place of its stem.
///
/// **Only the stem's bytes change.** The result is the token's own bytes with
/// `new_target` spliced over [`Link::stem_range`], so the embed marker, the
/// padding the author wrote, the protocol prefix, the fragment and the title
/// all survive as written: `[[ Old | Title ]]` becomes `[[ New | Title ]]`.
/// Minimal diffs are this crate's identity, and a rename cascade whose hunks
/// read as exactly the rename is what that identity is for.
///
/// Returns `None` when `new_target` is not representable
/// ([`wikilink_target_is_representable`]): emitting it would corrupt the link
/// into a different shape, so the caller refuses or skips instead. Also `None`
/// for a link with no usable stem span — an inline Markdown link, or a
/// hand-built fact whose span does not index its own bytes. Rewriting a
/// Markdown link is not a token-level edit at all: its target is relative to
/// the document it sits in, so one move produces different bytes per
/// referencing file.
pub fn reconstruct_wikilink(link: &Link, new_target: &str) -> Option<String> {
    if link.family != LinkFamily::Wikilink || !wikilink_target_is_representable(new_target) {
        return None;
    }
    let stem = link.stem_range.clone()?;
    if stem.start > stem.end
        || stem.end > link.raw.len()
        || !link.raw.is_char_boundary(stem.start)
        || !link.raw.is_char_boundary(stem.end)
    {
        return None;
    }
    let mut out = String::with_capacity(link.raw.len() - stem.len() + new_target.len());
    out.push_str(&link.raw[..stem.start]);
    out.push_str(new_target);
    out.push_str(&link.raw[stem.end..]);
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

/// Every `[[…]]` token's byte range, code exclusion applied.
///
/// The ranges a `#tag` scan must not read into, produced without building the
/// facts: a hash inside a link token is that link's syntax.
pub(crate) fn wikilink_ranges(text: &str, ignored: &[Range<usize>]) -> Vec<Range<usize>> {
    WIKILINK_RE
        .find_iter(text)
        .map(|found| found.start()..found.end())
        .filter(|range| !overlaps_any(ignored, range))
        .collect()
}
