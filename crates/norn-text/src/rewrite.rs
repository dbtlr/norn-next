//! Rewriting a document's links: every link of one family whose target is one
//! text, respelled to another — the pure text half of a link cascade. See
//! [`Document::rewrite_links`].

use std::collections::HashSet;
use std::ops::Range;

use crate::document::{Document, frontmatter_of, splice_all};
use crate::link::{
    Link, LinkFamily, addresses_the_vault, parse_wikilinks_in_text, respelled, splice_tokens,
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

/// Why a matching link was not rewritten.
///
/// Plain rather than `#[non_exhaustive]`: a consumer that has not decided how
/// to word a new reason should fail to compile rather than fall into a
/// default arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RewriteSkip {
    /// `to` cannot be written where this link's target is written and read
    /// back as `to`: it is no target this family can spell, or the bytes it
    /// would put there read as something else in that place.
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
    /// and its target text, so a cascade names the links it rewrites by
    /// exactly those. `from` is compared against [`Link::target`] as this
    /// crate's one parse reads it — the text the store's `links.target`
    /// column is derived from, never decoded, unescaped or normalized on
    /// either side — and `to` is written as that same text: a percent-encoded
    /// Markdown destination is matched in its encoded spelling and stays
    /// encoded only if `to` is spelled so. The links matched are the ones the
    /// index holds: those [`Document::frontmatter_wikilinks`] and
    /// [`Document::links`] report. The two families' targets are different
    /// texts read by different grammars, so a rewrite names one family, and a
    /// Markdown link whose target happens to spell `from` is not touched by a
    /// wikilink rewrite. Frontmatter values are read for wikilinks only.
    ///
    /// Three kinds of link never match. An empty `from` matches nothing,
    /// because a target-less link — `[[#Heading]]` — addresses the document
    /// holding it and is never what a cascade renames. A link written with a
    /// protocol other than `vault` addresses something outside the vault,
    /// whose stem merely spells the same text. And code is opaque: a link
    /// written inside a code span or block is not a link.
    ///
    /// # Rewritten, or skipped with a reason
    ///
    /// Only a link's stem bytes change, so everything else it was written
    /// with — an embed marker, a title, an anchor, padding, a protocol, a
    /// Markdown destination's angle brackets — survives byte for byte, and so
    /// does every byte outside the links rewritten. A matching link that
    /// cannot carry `to` is left exactly as written and reported in
    /// [`RewrittenLinks::skipped`] with the [`RewriteSkip`] that says why; it
    /// is never forced. A rewrite that respells nothing returns the document's
    /// own bytes.
    ///
    /// The result is proven by reading it back: the frontmatter reads as it
    /// did with only the rewritten strings changed, and the document holds
    /// the same links in the same order with only the rewritten ones'
    /// targets changed. An edit that does not read back is skipped where it
    /// stands rather than returned.
    pub fn rewrite_links(&self, family: LinkFamily, from: &str, to: &str) -> RewrittenLinks {
        let mut edits = Vec::new();
        let mut skipped = Vec::new();
        if family == LinkFamily::Wikilink {
            self.frontmatter_edits(from, to, &mut edits, &mut skipped);
        }
        for link in self.links() {
            if link.family != family || !matches(&link, from) {
                continue;
            }
            match respelled(&link, to) {
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
        if !edits.is_empty() && !self.reads_as_rewritten(&text, &edits, to) {
            edits = self.provable_alone(edits, to, &mut skipped);
            text = self.spliced(&edits);
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
        from: &str,
        to: &str,
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
                    if !matches(&link, from) {
                        return None;
                    }
                    match respelled(&link, to) {
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
    /// document with exactly those edits' links respelled: the same
    /// frontmatter with only the rewritten strings changed, and the same links
    /// of both families, in the same order, with only the rewritten ones'
    /// targets changed.
    ///
    /// This is the proof every rewrite returns under, and it is about the whole
    /// document because what a target's bytes mean depends on what surrounds
    /// them: a backtick in a wikilink stem can pair with one later in the
    /// paragraph and turn the text between into code, a backslash can escape
    /// the bracket closing the Markdown link around it, and a space ends a
    /// bare destination. A link's title is not compared — it is display text,
    /// and the bracket text of `[[a]](b)` is another link's bytes, which a
    /// rewrite of that link rightly changes.
    fn reads_as_rewritten(&self, text: &str, edits: &[Edit<'_>], to: &str) -> bool {
        let reread = Document::parse(text);
        if reread.frontmatter().cloned() != self.frontmatter_after(edits) {
            return false;
        }
        let respelled: HashSet<(LinkFamily, usize)> = edits
            .iter()
            .flat_map(|edit| &edit.links)
            .map(|link| (link.family, link.span.byte_offset))
            .collect();
        let (before, after) = (self.all_links(), reread.all_links());
        before.len() == after.len()
            && before.iter().zip(&after).all(|(was, is)| {
                let target = if respelled.contains(&(was.family, was.span.byte_offset)) {
                    to
                } else {
                    &was.target
                };
                reading(was) == reading(is) && is.target == target
            })
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
        edits: Vec<Edit<'d>>,
        to: &str,
        skipped: &mut Vec<SkippedLink>,
    ) -> Vec<Edit<'d>> {
        let mut kept = Vec::with_capacity(edits.len());
        for edit in edits {
            kept.push(edit);
            if !self.reads_as_rewritten(&self.spliced(&kept), &kept, to) {
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

    /// Every link this document holds, frontmatter first and then the body,
    /// in the order the index derives them in.
    fn all_links(&self) -> Vec<Link> {
        let mut links = self.frontmatter_wikilinks();
        links.extend(self.links());
        links
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

/// Whether `link` is one a rewrite of `from` names.
fn matches(link: &Link, from: &str) -> bool {
    !from.is_empty() && link.target == from && addresses_the_vault(link)
}

/// What a link reads as, its target and its display text aside: the parts a
/// rewrite must leave as they were.
fn reading(link: &Link) -> (LinkFamily, bool, Option<&str>, Option<&str>, Option<&str>) {
    (
        link.family,
        link.embed,
        link.protocol.as_deref(),
        link.anchor.as_deref(),
        link.block_ref.as_deref(),
    )
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
