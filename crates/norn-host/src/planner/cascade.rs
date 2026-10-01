//! Link cascades: the rewrites a plan's document moves and deletes generate,
//! so that a link naming a moved document names it where it lands, and one
//! naming a deleted document names the document its delete rewrites them to.
//!
//! **Which links a move breaks is the change set's own question.** A link
//! follows a move where, before the plan, it resolves to exactly the document
//! the move carries away, and after the plan does not resolve to exactly the
//! file the move carries it to — the one rule [`left_behind`] states, which
//! the change set records and advises by too. Both sides are read through
//! the store's resolution door, over the overlay and probes the change set
//! itself reads ([`reach`]), so the backlinks a cascade rewrites are the
//! links the set would otherwise record leaving: the store's links under a
//! key that could name the moved document, and the links the plan's own
//! documents hold. A path the plan vacates and refills is overlaid as
//! replaced, so a link naming the document that left is found there though
//! its path still resolves. A move that keeps its document's stem, so its
//! bare backlinks still name it alone, and a relative link between two
//! documents one folder move carries together, need nothing. A link resolving
//! to several documents before the plan is never rewritten: which it names is
//! not known, and the forecast says so.
//!
//! **A delete reads its backlinks through the same door.** A link is a
//! deleted document's backlink where it resolves before the plan to exactly
//! that document ([`removed_by`]): an ambiguous link is a backlink of none, a
//! link the document holds goes with it, and a link in a holder the plan
//! removes or edits away is none where the plan leaves the vault, while one
//! the plan adds is. Backlinks are read here before any cascade writes a
//! link, so a link a move's cascade respells to the deleted document's name
//! is judged by the text it had, as the change set judges it. Each delete is
//! resolved by the one rule its link choice is held to
//! ([`Removal::kept_by`]). Saying neither flag, a delete a backlink names is left
//! unresolved naming every holder and how many links; leaving them broken it
//! generates nothing. Rewriting them, its `rewrite_to` is read through the
//! same door where the plan leaves the vault ([`rewrite_targets`]) and must
//! name one document there, other than the one it removes; every backlink
//! not already naming that document after the plan ([`rewritten_for`]) is
//! respelled to it as below, both syntaxes alike. An ambiguous link that
//! could name the deleted document is never rewritten, and the forecast says
//! so.
//!
//! **A link is respelled in its own style, and only to a spelling that reads
//! back.** The new address is the shortest spelling of the link's
//! destination — the file the moved document lands at, or the delete's
//! target — the link's syntax and protocol can write: for a bare wikilink
//! the shortest suffix of the destination that names it alone with every
//! target of the plan at its after-state — at least two segments where the
//! link was written path-qualified, so it stays so — written with the
//! document extension exactly where the link was; for a `vault://` wikilink
//! the destination's root path, in that same style; and for a Markdown link the
//! destination's path from the holder's folder, or from the root where the
//! link was written from the root, in the link's own percent-escaping. Each
//! candidate is probed through the same door, from the holder where it stands
//! after the plan, and the first that resolves to exactly the destination is
//! written. A link no candidate reaches is left as written, and the forecast
//! says it is unrepresentable.
//!
//! **A moved document's own relative links name what they named.** Each
//! relative Markdown link a moved document holds is read from where the
//! document stood, and where its spelling no longer reaches, from where the
//! document lands, the file the link must name after the plan — the file it
//! named, wherever the plan carries that file, or the target of a delete
//! removing it rewriting the links naming it — it is respelled from there
//! toward that file, whether it names a document or an attachment; a
//! spelling still reaching it is kept as written, and an anchor-only link
//! names its holder wherever it goes.
//!
//! **Every effect of the plan on a link is read by one rule.** Which file a
//! link must name after the plan ([`Cascade::final_document`]) counts the
//! moves and the rewriting deletes alike, so a link reached both ways — a
//! backlink of a deleted document in a holder the plan moves — is respelled
//! once, to the delete's target, from where its holder lands.
//!
//! **A cascade travels on the operation it serves.** Each rewrite is one
//! `rewrite_link` per holder, syntax and address, named at the holder's
//! after-state path, generated once however many ways it is reached and
//! carried by the move that lands the named document — a moved document's
//! own relative links by its own move — or by the delete whose target it
//! names. Composition then writes it on the
//! holder's final bytes ([`super::compose::compose`]), the same bytes it was
//! read from here.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use norn_fs::{NormalizedPath, PathNormalizer};
use norn_store::{LinkFact, ProbedLink, TargetNaming};
use norn_wire::{
    Backlinks, DOCUMENT_EXTENSION, DocumentPath, LinkAddress, LinkFamily, LinkRewrite, Resolves,
    UnresolvedReason,
};

use super::compose::Composition;
use super::lineage::{Lineage, Removal};
use super::links::{
    EntryKey, LinkIndex, Target, WrittenLinks, address, family_name, left_behind, reach,
    removed_by, rewrite_destination, rewrite_targets, rewritten_for, stored_path, wire_family,
};
use crate::derivation::document_links;

/// What a plan's moves and deletes generate from the links naming what they
/// carry away or remove.
#[derive(Debug, Default)]
pub(crate) struct Generated {
    /// The link cascade of each operation that carries one, by its position.
    pub(crate) cascades: BTreeMap<usize, Vec<LinkRewrite>>,
    /// Each delete its link choice leaves unresolved
    /// ([`Removal::kept_by`]), by its position, and why: one forbidding the
    /// links naming its document, which a link names; one rewriting them,
    /// whose `rewrite_to` names no one document a link can be respelled
    /// toward.
    pub(crate) unresolved: BTreeMap<usize, UnresolvedReason>,
}

/// What the plan `composition` composed generates from the links naming what
/// it carries away or removes, judged through `index`.
///
/// - **Each move's cascade**, by its position: every link that names a
///   document the plan's moves carry away, as `lineage` follows them, and
///   would not name it after, respelled to name it where it lands.
/// - **Each delete rewriting the links naming its document**: its
///   `rewrite_to` must name one document where the plan leaves the vault
///   ([`rewrite_targets`]), else the delete is left unresolved ([`unnamed`]);
///   where it does, every link naming the removed document that does not
///   already name the target after the plan ([`rewritten_for`]) is respelled
///   to name the target, by the same spellings a move's cascade writes.
/// - **Each delete forbidding the links naming its document** that a link
///   names is left unresolved, naming every holder ([`backlinks`]). A delete
///   leaving them broken generates nothing.
///
/// Empty, and the index never asked, where no move carries a document and no
/// delete forbids or rewrites the links naming its own.
pub(crate) fn generate<I: LinkIndex + ?Sized>(
    composition: &Composition,
    lineage: &Lineage,
    normalizer: &PathNormalizer,
    index: &I,
) -> Result<Generated, I::Error> {
    let reads_backlinks = lineage
        .removals()
        .any(|removal| removal.backlinks != Backlinks::LeftBroken);
    if lineage.drawing().next().is_none() && !reads_backlinks {
        return Ok(Generated::default());
    }
    let mut cascade = Cascade {
        composition,
        lineage,
        normalizer,
        destinations: BTreeMap::new(),
    };
    let targets = Target::of(composition);
    let (overlay, probed) = reach(&targets, lineage, normalizer, &WrittenLinks::default());

    // What each rewriting delete's target names after the plan, and the one
    // document its links are respelled toward where it names one a link can
    // name.
    let rewritten_to = rewrite_targets(lineage, &overlay, index)?;
    for removal in lineage.removals() {
        if let Some(destination) = rewritten_to
            .get(&removal.position)
            .and_then(|named| rewrite_destination(named, normalizer))
        {
            cascade.destinations.insert(removal.position, destination);
        }
    }

    // The links the moves leave behind and the links a rewriting delete
    // respells, each with the file it must name after and the operation that
    // carries its rewrite; and every backlink of a document a delete
    // forbidding them removes, by the delete's position.
    let mut breaking: Vec<Breaking> = Vec::new();
    let mut forbidden: BTreeMap<usize, Vec<norn_store::DocumentPath>> = BTreeMap::new();
    index.changes(&overlay, &probed, &mut |change| {
        // Whether the link is one a cascade respells: a backlink of a
        // document a delete removes rewriting the links naming it, or a link
        // a move leaves behind.
        let respelled = match removed_by(&change.before, lineage, normalizer) {
            Some(removal) => match removal.backlinks {
                Backlinks::Forbidden => {
                    forbidden
                        .entry(removal.position)
                        .or_default()
                        .push(change.holder);
                    return;
                }
                Backlinks::RewrittenTo(_) => {
                    rewritten_for(removal, &change.after, &rewritten_to, normalizer)
                }
                Backlinks::LeftBroken => false,
            },
            None => left_behind(&change.before, &change.after, lineage, normalizer).is_some(),
        };
        let Resolves::One { path: named } = &change.before else {
            return;
        };
        let Some(destination) = respelled
            .then(|| cascade.final_document(named.as_str()))
            .flatten()
        else {
            return;
        };
        breaking.push(Breaking {
            holder: change.holder,
            link: change.link,
            to: destination.file,
            at: destination.at,
            owner: destination.owner,
        });
    })?;

    // Each delete is resolved by the one rule its link choice is held to,
    // the rule the applier holds a resolved plan's deletes to again.
    let mut unresolved: BTreeMap<usize, UnresolvedReason> = BTreeMap::new();
    for removal in lineage.removals() {
        let holders = forbidden.remove(&removal.position);
        let named = rewritten_to.get(&removal.position);
        if removal.kept_by(holders.is_some(), named, normalizer) {
            continue;
        }
        let reason = match holders {
            Some(holders) => backlinks(holders),
            None => unnamed(removal, named, lineage, normalizer),
        };
        unresolved.insert(removal.position, reason);
    }

    let mut rewrites: BTreeMap<EntryKey, (usize, LinkRewrite)> = cascade.own_relative_links();

    // Every spelling each breaking link could take, probed from its holder
    // in one judgment; a link written twice in one holder is one key, asked
    // about once.
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
    // Each answer is keyed as the probe was: a candidate spelled alike in two
    // syntaxes is two questions, read by two grammars.
    let mut answered: BTreeMap<EntryKey, Resolves> = BTreeMap::new();
    if !probes.is_empty() {
        index.changes(&overlay, &probes, &mut |change| {
            if let (true, Some(holder)) = (change.written, wire_path(&change.holder)) {
                answered.insert(entry(&holder, &change.link), change.after);
            }
        })?;
    }
    for (at, candidates) in asked {
        let broken = &breaking[at];
        let Some(holder) = wire_path(&broken.holder) else {
            continue;
        };
        let reads_back = candidates.into_iter().find(|candidate| {
            matches!(
                answered.get(&entry(&holder, &spelled(&broken.link, candidate))),
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
    Ok(Generated {
        cascades,
        unresolved,
    })
}

/// Why the delete `removal` rewriting the links naming its document does not
/// resolve, its `rewrite_to` naming `named` after the plan rather than one
/// document a link can be respelled toward: one at a path the vault's rule or
/// the store's grammar refuses, said so; several, headed as a read heads an
/// ambiguous target; or none — the document the delete removes itself, said
/// so, or no document at all.
fn unnamed(
    removal: &Removal,
    named: Option<&TargetNaming>,
    lineage: &Lineage,
    normalizer: &PathNormalizer,
) -> UnresolvedReason {
    let address = removal.rewrite_to().unwrap_or_default();
    if let Some(named) = named {
        match (&named.after, &named.candidates) {
            // A document the store names that the vault's rule does not, or
            // the other way round, is no file a link can be respelled toward:
            // the delete is left out saying so, never landed leaving the
            // links it was to rewrite.
            (Resolves::One { path }, _) => {
                return UnresolvedReason::no_longer_resolves(format!(
                    "`rewrite_to` `{address}` names `{path}`, which is no path the links naming the removed document can be rewritten toward"
                ));
            }
            (Resolves::Several {}, Some(candidates)) => {
                return UnresolvedReason::ambiguous_target(candidates.clone());
            }
            _ => {}
        }
    }
    let itself = named.and_then(|named| match &named.before {
        Resolves::One { path } => removed_by(&named.before, lineage, normalizer)
            .is_some_and(|removes| removes.position == removal.position)
            .then_some(path),
        _ => None,
    });
    UnresolvedReason::no_longer_resolves(match itself {
        Some(path) => format!(
            "`rewrite_to` `{address}` names `{path}`, the document the delete removes, so it leaves no document for the links naming it to name"
        ),
        None => format!(
            "`rewrite_to` `{address}` names no one document where the plan leaves the vault, so the links naming the removed document have nothing to be rewritten to"
        ),
    })
}

/// Why a delete forbidding the links naming its document does not resolve,
/// each of `held` the holder of one such link where it stands after the
/// plan: every holding document, each once and in path order, and how many
/// links name it.
fn backlinks(held: Vec<norn_store::DocumentPath>) -> UnresolvedReason {
    let total = held.len() as u64;
    let holders: BTreeSet<DocumentPath> = held.iter().filter_map(wire_path).collect();
    UnresolvedReason::has_backlinks(holders.into_iter().collect(), total)
}

/// One link a cascade respells: where it is held after the plan, as it is
/// written, the file it must name after and that file's path there, and the
/// operation carrying its rewrite — the move landing that file, or the delete
/// whose target it is.
struct Breaking {
    holder: norn_store::DocumentPath,
    link: LinkFact,
    to: NormalizedPath,
    at: norn_store::DocumentPath,
    owner: usize,
}

/// What a cascade is generated from: the composed plan, where its content
/// ends, and the document each delete rewriting the links naming its own
/// rewrites them to.
struct Cascade<'a> {
    composition: &'a Composition,
    lineage: &'a Lineage,
    normalizer: &'a PathNormalizer,
    /// The one document each delete rewriting the links naming its document
    /// names where the plan leaves the vault, and its path there, by the
    /// delete's position.
    destinations: BTreeMap<usize, (NormalizedPath, norn_store::DocumentPath)>,
}

/// The document a link must name after the plan, as [`Cascade::final_document`]
/// reads it.
struct Destination {
    /// The file.
    file: NormalizedPath,
    /// Its path where the plan leaves the vault.
    at: norn_store::DocumentPath,
    /// The operation whose cascade carries a rewrite toward it: the move
    /// landing it, or the delete whose target it is.
    owner: usize,
}

impl Cascade<'_> {
    fn identity(&self, path: &str) -> Option<NormalizedPath> {
        self.normalizer.normalize(Path::new(path)).ok()
    }

    /// **The one rule a link's final document is read by**: where a link
    /// named the document standing at `named` before the plan, the document
    /// it must name after it, every effect of the plan counted — where a
    /// delete removes the document rewriting the links naming it, the one
    /// document its `rewrite_to` names; where a move carries it, the file it
    /// lands at. `None` where the plan leaves the document where it stood,
    /// or removes it forbidding or breaking the links naming it.
    ///
    /// A backlink a cascade respells and a moved document's own relative
    /// link are both pointed here, so a holder the plan moves names a
    /// deleted document's target from where it lands, whichever operation
    /// writes its rewrite.
    fn final_document(&self, named: &str) -> Option<Destination> {
        let file = self.identity(named)?;
        if let Some(removal) = self.lineage.removed_by(&file) {
            let (to, at) = self.destinations.get(&removal.position)?;
            return Some(Destination {
                file: to.clone(),
                at: at.clone(),
                owner: removal.position,
            });
        }
        let (to, drawn) = self.lineage.carried_to(&file)?;
        Some(Destination {
            file: to.clone(),
            at: self.spelling(to).and_then(stored_path)?,
            owner: *drawn.moves.last()?,
        })
    }

    /// The spelling the plan writes the file `file` at.
    fn spelling(&self, file: &NormalizedPath) -> Option<&DocumentPath> {
        self.composition.read_at.get(file)
    }

    /// The respellings of every relative Markdown link a moved document
    /// holds whose spelling no longer reaches, from where the document
    /// lands, the file the link must name after the plan
    /// ([`Self::final_document`]), keyed as the change set keys the link,
    /// each on the move that lands its holder: read from where the document
    /// stood, spelled from where it lands.
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
                    .final_document(named)
                    .map(|destination| destination.at.as_str().to_string())
                    .unwrap_or_else(|| named.clone());
                // A spelling that still reaches the file from where the
                // document lands is kept, however short another would be.
                if norn_store::named_paths(&link, &holder) == [named.as_str()] {
                    continue;
                }
                let Some(respelled) = norn_store::relative_spelling(&holder, &named, &link.target)
                else {
                    continue;
                };
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
        let to = &broken.at;
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

/// The spellings in the style the link was written in: `with` the document
/// extension where it was written with it, else `without`. The other style
/// is never a fallback — a link whose own style spells nothing that reads
/// back is unrepresentable, not given an extension its author did not write
/// or stripped of one they did.
fn styled(with_extension: bool, with: Vec<String>, without: Vec<String>) -> Vec<String> {
    if with_extension { with } else { without }
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
