//! Rewriting a document's links: every link of one family whose target is one
//! text, respelled to another — the pure text half of a link cascade. See
//! [`Document::rewrite_links`].

use std::ops::Range;

use crate::document::{Document, splice_all};
use crate::link::{
    Link, LinkFamily, RewriteSkip, addresses_the_vault, parse_wikilinks_in_text, respelled,
    splice_tokens, split_protocol,
};
use crate::span::LineCursor;

/// What [`Document::rewrite_links`] produced: the whole rewritten document,
/// how many links it respelled, and each matching link it left alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewrittenLinks {
    /// The whole document, rewritten. Byte-identical to the input when nothing
    /// was rewritten.
    pub text: String,
    /// How many links now read `to` that read `from`.
    pub rewritten: usize,
    /// Every matching link left as written, in document order, each with why.
    pub skipped: Vec<SkippedLink>,
}

/// A link that matched a rewrite and was left exactly as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedLink {
    /// The link as the input document holds it, in source coordinates.
    pub link: Link,
    pub reason: RewriteSkip,
}

impl Document<'_> {
    /// Respell every link of `family` whose target is `from` to `to`, and
    /// return the whole rewritten document.
    ///
    /// A move, a redirecting delete or a wikilink rewrite expands, per
    /// document holding an affected link, into *in this document, every link
    /// of this family whose target is `from` is respelled `to`*, and this is
    /// what composes that document's after-bytes — for the planner that
    /// previews them and for the applier that writes them alike, so the two
    /// cannot disagree.
    ///
    /// # The match is the index's
    ///
    /// What a link resolves to depends on the document holding it, its family
    /// and its address, so a cascade names the links it rewrites by exactly
    /// those. `from` and `to` are addresses as written: the target, with its
    /// `protocol://` prefix when it has one — `vault://notes/x` or `notes/x`,
    /// the index's protocol and target columns recombined. Each is split by
    /// the one protocol splitter a link's own address is, and a link matches
    /// only when both its [`Link::protocol`] and its [`Link::target`] equal
    /// `from`'s. That makes `[[X]]` and `[[vault://X]]` two keys: a `vault://`
    /// link is read from the vault root and a protocol-free one is not, so in
    /// one document they can name different documents.
    ///
    /// The target is compared as this crate's one parse reads it — the text
    /// the store's `links.target` column is derived from, never decoded,
    /// unescaped or normalized on either side — and `to`'s is written as that
    /// same text: a percent-encoded Markdown destination is matched in its
    /// encoded spelling and stays encoded only if `to` is spelled so. The
    /// links matched are the ones the index holds: those
    /// [`Document::frontmatter_wikilinks`] and [`Document::links`] report. The
    /// two families' targets are different texts read by different grammars,
    /// so a rewrite names one family, and a Markdown link whose target happens
    /// to spell `from` is not touched by a wikilink rewrite. Frontmatter values
    /// are read for wikilinks only.
    ///
    /// Three kinds of link never match. An empty target matches nothing,
    /// because a target-less link — `[[#Heading]]` — addresses the document
    /// holding it and is never what a cascade renames. A link written with a
    /// protocol other than `vault` addresses something outside the vault,
    /// whatever `from` spells. And code is opaque: a link written inside a
    /// code span or block is not a link.
    ///
    /// # Rewritten, or skipped with a reason
    ///
    /// Only a link's stem bytes change, so everything else it was written
    /// with — an embed marker, a title, an anchor, padding, a protocol, a
    /// Markdown destination's angle brackets — survives byte for byte, and so
    /// does every byte outside the links rewritten. A rewrite never changes
    /// how a link is addressed: a `to` whose protocol is not the matched
    /// link's is [`RewriteSkip::Unrepresentable`] there. So is a Markdown `to`
    /// holding `|`, which would end a table cell this crate does not read
    /// tables to see. A matching link that
    /// cannot carry `to` is left exactly as written and reported in
    /// [`RewrittenLinks::skipped`] with the [`RewriteSkip`] that says why; it
    /// is never forced. A rewrite that respells nothing returns the document's
    /// own bytes.
    ///
    /// The result is proven by reading it back: everything the index derives
    /// from the document reads as it did, where its bytes moved to, with only
    /// the rewritten targets changed — the same links in the same order, and
    /// the same headings, tags and code. A heading or a link title whose own
    /// text holds a rewritten link reads exactly as it did with that link's
    /// old token replaced by its new one, so a target whose bytes would read
    /// as markup there — a `*` pairing with a literal one later in the
    /// heading — is caught. A text that does not hold the old token as written
    /// (or, for `[[a]](b)`, where the two links share their outer brackets,
    /// the token inside them) cannot be proven so and skips the edit. An edit
    /// that does not read back is skipped where it stands rather than
    /// returned.
    pub fn rewrite_links(&self, family: LinkFamily, from: &str, to: &str) -> RewrittenLinks {
        let (from, to) = (Address::of(from), Address::of(to));
        let to = &to;
        let mut edits = Vec::new();
        let mut skipped = Vec::new();
        if family == LinkFamily::Wikilink {
            self.frontmatter_edits(&from, to, &mut edits, &mut skipped);
        }
        for link in self.links() {
            if link.family != family || !from.names(&link) {
                continue;
            }
            match to.respell(&link) {
                Ok(token) => edits.push(Edit {
                    range: link.range(),
                    replacement: token,
                    links: vec![link],
                    in_frontmatter: false,
                }),
                Err(reason) => skipped.push(SkippedLink { link, reason }),
            }
        }

        let mut text = self.spliced(&edits);
        if !edits.is_empty() {
            let before = Reading::of(self);
            if !self.reads_as_rewritten(&before, &text, &edits, to) {
                edits = self.kept_in_order(&before, edits, to, &mut skipped);
                text = self.spliced(&edits);
            }
        }
        skipped.sort_by_key(|skip| skip.link.span.byte_offset);
        RewrittenLinks {
            text,
            rewritten: edits.iter().map(|edit| edit.links.len()).sum(),
            skipped,
        }
    }

    /// One edit per frontmatter string holding a link to rewrite.
    ///
    /// A string is rewritten whole or not at all: every matching link in it
    /// carries the same `to` in the same quoting, so where the string's
    /// grammar refuses one — a quote character inside a scalar quoted with it,
    /// an escape, a `: ` a plain scalar reads as a mapping — it refuses all of
    /// them. The block's byte bound is the exception to that symmetry, since
    /// each respelled link grows the block: one link may fit where two in the
    /// same string do not, and the string is still skipped whole. Like every
    /// edit, a string is proven by reading the document back
    /// ([`Document::reads_as_rewritten`]), so what a plain scalar can carry is
    /// the YAML reader's answer rather than a list of hazards somebody
    /// maintains.
    fn frontmatter_edits(
        &self,
        from: &Address,
        to: &Address,
        edits: &mut Vec<Edit>,
        skipped: &mut Vec<SkippedLink>,
    ) {
        let mut cursor = LineCursor::new(self.source());
        for literal in self.literal_texts() {
            let mut links = Vec::new();
            let text = splice_tokens(
                literal.text,
                &parse_wikilinks_in_text(literal.text),
                |token| {
                    let link = Link {
                        span: cursor.span_at(literal.start + token.span.byte_offset),
                        ..token.clone()
                    };
                    if !from.names(&link) {
                        return None;
                    }
                    match to.respell(&link) {
                        Ok(respelled) => {
                            links.push(link);
                            Some(respelled)
                        }
                        Err(reason) => {
                            skipped.push(SkippedLink { link, reason });
                            None
                        }
                    }
                },
            );
            if links.is_empty() {
                continue;
            }
            edits.push(Edit {
                range: literal.start..literal.start + literal.text.len(),
                replacement: text,
                links,
                in_frontmatter: true,
            });
        }
    }

    /// Whether `text` — this document with `edits` spliced in — reads as this
    /// document with exactly those edits' links respelled.
    ///
    /// What the index derives from a document is read back from `text` and
    /// compared with what this document's own [`Reading`] becomes under the
    /// edits ([`Reading::respelled`]): the links of both families, frontmatter
    /// ones included, in the same order, with only the rewritten ones' targets
    /// changed; the headings and the tags of both sources as they were, a
    /// heading or link title holding a rewritten link with only that link's
    /// token changed; and the code ranges, which decide which bytes can be any
    /// of those. Each is compared where its bytes moved to, so a fact that
    /// survives one byte further on is a fact that moved, and the proof fails.
    ///
    /// This is the proof every rewrite returns under, and it is about the whole
    /// document because what a target's bytes mean depends on what surrounds
    /// them: a backtick in a wikilink stem can pair with one later in the
    /// paragraph and turn the text between into code, a backslash can escape
    /// the bracket closing the Markdown link around it, a space ends a bare
    /// destination, and `<!--` opens a comment that hides every tag after it.
    fn reads_as_rewritten(
        &self,
        before: &Reading,
        text: &str,
        edits: &[Edit],
        to: &Address,
    ) -> bool {
        let respelling = Respelling::of(edits, &to.target);
        before
            .respelled(&respelling)
            .is_some_and(|expected| Reading::of(&Document::parse(text)) == expected)
    }

    /// The edits kept greedily in document order: each is kept when it reads
    /// back alongside the ones kept before it, and every other edit's links
    /// are skipped.
    ///
    /// The answer depends on that order. Two edits that each read back alone
    /// may not read back together — two frontmatter strings that each fit the
    /// block's byte bound, a backtick in one stem pairing with one in another
    /// — and then the first is kept and the second skipped, though the
    /// opposite choice would have read back as well. Document order is the
    /// tie-break because it is the one a reader of the skips can predict.
    ///
    /// This is the path a rewrite takes only when the edits together did not
    /// read back, which is rare and is what the extra reads are spent on. An
    /// edit refused here is one whose target bytes read as something else where
    /// they were written, so a frontmatter one is skipped as corrupting its
    /// value and a body one as unrepresentable there.
    fn kept_in_order(
        &self,
        before: &Reading,
        edits: Vec<Edit>,
        to: &Address,
        skipped: &mut Vec<SkippedLink>,
    ) -> Vec<Edit> {
        let mut kept = Vec::with_capacity(edits.len());
        for edit in edits {
            kept.push(edit);
            if !self.reads_as_rewritten(before, &self.spliced(&kept), &kept, to) {
                let refused = kept.pop().expect("the edit just kept");
                let reason = if refused.in_frontmatter {
                    RewriteSkip::WouldCorruptFrontmatter
                } else {
                    RewriteSkip::Unrepresentable
                };
                skipped.extend(refused.skipped(reason));
            }
        }
        kept
    }

    /// This document's source with each edit's range replaced. The edits are
    /// in document order and do not overlap.
    fn spliced(&self, edits: &[Edit]) -> String {
        let runs: Vec<(Range<usize>, &str)> = edits
            .iter()
            .map(|edit| (edit.range.clone(), edit.replacement.as_str()))
            .collect();
        splice_all(self.source(), &runs)
    }
}

/// One run of source bytes a rewrite replaces, and the links it respells
/// there: one body token, or one frontmatter string holding one or more,
/// whose range is exactly the string's text.
struct Edit {
    range: Range<usize>,
    replacement: String,
    links: Vec<Link>,
    in_frontmatter: bool,
}

impl Edit {
    /// This edit's links, each skipped for `reason`.
    fn skipped(self, reason: RewriteSkip) -> impl Iterator<Item = SkippedLink> {
        self.links
            .into_iter()
            .map(move |link| SkippedLink { link, reason })
    }
}

/// A link's address as a rewrite is handed it: written whole, with any
/// `protocol://` prefix, and read by the one splitter a link's own is.
struct Address {
    protocol: Option<String>,
    target: String,
}

impl Address {
    fn of(written: &str) -> Self {
        let (protocol, target) = split_protocol(written);
        Address { protocol, target }
    }

    /// Whether this address is `link`'s: the same protocol, or none on both,
    /// and the same target — never an empty one, and never a link addressed
    /// outside the vault.
    fn names(&self, link: &Link) -> bool {
        !self.target.is_empty()
            && link.protocol == self.protocol
            && link.target == self.target
            && addresses_the_vault(link)
    }

    /// `link`'s bytes with this address's target over its stem, or why it
    /// cannot carry it. A rewrite never changes how a link is addressed, so a
    /// protocol other than the link's own is unrepresentable there.
    fn respell(&self, link: &Link) -> Result<String, RewriteSkip> {
        let token = respelled(link, &self.target)?;
        if link.protocol != self.protocol {
            return Err(RewriteSkip::Unrepresentable);
        }
        Ok(token)
    }
}

/// What the index derives from one document — its links of both families,
/// its headings and its tags from both sources — and the code ranges that
/// decide which bytes can be any of them, each placed by the bytes it stands
/// at in the source. A rewrite is proven by comparing two of these, one fact
/// once.
///
/// A position is a byte offset alone: a rewrite writes no line break and
/// removes none, so where a fact's line and column land follows from where
/// its byte does. Three derived facts are read through others rather than
/// compared again:
///
/// - A heading's slug is the heading texts' own function, in order.
/// - The frontmatter's value. A frontmatter link is reported only where its
///   string reads as exactly the bytes it is written with, so a rewritten
///   string that read as anything else — or a block that stopped reading, or
///   a structure that changed around the string — loses the link the
///   rewrite placed there.
/// - Block ids. A definition is the last word on its line outside code, and
///   a stem is never the last bytes on its line, since `]]` or `)` follows
///   it, so a rewrite changes a definition only by changing what is code.
#[derive(Debug, PartialEq)]
struct Reading {
    links: Vec<LinkReading>,
    headings: Vec<HeadingReading>,
    /// Frontmatter tags first, then body ones, as the index orders them; a
    /// frontmatter tag whose entry names no bytes has no position.
    tags: Vec<(String, Option<usize>)>,
    code: Vec<Range<usize>>,
}

/// A link as the index reads it, at the bytes its token spans.
#[derive(Debug, Clone, PartialEq)]
struct LinkReading {
    family: LinkFamily,
    embed: bool,
    protocol: Option<String>,
    target: String,
    title: Option<String>,
    anchor: Option<String>,
    block_ref: Option<String>,
    at: Range<usize>,
}

/// A heading as the index reads it: where its construct begins and ends.
#[derive(Debug, PartialEq)]
struct HeadingReading {
    level: u8,
    text: String,
    at: Range<usize>,
    inside_container: bool,
}

impl Reading {
    fn of(document: &Document<'_>) -> Self {
        let scan = document.scan_body();
        let body = document.body_start();
        let links = document
            .frontmatter_wikilinks()
            .into_iter()
            .map(|link| LinkReading::of(link, 0))
            .chain(
                scan.links()
                    .into_iter()
                    .map(|link| LinkReading::of(link, body)),
            )
            .collect();
        let headings = scan
            .headings()
            .iter()
            .map(|heading| HeadingReading {
                level: heading.level,
                text: heading.text.clone(),
                at: body + heading.span.byte_offset..body + heading.body_offset,
                inside_container: heading.inside_container,
            })
            .collect();
        let tags = document
            .frontmatter_tags()
            .into_iter()
            .map(|tag| (tag.name, tag.span.map(|span| span.byte_offset)))
            .chain(
                scan.tags()
                    .into_iter()
                    .map(|tag| (tag.name, tag.span.map(|span| body + span.byte_offset))),
            )
            .collect();
        let code = scan
            .code_ranges()
            .iter()
            .map(|range| body + range.start..body + range.end)
            .collect();
        Reading {
            links,
            headings,
            tags,
            code,
        }
    }

    /// What this reading becomes under `respelling`: every fact where its
    /// bytes moved to, each respelled link targeting `to`, and each heading or
    /// link title holding a respelled link with that link's token respelled
    /// ([`Respelling::carried`]). `None` where such a text does not hold the
    /// link's token as written, so what it should read as is unknown.
    fn respelled(&self, respelling: &Respelling<'_>) -> Option<Reading> {
        let moved = |at: &Range<usize>| respelling.moved(at.start)..respelling.moved(at.end);
        let links = self
            .links
            .iter()
            .map(|link| {
                let except = Some((link.family, link.at.start));
                let title = match &link.title {
                    Some(title) => Some(respelling.carried(title, &link.at, except)?),
                    None => None,
                };
                Some(LinkReading {
                    target: if respelling.respells(link) {
                        respelling.to.to_string()
                    } else {
                        link.target.clone()
                    },
                    title,
                    at: moved(&link.at),
                    ..link.clone()
                })
            })
            .collect::<Option<_>>()?;
        let headings = self
            .headings
            .iter()
            .map(|heading| {
                Some(HeadingReading {
                    text: respelling.carried(&heading.text, &heading.at, None)?,
                    at: moved(&heading.at),
                    ..*heading
                })
            })
            .collect::<Option<_>>()?;
        Some(Reading {
            links,
            headings,
            tags: self
                .tags
                .iter()
                .map(|(name, at)| (name.clone(), at.map(|at| respelling.moved(at))))
                .collect(),
            code: self.code.iter().map(moved).collect(),
        })
    }
}

impl LinkReading {
    /// `link` as read, its token placed `base` bytes further into the source.
    fn of(link: Link, base: usize) -> Self {
        let at = link.range();
        LinkReading {
            family: link.family,
            embed: link.embed,
            protocol: link.protocol,
            target: link.target,
            title: link.title,
            anchor: link.anchor,
            block_ref: link.block_ref,
            at: base + at.start..base + at.end,
        }
    }
}

/// The links a set of edits respells, in document order: exactly what the
/// edits replace, so where every other byte moves to follows.
struct Respelling<'t> {
    stems: Vec<RespelledStem<'t>>,
    to: &'t str,
}

/// One link a respelling rewrites: its family and the byte its token begins
/// at, which name it; the source bytes its stem stands at; and its token as
/// written and as respelled.
struct RespelledStem<'t> {
    family: LinkFamily,
    at: usize,
    stem: Range<usize>,
    old: &'t str,
    new: String,
}

impl<'t> Respelling<'t> {
    fn of(edits: &'t [Edit], to: &'t str) -> Self {
        let stems = edits
            .iter()
            .flat_map(|edit| &edit.links)
            .filter_map(|link| {
                let at = link.span.byte_offset;
                let stem = link.stem_range.clone()?;
                let new = format!("{}{to}{}", &link.raw[..stem.start], &link.raw[stem.end..]);
                Some(RespelledStem {
                    family: link.family,
                    at,
                    stem: at + stem.start..at + stem.end,
                    old: &link.raw,
                    new,
                })
            })
            .collect();
        Respelling { stems, to }
    }

    /// Whether `link` is one this respelling rewrites.
    fn respells(&self, link: &LinkReading) -> bool {
        self.stems
            .iter()
            .any(|stem| (stem.family, stem.at) == (link.family, link.at.start))
    }

    /// Where the source byte at `at` stands once every stem ending at or
    /// before it is written as `to`.
    fn moved(&self, at: usize) -> usize {
        let (grown, shrunk) = self
            .stems
            .iter()
            .filter(|respelled| respelled.stem.end <= at)
            .fold((0, 0), |(grown, shrunk), respelled| {
                (grown + self.to.len(), shrunk + respelled.stem.len())
            });
        at + grown - shrunk
    }

    /// `text`, the text of a fact whose bytes span `range`, as it reads once
    /// every respelled link other than `except` beginning inside `range` is
    /// rewritten: each such link's old token replaced by its new one, in
    /// order, each found after the one before.
    ///
    /// A link beginning where a Markdown link does — `[[a]](b)` — shares its
    /// brackets with it, so that link's bracket text holds the token inside
    /// its outer brackets, `[a]`, and that is what is replaced when the whole
    /// token is not there.
    ///
    /// `None` where `text` does not hold a link's old token as written — the
    /// link's own title holding markup the text flattens, say — so the proof
    /// cannot say what the text reads as and fails rather than guess. A
    /// literal copy of the token earlier in the text, in a code span say, is
    /// found first, and the proof fails then too: conservative, never wrong.
    fn carried(
        &self,
        text: &str,
        range: &Range<usize>,
        except: Option<(LinkFamily, usize)>,
    ) -> Option<String> {
        let mut expected = String::with_capacity(text.len());
        let mut rest = text;
        for respelled in self
            .stems
            .iter()
            .filter(|stem| range.contains(&stem.at) && except != Some((stem.family, stem.at)))
        {
            let (found, old, new) = match rest.find(respelled.old) {
                Some(found) => (found, respelled.old, respelled.new.as_str()),
                None if except.is_some() && respelled.at == range.start => {
                    let old = respelled.old.get(1..respelled.old.len() - 1)?;
                    let new = respelled.new.get(1..respelled.new.len() - 1)?;
                    (rest.find(old)?, old, new)
                }
                None => return None,
            };
            expected.push_str(&rest[..found]);
            expected.push_str(new);
            rest = &rest[found + old.len()..];
        }
        expected.push_str(rest);
        Some(expected)
    }
}
