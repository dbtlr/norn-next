//! Rewriting a document's links: every link of one family whose target is one
//! text, respelled to another.
//!
//! This is the pure text half of a link cascade. A move, a redirecting delete
//! or a wikilink rewrite expands, per document holding an affected link, into
//! *in this document, every link of this family whose target is `from` is
//! respelled `to`*, and [`Document::rewrite_links`] is what composes that
//! document's after-bytes — for the planner that previews them and for the
//! applier that writes them alike, so the two cannot disagree.
//!
//! # The match is the index's
//!
//! What a link resolves to depends on the document holding it, its family and
//! its target text, so a cascade names the links it rewrites by exactly those.
//! `from` is compared against [`Link::target`] as this crate's one parse reads
//! it — the text the store's `links.target` column is derived from, never
//! decoded, unescaped or normalized on either side — so a link the index holds
//! under `from` is a link this rewrite matches, and no other is. The two
//! families' targets are different texts read by different grammars, so a
//! rewrite names one family, and a Markdown link whose target happens to spell
//! `from` is not touched by a wikilink rewrite.
//!
//! Three kinds of link never match. An empty `from` matches nothing, because
//! a target-less link — `[[#Heading]]` — addresses the document holding it
//! and is never what a cascade renames. A link written with a protocol other
//! than `vault` addresses something outside the vault, whose stem merely
//! spells the same text. And code is opaque: a link written inside a code span
//! or block is not a link.
//!
//! # Rewritten, or skipped with a reason
//!
//! Only a link's stem bytes change, so everything else it was written with —
//! an embed marker, a title, an anchor, padding, a protocol — survives byte for
//! byte, and so does every byte outside the links rewritten. A matching link
//! that cannot carry `to` is left exactly as written and reported in
//! [`RewrittenLinks::skipped`] with the [`RewriteSkip`] that says why; it is
//! never forced. A rewrite that matched nothing returns the document's own
//! bytes.

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
/// Plain rather than `#[non_exhaustive]`, like every enum here a caller
/// reports from: a new reason should reach every place that words one.
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

impl<'a> Document<'a> {
    /// Respell every link of `family` whose target is `from` to `to`, and
    /// return the whole rewritten document.
    ///
    /// See [the module](crate::rewrite) for what matches, what is preserved
    /// and why a matching link is skipped rather than written.
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
                }),
                Err(reason) => skipped.push(SkippedLink { link, reason }),
            }
        }
        skipped.sort_by_key(|skip| skip.link.span.byte_offset);
        RewrittenLinks {
            text: self.spliced(&edits),
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
    fn frontmatter_edits(
        &self,
        from: &str,
        to: &str,
        edits: &mut Vec<Edit>,
        skipped: &mut Vec<SkippedLink>,
    ) {
        let Some(Value::Map(map)) = self.frontmatter() else {
            return;
        };
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
            let mut expected = map.clone();
            with_text(&mut expected, literal.field, literal.item, &text);
            let edit = Edit {
                range: literal.start..literal.start + literal.text.len(),
                replacement: text,
                links,
            };
            if frontmatter_of(&self.spliced(std::slice::from_ref(&edit)))
                == Some(Value::Map(expected))
            {
                edits.push(edit);
            } else {
                skipped.extend(edit.links.into_iter().map(|link| SkippedLink {
                    link,
                    reason: RewriteSkip::WouldCorruptFrontmatter,
                }));
            }
        }
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
/// there: one body token, or one frontmatter string holding one or more.
struct Edit {
    range: Range<usize>,
    replacement: String,
    links: Vec<Link>,
}

/// Whether `link` is one a rewrite of `from` names.
fn matches(link: &Link, from: &str) -> bool {
    !from.is_empty() && link.target == from && addresses_the_vault(link)
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
