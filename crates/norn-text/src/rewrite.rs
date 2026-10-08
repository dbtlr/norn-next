//! Rewriting a document's links: for each address a batch names, every link
//! of its family whose address it is, respelled to its own target — the pure
//! text half of a link cascade. See [`Document::rewrite_links`].

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ops::Range;

use crate::document::{Document, splice_all};
use crate::link::{Link, LinkFamily, RewriteSkip, addresses_the_vault, respelled, split_protocol};

/// One address a link rewrite respells: in the document rewritten, every link
/// of `family` whose address is `from` is respelled `to`.
///
/// It is the document-local half of a cascade's per-document rewrite — the
/// wire's `LinkRewrite` adds the path of the document holding the links — and
/// is named for the address it keys links by, so the two never share a name
/// in a module that holds both.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AddressRewrite {
    /// The family of the links respelled.
    pub family: LinkFamily,
    /// The address a respelled link is written with, protocol prefix and all.
    pub from: String,
    /// The address it is written with after, protocol prefix and all.
    pub to: String,
}

impl AddressRewrite {
    /// Every link of `family` whose address is `from`, respelled `to`.
    pub fn new(family: LinkFamily, from: impl Into<String>, to: impl Into<String>) -> Self {
        AddressRewrite {
            family,
            from: from.into(),
            to: to.into(),
        }
    }
}

/// What [`Document::rewrite_links`] produced: the whole rewritten document,
/// how many links it respelled, and each matching link it left alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewrittenLinks {
    /// The whole document, rewritten. Byte-identical to the input when nothing
    /// was rewritten.
    pub text: String,
    /// How many links now read their rewrite's `to` that read its `from`.
    pub rewritten: usize,
    /// Every matching link left as written, in document order — frontmatter
    /// first, as the index numbers them — each with why.
    pub skipped: Vec<SkippedLink>,
}

/// A link that matched a rewrite and was left exactly as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedLink {
    /// The link as the input document holds it, in source coordinates where
    /// it has a place there.
    pub link: Link,
    pub reason: RewriteSkip,
}

impl Document<'_> {
    /// Respell, for each [`AddressRewrite`] in `rewrites`, every link of its
    /// family whose address is its `from` to its `to`, and return the whole
    /// rewritten document.
    ///
    /// A move, a redirecting delete or a wikilink rewrite expands, per
    /// document holding an affected link, into *in this document, every link
    /// of this family whose address is `from` is respelled `to`* — one such
    /// rewrite per address the document holds an affected link under — and
    /// this is what composes that document's after-bytes from all of them at
    /// once, for the planner that previews them and for the applier that
    /// writes them alike, so the two cannot disagree. A single rewrite is a
    /// batch of one.
    ///
    /// # One pass over one parse
    ///
    /// Every link is matched against this document as it is, by the address
    /// it is written with here, and respelled to the `to` of the rewrite that
    /// names that address. No link is matched against bytes another rewrite in
    /// the batch wrote, so a rewrite whose `to` is another's `from` never
    /// chains: `../index.md → index.md` and `index.md → e/index.md` respell a
    /// link written `../index.md` to `index.md` and one written `index.md` to
    /// `e/index.md`, and two rewrites swapping `a` and `b` swap them.
    ///
    /// A batch names each address — a family, a protocol and a target — once.
    /// Naming one twice with the same `to` says the same thing twice and is
    /// one rewrite. Naming one twice with different `to`s contradicts itself,
    /// and no choice between the two is this crate's to make: every link under
    /// that address is left as written and skipped as
    /// [`RewriteSkip::ConflictingRewrites`], whatever else would have refused
    /// it, and the rest of the batch is rewritten as if the address were not
    /// named.
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
    /// tables to see. Two links of one batch whose stems nest — a wikilink
    /// written in a Markdown destination — cannot both be respelled, since
    /// either rewrites the other's bytes, so the inner one is unrepresentable
    /// and the outer one is proven like any other. A frontmatter link with no
    /// place in the source — one in a flow sequence, an escaped or folded
    /// scalar, a nested value — has no bytes to write over and is
    /// [`RewriteSkip::NotWrittenLiterally`], whatever its `to`. A matching
    /// link that cannot carry its `to` is left exactly as written and
    /// reported in [`RewrittenLinks::skipped`] with the [`RewriteSkip`] that
    /// says why; it is never forced. A batch that respells nothing returns
    /// the document's own bytes.
    ///
    /// The result is proven by reading it back: everything the index derives
    /// from the document reads as it did, where its bytes moved to, with only
    /// the rewritten targets changed, each to its own rewrite's `to` — the
    /// same links in the same order, and the same headings, tags and code. A
    /// heading or a link title whose own
    /// text holds a rewritten wikilink reads exactly as it did with that
    /// link's old token replaced by its new one, so a target whose bytes would
    /// read as markup there — a `*` pairing with a literal one later in the
    /// heading — is caught. A text that does not hold the old token as written
    /// (or, for `[[a]](b)`, where the two links share their outer brackets,
    /// the token inside them) cannot be proven so and skips the edit. Such a
    /// text reads a Markdown link as its bracket text alone, so one holding a
    /// rewritten Markdown link reads exactly as it did. An edit that does not
    /// read back is skipped where it stands rather than returned.
    pub fn rewrite_links(&self, rewrites: &[AddressRewrite]) -> RewrittenLinks {
        let rules = Rules::of(rewrites);
        let mut edits = Vec::new();
        let mut skipped = Skips::default();
        let frontmatter = self
            .frontmatter_wikilinks()
            .into_iter()
            .map(|link| (link, true));
        let body = self.links().into_iter().map(|link| (link, false));
        for (ordinal, (link, in_frontmatter)) in frontmatter.chain(body).enumerate() {
            let Some(rule) = rules.for_link(&link) else {
                continue;
            };
            match rule.and_then(|to| Ok((to.respell(&link)?, to.target.clone()))) {
                Ok(((at, range, token), to)) => edits.push(Edit {
                    ordinal,
                    at,
                    range,
                    to,
                    token,
                    link,
                    in_frontmatter,
                }),
                Err(reason) => skipped.push(ordinal, link, reason),
            }
        }
        let mut edits = disjoint(edits, &mut skipped);

        let mut text = self.spliced(&edits);
        if !edits.is_empty() {
            let before = Reading::of(self);
            if !self.reads_as_rewritten(&before, &text, &edits) {
                edits = self.kept_in_order(&before, edits, &mut skipped);
                text = self.spliced(&edits);
            }
        }
        RewrittenLinks {
            text,
            rewritten: edits.len(),
            skipped: skipped.in_document_order(),
        }
    }

    /// Whether `text` — this document with `edits` spliced in — reads as this
    /// document with exactly those edits' links respelled.
    ///
    /// What the index derives from a document is read back from `text` and
    /// compared with what this document's own [`Reading`] becomes under the
    /// edits ([`Reading::respelled`]): the links of both families, frontmatter
    /// ones included, in the same order, with only the rewritten ones' targets
    /// changed, each to its own edit's; the headings and the tags of both
    /// sources as they were, a heading or link title holding a rewritten
    /// wikilink with only that link's token changed; and the code ranges,
    /// which decide which bytes can be any of those. Each is compared where
    /// its bytes moved to, so a fact that survives one byte further on is a
    /// fact that moved, and the proof fails.
    ///
    /// This is the proof every rewrite returns under, and it is about the whole
    /// document because what a target's bytes mean depends on what surrounds
    /// them: a backtick in a wikilink stem can pair with one later in the
    /// paragraph and turn the text between into code, a backslash can escape
    /// the bracket closing the Markdown link around it, a space ends a bare
    /// destination, `<!--` opens a comment that hides every tag after it, and
    /// a quote ends the frontmatter scalar quoted with it.
    fn reads_as_rewritten(&self, before: &Reading, text: &str, edits: &[Edit]) -> bool {
        let respelling = Respelling::of(edits, before);
        before
            .respelled(&respelling)
            .is_some_and(|expected| Reading::of(&Document::parse(text)) == expected)
    }

    /// The edits kept greedily in document order: each is kept when it reads
    /// back alongside the ones kept before it, and every other edit's link is
    /// skipped.
    ///
    /// The answer depends on that order. Two edits that each read back alone
    /// may not read back together — two frontmatter links that each fit the
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
    fn kept_in_order(&self, before: &Reading, edits: Vec<Edit>, skipped: &mut Skips) -> Vec<Edit> {
        let mut kept = Vec::with_capacity(edits.len());
        for edit in edits {
            kept.push(edit);
            if !self.reads_as_rewritten(before, &self.spliced(&kept), &kept) {
                let refused = kept.pop().expect("the edit just kept");
                let reason = if refused.in_frontmatter {
                    RewriteSkip::WouldCorruptFrontmatter
                } else {
                    RewriteSkip::Unrepresentable
                };
                skipped.push(refused.ordinal, refused.link, reason);
            }
        }
        kept
    }

    /// This document's source with each edit's stem written as its target.
    /// The edits are in document order and do not overlap.
    fn spliced(&self, edits: &[Edit]) -> String {
        let runs: Vec<(Range<usize>, &str)> = edits
            .iter()
            .map(|edit| (edit.range.clone(), edit.to.as_str()))
            .collect();
        splice_all(self.source(), &runs)
    }
}

/// One link a rewrite respells: the source bytes its stem stands at, the
/// target written over them, and the link's whole token once they are.
///
/// An edit is the stem alone, so `[[a]](b)` — a wikilink and a Markdown link
/// sharing their outer brackets — is two edits that do not touch. A
/// frontmatter link is an edit of its own like a body one: a string's text is
/// its source bytes, so a stem in it is written where it stands, and each link
/// in one string, carrying its own rewrite's `to`, is proven and skipped on
/// its own.
struct Edit {
    /// Where the link stands among the document's links, frontmatter first,
    /// which is the order its skip is reported in.
    ordinal: usize,
    /// The source byte the link's token begins at.
    at: usize,
    range: Range<usize>,
    to: String,
    token: String,
    link: Link,
    in_frontmatter: bool,
}

/// `edits` in document order, each whose stem lies inside one before it
/// skipped as unrepresentable.
///
/// Stems nest where one link is written inside another's: `[t]([[b]])` is a
/// Markdown link whose destination holds a wikilink. Respelling the outer
/// stem rewrites the inner link's bytes, so the two cannot both be written;
/// the outer one is kept here and stands or falls by the read-back, which
/// finds the inner link it rewrote.
fn disjoint(mut edits: Vec<Edit>, skipped: &mut Skips) -> Vec<Edit> {
    edits.sort_by_key(|edit| edit.range.start);
    let mut kept: Vec<Edit> = Vec::with_capacity(edits.len());
    for edit in edits {
        if kept
            .last()
            .is_some_and(|last| edit.range.start < last.range.end)
        {
            skipped.push(edit.ordinal, edit.link, RewriteSkip::Unrepresentable);
        } else {
            kept.push(edit);
        }
    }
    kept
}

/// The links a rewrite leaves as written, each with where it stands among the
/// document's links. A link with no place in the source has no byte offset to
/// order it by, so the skips are ordered by that position instead.
#[derive(Default)]
struct Skips(Vec<(usize, SkippedLink)>);

impl Skips {
    fn push(&mut self, ordinal: usize, link: Link, reason: RewriteSkip) {
        self.0.push((ordinal, SkippedLink { link, reason }));
    }

    fn in_document_order(mut self) -> Vec<SkippedLink> {
        self.0.sort_by_key(|(ordinal, _)| *ordinal);
        self.0.into_iter().map(|(_, skip)| skip).collect()
    }
}

/// A batch's rewrites keyed by the address each matches: its family, its
/// protocol and its target. An address named twice with two `to`s is a
/// conflict rather than either.
struct Rules(HashMap<(LinkFamily, Option<String>, String), Result<Address, RewriteSkip>>);

impl Rules {
    fn of(rewrites: &[AddressRewrite]) -> Self {
        let mut rules = HashMap::with_capacity(rewrites.len());
        for rewrite in rewrites {
            let from = Address::of(&rewrite.from);
            let to = Address::of(&rewrite.to);
            match rules.entry((rewrite.family, from.protocol, from.target)) {
                Entry::Vacant(entry) => {
                    entry.insert(Ok(to));
                }
                Entry::Occupied(mut entry) => {
                    let rule = entry.get_mut();
                    if rule.as_ref().is_ok_and(|named| *named != to) {
                        *rule = Err(RewriteSkip::ConflictingRewrites);
                    }
                }
            }
        }
        Rules(rules)
    }

    /// The address `link` is respelled to, why the batch cannot say, or
    /// `None` where no rewrite names it — never for an empty target or a link
    /// addressed outside the vault.
    fn for_link(&self, link: &Link) -> Option<Result<&Address, RewriteSkip>> {
        if link.target.is_empty() || !addresses_the_vault(link) {
            return None;
        }
        let key = (link.family, link.protocol.clone(), link.target.clone());
        self.0
            .get(&key)
            .map(|rule| rule.as_ref().map_err(|&reason| reason))
    }
}

/// A link's address as a rewrite is handed it: written whole, with any
/// `protocol://` prefix, and read by the one splitter a link's own is.
#[derive(PartialEq)]
struct Address {
    protocol: Option<String>,
    target: String,
}

impl Address {
    fn of(written: &str) -> Self {
        let (protocol, target) = split_protocol(written);
        Address { protocol, target }
    }

    /// The source byte `link`'s token begins at, the source bytes its stem
    /// stands at, and `link`'s token with this address's target written over
    /// them ([`respelled`]), or why the link cannot carry it. A link with no
    /// place in the source has no bytes to write over, whatever the target. A
    /// rewrite never changes how a link is addressed, so a protocol other than
    /// the link's own is unrepresentable there.
    fn respell(&self, link: &Link) -> Result<(usize, Range<usize>, String), RewriteSkip> {
        let at = link
            .span
            .ok_or(RewriteSkip::NotWrittenLiterally)?
            .byte_offset;
        let token = respelled(link, &self.target)?;
        if link.protocol != self.protocol {
            return Err(RewriteSkip::Unrepresentable);
        }
        let stem = link
            .stem_range
            .clone()
            .expect("a link respelled names its stem");
        Ok((at, at + stem.start..at + stem.end, token))
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
/// - The frontmatter's value. A frontmatter link is placed only where its
///   string reads as exactly the bytes it is written with, so a rewritten
///   string that read as anything else — or a block that stopped reading, or
///   a structure that changed around the string — loses the place of the
///   link the rewrite placed there, or the link. A link with no place is
///   compared by what it reads as and where it stands among the links.
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

/// A link as the index reads it, at the bytes its token spans where it has
/// a place in the source.
#[derive(Debug, Clone, PartialEq)]
struct LinkReading {
    family: LinkFamily,
    embed: bool,
    protocol: Option<String>,
    target: String,
    title: Option<String>,
    anchor: Option<String>,
    block_ref: Option<String>,
    at: Option<Range<usize>>,
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
    /// bytes moved to, each respelled link targeting its own `to`, and each heading or
    /// link title holding a respelled wikilink with that link's token
    /// respelled ([`Respelling::carried`]). `None` where such a text does not
    /// hold the wikilink as written, so what it should read as is unknown.
    fn respelled(&self, respelling: &Respelling<'_>) -> Option<Reading> {
        let moved = |at: &Range<usize>| respelling.moved(at.start)..respelling.moved(at.end);
        let links = self
            .links
            .iter()
            .map(|link| {
                // A link with no place holds no respelled stem, in its title
                // or as itself, and reads as it did.
                let Some(at) = &link.at else {
                    return Some(link.clone());
                };
                let except = Some((link.family, at.start));
                let title = match &link.title {
                    Some(title) => Some(respelling.carried(title, at, except)?),
                    None => None,
                };
                Some(LinkReading {
                    target: respelling
                        .target_of(link.family, at.start)
                        .unwrap_or(&link.target)
                        .to_string(),
                    title,
                    at: Some(moved(at)),
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
        let at = link.range().map(|at| base + at.start..base + at.end);
        LinkReading {
            family: link.family,
            embed: link.embed,
            protocol: link.protocol,
            target: link.target,
            title: link.title,
            anchor: link.anchor,
            block_ref: link.block_ref,
            at,
        }
    }
}

/// The links a set of edits respells, in document order: exactly what the
/// edits replace, so where every other byte moves to follows.
struct Respelling<'t> {
    stems: Vec<RespelledStem<'t>>,
}

/// One link a respelling rewrites: its family and the byte its token begins
/// at, which name it; the source bytes its stem stands at and the target
/// written over them; and the text a heading or link title holding it reads
/// it as, before and after ([`Respelling::carried`]).
struct RespelledStem<'t> {
    family: LinkFamily,
    at: usize,
    stem: Range<usize>,
    to: &'t str,
    read_as: Option<(&'t str, &'t str)>,
}

impl<'t> Respelling<'t> {
    /// The links `edits` respell, each to its own edit's target, each read
    /// against `before`, the document's reading before the edits.
    fn of(edits: &'t [Edit], before: &Reading) -> Self {
        let stems = edits
            .iter()
            .map(|edit| RespelledStem {
                family: edit.link.family,
                at: edit.at,
                stem: edit.range.clone(),
                to: &edit.to,
                read_as: Self::read_as(&edit.link, edit.at, &edit.token, before),
            })
            .collect();
        Respelling { stems }
    }

    /// What a heading or link title holding `link` reads it as, before and
    /// once respelled to the token `new`.
    ///
    /// Such a text is flattened: a Markdown link reads as its bracket text
    /// alone, so its destination — the only bytes a rewrite changes — never
    /// stands in it, and respelling one changes no text (`None`). A wikilink
    /// reads as its token, except where it is the bracket text of a Markdown
    /// link beginning at the same byte — `[[a]](b)` — and the two share the
    /// outer brackets: the text holds the token inside them, `[a]`.
    fn read_as(
        link: &'t Link,
        at: usize,
        new: &'t str,
        before: &Reading,
    ) -> Option<(&'t str, &'t str)> {
        if link.family == LinkFamily::Markdown {
            return None;
        }
        let shares_brackets = before.links.iter().any(|other| {
            other.family == LinkFamily::Markdown
                && other.at.as_ref().is_some_and(|other| other.start == at)
        });
        if !shares_brackets {
            return Some((&link.raw, new));
        }
        fn inside(token: &str) -> Option<&str> {
            token.get(1..token.len().checked_sub(1)?)
        }
        Some((inside(&link.raw)?, inside(new)?))
    }

    /// The target the link of `family` whose token begins at `at` is
    /// respelled to, where this respelling rewrites it.
    fn target_of(&self, family: LinkFamily, at: usize) -> Option<&'t str> {
        self.stems
            .iter()
            .find(|stem| (stem.family, stem.at) == (family, at))
            .map(|stem| stem.to)
    }

    /// Where the source byte at `at` stands once every stem ending at or
    /// before it is written as its target.
    fn moved(&self, at: usize) -> usize {
        let (grown, shrunk) = self
            .stems
            .iter()
            .filter(|respelled| respelled.stem.end <= at)
            .fold((0, 0), |(grown, shrunk), respelled| {
                (grown + respelled.to.len(), shrunk + respelled.stem.len())
            });
        at + grown - shrunk
    }

    /// `text`, the text of a fact whose bytes span `range`, as it reads once
    /// every respelled link other than `except` beginning inside `range` is
    /// rewritten: what the text reads each such link as replaced by what it
    /// reads it as respelled ([`Respelling::read_as`]), in order, each found
    /// after the one before. A respelled Markdown link leaves the text as it
    /// was; the read-back of the links themselves is what proves its new
    /// destination still reads as one.
    ///
    /// `None` where `text` does not hold a wikilink as it should read — the
    /// link holding markup the text flattens, say — so the proof cannot say
    /// what the text reads as and fails rather than guess. A literal copy of
    /// it earlier in the text, in a code span say, is found first, and the
    /// proof fails then too: conservative, never wrong.
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
            let Some((old, new)) = &respelled.read_as else {
                continue;
            };
            let found = rest.find(old)?;
            expected.push_str(&rest[..found]);
            expected.push_str(new);
            rest = &rest[found + old.len()..];
        }
        expected.push_str(rest);
        Some(expected)
    }
}
