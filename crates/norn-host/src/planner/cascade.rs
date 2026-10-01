//! Link cascades: the rewrites a plan's document moves generate, so that a
//! link naming a moved document names it where it lands.
//!
//! **Which links a move breaks is the change set's own question.** A link
//! follows a move where, before the plan, it resolves to exactly the document
//! the move carries away, and after the plan does not resolve to exactly the
//! file the move carries it to. Both sides are read through the store's
//! resolution door, over the overlay and probes the change set itself reads
//! ([`reach`]), so the backlinks a cascade rewrites are the links the set
//! would otherwise record leaving: the store's links under a key that could
//! name the moved document, and the links the plan's own documents hold. A
//! move that keeps its document's stem, so its bare backlinks still name it
//! alone, and a relative link between two documents one folder move carries
//! together, need nothing. A link resolving to several documents before the
//! plan is never rewritten: which it names is not known, and the forecast
//! says so.
//!
//! **A link is respelled in its own style, and only to a spelling that reads
//! back.** The new address is the shortest spelling of the moved document's
//! destination the link's syntax and protocol can write: for a bare wikilink
//! the shortest suffix of the destination that names it alone with every
//! target of the plan at its after-state — at least two segments where the
//! link was written path-qualified, so it stays so — keeping the document
//! extension only where the link was written with it; for a `vault://`
//! wikilink the destination's root path; and for a Markdown link the
//! destination's path from the holder's folder, or from the root where the
//! link was written from the root, in the link's own percent-escaping. Each
//! candidate is probed through the same door, from the holder where it stands
//! after the plan, and the first that resolves to exactly the destination is
//! written. A link no candidate reaches is left as written, and the forecast
//! says it is unrepresentable.
//!
//! **A moved document's own relative links name what they named.** Each
//! relative Markdown link a moved document holds is read from where the
//! document stood, and respelled from where it lands toward the same file —
//! or toward where the plan carries that file — whether it names a document
//! or an attachment; an anchor-only link names its holder wherever it goes.
//!
//! **A cascade travels on the move it serves.** Each rewrite is one
//! `rewrite_link` per holder, syntax and address, named at the holder's
//! after-state path, generated once however many ways it is reached and
//! carried by the move that lands the named document — a moved document's
//! own relative links by its own move. Composition then writes it on the
//! holder's final bytes ([`super::compose::compose`]), the same bytes it was
//! read from here.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use norn_fs::{NormalizedPath, PathNormalizer};
use norn_store::{LinkFact, ProbedLink};
use norn_wire::{DOCUMENT_EXTENSION, DocumentPath, LinkAddress, LinkFamily, LinkRewrite, Resolves};

use super::compose::Composition;
use super::lineage::Lineage;
use super::links::{
    EntryKey, LinkIndex, Target, WrittenLinks, address, family_name, reach, stored_path,
    wire_family,
};
use crate::derivation::document_links;

/// The link cascade each move of the plan `composition` composed generates,
/// by the move's position: every link that names a document the plan's moves
/// carry away, as `lineage` follows them, and would not name it after,
/// respelled to name it where it lands, judged through `index`. Empty, and
/// the index never asked, where no move carries a document.
pub(crate) fn generate<I: LinkIndex + ?Sized>(
    composition: &Composition,
    lineage: &Lineage,
    normalizer: &PathNormalizer,
    index: &I,
) -> Result<BTreeMap<usize, Vec<LinkRewrite>>, I::Error> {
    if lineage.drawing().next().is_none() {
        return Ok(BTreeMap::new());
    }
    let cascade = Cascade {
        composition,
        lineage,
        normalizer,
    };
    let targets = Target::of(composition);
    let (overlay, probed) = reach(&targets, lineage, normalizer, &WrittenLinks::default());

    // The links the moves break, each with the file it must name after.
    let mut breaking: Vec<Breaking> = Vec::new();
    index.changes(&overlay, &probed, &mut |change| {
        let Resolves::One { path } = &change.before else {
            return;
        };
        let Some((to, owner)) = cascade.carried(path.as_str()) else {
            return;
        };
        if let Resolves::One { path } = &change.after
            && cascade.identity(path.as_str()).as_ref() == Some(&to)
        {
            return;
        }
        breaking.push(Breaking {
            holder: change.holder,
            link: change.link,
            to,
            owner,
        });
    })?;

    let mut rewrites: BTreeMap<EntryKey, (usize, LinkRewrite)> = cascade.own_relative_links();

    // Every spelling each breaking link could take, probed from its holder
    // in one judgment.
    // A link written twice in one holder is one key, asked about once.
    let mut asked: Vec<(usize, Vec<String>)> = Vec::new();
    let mut probes: Vec<ProbedLink> = Vec::new();
    let mut seen: BTreeSet<EntryKey> = BTreeSet::new();
    for (at, broken) in breaking.iter().enumerate() {
        let Some(holder) = wire_path(&broken.holder) else {
            continue;
        };
        let key = entry(&holder, &broken.link);
        if rewrites.contains_key(&key) || !seen.insert(key) {
            continue;
        }
        let candidates = cascade.candidates(broken);
        for candidate in &candidates {
            probes.push(ProbedLink {
                before_holder: broken.holder.clone(),
                after_holder: broken.holder.clone(),
                link: spelled(&broken.link, candidate),
                written: true,
            });
        }
        asked.push((at, candidates));
    }
    let mut answered: BTreeMap<(String, String), Resolves> = BTreeMap::new();
    if !probes.is_empty() {
        index.changes(&overlay, &probes, &mut |change| {
            if change.written {
                answered.insert(
                    (change.holder.as_str().to_string(), address(&change.link)),
                    change.after,
                );
            }
        })?;
    }
    for (at, candidates) in asked {
        let broken = &breaking[at];
        let Some(holder) = wire_path(&broken.holder) else {
            continue;
        };
        let reads_back = candidates.into_iter().find(|candidate| {
            let probed = address(&spelled(&broken.link, candidate));
            matches!(
                answered.get(&(broken.holder.as_str().to_string(), probed)),
                Some(Resolves::One { path })
                    if cascade.identity(path.as_str()).as_ref() == Some(&broken.to)
            )
        });
        let Some(candidate) = reads_back else {
            continue;
        };
        let rewrite = LinkRewrite::new(
            holder.clone(),
            wire_family(broken.link.family),
            address(&broken.link),
            address(&spelled(&broken.link, &candidate)),
        );
        rewrites
            .entry(entry(&holder, &broken.link))
            .or_insert((broken.owner, rewrite));
    }

    let mut cascades: BTreeMap<usize, Vec<LinkRewrite>> = BTreeMap::new();
    for (owner, rewrite) in rewrites.into_values() {
        cascades.entry(owner).or_default().push(rewrite);
    }
    Ok(cascades)
}

/// One link a move breaks: where it is held after the plan, as it is
/// written, the file it must name after, and the move that lands that file.
struct Breaking {
    holder: norn_store::DocumentPath,
    link: LinkFact,
    to: NormalizedPath,
    owner: usize,
}

/// What a cascade is generated from: the composed plan and where its content
/// ends.
struct Cascade<'a> {
    composition: &'a Composition,
    lineage: &'a Lineage,
    normalizer: &'a PathNormalizer,
}

impl Cascade<'_> {
    fn identity(&self, path: &str) -> Option<NormalizedPath> {
        self.normalizer.normalize(Path::new(path)).ok()
    }

    /// The file the plan's moves carry the document at `path` to, and the
    /// move that lands it there; `None` where the plan moves it nowhere.
    fn carried(&self, path: &str) -> Option<(NormalizedPath, usize)> {
        let (to, drawn) = self.lineage.carried_to(&self.identity(path)?)?;
        Some((to.clone(), *drawn.moves.last()?))
    }

    /// The spelling the plan writes the file `file` at.
    fn spelling(&self, file: &NormalizedPath) -> Option<&DocumentPath> {
        self.composition.read_at.get(file)
    }

    /// The respellings of every relative Markdown link a moved document
    /// holds, keyed as the change set keys the link, each on the move that
    /// lands its holder: read from where the document stood, spelled from
    /// where it lands toward the file it named, or where the plan carries
    /// that file.
    fn own_relative_links(&self) -> BTreeMap<EntryKey, (usize, LinkRewrite)> {
        let mut rewrites = BTreeMap::new();
        for (file, drawn) in self.lineage.drawing() {
            let (Some(owner), Some(landed), Some(stood)) = (
                drawn.moves.last(),
                self.spelling(file),
                self.spelling(&drawn.from),
            ) else {
                continue;
            };
            let (Some(holder), Some(source)) = (stored_path(landed), stored_path(stood)) else {
                continue;
            };
            let Some(bytes) = self
                .composition
                .targets
                .get(landed)
                .and_then(|target| target.after.as_deref())
            else {
                continue;
            };
            for link in document_links(bytes) {
                let relative = matches!(
                    LinkAddress::of(
                        wire_family(link.family),
                        link.protocol.as_deref(),
                        &link.target
                    ),
                    LinkAddress::Relative(_)
                );
                if !relative {
                    continue;
                }
                let [named] = &norn_store::named_paths(&link, &source)[..] else {
                    continue;
                };
                let named = self
                    .carried(named)
                    .and_then(|(to, _)| self.spelling(&to).map(|at| at.as_str().to_string()))
                    .unwrap_or_else(|| named.clone());
                let Some(respelled) = norn_store::relative_spelling(&holder, &named, &link.target)
                else {
                    continue;
                };
                if respelled == link.target {
                    continue;
                }
                let rewrite = LinkRewrite::new(
                    landed.clone(),
                    LinkFamily::Markdown,
                    address(&link),
                    respelled,
                );
                rewrites
                    .entry(entry(landed, &link))
                    .or_insert((*owner, rewrite));
            }
        }
        rewrites
    }

    /// Every spelling the link `broken` could take to name its destination,
    /// in the order they are tried: each a target text, its protocol kept.
    fn candidates(&self, broken: &Breaking) -> Vec<String> {
        let Some(to) = self.spelling(&broken.to).and_then(stored_path) else {
            return Vec::new();
        };
        let link = &broken.link;
        let written_with_extension = names_the_extension(&link.target);
        match LinkAddress::of(
            wire_family(link.family),
            link.protocol.as_deref(),
            &link.target,
        ) {
            LinkAddress::Suffix(target) => {
                let least = if target.contains('/') {
                    2.min(to.depth())
                } else {
                    1
                };
                let leaf = to.as_str().rsplit('/').next().unwrap_or_default();
                let (with, without): (Vec<String>, Vec<String>) = to
                    .suffix_spellings()
                    .filter(|spelling| spelling.split('/').count() >= least)
                    .partition(|spelling| spelling.ends_with(leaf) && leaf != to.stem());
                styled(written_with_extension, with, without)
            }
            LinkAddress::RootedName(_) => {
                let path = to.as_str().to_string();
                let stem = strip_extension(&path).map(str::to_string);
                styled(
                    written_with_extension,
                    vec![path],
                    stem.into_iter().collect(),
                )
            }
            LinkAddress::Relative(_) => {
                norn_store::relative_spelling(&broken.holder, to.as_str(), &link.target)
                    .into_iter()
                    .collect()
            }
            LinkAddress::Rooted(_) => norn_store::rooted_spelling(to.as_str(), &link.target)
                .into_iter()
                .collect(),
            LinkAddress::Elsewhere | LinkAddress::HoldingDocument => Vec::new(),
        }
    }
}

/// `with` and `without` the document extension, the style the link was
/// written in first.
fn styled(with_extension: bool, with: Vec<String>, without: Vec<String>) -> Vec<String> {
    if with_extension {
        with.into_iter().chain(without).collect()
    } else {
        without.into_iter().chain(with).collect()
    }
}

/// Whether `target`'s last segment carries the document extension, in any
/// ASCII case: how a wikilink written with its extension is told apart.
fn names_the_extension(target: &str) -> bool {
    strip_extension(target).is_some()
}

/// `path` without the document extension its last segment carries, in any
/// ASCII case; `None` where it carries none.
fn strip_extension(path: &str) -> Option<&str> {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    let dot = leaf.rfind('.').filter(|&dot| dot > 0)?;
    leaf[dot + 1..]
        .eq_ignore_ascii_case(DOCUMENT_EXTENSION)
        .then(|| &path[..path.len() - (leaf.len() - dot)])
}

/// `link` written with the target `target`, its protocol kept and nothing
/// else of it: what a candidate spelling is probed as.
fn spelled(link: &LinkFact, target: &str) -> LinkFact {
    LinkFact {
        family: link.family,
        embed: false,
        protocol: link.protocol.clone(),
        target: target.to_string(),
        title: None,
        anchor: None,
        span: norn_store::Span {
            line: 0,
            column: 0,
            byte_offset: 0,
        },
    }
}

/// A stored path as the wire names it.
fn wire_path(path: &norn_store::DocumentPath) -> Option<DocumentPath> {
    DocumentPath::new(path.as_str()).ok()
}

/// The change set's key for `link`, held at `holder`.
fn entry(holder: &DocumentPath, link: &LinkFact) -> EntryKey {
    (
        holder.as_str().to_string(),
        family_name(wire_family(link.family)),
        address(link),
    )
}
