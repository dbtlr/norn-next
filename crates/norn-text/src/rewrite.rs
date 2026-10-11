//! Rewriting a document's links: for each address a batch names, every link
//! of its family whose address it is, respelled to its own target — the pure
//! text half of a link cascade. See [`Document::rewrite_links`].

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
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

/// The most whole-document reads one [`Document::rewrite_links`] spends
/// proving which of its edits read back.
///
/// A batch that reads back whole is proven by one read. One that does not is
/// searched for the edits to leave as written, and each read of that search
/// costs the whole document; a batch where most edits fail would otherwise
/// read the document once per edit, quadratic in a holder's links. Past this
/// many reads the rewrite answers [`RewriteUnproven`] and writes nothing.
pub const REWRITE_PROOF_READS: usize = 128;

/// Why [`Document::rewrite_links`] rewrote nothing: proving which of the
/// batch's edits read back took more than [`REWRITE_PROOF_READS`] reads of
/// the document. A smaller batch is proven in fewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RewriteUnproven {
    /// The reads spent before the rewrite stopped.
    pub reads: usize,
}

impl std::fmt::Display for RewriteUnproven {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "proving which of the batch's link rewrites read back took more than {} reads of the document",
            self.reads
        )
    }
}

impl std::error::Error for RewriteUnproven {}

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
    /// scalar, a nested value, or a block whose fields cannot be told apart
    /// — has no bytes to write over and is [`RewriteSkip::Unplaced`],
    /// whatever its `to`. A matching link that cannot carry its `to` is left
    /// exactly as written and reported in [`RewrittenLinks::skipped`] with the
    /// [`RewriteSkip`] that says why; it is never forced. A batch that
    /// respells nothing returns the document's own bytes.
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
    ///
    /// # Errors
    ///
    /// [`RewriteUnproven`] where finding the edits that read back takes more
    /// than [`REWRITE_PROOF_READS`] reads of the document: nothing is
    /// rewritten, and a smaller batch may be proven.
    pub fn rewrite_links(
        &self,
        rewrites: &[AddressRewrite],
    ) -> Result<RewrittenLinks, RewriteUnproven> {
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

        let mut text = self.spliced(edits.iter());
        if !edits.is_empty() {
            let mut proof = Proof::of(self);
            if !proof.reads_back(&edits.iter().collect::<Vec<_>>())? {
                edits = proof.kept_in_runs(edits, &mut skipped)?;
                text = self.spliced(edits.iter());
            }
        }
        Ok(RewrittenLinks {
            text,
            rewritten: edits.len(),
            skipped: skipped.in_document_order(),
        })
    }

    /// This document's source with each edit's stem written as its target.
    /// The edits are in document order and do not overlap.
    fn spliced<'e>(&self, edits: impl Iterator<Item = &'e Edit>) -> String {
        let runs: Vec<(Range<usize>, &str)> = edits
            .map(|edit| (edit.range.clone(), edit.to.as_str()))
            .collect();
        splice_all(self.source(), &runs)
    }
}

/// The read-back proof of one document's edits, and the whole-document reads
/// it has spent ([`REWRITE_PROOF_READS`]).
struct Proof<'a, 'd> {
    document: &'a Document<'d>,
    before: Reading,
    reads: usize,
}

impl<'a, 'd> Proof<'a, 'd> {
    fn of(document: &'a Document<'d>) -> Self {
        Proof {
            document,
            before: Reading::of(document),
            reads: 0,
        }
    }

    /// Whether this document with `edits` spliced in reads as this document
    /// with exactly those edits' links respelled; `edits` are in document
    /// order.
    ///
    /// What the index derives from a document is read back from the spliced
    /// text and compared with what this document's own [`Reading`] becomes
    /// under the edits ([`Reading::respelled`]): the links of both families,
    /// frontmatter ones included, in the same order, with only the rewritten
    /// ones' targets changed, each to its own edit's; the headings and the
    /// tags of both sources as they were, a heading or link title holding a
    /// rewritten wikilink with only that link's token changed; and the code
    /// ranges, which decide which bytes can be any of those. Each is compared
    /// where its bytes moved to, so a fact that survives one byte further on
    /// is a fact that moved, and the proof fails.
    ///
    /// This is the proof every rewrite returns under, and it is about the
    /// whole document because what a target's bytes mean depends on what
    /// surrounds them: a backtick in a wikilink stem can pair with one later
    /// in the paragraph and turn the text between into code, a backslash can
    /// escape the bracket closing the Markdown link around it, a space ends a
    /// bare destination, `<!--` opens a comment that hides every tag after
    /// it, and a quote ends the frontmatter scalar quoted with it. So each
    /// call is one read of the whole document, and the
    /// [`REWRITE_PROOF_READS`]th is the last a rewrite may spend.
    fn reads_back(&mut self, edits: &[&Edit]) -> Result<bool, RewriteUnproven> {
        if self.reads == REWRITE_PROOF_READS {
            return Err(RewriteUnproven { reads: self.reads });
        }
        self.reads += 1;
        #[cfg(test)]
        tests::PROOF_READS.with(|reads| reads.set(reads.get() + 1));
        let respelling = Respelling::of(edits, &self.before);
        let text = self.document.spliced(edits.iter().copied());
        Ok(self
            .before
            .respelled(&respelling)
            .is_some_and(|expected| Reading::of(&Document::parse(&text)) == expected))
    }

    /// The edits kept in runs in document order, where `edits` together do
    /// not read back: from the first edit not yet decided, the longest run
    /// found to read back alongside the edits kept before it is kept, and the
    /// edit ending it is skipped. Each run is found by doubling its length
    /// until it fails to read back, then halving the gap between the longest
    /// that did and the shortest that did not, so the reads follow the edits
    /// skipped — about two for each per doubling of the run before it — rather
    /// than the edits in the batch.
    ///
    /// The answer is the one keeping each edit in turn when it reads back
    /// alongside the ones kept before it would give, wherever an edit that
    /// spoils a run spoils every longer one. Document order is the tie-break
    /// because it is the one a reader of the skips can predict: two edits
    /// that each read back alone may not read back together — two frontmatter
    /// links that each fit the block's byte bound, a backtick in one stem
    /// pairing with one in another — and then the first is kept and the
    /// second skipped, though the opposite choice would have read back as
    /// well.
    ///
    /// This is the path a rewrite takes only when the edits together did not
    /// read back, which is rare and is what the extra reads are spent on. An
    /// edit refused here is one whose target bytes read as something else
    /// where they were written, so a frontmatter one is skipped as corrupting
    /// its value and a body one as unrepresentable there.
    fn kept_in_runs(
        &mut self,
        edits: Vec<Edit>,
        skipped: &mut Skips,
    ) -> Result<Vec<Edit>, RewriteUnproven> {
        let mut kept: Vec<&Edit> = Vec::with_capacity(edits.len());
        let mut refused = vec![false; edits.len()];
        let mut next = 0;
        while next < edits.len() {
            let rest = &edits[next..];
            // The longest run known to read back, and the shortest known not
            // to.
            let mut good = 0;
            let mut bad = None;
            let mut run = 1;
            while bad.is_none() && good < rest.len() {
                let length = run.min(rest.len());
                if self.reads_back_with(&kept, &rest[..length])? {
                    good = length;
                    run *= 2;
                } else {
                    bad = Some(length);
                }
            }
            if let Some(mut bad) = bad {
                while bad - good > 1 {
                    let length = good + (bad - good) / 2;
                    if self.reads_back_with(&kept, &rest[..length])? {
                        good = length;
                    } else {
                        bad = length;
                    }
                }
                refused[next + good] = true;
            }
            kept.extend(&rest[..good]);
            next += good + usize::from(bad.is_some());
        }
        let mut kept = Vec::with_capacity(edits.len());
        for (edit, refused) in edits.into_iter().zip(refused) {
            if !refused {
                kept.push(edit);
                continue;
            }
            let reason = if edit.in_frontmatter {
                RewriteSkip::WouldCorruptFrontmatter
            } else {
                RewriteSkip::Unrepresentable
            };
            skipped.push(edit.ordinal, edit.link, reason);
        }
        Ok(kept)
    }

    /// Whether `kept`, then `run`, read back together.
    fn reads_back_with(&mut self, kept: &[&Edit], run: &[Edit]) -> Result<bool, RewriteUnproven> {
        let edits: Vec<&Edit> = kept.iter().copied().chain(run).collect();
        self.reads_back(&edits)
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
        let at = link.span.ok_or(RewriteSkip::Unplaced)?.byte_offset;
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
///
/// A proof asks it, for each fact of the document, which link it is, where
/// its bytes moved and which respelled links its text holds, so each answer
/// is a lookup rather than a pass over the stems: by the link's name, by a
/// search of the stems in order, and by a search of them ordered by where
/// their tokens begin.
struct Respelling<'t> {
    stems: Vec<RespelledStem<'t>>,
    /// Each stem's place in `stems`, by the family and token start naming its
    /// link.
    named: HashMap<(LinkFamily, usize), usize>,
    /// The bytes the first `k` stems write and the bytes they replace, at
    /// `k`: so where a byte moves is one search away.
    written: Vec<(usize, usize)>,
    /// The places in `stems`, ordered by the byte each token begins at and
    /// then by place.
    by_token: Vec<usize>,
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
    /// against `before`, the document's reading before the edits. The edits
    /// are in document order and do not overlap.
    fn of(edits: &[&'t Edit], before: &Reading) -> Self {
        let markdown_starts: HashSet<usize> = before
            .links
            .iter()
            .filter(|link| link.family == LinkFamily::Markdown)
            .filter_map(|link| link.at.as_ref().map(|at| at.start))
            .collect();
        let stems: Vec<RespelledStem<'t>> = edits
            .iter()
            .map(|&edit| RespelledStem {
                family: edit.link.family,
                at: edit.at,
                stem: edit.range.clone(),
                to: &edit.to,
                read_as: Self::read_as(&edit.link, &edit.token, markdown_starts.contains(&edit.at)),
            })
            .collect();
        let mut named = HashMap::with_capacity(stems.len());
        let mut written = Vec::with_capacity(stems.len() + 1);
        written.push((0, 0));
        for (place, stem) in stems.iter().enumerate() {
            count_stem_read();
            named.insert((stem.family, stem.at), place);
            let (grown, shrunk) = written[place];
            written.push((grown + stem.to.len(), shrunk + stem.stem.len()));
        }
        let mut by_token: Vec<usize> = (0..stems.len()).collect();
        by_token.sort_by_key(|&place| (stems[place].at, place));
        Respelling {
            stems,
            named,
            written,
            by_token,
        }
    }

    /// What a heading or link title holding `link` reads it as, before and
    /// once respelled to the token `new`.
    ///
    /// Such a text is flattened: a Markdown link reads as its bracket text
    /// alone, so its destination — the only bytes a rewrite changes — never
    /// stands in it, and respelling one changes no text (`None`). A wikilink
    /// reads as its token, except where it is the bracket text of a Markdown
    /// link beginning at the same byte — `[[a]](b)`, `shares_brackets` — and
    /// the two share the outer brackets: the text holds the token inside
    /// them, `[a]`.
    fn read_as(link: &'t Link, new: &'t str, shares_brackets: bool) -> Option<(&'t str, &'t str)> {
        if link.family == LinkFamily::Markdown {
            return None;
        }
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
        let place = *self.named.get(&(family, at))?;
        count_stem_read();
        Some(self.stems[place].to)
    }

    /// Where the source byte at `at` stands once every stem ending at or
    /// before it is written as its target. The stems do not overlap, so
    /// their ends ascend in document order.
    fn moved(&self, at: usize) -> usize {
        let ended = self.stems.partition_point(|respelled| {
            count_stem_read();
            respelled.stem.end <= at
        });
        let (grown, shrunk) = self.written[ended];
        at + grown - shrunk
    }

    /// `text`, the text of a fact whose bytes span `range`, as it reads once
    /// every respelled link other than `except` beginning inside `range` is
    /// rewritten: what the text reads each such link as replaced by what it
    /// reads it as respelled ([`Respelling::read_as`]), in document order,
    /// each found after the one before. A respelled Markdown link leaves the
    /// text as it was; the read-back of the links themselves is what proves
    /// its new destination still reads as one.
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
        let begins_before = |bound: usize| {
            self.by_token.partition_point(|&place| {
                count_stem_read();
                self.stems[place].at < bound
            })
        };
        let mut inside: Vec<usize> = self.by_token
            [begins_before(range.start)..begins_before(range.end)]
            .iter()
            .copied()
            .filter(|&place| {
                count_stem_read();
                let stem = &self.stems[place];
                except != Some((stem.family, stem.at))
            })
            .collect();
        inside.sort_unstable();
        let mut expected = String::with_capacity(text.len());
        let mut rest = text;
        for place in inside {
            let Some((old, new)) = &self.stems[place].read_as else {
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

/// Count one respelled stem a proof's lookup read ([`tests::STEMS_READ`]).
fn count_stem_read() {
    #[cfg(test)]
    tests::STEMS_READ.with(|reads| reads.set(reads.get() + 1));
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    thread_local! {
        /// Respelled stems the proofs on this thread read
        /// ([`count_stem_read`]).
        pub(super) static STEMS_READ: Cell<usize> = const { Cell::new(0) };
        /// Whole-document reads the proofs on this thread spent
        /// ([`Proof::reads_back`]).
        pub(super) static PROOF_READS: Cell<usize> = const { Cell::new(0) };
    }

    /// What `rewrites` make of `source`, the respelled stems its proof read,
    /// and the whole-document reads it spent.
    fn counted(
        source: &str,
        rewrites: &[AddressRewrite],
    ) -> (Result<RewrittenLinks, RewriteUnproven>, usize, usize) {
        STEMS_READ.with(|reads| reads.set(0));
        PROOF_READS.with(|reads| reads.set(0));
        let rewritten = Document::parse(source).rewrite_links(rewrites);
        (
            rewritten,
            STEMS_READ.with(Cell::get),
            PROOF_READS.with(Cell::get),
        )
    }

    fn wikilink(from: &str, to: &str) -> AddressRewrite {
        AddressRewrite::new(LinkFamily::Wikilink, from, to)
    }

    /// A hub of `entries` entries, each a heading holding a wikilink, a list
    /// item linking a document beside a tag and a code span, and a Markdown
    /// link whose text holds a wikilink; and a rewrite of every link it
    /// holds into `moved/`.
    fn hub(entries: usize) -> (String, Vec<AddressRewrite>) {
        let mut source = String::new();
        let mut rewrites = Vec::new();
        for entry in 0..entries {
            source.push_str(&format!(
                "## See [[h{entry}]]\n\n- [[d{entry}]] #tag{entry} `c{entry}`\n- [about [[t{entry}]]](x{entry}.md)\n\n"
            ));
            for stem in ["h", "d", "t"] {
                rewrites.push(wikilink(
                    &format!("{stem}{entry}"),
                    &format!("moved/{stem}{entry}"),
                ));
            }
            rewrites.push(AddressRewrite::new(
                LinkFamily::Markdown,
                format!("x{entry}.md"),
                format!("moved/x{entry}.md"),
            ));
        }
        (source, rewrites)
    }

    /// **A hub's read-back reads its respelled stems in time linear in its
    /// rewrites**: every link, heading, tag and code span is placed, named
    /// and carried by a lookup, so doubling the hub's entries doubles the
    /// stems read, give or take a search's one more step — where a pass over
    /// every stem per fact quadruples them.
    #[test]
    fn a_hubs_read_back_reads_its_stems_in_time_linear_in_its_rewrites() {
        let stems_read = |entries| {
            let (source, rewrites) = hub(entries);
            let (rewritten, stems, reads) = counted(&source, &rewrites);
            let rewritten = rewritten.expect("a batch reading back is proven");
            assert!(rewritten.skipped.is_empty(), "{:?}", rewritten.skipped);
            assert_eq!(rewritten.rewritten, rewrites.len());
            assert_eq!(reads, 1, "a batch reading back whole is one read");
            stems
        };
        let (few, many) = (stems_read(128), stems_read(256));
        assert!(
            many * 10 <= few * 25,
            "doubling a hub's rewrites took the stems read from {few} to {many}"
        );
    }

    /// A hub of `entries` list items, each linking a document, where the
    /// item at `spoiled` also holds a lone backtick, so a backtick written in
    /// its link's target pairs with it; and a rewrite of every link into
    /// `moved/`, the spoiled one's target holding a backtick.
    fn spoiled_hub(entries: usize, spoiled: usize) -> (String, Vec<AddressRewrite>) {
        let mut source = String::new();
        let mut rewrites = Vec::new();
        for entry in 0..entries {
            if entry == spoiled {
                source.push_str(&format!("- [[d{entry}]] and a lone ` tick\n"));
                rewrites.push(wikilink(&format!("d{entry}"), &format!("odd`/d{entry}")));
            } else {
                source.push_str(&format!("- [[d{entry}]]\n"));
                rewrites.push(wikilink(&format!("d{entry}"), &format!("moved/d{entry}")));
            }
        }
        (source, rewrites)
    }

    /// **One edit that does not read back costs a hub reads logarithmic in
    /// its rewrites**, not one per rewrite: the runs either side of it are
    /// found by doubling and halving, every other link is respelled, and the
    /// one is skipped as unrepresentable.
    #[test]
    fn one_edit_not_reading_back_costs_reads_logarithmic_in_the_hubs_rewrites() {
        for entries in [256_usize, 1024] {
            let (source, rewrites) = spoiled_hub(entries, entries / 2 + 1);
            let (rewritten, _, reads) = counted(&source, &rewrites);
            let rewritten = rewritten.expect("a proof within its reads");
            assert_eq!(rewritten.rewritten, entries - 1);
            let [skipped] = rewritten.skipped.as_slice() else {
                panic!("one skip: {:?}", rewritten.skipped);
            };
            assert_eq!(skipped.link.target, format!("d{}", entries / 2 + 1));
            assert_eq!(skipped.reason, RewriteSkip::Unrepresentable);
            let bound = 4 * entries.ilog2() as usize + 1;
            assert!(
                reads <= bound,
                "{entries} rewrites with one spoiled took {reads} reads, past {bound}"
            );
        }
    }

    /// One paragraph of `links` wikilinks, each rewritten to a target holding
    /// a backtick: alone, the first reads back, its backtick unpaired; every
    /// later one pairs with a backtick before it.
    fn backticked(links: usize) -> (String, Vec<AddressRewrite>) {
        let source: String = (0..links).map(|link| format!("[[d{link}]] ")).collect();
        let rewrites = (0..links)
            .map(|link| wikilink(&format!("d{link}"), &format!("odd`{link}")))
            .collect();
        (format!("{}\n", source.trim_end()), rewrites)
    }

    /// **A batch whose edits mostly do not read back is searched within
    /// [`REWRITE_PROOF_READS`] reads of the document, and past them rewrites
    /// nothing**: under the bound the first link is respelled and every
    /// other skipped, one read each; past it the rewrite answers
    /// [`RewriteUnproven`], having spent exactly the bound.
    #[test]
    fn a_batch_mostly_not_reading_back_is_searched_within_its_reads_and_past_them_rewrites_nothing()
    {
        let (source, rewrites) = backticked(REWRITE_PROOF_READS - 28);
        let (rewritten, _, reads) = counted(&source, &rewrites);
        let rewritten = rewritten.expect("a proof within its reads");
        assert_eq!(rewritten.rewritten, 1);
        assert_eq!(rewritten.skipped.len(), rewrites.len() - 1);
        assert!(
            rewritten
                .skipped
                .iter()
                .all(|skip| skip.reason == RewriteSkip::Unrepresentable)
        );
        assert!(reads <= REWRITE_PROOF_READS, "{reads}");

        let (source, rewrites) = backticked(2 * REWRITE_PROOF_READS);
        let (rewritten, _, reads) = counted(&source, &rewrites);
        assert_eq!(
            rewritten,
            Err(RewriteUnproven {
                reads: REWRITE_PROOF_READS
            })
        );
        assert_eq!(reads, REWRITE_PROOF_READS);
    }

    /// `source` rewritten by `rewrites` keeping each edit in turn where it
    /// reads back alongside the ones kept before it: the rule the runs answer
    /// for, read one edit at a time.
    fn kept_one_at_a_time(source: &str, rewrites: &[AddressRewrite]) -> String {
        let document = Document::parse(source);
        let rules = Rules::of(rewrites);
        let mut skipped = Skips::default();
        let mut edits = Vec::new();
        let frontmatter = document
            .frontmatter_wikilinks()
            .into_iter()
            .map(|link| (link, true));
        let body = document.links().into_iter().map(|link| (link, false));
        for (ordinal, (link, in_frontmatter)) in frontmatter.chain(body).enumerate() {
            let Some(Ok(to)) = rules.for_link(&link) else {
                continue;
            };
            if let Ok((at, range, token)) = to.respell(&link) {
                edits.push(Edit {
                    ordinal,
                    at,
                    range,
                    to: to.target.clone(),
                    token,
                    link,
                    in_frontmatter,
                });
            }
        }
        let edits = disjoint(edits, &mut skipped);
        let mut proof = Proof::of(&document);
        let mut kept: Vec<&Edit> = Vec::new();
        for edit in &edits {
            kept.push(edit);
            if !proof.reads_back(&kept).expect("within the reads") {
                kept.pop();
            }
        }
        document.spliced(kept.into_iter())
    }

    /// **The runs keep what keeping each edit in turn keeps** wherever an
    /// edit spoiling a run spoils every longer one: backticks pairing across
    /// links, a lone backtick a single target pairs with, and frontmatter
    /// links whose targets would end their quoted scalars.
    #[test]
    fn the_runs_keep_what_keeping_each_edit_in_turn_keeps() {
        let cases: Vec<(String, Vec<AddressRewrite>)> = vec![
            backticked(9),
            spoiled_hub(40, 0),
            spoiled_hub(40, 17),
            spoiled_hub(40, 39),
            (
                "---\nup: \"[[a]]\"\nside: \"[[b]]\"\nnext: '[[c]]'\n---\n[[a]] `x [[b]] and [[c]]\n"
                    .to_string(),
                vec![
                    wikilink("a", "q\"a"),
                    wikilink("b", "b`b"),
                    wikilink("c", "c'c"),
                ],
            ),
            (
                "[[a]] [[b]] [[c]] [[d]] ` [[e]] [[f]]\n".to_string(),
                vec![
                    wikilink("a", "a`"),
                    wikilink("b", "b"),
                    wikilink("c", "`c"),
                    wikilink("d", "d`d"),
                    wikilink("e", "e"),
                    wikilink("f", "f`"),
                ],
            ),
        ];
        for (source, rewrites) in cases {
            let rewritten = Document::parse(&source)
                .rewrite_links(&rewrites)
                .expect("within the reads");
            assert!(!rewritten.skipped.is_empty(), "{source}");
            assert_eq!(
                rewritten.text,
                kept_one_at_a_time(&source, &rewrites),
                "{source}"
            );
        }
    }
}
