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

use crate::document::Document;
use crate::link::{Link, LinkFamily, addresses_the_vault, respelled, splice_tokens};

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
        let mut rewritten = 0;
        let mut skipped = Vec::new();
        let body_links: Vec<Link> = self
            .scan_body()
            .links()
            .into_iter()
            .filter(|link| link.family == family)
            .collect();
        let body = splice_tokens(self.body(), &body_links, |link| {
            if !matches(link, from) {
                return None;
            }
            match respelled(link, to) {
                Ok(token) => {
                    rewritten += 1;
                    Some(token)
                }
                Err(reason) => {
                    skipped.push(SkippedLink {
                        link: link.clone(),
                        reason,
                    });
                    None
                }
            }
        });
        let mut text = self.source()[..self.body_start()].to_string();
        text.push_str(&body);
        RewrittenLinks {
            text,
            rewritten,
            skipped,
        }
    }
}

/// Whether `link` is one a rewrite of `from` names.
fn matches(link: &Link, from: &str) -> bool {
    !from.is_empty() && link.target == from && addresses_the_vault(link)
}
