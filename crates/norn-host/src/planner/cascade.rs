//! Link cascades: the rewrites a plan's document moves, deletes and wikilink
//! rewrites generate, so that a link naming a moved document names it where
//! it lands, one naming a deleted document names the document its delete
//! rewrites them to, and a wikilink naming a rewrite's `old` names its `new`.
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
//! the plan writes is. Saying neither flag, a delete a backlink names is left
//! unresolved naming every holder and how many links; leaving them broken it
//! generates nothing. Rewriting them, its `rewrite_to` is read through the
//! same door where the plan leaves the vault ([`rewrite_targets`]) and must
//! name one document there, other than the one it removes; every backlink
//! not already naming that document after the plan ([`rewritten_for`]) is
//! respelled to it as below, both syntaxes alike. An ambiguous link that
//! could name the deleted document is never rewritten, and the forecast says
//! so.
//!
//! **A wikilink rewrite reads the wikilinks naming its `old` through the same
//! door.** Its `old` is read as the vault stands before the plan and its
//! `new` where the plan leaves it ([`retarget_namings`]). Where `old` names
//! one document, the door reaches every link naming that document though the
//! plan changes nothing there, and every wikilink resolving to exactly it
//! before the plan, whatever its spelling, is retargeted; where `old` names
//! none, every broken wikilink filed under `old` in any case is
//! ([`selecting`](super::links::selecting)). A wikilink already naming `new`'s document after the
//! plan is left alone, one resolving to several documents is never rewritten
//! and the forecast says so, and a Markdown link is no wikilink. An `old`
//! naming several documents, a `new` naming none or several, the two naming
//! one document, and no wikilink to retarget each leave the rewrite
//! unresolved, the ambiguous ones with the head of their candidates. A
//! wikilink a rewrite retargets is that rewrite's, whatever a move or a
//! delete of the plan would do with it: its author said what it names. A
//! backlink of a document a delete forbidding them removes is one the
//! rewrite clears only by respelling it; one it leaves as written still
//! leaves the delete unresolved.
//!
//! **A link is respelled in its own style, and only to a spelling that reads
//! back.** The new address is the shortest spelling of the link's
//! destination — the file the moved document lands at, the delete's target,
//! or the wikilink rewrite's `new` — the link's syntax and protocol can
//! write: for a bare wikilink
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
//! wikilink rewrites, the moves and the rewriting deletes alike, in that
//! precedence ([`decider`]), so a link reached several ways — a backlink of
//! a deleted document in a holder the plan moves, or a wikilink a rewrite
//! retargets whose document a move carries away — is respelled once: to the
//! rewrite's `new` where a wikilink rewrite names it, else to the delete's
//! target, from where its holder lands.
//!
//! **A cascade travels on the operation it serves.** Each rewrite is one
//! `rewrite_link` per holder, syntax and address, named at the holder's
//! after-state path, generated once however many ways it is reached and
//! carried by the move that lands the named document — a moved document's
//! own relative links by its own move — by the delete whose target it
//! names, or by the wikilink rewrite retargeting it. Composition then writes
//! it on the
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
use super::lineage::{Lineage, Removal, Retarget};
use super::links::{
    Decider, EntryKey, LinkIndex, Named, RetargetNaming, Target, WrittenLinks, address, decider,
    family_name, reach, reaching, removed_by, respells, retarget_namings, rewrite_targets,
    stored_path, wire_family,
};
use crate::derivation::document_links;

/// What a plan's moves, deletes and wikilink rewrites generate from the
/// links naming what they carry away, remove or retarget.
#[derive(Debug, Default)]
pub(crate) struct Generated {
    /// The link cascade of each operation that carries one, by its position.
    pub(crate) cascades: BTreeMap<usize, Vec<LinkRewrite>>,
    /// Each operation the links it reads leave unresolved, by its position,
    /// and why: a delete whose `rewrite_to` names no one document, or which
    /// forbids its backlinks while a link names its document; and a wikilink
    /// rewrite whose ends name no one document each, or which retargets no
    /// wikilink.
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
/// - **Each wikilink rewrite**: its `old` must name one document or none
///   before the plan, and its `new` one other document after it
///   ([`retarget_destination`]); where they do, every wikilink naming `old`
///   that does not already name `new` after the plan ([`respells`]) is
///   respelled to name it, by the same spellings, and a rewrite retargeting
///   none is left unresolved ([`unmatched`]).
///
/// Empty, and the index never asked, where no move carries a document, no
/// delete forbids or rewrites the links naming its own, and no wikilink
/// rewrite stands.
pub(crate) fn generate<I: LinkIndex + ?Sized>(
    composition: &Composition,
    lineage: &Lineage,
    normalizer: &PathNormalizer,
    index: &I,
) -> Result<Generated, I::Error> {
    let reads_backlinks = lineage
        .removals()
        .any(|removal| removal.backlinks != Backlinks::LeftBroken);
    if lineage.drawing().next().is_none()
        && !reads_backlinks
        && lineage.retargets().next().is_none()
    {
        return Ok(Generated::default());
    }
    let mut cascade = Cascade {
        composition,
        lineage,
        normalizer,
        namings: BTreeMap::new(),
        destinations: BTreeMap::new(),
    };
    let targets = Target::of(composition);
    let (overlay, probed) = reach(&targets, lineage, normalizer, &WrittenLinks::default());

    // What each rewriting delete's target names after the plan: the one
    // document its links are respelled to, or why the delete is left
    // unresolved.
    let rewritten_to = rewrite_targets(lineage, &overlay, index)?;
    let mut unresolved: BTreeMap<usize, UnresolvedReason> = BTreeMap::new();
    for removal in lineage.removals() {
        let Some(named) = rewritten_to.get(&removal.position) else {
            continue;
        };
        match &named.after {
            Resolves::One { path } => {
                match (cascade.identity(path.as_str()), stored_path(path)) {
                    (Some(file), Some(at)) => {
                        cascade.destinations.insert(removal.position, (file, at));
                    }
                    // A document the store names that the vault's rule does
                    // not, or the other way round, is no file a link can be
                    // respelled toward: the delete is left out saying so,
                    // never landed leaving the links it was to rewrite.
                    _ => {
                        let address = removal.rewrite_to().unwrap_or_default();
                        unresolved.insert(
                            removal.position,
                            UnresolvedReason::no_longer_resolves(format!(
                                "`rewrite_to` `{address}` names `{path}`, which is no path the links naming the removed document can be rewritten toward"
                            )),
                        );
                    }
                }
            }
            _ => {
                unresolved.insert(
                    removal.position,
                    unnamed(removal, named, lineage, normalizer),
                );
            }
        }
    }

    // What each wikilink rewrite's ends name, and the one document its
    // wikilinks are retargeted to, or why the rewrite is left unresolved;
    // the links naming its `old` are reached though the plan changes nothing
    // they name.
    cascade.namings = retarget_namings(lineage, &overlay, index)?;
    for retarget in lineage.retargets() {
        match retarget_destination(retarget, &cascade.namings[&retarget.position], normalizer) {
            Ok(path) => {
                if let (Some(file), Some(at)) = (cascade.identity(path.as_str()), stored_path(path))
                {
                    cascade.destinations.insert(retarget.position, (file, at));
                }
            }
            Err(reason) => {
                unresolved.insert(retarget.position, reason);
            }
        }
    }
    let overlay = reaching(overlay, lineage, &cascade.namings);

    // The links a cascade respells — the links the moves leave behind, the
    // links a rewriting delete respells and the wikilinks a wikilink rewrite
    // retargets — each with the file it must name after and the operation
    // that carries its rewrite; every backlink of a document a delete
    // forbidding them removes, by the delete's position, and each such
    // backlink a wikilink rewrite decides instead, which stays one where it
    // is not respelled; and how many wikilinks each wikilink rewrite
    // retargets.
    let mut breaking: Vec<Breaking> = Vec::new();
    let mut forbidden: BTreeMap<usize, Vec<norn_store::DocumentPath>> = BTreeMap::new();
    let mut contested: Vec<(usize, norn_store::DocumentPath, Option<EntryKey>)> = Vec::new();
    let mut retargeting: BTreeMap<usize, usize> = BTreeMap::new();
    index.changes(&overlay, &probed, &mut |change| {
        let Some(destination) = cascade.final_document(&change.link, Named::of(&change)) else {
            return;
        };
        let forbidding = removed_by(&change.before, lineage, normalizer)
            .filter(|removal| removal.backlinks == Backlinks::Forbidden);
        if let Some(removal) = forbidding {
            match destination.by {
                Decider::Removal(_) => {
                    forbidden
                        .entry(removal.position)
                        .or_default()
                        .push(change.holder);
                    return;
                }
                _ => contested.push((
                    removal.position,
                    change.holder.clone(),
                    wire_path(&change.holder).map(|holder| entry(&holder, &change.link)),
                )),
            }
        }
        let respelled = respells(
            destination.by,
            &change.before,
            &change.after,
            lineage,
            &rewritten_to,
            &cascade.namings,
            normalizer,
        );
        if !respelled {
            return;
        }
        if let Decider::Retarget(retarget) = destination.by {
            *retargeting.entry(retarget.position).or_default() += 1;
        }
        let Some((to, at)) = destination.to else {
            return;
        };
        breaking.push(Breaking {
            holder: change.holder,
            link: change.link,
            to,
            at,
            owner: destination.by.owner(),
        });
    })?;

    for retarget in lineage.retargets() {
        if !retargeting.contains_key(&retarget.position) {
            unresolved
                .entry(retarget.position)
                .or_insert_with(|| unmatched(retarget, &cascade.namings[&retarget.position]));
        }
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

    // A backlink of a document a delete forbidding them removes that a
    // wikilink rewrite decides is no backlink once the rewrite respells it;
    // one it leaves as written still names the removed document.
    for (position, holder, key) in contested {
        if !key.is_some_and(|key| rewrites.contains_key(&key)) {
            forbidden.entry(position).or_default().push(holder);
        }
    }
    for (position, holders) in forbidden {
        unresolved.insert(position, backlinks(holders));
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
/// resolve, its `rewrite_to` naming not one document but `named` after the
/// plan: several, headed as a read heads an ambiguous target; or none — the
/// document the delete removes itself, said so, or no document at all.
fn unnamed(
    removal: &Removal,
    named: &TargetNaming,
    lineage: &Lineage,
    normalizer: &PathNormalizer,
) -> UnresolvedReason {
    let address = removal.rewrite_to().unwrap_or_default();
    if let (Resolves::Several {}, Some(candidates)) = (&named.after, &named.candidates) {
        return UnresolvedReason::ambiguous_target(candidates.clone());
    }
    let itself = match &named.before {
        Resolves::One { path } => removed_by(&named.before, lineage, normalizer)
            .is_some_and(|removes| removes.position == removal.position)
            .then_some(path),
        _ => None,
    };
    UnresolvedReason::no_longer_resolves(match itself {
        Some(path) => format!(
            "`rewrite_to` `{address}` names `{path}`, the document the delete removes, so it leaves no document for the links naming it to name"
        ),
        None => format!(
            "`rewrite_to` `{address}` names no one document where the plan leaves the vault, so the links naming the removed document have nothing to be rewritten to"
        ),
    })
}

/// The document the wikilink rewrite `retarget` retargets its wikilinks to,
/// its ends naming as `named` says, or why it does not resolve: its `old`
/// names several documents before the plan, headed there; its `new` names
/// none or several where the plan leaves the vault, the latter headed; or
/// the two name one document, so no wikilink would change.
fn retarget_destination<'n>(
    retarget: &Retarget,
    named: &'n RetargetNaming,
    normalizer: &PathNormalizer,
) -> Result<&'n DocumentPath, UnresolvedReason> {
    let (old, new) = (&retarget.old, &retarget.new);
    if let (Resolves::Several {}, Some(candidates)) = (&named.old.before, &named.old.candidates) {
        return Err(UnresolvedReason::ambiguous_target(candidates.clone()));
    }
    let to = match (&named.new.after, &named.new.candidates) {
        (Resolves::One { path }, _) => path,
        (Resolves::Several {}, Some(candidates)) => {
            return Err(UnresolvedReason::ambiguous_target(candidates.clone()));
        }
        _ => {
            return Err(UnresolvedReason::no_longer_resolves(format!(
                "`new` `{new}` names no one document where the plan leaves the vault, so the wikilinks naming `{old}` have nothing to be retargeted to"
            )));
        }
    };
    let identity = |path: &DocumentPath| normalizer.normalize(Path::new(path.as_str())).ok();
    if let Resolves::One { path: from } = &named.old.before
        && identity(from).is_some()
        && identity(from) == identity(to)
    {
        return Err(UnresolvedReason::no_longer_resolves(format!(
            "`old` `{old}` and `new` `{new}` name the same document, `{to}`, so no wikilink would change"
        )));
    }
    Ok(to)
}

/// Why the wikilink rewrite `retarget`, its ends naming as `named` says,
/// does not resolve where no wikilink is one it retargets: it would change
/// nothing, and a rewrite of nothing is not landed as though it acted.
fn unmatched(retarget: &Retarget, named: &RetargetNaming) -> UnresolvedReason {
    let old = &retarget.old;
    UnresolvedReason::no_longer_resolves(match &named.old.before {
        Resolves::One { path } => format!(
            "no wikilink resolves to `{path}`, the document `old` `{old}` names, so the rewrite changes nothing"
        ),
        _ => format!(
            "no broken wikilink is filed under `old` `{old}`, so the rewrite changes nothing"
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
/// operation carrying its rewrite — the move landing that file, the delete
/// whose target it is, or the wikilink rewrite whose `new` it is.
struct Breaking {
    holder: norn_store::DocumentPath,
    link: LinkFact,
    to: NormalizedPath,
    at: norn_store::DocumentPath,
    owner: usize,
}

/// What a cascade is generated from: the composed plan, where its content
/// ends, what each wikilink rewrite's ends name, and the document each
/// delete rewriting the links naming its own, and each wikilink rewrite,
/// rewrites them to.
struct Cascade<'a> {
    composition: &'a Composition,
    lineage: &'a Lineage,
    normalizer: &'a PathNormalizer,
    /// What each wikilink rewrite's two ends name, by its position.
    namings: BTreeMap<usize, RetargetNaming>,
    /// The one document each delete rewriting the links naming its document
    /// names where the plan leaves the vault, and each wikilink rewrite's
    /// `new` names there, and its path there, by the operation's position.
    destinations: BTreeMap<usize, (NormalizedPath, norn_store::DocumentPath)>,
}

/// What a link must name after the plan, as [`Cascade::final_document`]
/// reads it: the operation deciding it, and the file and its path where the
/// plan leaves the vault, where that operation names one.
struct Destination<'l> {
    /// The operation deciding what the link names.
    by: Decider<'l>,
    /// The file, and its path where the plan leaves the vault: none for a
    /// delete forbidding or breaking the links naming its document, and none
    /// for an operation whose target names no one document.
    to: Option<(NormalizedPath, norn_store::DocumentPath)>,
}

impl<'a> Cascade<'a> {
    fn identity(&self, path: &str) -> Option<NormalizedPath> {
        self.normalizer.normalize(Path::new(path)).ok()
    }

    /// **The one rule a link's final document is read by**: where a link
    /// written as `link` named `named` before the plan, the operation that
    /// decides what it names after it ([`decider`]) and the document that is,
    /// every effect of the plan counted — where a wikilink rewrite's `old`
    /// names the link, the one document its `new` names, whatever a move or
    /// a delete would do with it; where a delete removes the document
    /// rewriting the links naming it, the one document its `rewrite_to`
    /// names; where a move carries it, the file it lands at. `None` where the
    /// plan leaves what it named where it stood.
    ///
    /// A link a cascade respells and a moved document's own relative link
    /// are both pointed here, so a holder the plan moves names a deleted
    /// document's target from where it lands, whichever operation writes its
    /// rewrite, and a wikilink a rewrite retargets is retargeted once.
    fn final_document(&self, link: &LinkFact, named: Named<'_>) -> Option<Destination<'a>> {
        let by = decider(link, named, self.lineage, &self.namings, self.normalizer)?;
        let to = match by {
            Decider::Retarget(_) | Decider::Removal(_) => {
                self.destinations.get(&by.owner()).cloned()
            }
            Decider::Move { to, .. } => self
                .spelling(to)
                .and_then(stored_path)
                .map(|at| (to.clone(), at)),
        };
        Some(Destination { by, to })
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
                    .final_document(&link, Named::One(named))
                    .and_then(|destination| destination.to)
                    .map(|(_, at)| at.as_str().to_string())
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
