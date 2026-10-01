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
//! [`Link::resolution`] is where that is decided. **Protocol first, family
//! second**: a target opening with a recognized `protocol://` prefix addresses
//! whatever that protocol addresses, and the form it was written in says
//! nothing about it. Only a target with no protocol falls through to its
//! family — a wikilink stem is a path *suffix*, an inline Markdown link's is a
//! path *relative to the containing document*.
//!
//! None of those readings happens here. Matching a target to a document —
//! suffix resolution, path joining, ambiguity, containment, a path's
//! percent-decoding — is **resolution**, and a [`Link`] carries no vault
//! knowledge to do it with. A Markdown link's fragment is the one part decoded
//! here, once ([`Link::anchor`]), because that family percent-encodes it and
//! decoding it needs no vault.
//!
//! # Recognition is lossless
//!
//! `[[x]]` and `[[vault://x]]` are two different facts and stay two different
//! facts: the protocol is recorded where it was written and never supplied,
//! never dropped, and never normalized away. The same holds of the raw bytes
//! — a decoded fragment included, which [`Link::raw`] keeps as written — and
//! the span, which is what lets a rewrite put new bytes over the stem and
//! leave every other byte of the token alone.

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

use crate::body::overlaps_any;
use crate::span::{LineCursor, SourceSpan, split_lines_inclusive};

static WIKILINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(!?)\[\[([^\]]+)\]\]").expect("valid wikilink regex"));

/// The one protocol identifier this crate names. It is reserved rather than
/// resolved: an absent protocol is the vault protocol wherever protocols come
/// to mean anything, which is why a `vault://` stem is still a vault path and
/// rewrites like one.
const VAULT_PROTOCOL: &str = "vault";

static BLOCK_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|\s)\^([A-Za-z0-9_-]+)\s*$").expect("valid block id regex"));

/// The written form a link was recognized from.
///
/// Plain rather than `#[non_exhaustive]`: a third family arriving should break
/// every `match` that dispatches on this one, because a family whose
/// resolution nobody chose is the defect this enum exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkFamily {
    /// `[[…]]`, the vault idiom. A protocol-free wikilink stem is a
    /// right-to-left, segment-aligned path *suffix*: `glossary` addresses any
    /// `**/glossary.md`, `norn/glossary` only `**/norn/glossary.md`, and
    /// never anything relative to the document it was written in.
    Wikilink,
    /// `[title](target)`, an inline Markdown link. A protocol-free destination
    /// is a filesystem path resolved against the containing document's
    /// directory — `./`, `../`, or a bare segment — and against the vault root
    /// when the path is rooted. Exact, with no ambiguity class.
    ///
    /// Reference-style links, autolinks and images are not this family and
    /// produce no link fact — see [`crate::BodyScan`].
    Markdown,
}

/// How a resolver is meant to reach what a link addresses.
///
/// **Protocol first, family second.** A recognized `protocol://` prefix
/// shadows the family, because the family says which grammar the author wrote
/// and the protocol says what the address is: `[t](https://example.com)` is
/// not a relative path and `[[https://x|Docs]]` is not a suffix address, so
/// deriving the reading from the family alone states a false fact about both.
///
/// Plain rather than `#[non_exhaustive]`: matching this exhaustively is the
/// point, and a caller that has not decided what to do with a protocol should
/// fail to compile rather than fall into a default arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resolution<'a> {
    /// The target was written with this `protocol://` prefix, sentinel
    /// excluded. What the protocol addresses is the protocol's business;
    /// nothing in this crate knows. The reserved `vault` arrives here too — it
    /// is a written protocol like any other until a layer means something by
    /// it.
    Protocol(&'a str),
    /// A path suffix, per [`LinkFamily::Wikilink`].
    Suffix,
    /// A path relative to the containing document, per
    /// [`LinkFamily::Markdown`].
    RelativePath,
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
    /// A wikilink always has one. A Markdown link has one when its destination
    /// stands in the token exactly as the parse read it, which is the ordinary
    /// case, inside `<…>` or not; `None` when it does not, because a
    /// destination written with backslash escapes or entity references is not
    /// made of the bytes that produced it, and a span that is not certainly
    /// right is absent rather than guessed.
    ///
    /// A span being present is not a licence to splice one family's grammar
    /// over the other's: a rewrite names one family, and a wikilink rewrite
    /// never reaches a Markdown link, whose target is relative to the
    /// document it sits in. See [`Document::rewrite_links`](crate::Document::rewrite_links).
    pub stem_range: Option<Range<usize>>,
    /// The display title, trimmed — after `|` for a wikilink, the bracket text
    /// for a Markdown link. A Markdown link always has one, `Some("")`
    /// included, because its brackets are always written.
    pub title: Option<String>,
    /// A heading anchor after `#`. Mutually exclusive with `block_ref`. A
    /// wikilink's is as written; a Markdown link's is percent-decoded once,
    /// because that family percent-encodes its fragment.
    pub anchor: Option<String>,
    /// A block reference after `#^`, as written for a wikilink and
    /// percent-decoded once for a Markdown link, as [`Link::anchor`] is.
    pub block_ref: Option<String>,
    /// Where the whole token begins.
    pub span: SourceSpan,
}

impl Link {
    /// The token's byte range in the text it was parsed from.
    pub fn range(&self) -> Range<usize> {
        self.span.byte_offset..self.span.byte_offset + self.raw.len()
    }

    /// How this link's stem is to be resolved: its protocol when it was
    /// written with one, and otherwise its family's reading.
    ///
    /// Derived rather than stored, because two fields that must agree are two
    /// fields that can disagree, and the protocol and the family are the ones
    /// that were observed.
    pub fn resolution(&self) -> Resolution<'_> {
        match self.protocol.as_deref() {
            Some(scheme) => Resolution::Protocol(scheme),
            None => match self.family {
                LinkFamily::Wikilink => Resolution::Suffix,
                LinkFamily::Markdown => Resolution::RelativePath,
            },
        }
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
                    (target, Some(title.trim()))
                });
            let padding = target_part.len() - target_part.trim_start().len();
            let (addressed, anchor, block_ref) = split_fragment(target_part.trim());
            // Whitespace between the stem and the fragment is padding, not
            // part of the address: `[[note #Head]]` addresses `note`, and no
            // resolver will ever match a target with a space welded to its
            // end. Trimming it here — rather than in the caller — is also what
            // keeps it outside `stem_range`, so a rewrite leaves it standing.
            let addressed = addressed.trim_end();
            let (protocol, stem) = split_protocol(addressed);
            // A sentinel is a prefix rather than a separator, so whitespace
            // behind it is padding on the stem's left exactly as the space in
            // `[[note #Head]]` is padding on its right: `[[vault:// Note]]`
            // addresses `Note`. Trimming both sides is also what lets the token
            // round-trip to itself — a padded target is not representable, so
            // an untrimmed one reads as a target no rewrite would accept.
            let stem_padding = stem.len() - stem.trim_start().len();
            let target = stem[stem_padding..].to_string();
            // The stem sits at a known offset inside the token: past the
            // fences and any embed marker, past the padding the author wrote,
            // past the protocol prefix and the padding behind it. A rewrite
            // writes over exactly that, so padding, protocol, fragment and
            // title bytes are never in the edited range.
            let stem_start = inner.start() - full_match.start()
                + padding
                + protocol.as_ref().map_or(0, |scheme| scheme.len() + 3)
                + stem_padding;

            Some(Link {
                family: LinkFamily::Wikilink,
                raw,
                embed,
                protocol,
                stem_range: Some(stem_start..stem_start + target.len()),
                target,
                title: title.map(str::to_string),
                anchor: anchor.map(str::to_string),
                block_ref: block_ref.map(str::to_string),
                span: cursor.span_at(full_match.start()),
            })
        })
        .collect()
}

/// Build the fact for one inline Markdown link.
///
/// `destination` is the CommonMark parse's link destination and `text` its
/// bracket text, so backslash escapes and entity references are already
/// resolved — the document is read through one parser, and re-deciding what
/// that parser already answered is how two readings drift. `text_end` is where
/// the bracket text ended, which is the parse's answer to the one question the
/// destination bytes cannot be found without.
///
/// The fragment split runs over those source bytes rather than over the
/// resolved destination, because the two disagree about what a `#` is: the
/// parse resolves `\#` to a hash, and a split over its answer reads the real
/// filename `note\#draft.md` as a note with a `draft.md` anchor. An escaped
/// hash is a literal; an unescaped one opens a fragment, inside `<…>` as well
/// as outside it. Percent-encoding is not read before the split, so
/// `note%23draft.md` names a file and carries no anchor.
///
/// **The fragment is percent-decoded once, after the split** ([`decoded`]):
/// a Markdown link is the family that percent-encodes its fragment, so this
/// is the one place the family and the encoding are both known. A `%23` it
/// decodes is a `#` in the anchor like a written one. The target is recorded
/// as written; reading it as a path is resolution's.
pub(crate) fn markdown_link(
    raw: &str,
    destination: &str,
    text: &str,
    text_end: usize,
    span: SourceSpan,
) -> Link {
    let written = destination_span(raw, text_end);
    let (hash, stem_origin) = match &written {
        // The source carries the destination byte for byte, so the split and
        // the stem's own bytes are both nameable inside the token. Angle
        // brackets are not the destination's bytes but delimiters around
        // them, like the parentheses, so a bracketed destination written
        // without escapes is carried byte for byte too.
        Some(found) if &raw[found.clone()] == destination => {
            (destination.find('#'), Some(found.start))
        }
        // The parse resolved something. The split still belongs to the source,
        // so it is located there and mapped onto the resolved destination —
        // and the stem's bytes are no longer nameable, because the bytes that
        // produced them are not the bytes it is made of.
        Some(found) => (
            unescaped_hash(&raw[found.clone()], destination).unwrap_or(destination.find('#')),
            None,
        ),
        // The destination could not be located in the token at all, so there
        // is nothing to read but what the parse reported.
        None => (destination.find('#'), None),
    };

    let (addressed, anchor, block_ref) = split_at_hash(destination, hash);
    let (protocol, target) = split_protocol(addressed);
    let stem_start =
        stem_origin.map(|start| start + protocol.as_ref().map_or(0, |scheme| scheme.len() + 3));
    Link {
        family: LinkFamily::Markdown,
        raw: raw.to_string(),
        embed: false,
        protocol,
        stem_range: stem_start.map(|start| start..start + target.len()),
        target,
        title: Some(text.trim().to_string()),
        anchor: anchor.map(decoded),
        block_ref: block_ref.map(decoded),
        span,
    }
}

/// `fragment` with every `%` followed by two hexadecimal digits, in either
/// case, read as the byte they spell, or `fragment` as written where the
/// bytes that makes are not UTF-8. A `%` not followed by two hexadecimal
/// digits is itself.
///
/// `norn-store` decodes a Markdown target's path segments by the same rule
/// with its own copy of this loop: the store depends on no text layer, and a
/// segment that decodes to no UTF-8 names no document there rather than
/// standing as written.
fn decoded(fragment: &str) -> String {
    let bytes = fragment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let spelled = (bytes[at] == b'%')
            .then(|| bytes.get(at + 1..at + 3))
            .flatten()
            .filter(|digits| digits.iter().all(u8::is_ascii_hexdigit))
            .and_then(|digits| std::str::from_utf8(digits).ok())
            .and_then(|digits| u8::from_str_radix(digits, 16).ok());
        match spelled {
            Some(byte) => {
                decoded.push(byte);
                at += 3;
            }
            None => {
                decoded.push(bytes[at]);
                at += 1;
            }
        }
    }
    String::from_utf8(decoded).unwrap_or_else(|_| fragment.to_string())
}

/// Locate the destination inside an inline Markdown link token: the bytes it
/// was written as, inside the `<…>` when it was written in one.
///
/// `text_end` is where the parse said the bracket text ended, so the closing
/// `]` is the next one after it however many brackets the text itself carried.
/// From there the shape is CommonMark's own: `(`, optional whitespace, then
/// either a `<…>` destination or a bare one running to the first unescaped
/// space or to the closing paren, with nested parens counted.
///
/// `None` when the token does not have that shape, which is the answer that
/// leaves the fragment split to the parse's own destination.
fn destination_span(raw: &str, text_end: usize) -> Option<Range<usize>> {
    let close = text_end + raw.get(text_end..)?.find(']')?;
    let after = raw.get(close + 1..)?.strip_prefix('(')?;
    let lead = after.len() - after.trim_start().len();
    let start = close + 2 + lead;
    let rest = &after[lead..];

    if let Some(inside) = rest.strip_prefix('<') {
        let end = unescaped_index(inside, '>')?;
        return Some(start + 1..start + 1 + end);
    }

    let mut depth = 0usize;
    let mut end = rest.len();
    let mut chars = rest.char_indices();
    while let Some((at, ch)) = chars.next() {
        match ch {
            '\\' => {
                chars.next();
            }
            '(' => depth += 1,
            ')' if depth == 0 => {
                end = at;
                break;
            }
            ')' => depth -= 1,
            _ if ch.is_whitespace() => {
                end = at;
                break;
            }
            _ => {}
        }
    }
    Some(start..start + end)
}

/// Where the fragment opens in `destination`, given the `source` bytes it was
/// written as: the first `#` the source does not escape, as an index into the
/// resolved destination.
///
/// `None` when unescaping `source` does not reproduce `destination` — the
/// parse resolved something beyond backslash escapes, an entity reference say,
/// so the source cannot be mapped onto its answer and guessing which byte
/// corresponds to which is exactly the drift one parser exists to prevent.
fn unescaped_hash(source: &str, destination: &str) -> Option<Option<usize>> {
    let mut resolved = String::with_capacity(source.len());
    let mut hash = None;
    let mut chars = source.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\'
            && let Some(escaped) = chars.clone().next()
            && escaped.is_ascii_punctuation()
        {
            chars.next();
            resolved.push(escaped);
            continue;
        }
        if ch == '#' && hash.is_none() {
            hash = Some(resolved.len());
        }
        resolved.push(ch);
    }
    (resolved == destination).then_some(hash)
}

/// The index of the first unescaped `needle` in `text`.
fn unescaped_index(text: &str, needle: char) -> Option<usize> {
    let mut chars = text.char_indices();
    while let Some((at, ch)) = chars.next() {
        if ch == '\\' {
            chars.next();
        } else if ch == needle {
            return Some(at);
        }
    }
    None
}

/// `text` with each of `links` that `replace` answers for replaced by its
/// answer, and every other byte as written. The links are `text`'s own, in
/// document order and not overlapping, as one family's tokens always are.
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
///
/// `vault` is reserved. An absent protocol is the vault protocol wherever
/// protocols come to mean anything, so nothing else may claim the name — a
/// reservation the grammar records and nothing here enforces, because
/// recognition is all this layer does.
pub(crate) fn split_protocol(addressed: &str) -> (Option<String>, String) {
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
/// What an anchor *addresses* is the section resolver's business rather than
/// this function's ([`crate::anchor_readings`]). The fragment returned here is
/// as written; a Markdown link's parse decodes it afterwards.
pub(crate) fn split_fragment(raw: &str) -> (&str, Option<&str>, Option<&str>) {
    split_at_hash(raw, raw.find('#'))
}

/// [`split_fragment`] with the splitting hash already located, which is how a
/// Markdown destination is split: where its fragment opens is a question about
/// the bytes it was written as, not about the string the parse resolved.
fn split_at_hash(raw: &str, hash: Option<usize>) -> (&str, Option<&str>, Option<&str>) {
    let Some(hash) = hash else {
        return (raw, None, None);
    };
    let (target, reference) = raw.split_at(hash);
    let reference = &reference[1..];
    match reference.strip_prefix('^') {
        Some(block_ref) => (target, None, Some(block_ref)),
        None => (target, Some(reference), None),
    }
}

/// Whether `target` can be written as a wikilink stem — writing a link with it
/// and re-parsing yields the same target.
///
/// The delimiter bytes a target must not contain are `|` (begins the title),
/// `#` (begins the anchor or block reference) and `[` / `]` (the fences). A
/// target carrying one would re-parse as a different link shape — `a|b` reads
/// as target `a` with title `b`.
///
/// A target must also carry something, carry it on one line, and carry no
/// surrounding whitespace. `[[]]` is not a token this grammar recognizes at
/// all, so a link written with one vanishes; a target holding `\n` or `\r`
/// splices a line break into whatever the link sat in, which ends a table row
/// and leaves a blockquote mid-paragraph; and a padded target such as
/// `" New "` re-parses trimmed, so the fact it produces is not the fact it was
/// written from.
///
/// A target opening with a recognized `protocol://` prefix is refused for the
/// same reason: written as a stem it re-parses as a protocol and a shorter
/// stem, which is a different fact. A rewrite preserves the protocol the
/// author wrote, and changing that protocol is not a target rewrite.
///
/// Every other byte, a bare `^` included, round-trips.
pub(crate) fn wikilink_target_is_representable(target: &str) -> bool {
    !target.is_empty()
        && target.trim() == target
        && !target.contains(['|', '#', '[', ']', '\n', '\r'])
        && split_protocol(target).0.is_none()
}

/// Whether `link` addresses the vault: written with no protocol, or with the
/// reserved `vault` one. Any other protocol's stem is part of somebody else's
/// address, and a vault rename has no business in it.
pub(crate) fn addresses_the_vault(link: &Link) -> bool {
    !matches!(link.resolution(), Resolution::Protocol(scheme) if scheme != VAULT_PROTOCOL)
}

/// Why a matching link was not rewritten.
///
/// Plain rather than `#[non_exhaustive]`: a consumer that has not decided how
/// to word a new reason should fail to compile rather than fall into a
/// default arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RewriteSkip {
    /// `to` cannot be written where this link's target is written and read
    /// back as `to`: it is no target this family can spell, its protocol is
    /// not the link's, or the bytes it would put there read as something else
    /// in that place.
    Unrepresentable,
    /// The link is written in a frontmatter value that cannot hold `to` and
    /// still read as the same YAML with only the target changed — a quote
    /// character inside a quoted scalar, or text a plain scalar cannot carry.
    WouldCorruptFrontmatter,
    /// The link's own bytes give no place to write any target: a wikilink
    /// token spanning a line break, or a Markdown destination written with
    /// escapes or entity references, whose target is not the bytes it was
    /// written as.
    LinkNotRewritable,
}

/// `link`'s own bytes with `to` written over its stem, or why the link cannot
/// carry it — the one stem splice both families' rewrites go through.
///
/// Only the stem's bytes change, so the embed marker, the padding the author
/// wrote, the protocol prefix, the fragment and the title all survive as
/// written: `[[ Old | Title ]]` becomes `[[ New | Title ]]`. Minimal diffs are
/// this crate's identity, and a rename cascade whose hunks read as exactly the
/// rename is what that identity is for. Callers reach this only through
/// [`Document::rewrite_links`](crate::Document::rewrite_links), which proves
/// what it splices by reading the whole document back.
///
/// What may be written depends on the family. A wikilink takes only a target
/// [`wikilink_target_is_representable`] accepts, and a wikilink token carrying
/// a line break takes nothing: `[[Target\nOther]]` is recognized, and an
/// unclosed `[[` can make one span two paragraphs, so splicing a one-line
/// replacement over those bytes would reflow the text the token swallowed. A Markdown
/// destination takes any `to` that is non-empty and on one line, because
/// CommonMark forbids a line ending inside a destination and an empty one
/// would turn a link to a document into a link to the document holding it;
/// whether the bytes then read back as `to` in the place they were written —
/// a space in a bare destination, a `)` that closes it early — is a question
/// about the whole document, answered by re-reading it.
///
/// A link whose stem has no span inside its own bytes cannot be written over
/// at all: a Markdown destination written with escapes or entity references,
/// or one the parse could not locate in its token. A span that does not index
/// the token's bytes is refused the same way rather than sliced, because
/// [`Link`]'s fields are public and the relationship is checked where it is
/// used.
pub(crate) fn respelled(link: &Link, to: &str) -> Result<String, RewriteSkip> {
    let fits = match link.family {
        LinkFamily::Wikilink => {
            if link.raw.contains(['\n', '\r']) {
                return Err(RewriteSkip::LinkNotRewritable);
            }
            wikilink_target_is_representable(to)
        }
        LinkFamily::Markdown => !to.is_empty() && !to.contains(['\n', '\r']),
    };
    let stem = link
        .stem_range
        .clone()
        .filter(|stem| {
            stem.start <= stem.end
                && stem.end <= link.raw.len()
                && link.raw.is_char_boundary(stem.start)
                && link.raw.is_char_boundary(stem.end)
        })
        .ok_or(RewriteSkip::LinkNotRewritable)?;
    if !fits {
        return Err(RewriteSkip::Unrepresentable);
    }
    let mut out = String::with_capacity(link.raw.len() - stem.len() + to.len());
    out.push_str(&link.raw[..stem.start]);
    out.push_str(to);
    out.push_str(&link.raw[stem.end..]);
    Ok(out)
}

/// One trailing block-id definition (`… ^block-id`) — the target side of a
/// `#^` reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockId {
    /// The id as written, without the `^`.
    pub id: String,
    /// Where the definition begins — its `^` marker, the same convention every
    /// construct in this crate follows for its span.
    pub span: SourceSpan,
}

/// Trailing block-id definitions (`… ^block-id`), one per line that carries
/// one.
///
/// The alphabets of the two halves are asymmetric, and the asymmetry is
/// recorded rather than resolved: a `#^id` *reference* is stored raw and
/// carries whatever the author wrote, while a definition is matched against
/// ASCII, so `^ünicode` defines nothing for `[[N#^ünicode]]` to point at.
/// Widening the definition's alphabet is a behaviour change rather than a
/// correction, and it belongs wherever references and definitions are matched
/// to each other.
pub(crate) fn parse_block_ids_in(body: &str, ignored: &[Range<usize>]) -> Vec<BlockId> {
    let mut block_ids = Vec::new();
    let mut cursor = LineCursor::new(body);
    let mut line_start = 0;
    // `BLOCK_ID_RE` anchors on `$`, so it reports one candidate per chunk: the
    // last. Lines therefore have to arrive one per break the cursor counts —
    // a chunk spanning a break yields the definition after it and hides every
    // definition before.
    for line in split_lines_inclusive(body) {
        if let Some(block_id) = BLOCK_ID_RE.captures(line).and_then(|c| c.get(1)) {
            let id_range = (line_start + block_id.start())..(line_start + block_id.end());
            if !overlaps_any(ignored, &id_range) {
                // Group 1 sits directly after the `\^` in the pattern, so the
                // marker is always the byte before it.
                let marker = id_range.start - 1;
                debug_assert_eq!(body.as_bytes()[marker], b'^');
                block_ids.push(BlockId {
                    id: block_id.as_str().to_string(),
                    span: cursor.span_at(marker),
                });
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
