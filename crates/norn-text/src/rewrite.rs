//! Rewriting a document's links: every link of one family whose target is one
//! text, respelled to another — the pure text half of a link cascade. See
//! [`Document::rewrite_links`].

use std::ops::Range;

use crate::document::{Document, frontmatter_of, splice_all};
use crate::link::{
    Link, LinkFamily, RewriteSkip, addresses_the_vault, parse_wikilinks_in_text, respelled,
    splice_tokens, split_protocol,
};
use crate::span::LineCursor;
use crate::value::{Mapping, Value};

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
    /// link's is [`RewriteSkip::Unrepresentable`] there. A matching link that
    /// cannot carry `to` is left exactly as written and reported in
    /// [`RewrittenLinks::skipped`] with the [`RewriteSkip`] that says why; it
    /// is never forced. A rewrite that respells nothing returns the document's
    /// own bytes.
    ///
    /// The result is proven by reading it back: everything the index derives
    /// from the document reads as it did, where its bytes moved to, with only
    /// the rewritten targets changed — the frontmatter but for the rewritten
    /// strings, the same links in the same order, and the same headings, tags,
    /// block ids and code. A heading or a link title whose own text holds a
    /// rewritten link reads that link's new spelling. An edit that does not
    /// read back is skipped where it stands rather than returned.
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
                    slot: None,
                }),
                Err(reason) => skipped.push(SkippedLink { link, reason }),
            }
        }

        let mut text = self.spliced(&edits);
        if !edits.is_empty() {
            let before = Reading::of(self);
            if !self.reads_as_rewritten(&before, &text, &edits, to) {
                edits = self.provable_alone(&before, edits, to, &mut skipped);
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

    /// One edit per frontmatter string holding a link to rewrite, each proven
    /// by re-reading the block it would produce.
    ///
    /// A string is rewritten whole or not at all: every matching link in it
    /// carries the same `to` in the same quoting, so a value that cannot hold
    /// one cannot hold any, and each is skipped as
    /// [`RewriteSkip::WouldCorruptFrontmatter`]. The proof is the block read
    /// back with only that string changed, compared against the mapping it
    /// came from with only that string's value changed — the same proof every
    /// frontmatter edit here makes, so what a plain scalar can carry is the
    /// YAML reader's answer rather than a list of hazards somebody maintains.
    /// A string that would grow the block past its byte bound reads back as
    /// nothing, and is skipped the same way.
    fn frontmatter_edits<'d>(
        &'d self,
        from: &Address,
        to: &Address,
        edits: &mut Vec<Edit<'d>>,
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
            let edit = Edit {
                range: literal.start..literal.start + literal.text.len(),
                replacement: text,
                links,
                slot: Some((literal.field, literal.item)),
            };
            let alone = std::slice::from_ref(&edit);
            if frontmatter_of(&self.spliced(alone)) == self.frontmatter_after(alone) {
                edits.push(edit);
            } else {
                skipped.extend(edit.skipped(RewriteSkip::WouldCorruptFrontmatter));
            }
        }
    }

    /// Whether `text` — this document with `edits` spliced in — reads as this
    /// document with exactly those edits' links respelled.
    ///
    /// Everything the index derives from a document is read back from `text`
    /// and compared with what this document's own reading becomes under the
    /// edits ([`Reading::respelled`]): the frontmatter with only the rewritten
    /// strings changed; the links of both families, in the same order, with
    /// only the rewritten ones' targets changed; the headings, the tags of
    /// both sources and the block ids as they were; and the code ranges, which
    /// decide which bytes can be any of those. Each is compared where its
    /// bytes moved to, so a fact that survives one byte further on is a fact
    /// that moved, and the proof fails.
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
        edits: &[Edit<'_>],
        to: &Address,
    ) -> bool {
        let respelling = Respelling::of(edits, &to.target);
        let carried = before.carrying(&respelling);
        let expected = before.respelled(&respelling, self.frontmatter_after(edits), &carried);
        let mut actual = Reading::of(&Document::parse(text));
        actual.forget(&carried);
        actual == expected
    }

    /// The edits each provable alone and alongside the ones kept before it,
    /// in document order; every other edit's links are skipped.
    ///
    /// This is the path a rewrite takes only when the edits together did not
    /// read back, which is rare and is what the extra reads are spent on. An
    /// edit that cannot be proven is one whose target bytes read as something
    /// else where they were written, so a frontmatter one is skipped as
    /// corrupting its value and a body one as unrepresentable there.
    fn provable_alone<'d>(
        &'d self,
        before: &Reading,
        edits: Vec<Edit<'d>>,
        to: &Address,
        skipped: &mut Vec<SkippedLink>,
    ) -> Vec<Edit<'d>> {
        let mut kept = Vec::with_capacity(edits.len());
        for edit in edits {
            kept.push(edit);
            if !self.reads_as_rewritten(before, &self.spliced(&kept), &kept, to) {
                let refused = kept.pop().expect("the edit just kept");
                let reason = match refused.slot {
                    Some(_) => RewriteSkip::WouldCorruptFrontmatter,
                    None => RewriteSkip::Unrepresentable,
                };
                skipped.extend(refused.skipped(reason));
            }
        }
        kept
    }

    /// The frontmatter this document would read as with `edits`' strings
    /// written into it.
    fn frontmatter_after(&self, edits: &[Edit<'_>]) -> Option<Value> {
        let mut value = self.frontmatter().cloned();
        if let Some(Value::Map(map)) = value.as_mut() {
            for edit in edits {
                if let Some((field, item)) = edit.slot {
                    with_text(map, field, item, &edit.replacement);
                }
            }
        }
        value
    }

    /// This document's source with each edit's range replaced. The edits are
    /// in document order and do not overlap.
    fn spliced(&self, edits: &[Edit<'_>]) -> String {
        let runs: Vec<(Range<usize>, &str)> = edits
            .iter()
            .map(|edit| (edit.range.clone(), edit.replacement.as_str()))
            .collect();
        splice_all(self.source(), &runs)
    }
}

/// One run of source bytes a rewrite replaces, and the links it respells
/// there: one body token, or one frontmatter string holding one or more.
struct Edit<'d> {
    range: Range<usize>,
    replacement: String,
    links: Vec<Link>,
    /// For a frontmatter string, the field holding it and the item of that
    /// field's sequence it is, `None` for the field's own value. A
    /// frontmatter edit's range is exactly the string's text, so its
    /// replacement is the string's new value.
    slot: Option<(&'d str, Option<usize>)>,
}

impl Edit<'_> {
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

/// Everything the index derives from one document — its frontmatter, its
/// links of both families, its headings, its tags from both sources and its
/// block ids — and the code ranges that decide which bytes can be any of
/// them, each placed by the bytes it stands at in the source.
///
/// A position is a byte offset alone: a rewrite writes no line break and
/// removes none, so where a fact's line and column land follows from where
/// its byte does. A heading's slug is not read either, because it is the
/// heading texts' own function, in order, and they are read.
#[derive(Debug, PartialEq)]
struct Reading {
    frontmatter: Option<Value>,
    links: Vec<LinkReading>,
    headings: Vec<HeadingReading>,
    /// Frontmatter tags first, then body ones, as the index orders them; a
    /// frontmatter tag whose entry names no bytes has no position.
    tags: Vec<(String, Option<usize>)>,
    block_ids: Vec<(String, usize)>,
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
    text: Option<String>,
    at: Range<usize>,
    inside_container: bool,
}

/// The facts of a reading whose own text holds a respelled link's bytes:
/// each is changed by the rewrite because the link is part of it, so the
/// proof reads it without that text.
struct Carried {
    /// Links whose bracket text holds another respelled link — `[[a]](b)`.
    titles: Vec<usize>,
    /// Headings whose text holds a respelled link — `# See [[a]]`.
    headings: Vec<usize>,
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
                text: Some(heading.text.clone()),
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
        let block_ids = scan
            .block_ids()
            .into_iter()
            .map(|block| (block.id, body + block.span.byte_offset))
            .collect();
        let code = scan
            .code_ranges()
            .iter()
            .map(|range| body + range.start..body + range.end)
            .collect();
        Reading {
            frontmatter: document.frontmatter().cloned(),
            links,
            headings,
            tags,
            block_ids,
            code,
        }
    }

    /// What this reading becomes under `respelling`: every fact where its
    /// bytes moved to, each respelled link targeting `to`, the frontmatter
    /// read as `frontmatter`, and the `carried` facts' text forgotten.
    fn respelled(
        &self,
        respelling: &Respelling<'_>,
        frontmatter: Option<Value>,
        carried: &Carried,
    ) -> Reading {
        let moved = |at: &Range<usize>| respelling.moved(at.start)..respelling.moved(at.end);
        let mut reading = Reading {
            frontmatter,
            links: self
                .links
                .iter()
                .map(|link| LinkReading {
                    target: if respelling.respells(link) {
                        respelling.to.to_string()
                    } else {
                        link.target.clone()
                    },
                    at: moved(&link.at),
                    ..link.clone()
                })
                .collect(),
            headings: self
                .headings
                .iter()
                .map(|heading| HeadingReading {
                    at: moved(&heading.at),
                    text: heading.text.clone(),
                    ..*heading
                })
                .collect(),
            tags: self
                .tags
                .iter()
                .map(|(name, at)| (name.clone(), at.map(|at| respelling.moved(at))))
                .collect(),
            block_ids: self
                .block_ids
                .iter()
                .map(|(id, at)| (id.clone(), respelling.moved(*at)))
                .collect(),
            code: self.code.iter().map(moved).collect(),
        };
        reading.forget(carried);
        reading
    }

    /// The facts whose own text holds a link `respelling` respells.
    fn carrying(&self, respelling: &Respelling<'_>) -> Carried {
        Carried {
            titles: (0..self.links.len())
                .filter(|&index| {
                    let link = &self.links[index];
                    respelling.holds(&link.at, Some((link.family, link.at.start)))
                })
                .collect(),
            headings: (0..self.headings.len())
                .filter(|&index| respelling.holds(&self.headings[index].at, None))
                .collect(),
        }
    }

    /// This reading with the `carried` facts' text left unread.
    fn forget(&mut self, carried: &Carried) {
        for &index in &carried.titles {
            if let Some(link) = self.links.get_mut(index) {
                link.title = None;
            }
        }
        for &index in &carried.headings {
            if let Some(heading) = self.headings.get_mut(index) {
                heading.text = None;
            }
        }
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

/// The links a set of edits respells, each by its family and the byte its
/// token begins at, with the source bytes its stem stands at: exactly what
/// the edits replace, so where every other byte moves to follows.
struct Respelling<'t> {
    stems: Vec<(LinkFamily, usize, Range<usize>)>,
    to: &'t str,
}

impl<'t> Respelling<'t> {
    fn of(edits: &[Edit<'_>], to: &'t str) -> Self {
        let stems = edits
            .iter()
            .flat_map(|edit| &edit.links)
            .filter_map(|link| {
                let at = link.span.byte_offset;
                let stem = link.stem_range.clone()?;
                Some((link.family, at, at + stem.start..at + stem.end))
            })
            .collect();
        Respelling { stems, to }
    }

    /// Whether `link` is one this respelling rewrites.
    fn respells(&self, link: &LinkReading) -> bool {
        self.stems
            .iter()
            .any(|(family, at, _)| (*family, *at) == (link.family, link.at.start))
    }

    /// Where the source byte at `at` stands once every stem ending at or
    /// before it is written as `to`.
    fn moved(&self, at: usize) -> usize {
        let (grown, shrunk) = self
            .stems
            .iter()
            .filter(|(_, _, stem)| stem.end <= at)
            .fold((0, 0), |(grown, shrunk), (_, _, stem)| {
                (grown + self.to.len(), shrunk + stem.len())
            });
        at + grown - shrunk
    }

    /// Whether a respelled link other than `except` begins inside `range`.
    fn holds(&self, range: &Range<usize>, except: Option<(LinkFamily, usize)>) -> bool {
        self.stems
            .iter()
            .any(|(family, at, _)| range.contains(at) && except != Some((*family, *at)))
    }
}

/// Set the string at `field` — its own value, or the `item` of its sequence —
/// to `text`.
fn with_text(map: &mut Mapping, field: &str, item: Option<usize>, text: &str) {
    let value = match (map.get(field), item) {
        (Some(Value::Sequence(items)), Some(index)) => {
            let mut items = items.clone();
            items[index] = Value::String(text.to_string());
            Value::Sequence(items)
        }
        _ => Value::String(text.to_string()),
    };
    map.insert(field, value);
}
