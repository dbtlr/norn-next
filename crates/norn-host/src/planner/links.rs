//! The resolution change set: every link whose resolution a plan changes, and
//! every link whose text it writes, each recorded as a condition naming the
//! link as the plan leaves it with what it resolves to before the plan and
//! after it — and the advisories a forecast carries about them.
//!
//! **One computation, two callers.** The planner records the set a plan
//! resolves to, and the applier computes it again, after its intake, from the
//! resolved plan alone and refuses on any difference (ADR 0031): an entry the
//! plan records that the set computed again does not hold, or holds with
//! other values, and an entry the set computed again holds that the plan
//! does not record. Both compute it here, from the same inputs — the plan's
//! targets with whether a document stands at each before and after, the bytes
//! each composed document holds after, the plan's content lineage and its
//! operations — so the two can only differ where the vault outside the plan
//! moved.
//!
//! **What is judged where.** A document the plan writes is read from the
//! bytes the plan composed, since the store holds its before-state: each link
//! it holds is keyed at its after-state, and read before the plan from where
//! the document's content stood — a moved document's source — so a relative
//! link a move breaks is seen breaking. Every other link is the store's, read
//! through the [`LinkIndex`] by the keys that could name a target whose
//! presence the plan changes, or whose document it replaces
//! (`norn_store::Snapshot::resolution_changes`). A link in a document the
//! plan removes is no entry: its disappearance is the plan's own transition.
//!
//! **An entry is a link whose resolution changes, whose text the plan
//! writes, or that is left behind** ([`left_behind`]). The vocabulary an
//! entry records a resolution in names paths, so a link left naming a path
//! the plan vacates and refills is recorded naming that path on both sides:
//! what changed is the document there, which the entry's presence in the set
//! says, and the forecast says why the cascade left it. A backlink to such a
//! path another writer adds after planning is then an entry the plan does
//! not record, and refuses it as one to a vacated path does.
//!
//! **A plan that changes no document's presence and writes no link is
//! answered without reading anything.** No link's resolution moves unless a
//! document appears or disappears somewhere a key names, or the plan writes
//! the link's text, so such a plan — every edit-only plan, a frontmatter set
//! among them — records nothing, and the index is never asked: nothing is
//! minted for it. A document a move carries away lands somewhere nothing
//! stood or leaves somewhere nothing stands after, since moves closing a
//! cycle are refused, so a plan replacing a document a link follows always
//! changes a presence too.
//!
//! **Which links a plan writes is one predicate** ([`WrittenLinks::holds`]):
//! a link a rewrite of the plan writes — a `rewrite_link` operation's, or
//! one of a cascade's — held in its document under the syntax and the
//! address the rewrite writes. Every such link is an entry whatever it
//! resolves to, so a rewrite is checked where it lands.
//!
//! **What the forecast says of a link a cascade did not follow.** A link
//! that named a document a move carries away, and does not name it where it
//! lands, a document a delete removes rewriting the links naming it, and
//! does not name the delete's target, or a wikilink a wikilink rewrite
//! retargets ([`retargeted_by`]) that does not name its `new`, is advised on
//! as the cascade's skip —
//! the text layer's reason, or unrepresentable where no spelling read back —
//! and an ambiguous link that could name such a document — or the document
//! a wikilink rewrite's `old` names — as skipped for its ambiguity, each in
//! place of what its resolution alone would say. Every such link is an
//! entry, though nothing it names changes, so a wikilink another writer adds
//! after planning that a rewrite would retarget refuses the plan; the links
//! a wikilink rewrite could retarget are reached through the door though the
//! plan changes nothing they name ([`reaching`]). A link
//! that named a document a delete forbidding or breaking its links removes
//! is advised on as its resolution says: left broken, or retargeted where it
//! was ambiguous. Every link that named a removed document is an entry,
//! though the path it resolves to is refilled, so a backlink another writer
//! adds after planning refuses the plan. Both are read from the links the
//! plan does not write — a link a cascade respells to a path the plan
//! refills reads as left behind from its new address — and said even where a
//! cascade respelled another link of the holder to the same address, so the
//! two share a key ([`Kept`]). Both are read here, from the plan alone, so
//! the planner and the applier forecast alike for every holder not yet
//! landed. For a holder an interrupted apply already landed, its bytes cannot
//! tell a kept link from a written one, so a re-sent plan's forecast can omit
//! or mislabel that holder's skip advisory, while the condition entries the
//! applier checks stay exact.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use norn_fs::{NormalizedPath, PathNormalizer};
use norn_store::{LinkChange, PathOverlay, PlanSide, ProbedLink, TargetNaming};
use norn_text::RewriteSkip;
use norn_wire::{
    DocumentPath, FileState, LinkAddressKind, LinkAdvisory, LinkFamily, LinkHealth, LinkKey,
    Operation, OperationKind, PlanCondition, Resolves, Transition,
};

use super::compose::{Composition, Kept, Skipped};
use super::lineage::{Drawn, Lineage, Removal, Retarget};
use crate::derivation::document_links;

/// Where the links a plan does not write are judged: what each link the plan
/// reaches resolves to, over the store's documents with the plan's targets
/// overlaid.
pub(crate) trait LinkIndex {
    /// Why the index could not be read.
    type Error;

    /// Hand `each` every link `probed` names that the plan reaches, and every
    /// link the index holds under a key that could name a target of
    /// `overlay` whose presence changes, with what each resolves to before
    /// the plan and after it.
    fn changes(
        &self,
        overlay: &PathOverlay,
        probed: &[ProbedLink],
        each: &mut dyn FnMut(LinkChange),
    ) -> Result<(), Self::Error>;

    /// What the suffix address `address` names on each side of the plan
    /// `overlay` describes, read as a wikilink written with it is, with the
    /// head of what it names on the side `headed` where that is several.
    fn target(
        &self,
        overlay: &PathOverlay,
        address: &str,
        headed: PlanSide,
    ) -> Result<TargetNaming, Self::Error>;

    /// Say the index will not be read again for the plan at hand, so a handle
    /// it holds for that plan alone may be given back. A later read may take
    /// another.
    fn release(&self) {}
}

/// Whether the resolution change set of a plan with `transitions` and
/// `operations` reads the link index at all, which is whether an apply job
/// needs a read handle for it: [`reads_links_over`] each transition's
/// presence on its two sides.
pub(crate) fn reads_links(transitions: &[Transition], operations: &[Operation]) -> bool {
    reads_links_over(
        transitions.iter().map(|transition| {
            (
                matches!(transition.before, FileState::Present { .. }),
                matches!(transition.after, FileState::Present { .. }),
            )
        }),
        operations,
    )
}

/// **The one fast-path predicate**: whether a plan whose targets each stand
/// as `presence` — whether a document stands there before the plan, and
/// after it — and whose operations are `operations` reads the link index:
/// some target holds a document on one side and not the other, or some
/// operation rewrites a link, as a `rewrite_link`, a `rewrite_wikilink` —
/// whose wikilinks are read whether or not it rewrote one — or through its
/// cascade. A
/// plan of which neither holds records no entry and reads nothing, so
/// [`change_set`] answers it without asking and an apply job mints no handle
/// for it ([`reads_links`]).
fn reads_links_over<'o>(
    presence: impl IntoIterator<Item = (bool, bool)>,
    operations: impl IntoIterator<Item = &'o Operation>,
) -> bool {
    presence.into_iter().any(|(before, after)| before != after)
        || operations.into_iter().any(|operation| {
            matches!(
                operation.kind,
                OperationKind::RewriteLink { .. } | OperationKind::RewriteWikilink { .. }
            ) || !operation.cascade.is_empty()
        })
}

/// One file a plan writes, as the change set reads it.
pub(crate) struct Target<'a> {
    /// The file, at the spelling the plan writes it.
    pub(crate) path: &'a DocumentPath,
    /// Whether a document stands there before the plan.
    pub(crate) before: bool,
    /// What the document there holds after the plan, or `None` where none
    /// stands.
    pub(crate) after: Option<&'a [u8]>,
}

impl<'a> Target<'a> {
    /// Every file `composition` writes, as the change set reads it: whether
    /// a document stands there before the plan, and the bytes it composed
    /// for after.
    pub(crate) fn of(composition: &'a Composition) -> Vec<Target<'a>> {
        composition
            .targets
            .iter()
            .map(|(path, target)| Target {
                path,
                before: matches!(target.before, FileState::Present { .. }),
                after: target.after.as_deref(),
            })
            .collect()
    }
}

/// A plan's resolution change set, and the advisories its forecast carries
/// about the links it reaches.
#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct ChangeSet {
    /// One condition per entry, in the order of its [`EntryKey`]: its
    /// holder, its syntax, then its address.
    pub(crate) entries: Vec<PlanCondition>,
    /// What the plan does to a link a caller should look at, in the same
    /// order.
    pub(crate) advisories: Vec<LinkAdvisory>,
}

/// The resolution change set of the plan writing `targets`, whose content
/// follows `lineage` and whose operations are `operations`, every name read
/// through `normalizer`; the links the plan does not write are judged through
/// `index`. `skipped` is each link the plan's cascades left as written, with
/// why, which the forecast says of that link; `kept` each key that holds a
/// link the cascades did not match beside one they wrote there.
pub(crate) fn change_set<'o, I: LinkIndex + ?Sized>(
    targets: &[Target<'_>],
    lineage: &Lineage,
    normalizer: &PathNormalizer,
    operations: impl IntoIterator<Item = &'o Operation> + Clone,
    skipped: &[Skipped],
    kept: &[Kept],
    index: &I,
) -> Result<ChangeSet, I::Error> {
    let presence = targets
        .iter()
        .map(|target| (target.before, target.after.is_some()));
    if !reads_links_over(presence, operations.clone()) {
        return Ok(ChangeSet::default());
    }
    let written = WrittenLinks::of(operations, normalizer);
    let (overlay, probed) = reach(targets, lineage, normalizer, &written);
    let rewritten_to = rewrite_targets(lineage, &overlay, index)?;
    let retargeted = retarget_namings(lineage, &overlay, index)?;
    let overlay = reaching(overlay, lineage, &retargeted);
    let named_by_old: BTreeSet<NormalizedPath> = retargeted
        .values()
        .filter_map(|named| match &named.old.before {
            Resolves::One { path } => normalizer.normalize(Path::new(path.as_str())).ok(),
            _ => None,
        })
        .collect();
    // A document a move carries away, a delete removes rewriting the links
    // naming it, or a wikilink rewrite's `old` names: one an ambiguous link
    // could name is a document a cascade would follow, were it known which
    // the link names.
    let followed = |path: &str| {
        normalizer
            .normalize(Path::new(path))
            .ok()
            .is_some_and(|file| {
                lineage.carried_to(&file).is_some()
                    || named_by_old.contains(&file)
                    || lineage
                        .removed_by(&file)
                        .is_some_and(|removal| removal.rewrite_to.is_some())
            })
    };

    // Each key holding a link the cascades left as written, matched or not.
    let kept: BTreeSet<EntryKey> = kept
        .iter()
        .map(|kept| (&kept.holder, kept.syntax, &kept.address))
        .chain(
            skipped
                .iter()
                .map(|skip| (&skip.holder, skip.syntax, &skip.address)),
        )
        .map(|(holder, syntax, address)| {
            (
                holder.as_str().to_string(),
                family_name(syntax),
                address.clone(),
            )
        })
        .collect();
    let mut judged: BTreeMap<EntryKey, Judged> = BTreeMap::new();
    index.changes(&overlay, &probed, &mut |change| {
        let key = LinkKey::new(
            wire_path(&change.holder),
            wire_family(change.link.family),
            address(&change.link),
        );
        let entry = entry_key(&key);
        // Whether the key holds a link the plan does not write: every link
        // under a key a rewrite writes reads as written, so one a cascade
        // left as written there is known by its key alone.
        let unwritten = !change.written || kept.contains(&entry);
        // A link left behind by a cascade — a move's, a delete's rewriting
        // the links naming its document, or a wikilink rewrite's — and an
        // ambiguous link
        // that could name a document either follows, which is left as
        // written since which it names is not known — each read from a link
        // the plan does not write, since one it respells to a path it refills
        // reads as left behind from its new address.
        let removal = removed_by(&change.before, lineage, normalizer);
        let left_behind = unwritten
            && (left_behind(&change.before, &change.after, lineage, normalizer).is_some()
                || removal.is_some_and(|removal| {
                    rewritten_for(removal, &change.after, &rewritten_to, normalizer)
                })
                || retargeted_by(&change, lineage, &retargeted, normalizer).is_some());
        let ambiguous_among_moved = unwritten
            && matches!(change.before, Resolves::Several {})
            && change
                .before_targets
                .iter()
                .any(|target| followed(target.as_str()));
        let held = judged.entry(entry).or_insert_with(|| Judged {
            key,
            address: change.address,
            before: change.before,
            after: change.after,
            written: false,
            unwritten: false,
            members_moved: false,
            named_removed: false,
            left_behind: false,
            ambiguous_among_moved: false,
        });
        held.written |= change.written;
        held.unwritten |= unwritten;
        held.members_moved |= change.members_moved;
        held.named_removed |= removal.is_some();
        held.left_behind |= left_behind;
        held.ambiguous_among_moved |= ambiguous_among_moved;
    })?;

    let skips: BTreeMap<EntryKey, RewriteSkip> = skipped
        .iter()
        .map(|skip| {
            let key = (
                skip.holder.as_str().to_string(),
                family_name(skip.syntax),
                skip.address.clone(),
            );
            (key, skip.reason)
        })
        .collect();
    let mut set = ChangeSet::default();
    let mut advised: BTreeMap<EntryKey, LinkAdvisory> = BTreeMap::new();
    for (entry, judged) in judged {
        if let Some(advisory) = judged.advisory(skips.get(&entry).copied()) {
            advised.insert(entry, advisory);
        }
        if judged.records() {
            set.entries.push(PlanCondition::link_resolution(
                judged.key,
                judged.before,
                judged.after,
            ));
        }
    }
    // A skipped link the store's judgment did not reach still says why it
    // stayed, keyed as it would be.
    for skip in skipped {
        let entry = (
            skip.holder.as_str().to_string(),
            family_name(skip.syntax),
            skip.address.clone(),
        );
        advised.entry(entry).or_insert_with(|| {
            skip_advisory(
                LinkKey::new(skip.holder.clone(), skip.syntax, skip.address.clone()),
                skip.reason,
            )
        });
    }
    set.advisories = advised.into_values().collect();
    Ok(set)
}

/// What a plan reaches of the links the store holds: the overlay of every
/// target the plan writes, present before where a document stands there
/// before the plan and after where one stands after, and every link a
/// document the plan writes holds at its after-state, read from the bytes the
/// plan composed and probed from where its content stood before the plan.
///
/// **The one place a plan's two vaults are drawn.** The change set judges its
/// entries through this overlay and these probes, and a link cascade reads
/// the backlinks a move breaks through the same pair, so what a cascade
/// rewrites and what the set records of it are read alike. Whether a target
/// counts as a document on either side is decided here and nowhere else.
///
/// **A target whose document the plan replaces changes too.** A path
/// standing filled on both sides of the plan whose after-state is not drawn
/// from its own before-state — one a move vacates and another move or a
/// create refills — holds another document after the plan than before it
/// ([`replaces`]), so it is overlaid as replaced and every link the store
/// holds under a key that could name it is judged: a link naming the
/// document that left is found, where its path alone would say nothing
/// changed.
///
/// **A moved document's links are read from where it stood.** Each probe's
/// before-holder is its document's lineage source, where a move carried its
/// content from, so a relative link a move breaks is read breaking. A target
/// the store's grammar cannot name is no file the store reads, and is left
/// out; planning leaves an operation naming one unresolved.
pub(crate) fn reach(
    targets: &[Target<'_>],
    lineage: &Lineage,
    normalizer: &PathNormalizer,
    written: &WrittenLinks,
) -> (PathOverlay, Vec<ProbedLink>) {
    let identity = |path: &DocumentPath| normalizer.normalize(Path::new(path.as_str())).ok();
    // The spelling each file standing before the plan is written at, which
    // a moved document's links are read from before the plan.
    let stood: BTreeMap<NormalizedPath, &DocumentPath> = targets
        .iter()
        .filter(|target| target.before)
        .filter_map(|target| Some((identity(target.path)?, target.path)))
        .collect();
    let mut overlay = PathOverlay::new();
    let mut probed = Vec::new();
    for target in targets {
        let Some(stored) = stored_path(target.path) else {
            continue;
        };
        let file = identity(target.path);
        overlay = if replaces(target, file.as_ref(), lineage) {
            overlay.replacing(stored.clone())
        } else {
            overlay.with(stored.clone(), target.before, target.after.is_some())
        };
        let Some(bytes) = target.after else {
            continue;
        };
        let before_holder = file
            .as_ref()
            .and_then(|file| lineage.source(file))
            .and_then(|drawn| stood.get(&drawn.from).copied())
            .and_then(stored_path)
            .unwrap_or_else(|| stored.clone());
        for link in document_links(bytes) {
            let written = file.as_ref().is_some_and(|file| written.holds(file, &link));
            probed.push(ProbedLink {
                before_holder: before_holder.clone(),
                after_holder: stored.clone(),
                link,
                written,
            });
        }
    }
    (overlay, probed)
}

/// Whether the plan replaces the document standing at `target`, the file
/// `file`: a document stands there on both sides, and what stands there after
/// is not drawn from what stood there before — created, or carried there by
/// a move from another file. An edit in place, and a document moved away and
/// back, leave the same document there.
fn replaces(target: &Target<'_>, file: Option<&NormalizedPath>, lineage: &Lineage) -> bool {
    target.before
        && target.after.is_some()
        && file.is_some_and(|file| lineage.source(file).is_none_or(|drawn| drawn.from != *file))
}

/// **The one rule a link follows a move by**: where a link resolved before
/// the plan to exactly one document, and the plan's moves carry that
/// document to another file, the file the link must name after the plan, and
/// how the document got there — unless the link already resolves to exactly
/// that file after the plan. `None` for a link that needs nothing.
///
/// **What is compared is the document, not the path.** A link naming a path
/// the plan vacates and refills resolves to that same path on both sides, but
/// the document it named is the one the plan carries away, so it is left
/// behind exactly as a link the vacated path leaves broken is. A link whose
/// document stays, edited in place or moved away and back, needs nothing, and
/// so does one whose document's move keeps it named — a bare link to a
/// document keeping its stem, a relative link between two documents one
/// folder move carries together.
///
/// A link cascade rewrites exactly the links this names; the change set
/// records every link it names and advises on each the cascade left as
/// written.
pub(crate) fn left_behind<'l>(
    before: &Resolves,
    after: &Resolves,
    lineage: &'l Lineage,
    normalizer: &PathNormalizer,
) -> Option<(&'l NormalizedPath, &'l Drawn)> {
    let identity = |path: &DocumentPath| normalizer.normalize(Path::new(path.as_str())).ok();
    let Resolves::One { path } = before else {
        return None;
    };
    let (to, drawn) = lineage.carried_to(&identity(path)?)?;
    match after {
        Resolves::One { path } if identity(path).as_ref() == Some(to) => None,
        _ => Some((to, drawn)),
    }
}

/// **The one rule a delete's backlinks are read by**: where a link resolved
/// before the plan to exactly one document a delete of the plan removes, that
/// delete. A link resolving to several documents is a backlink of none of
/// them, and a link a removed document holds is gone with it, so neither is
/// ever handed here.
pub(crate) fn removed_by<'l>(
    before: &Resolves,
    lineage: &'l Lineage,
    normalizer: &PathNormalizer,
) -> Option<&'l Removal> {
    let Resolves::One { path } = before else {
        return None;
    };
    let file = normalizer.normalize(Path::new(path.as_str())).ok()?;
    lineage.removed_by(&file)
}

/// What the `rewrite_to` of each delete of the plan rewriting the links
/// naming its document names, by the delete's position, on each side of the
/// plan `overlay` describes, read through `index`: the one reading of it
/// planning resolves the delete by and the change set judges its cascade by.
pub(crate) fn rewrite_targets<I: LinkIndex + ?Sized>(
    lineage: &Lineage,
    overlay: &PathOverlay,
    index: &I,
) -> Result<BTreeMap<usize, TargetNaming>, I::Error> {
    let mut targets = BTreeMap::new();
    for removal in lineage.removals() {
        if let Some(address) = &removal.rewrite_to {
            targets.insert(
                removal.position,
                index.target(overlay, address, PlanSide::After)?,
            );
        }
    }
    Ok(targets)
}

/// **The one rule a delete's cascade follows**: whether a link that
/// resolved before the plan to exactly the document `removal` removes, and
/// resolves to `after` after it, is one the delete rewrites — the delete
/// rewrites the links naming its document, and the link does not already
/// name, after the plan, the one document its `rewrite_to` names there
/// (`targets`, by the delete's position).
///
/// A delete's cascade rewrites exactly the links this names; the change set
/// records every link a removed document leaves and advises on each this
/// names that the cascade left as written.
pub(crate) fn rewritten_for(
    removal: &Removal,
    after: &Resolves,
    targets: &BTreeMap<usize, TargetNaming>,
    normalizer: &PathNormalizer,
) -> bool {
    if removal.rewrite_to.is_none() {
        return false;
    }
    let identity = |path: &DocumentPath| normalizer.normalize(Path::new(path.as_str())).ok();
    let target = targets
        .get(&removal.position)
        .and_then(|target| match &target.after {
            Resolves::One { path } => identity(path),
            _ => None,
        });
    match (after, target) {
        (Resolves::One { path }, Some(target)) => identity(path).as_ref() != Some(&target),
        _ => true,
    }
}

/// What a wikilink rewrite's two ends name on each side of a plan.
#[derive(Debug)]
pub(crate) struct RetargetNaming {
    /// What its `old` names, headed where it names several before the plan.
    pub(crate) old: TargetNaming,
    /// What its `new` names, headed where it names several after the plan.
    pub(crate) new: TargetNaming,
}

/// What the two ends of each wikilink rewrite of the plan name, by the
/// rewrite's position, on each side of the plan `overlay` describes, read
/// through `index`: the one reading of them planning resolves the rewrite by
/// and the change set judges its cascade by.
///
/// **`old` is read before the plan, `new` after it.** The wikilinks a rewrite
/// retargets are the ones naming `old`'s document as the vault stands before
/// the plan — the same side every link's own resolution is read from by
/// [`retargeted_by`], as by a move's and a delete's rules — so the document
/// meant is the one the author saw, wherever the plan carries it, and the
/// plan's own documents cannot make it ambiguous. What they are retargeted to
/// must stand where the plan leaves the vault, as a delete's `rewrite_to`
/// must, so a `new` the plan creates is named.
pub(crate) fn retarget_namings<I: LinkIndex + ?Sized>(
    lineage: &Lineage,
    overlay: &PathOverlay,
    index: &I,
) -> Result<BTreeMap<usize, RetargetNaming>, I::Error> {
    let mut namings = BTreeMap::new();
    for retarget in lineage.retargets() {
        namings.insert(
            retarget.position,
            RetargetNaming {
                old: index.target(overlay, &retarget.old, PlanSide::Before)?,
                new: index.target(overlay, &retarget.new, PlanSide::After)?,
            },
        );
    }
    Ok(namings)
}

/// `overlay`, reaching the wikilinks each wikilink rewrite of the plan could
/// retarget, its ends named as `namings` says: every link naming the one
/// document its `old` names before the plan, or, where `old` names none,
/// every link filed under `old` in any case. An `old` naming several reaches
/// nothing, as its rewrite does not resolve.
pub(crate) fn reaching(
    overlay: PathOverlay,
    lineage: &Lineage,
    namings: &BTreeMap<usize, RetargetNaming>,
) -> PathOverlay {
    lineage.retargets().fold(overlay, |overlay, retarget| {
        match namings
            .get(&retarget.position)
            .map(|named| &named.old.before)
        {
            Some(Resolves::One { path }) => match stored_path(path) {
                Some(stored) => overlay.reaching(stored),
                None => overlay,
            },
            Some(Resolves::None {}) => overlay.reaching_filed_under(&retarget.old),
            _ => overlay,
        }
    })
}

/// **The one rule a wikilink rewrite's cascade follows**: the rewrite of the
/// plan that retargets `change`'s link, its ends named as `namings` says, or
/// `None` where none does. A rewrite retargets a wikilink that, before the
/// plan, resolves to exactly the one document its `old` names — whatever the
/// link's spelling — or, where `old` names no document, one that resolves to
/// nothing, is broken as link health judges it, and is filed under `old` in
/// any case ([`norn_store::filed_under`]); and that does not already name,
/// after the plan, the one document its `new` names there. A Markdown link,
/// and a wikilink resolving to several documents, is retargeted by none.
///
/// A wikilink rewrite's cascade rewrites exactly the links this names; the
/// change set records every link it names and advises on each the cascade
/// left as written.
pub(crate) fn retargeted_by<'l>(
    change: &LinkChange,
    lineage: &'l Lineage,
    namings: &BTreeMap<usize, RetargetNaming>,
    normalizer: &PathNormalizer,
) -> Option<&'l Retarget> {
    if change.link.family != norn_store::LinkFamily::Wikilink {
        return None;
    }
    let identity = |path: &DocumentPath| normalizer.normalize(Path::new(path.as_str())).ok();
    let one = |resolves: &Resolves| match resolves {
        Resolves::One { path } => identity(path),
        _ => None,
    };
    lineage.retargets().find(|retarget| {
        let Some(named) = namings.get(&retarget.position) else {
            return false;
        };
        let names_old = match (&named.old.before, &change.before) {
            (Resolves::One { .. }, Resolves::One { .. }) => {
                one(&named.old.before).is_some() && one(&named.old.before) == one(&change.before)
            }
            (Resolves::None {}, Resolves::None {}) => {
                LinkHealth::of_address(change.address, 0) == LinkHealth::Broken
                    && norn_store::filed_under(&change.link, &retarget.old)
            }
            _ => false,
        };
        let names_new = one(&change.after).is_some() && one(&change.after) == one(&named.new.after);
        names_old && !names_new
    })
}

/// What a link-resolution entry is ordered and matched by: its holder, its
/// syntax as the store names it — `None` for a syntax the store holds no link
/// of — then its address.
pub(crate) type EntryKey = (String, Option<&'static str>, String);

/// `link`'s [`EntryKey`]: the one key a plan's set is ordered by and the
/// applier's comparison of it matches a recorded entry to a computed one by.
pub(crate) fn entry_key(link: &LinkKey) -> EntryKey {
    (
        link.holder.as_str().to_string(),
        family_name(link.syntax),
        link.address.clone(),
    )
}

/// One link the change set judged, keyed as the plan leaves it.
struct Judged {
    key: LinkKey,
    address: LinkAddressKind,
    before: Resolves,
    after: Resolves,
    written: bool,
    /// The key holds a link the plan does not write: a link of no rewrite's,
    /// or one a cascade kept beside a link it wrote there.
    unwritten: bool,
    members_moved: bool,
    /// The link named a document a delete of the plan removes.
    named_removed: bool,
    /// The link named a document a move of the plan carries away, and does
    /// not name it where it lands, a document a delete of the plan removes
    /// rewriting the links naming it, and does not name the delete's target,
    /// or is a wikilink a wikilink rewrite of the plan retargets, and does
    /// not name its `new`: its cascade left it as written.
    left_behind: bool,
    /// The link was ambiguous before the plan, and one of the documents it
    /// could name is one a move of the plan carries away, a delete removes
    /// rewriting the links naming it, or a wikilink rewrite's `old` names.
    ambiguous_among_moved: bool,
}

impl Judged {
    /// Whether the change set records the link: what it resolves to changes,
    /// the plan writes its text, it is left behind, or it named a document
    /// the plan removes — the last two, where the path it resolves to is the
    /// same on both sides, saying the document there is not the one it
    /// named.
    fn records(&self) -> bool {
        self.before != self.after || self.written || self.left_behind || self.named_removed
    }

    /// What the forecast says about the link, where it says anything.
    ///
    /// **A link a cascade left as written says why, and only that.** One the
    /// text layer could not respell carries the reason it gave (`skip`); one
    /// that named a document a move carries away and was not respelled to
    /// follow it, with no reason given, had no spelling that reads back as
    /// that document from its holder, and is unrepresentable; and an
    /// ambiguous link that could name a moved document is skipped as
    /// ambiguous, since which it names is not known. Each replaces what the
    /// link's resolution alone would say of it, and is said wherever the key
    /// holds such a link, though a cascade wrote another link there too.
    ///
    /// **Otherwise each side is read as link health judges it**
    /// ([`LinkHealth::of_address`]): a link healthy or ambiguous before and
    /// broken after is left broken; one ambiguous after and not before is
    /// made ambiguous; and an ambiguous link the plan does not write — the
    /// key holding one, whatever else it holds — is retargeted where it is healthy after, or ambiguous after with members
    /// the plan moved — the last having no entry, since several on both
    /// sides is no change the set records. A side link health does not judge
    /// — an attachment's address resolving to no document — is never broken,
    /// so a link going there is recorded and not advised on.
    fn advisory(&self, skip: Option<RewriteSkip>) -> Option<LinkAdvisory> {
        let key = self.key.clone();
        if self.unwritten {
            if let Some(reason) = skip {
                return Some(skip_advisory(key, reason));
            }
            if self.left_behind {
                return Some(LinkAdvisory::skipped_unrepresentable(key));
            }
            if self.ambiguous_among_moved {
                return Some(LinkAdvisory::skipped_ambiguous(key));
            }
        }
        let health = |resolves: &Resolves| {
            let targets = match resolves {
                Resolves::None {} => 0,
                Resolves::One { .. } => 1,
                Resolves::Several {} => 2,
            };
            LinkHealth::of_address(self.address, targets)
        };
        match (health(&self.before), health(&self.after)) {
            (LinkHealth::Healthy | LinkHealth::Ambiguous, LinkHealth::Broken) => {
                Some(LinkAdvisory::left_broken(key))
            }
            (before, LinkHealth::Ambiguous) if !matches!(before, LinkHealth::Ambiguous) => {
                Some(LinkAdvisory::made_ambiguous(key))
            }
            (LinkHealth::Ambiguous, LinkHealth::Healthy) if self.unwritten => {
                Some(LinkAdvisory::retargeted(key))
            }
            (LinkHealth::Ambiguous, LinkHealth::Ambiguous)
                if self.unwritten && self.members_moved =>
            {
                Some(LinkAdvisory::retargeted(key))
            }
            _ => None,
        }
    }
}

/// The advisory on `link`, which the text layer left as written for
/// `reason`.
///
/// A link two rewrites of one batch name with two different targets cannot
/// carry one new address, and is unrepresentable: planning writes one
/// rewrite per holder, syntax and address, so only a plan no planning wrote
/// meets it.
fn skip_advisory(link: LinkKey, reason: RewriteSkip) -> LinkAdvisory {
    match reason {
        RewriteSkip::Unrepresentable | RewriteSkip::ConflictingRewrites => {
            LinkAdvisory::skipped_unrepresentable(link)
        }
        RewriteSkip::WouldCorruptFrontmatter => {
            LinkAdvisory::skipped_would_corrupt_frontmatter(link)
        }
        RewriteSkip::LinkNotRewritable => LinkAdvisory::skipped_not_rewritable(link),
    }
}

/// The links a plan writes: each a link of one syntax, at one address, in
/// one document's after-state, which a `rewrite_link` of the plan or a
/// rewrite of one of its cascades writes.
#[derive(Default)]
pub(crate) struct WrittenLinks {
    rewritten: BTreeSet<(NormalizedPath, &'static str, String)>,
}

impl WrittenLinks {
    /// The links `operations` write: each `rewrite_link`'s and each
    /// cascade rewrite's document identity, syntax, and the address it
    /// writes.
    fn of<'o>(
        operations: impl IntoIterator<Item = &'o Operation>,
        normalizer: &PathNormalizer,
    ) -> Self {
        let mut rewritten = BTreeSet::new();
        for operation in operations {
            let authored = match &operation.kind {
                OperationKind::RewriteLink {
                    path, syntax, to, ..
                } => Some((path, *syntax, to)),
                _ => None,
            };
            let cascaded = operation
                .cascade
                .iter()
                .map(|rewrite| (&rewrite.path, rewrite.syntax, &rewrite.to));
            for (path, syntax, to) in authored.into_iter().chain(cascaded) {
                if let (Ok(file), Some(syntax)) = (
                    normalizer.normalize(Path::new(path.as_str())),
                    family_name(syntax),
                ) {
                    rewritten.insert((file, syntax, to.clone()));
                }
            }
        }
        WrittenLinks { rewritten }
    }

    /// Whether the plan writes `link`, as its after-state holds it in the
    /// document `holder`: the one predicate "a link whose text the plan
    /// writes" is read by.
    fn holds(&self, holder: &NormalizedPath, link: &norn_store::LinkFact) -> bool {
        self.rewritten
            .contains(&(holder.clone(), link.family.as_str(), address(link)))
    }
}

/// A link's address as written: its protocol prefix, then its target, with
/// no anchor.
pub(crate) fn address(link: &norn_store::LinkFact) -> String {
    match &link.protocol {
        Some(protocol) => format!("{protocol}://{}", link.target),
        None => link.target.clone(),
    }
}

/// `path` as the store names it, where its grammar holds it; planning leaves
/// an operation naming any other unresolved.
pub(crate) fn stored_path(path: &DocumentPath) -> Option<norn_store::DocumentPath> {
    norn_store::DocumentPath::new(path.as_str()).ok()
}

/// A stored path as the wire names it.
fn wire_path(path: &norn_store::DocumentPath) -> DocumentPath {
    DocumentPath::new(path.as_str()).expect("a stored document path is a wire document path")
}

pub(crate) fn wire_family(family: norn_store::LinkFamily) -> LinkFamily {
    match family {
        norn_store::LinkFamily::Wikilink => LinkFamily::Wikilink,
        norn_store::LinkFamily::Markdown => LinkFamily::Markdown,
    }
}

/// A syntax's name as the store spells it, which orders the change set's
/// keys; `None` for a syntax the store holds no link of.
pub(crate) fn family_name(family: LinkFamily) -> Option<&'static str> {
    match family {
        LinkFamily::Wikilink => Some(norn_store::LinkFamily::Wikilink.as_str()),
        LinkFamily::Markdown => Some(norn_store::LinkFamily::Markdown.as_str()),
        _ => None,
    }
}

/// Link indexes over real stores, for cases that plan and apply: a plan's
/// change set is judged by the store's own door, never by a stand-in.
#[cfg(test)]
pub(crate) mod testing {
    use norn_store::{ContentModel, Snapshot, Store, StoredPathOrder};
    use norn_testkit::scratch::Scratch;
    use norn_wire::{VaultAddress, VaultName};

    use super::LinkIndex;
    use crate::apply::PlanSnapshot;

    /// An index no case may read: a plan that changes no document's presence
    /// and writes no link answers its change set without asking, so a case
    /// planning one proves it by handing this, which fails the case if read.
    pub(crate) struct Untouched<E>(std::marker::PhantomData<E>);

    impl<E> Untouched<E> {
        pub(crate) fn new() -> Self {
            Untouched(std::marker::PhantomData)
        }
    }

    impl<E> LinkIndex for Untouched<E> {
        type Error = E;

        fn changes(
            &self,
            _: &norn_store::PathOverlay,
            _: &[norn_store::ProbedLink],
            _: &mut dyn FnMut(norn_store::LinkChange),
        ) -> Result<(), E> {
            panic!(
                "a plan that moves no document's presence and writes no link read the link index"
            )
        }

        fn target(
            &self,
            _: &norn_store::PathOverlay,
            address: &str,
            _: norn_store::PlanSide,
        ) -> Result<norn_store::TargetNaming, E> {
            panic!("a plan that rewrites no link to a target named `{address}`")
        }
    }

    /// A snapshot established now over `store`, whose reader is the
    /// snapshot's own.
    pub(crate) fn snapshot_of(store: &Store) -> Snapshot {
        std::sync::Arc::new(
            store
                .open_reader()
                .reader
                .expect("a live store mints a reader"),
        )
        .try_take()
        .expect("a handle nothing is reading holds its connection")
        .establish()
        .expect("a snapshot")
    }

    /// The vault every case's plan names.
    pub(crate) fn vault() -> VaultAddress {
        VaultAddress::name(VaultName::new("notes").expect("a legal vault name"))
    }

    /// A store holding no document, pinning no schema, and a snapshot over
    /// it: the index a planning case over files alone judges its links on,
    /// where every link a plan reaches is one its own targets hold.
    pub(crate) struct EmptyStore {
        snapshot: Snapshot,
        declared: ContentModel,
        _store: Store,
        _scratch: Scratch,
    }

    impl EmptyStore {
        pub(crate) fn new() -> Self {
            let scratch = Scratch::new("planner-links");
            let store = Store::open_throwaway(
                scratch.join("store.sqlite3"),
                StoredPathOrder::Sensitive,
                crate::DERIVATION_VERSION,
            )
            .expect("a throwaway store");
            EmptyStore {
                snapshot: snapshot_of(&store),
                declared: ContentModel::none(),
                _store: store,
                _scratch: scratch,
            }
        }

        /// The index over the empty store.
        pub(crate) fn index(&self) -> PlanSnapshot<'_> {
            PlanSnapshot::held(vault(), &self.snapshot, &self.declared)
        }
    }

    /// The empty store's index, answering under whatever error a case's
    /// other seams answer under: it never refuses, so a refusal fails the
    /// case.
    impl<E> LinkIndex for (EmptyStore, std::marker::PhantomData<E>) {
        type Error = E;

        fn changes(
            &self,
            overlay: &norn_store::PathOverlay,
            probed: &[norn_store::ProbedLink],
            each: &mut dyn FnMut(norn_store::LinkChange),
        ) -> Result<(), E> {
            self.0
                .index()
                .changes(overlay, probed, each)
                .map_err(|refused| panic!("an empty store's index refused: {refused:?}"))
        }

        fn target(
            &self,
            overlay: &norn_store::PathOverlay,
            address: &str,
            headed: norn_store::PlanSide,
        ) -> Result<norn_store::TargetNaming, E> {
            self.0
                .index()
                .target(overlay, address, headed)
                .map_err(|refused| panic!("an empty store's index refused: {refused:?}"))
        }
    }

    /// `authored` planned as [`crate::planner::resolve::resolve`] plans it
    /// over `view`, its links judged on an empty store: a case over files
    /// alone, every link a plan reaches being one its own targets hold. An
    /// empty store's index never refuses, so a failure is the planning's.
    pub(crate) fn resolve_over_files<V: crate::planner::view::VaultView>(
        authored: norn_wire::AuthoredPlan,
        root: norn_wire::RootIdentity,
        met: &std::collections::BTreeSet<norn_wire::OperationId>,
        view: &V,
    ) -> Result<
        crate::planner::resolve::Resolution,
        crate::planner::resolve::PlanningFailure<V::Error>,
    > {
        use crate::planner::resolve::PlanningFailure;
        let store = EmptyStore::new();
        crate::planner::resolve::resolve(authored, root, met, view, &store.index()).map_err(
            |failure| match failure {
                PlanningFailure::Fault(fault) => PlanningFailure::Fault(fault),
                PlanningFailure::View(error) => PlanningFailure::View(error),
                PlanningFailure::Links(refused) => {
                    panic!("an empty store's index refused: {refused:?}")
                }
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use norn_fs::{CaseSensitivity, PathNormalizer};
    use norn_wire::{
        AuthoredPlan, DocumentPath, LinkAddressKind, LinkAdvisory, LinkFamily, LinkKey, Operation,
        OperationKind, PlanCondition, Resolves, RootIdentity,
    };

    use super::testing::{EmptyStore, Untouched, vault};
    use super::{ChangeSet, Judged, Target, change_set};
    use crate::planner::lineage::Lineage;
    use crate::planner::resolve::resolve;
    use crate::planner::view::memory::MemoryVault;

    fn path(text: &str) -> DocumentPath {
        DocumentPath::new(text).expect("a legal document path")
    }

    fn key(holder: &str, address: &str) -> LinkKey {
        LinkKey::new(path(holder), LinkFamily::Wikilink, address)
    }

    fn entries(conditions: &[PlanCondition]) -> Vec<PlanCondition> {
        conditions
            .iter()
            .filter(|condition| matches!(condition, PlanCondition::LinkResolution { .. }))
            .cloned()
            .collect()
    }

    fn planned(
        vault_files: &MemoryVault,
        operations: Vec<Operation>,
        links: &EmptyStore,
    ) -> crate::planner::resolve::Resolution {
        resolve(
            AuthoredPlan::new(vault(), operations),
            RootIdentity::from_device_and_inode(1, 2),
            &BTreeSet::new(),
            vault_files,
            &links.index(),
        )
        .unwrap_or_else(|failure| panic!("the plan resolves: {failure:?}"))
    }

    /// **A plan that changes no document's presence and writes no link
    /// records nothing and reads no index.** An edit to a document holding a
    /// link, and a frontmatter set, plan against an index that fails the case
    /// if it is read.
    #[test]
    fn an_edit_only_plan_records_nothing_and_reads_no_index() {
        let files = MemoryVault::with(&[("a.md", "---\ns: 1\n---\n[[b]]\n"), ("b.md", "b\n")]);
        let resolution = resolve(
            AuthoredPlan::new(
                vault(),
                vec![
                    Operation::new(OperationKind::str_replace(path("a.md"), "[[b]]", "[[c]]")),
                    Operation::new(OperationKind::set_frontmatter(
                        norn_wire::WriteTarget::path(path("a.md")),
                        "s",
                        norn_wire::AuthoredValue::string("2"),
                    )),
                ],
            ),
            RootIdentity::from_device_and_inode(1, 2),
            &BTreeSet::new(),
            &files,
            &Untouched::<()>::new(),
        )
        .expect("the plan resolves");
        assert!(
            resolution.unresolved.is_empty(),
            "{:?}",
            resolution.unresolved
        );
        assert_eq!(entries(&resolution.plan.conditions), []);
        assert!(resolution.forecast.links.is_empty());
    }

    /// **A link a created document holds is keyed where it stands after the
    /// plan, and read before it from the same place**: `[[b]]` in a created
    /// `a.md` named nothing before the plan, which also creates `b.md`, and
    /// names it after. Written twice, it is one entry.
    #[test]
    fn a_link_a_created_document_holds_is_one_entry_however_often_written() {
        let links = EmptyStore::new();
        let resolution = planned(
            &MemoryVault::default(),
            vec![
                Operation::new(OperationKind::create_document(
                    path("a.md"),
                    "[[b]] and [[b]]\n",
                )),
                Operation::new(OperationKind::create_document(path("b.md"), "b\n")),
            ],
            &links,
        );
        assert_eq!(
            entries(&resolution.plan.conditions),
            [PlanCondition::link_resolution(
                key("a.md", "b"),
                Resolves::none(),
                Resolves::one(path("b.md")),
            )]
        );
        assert!(resolution.forecast.links.is_empty());
    }

    /// **A link a `rewrite_link` writes is an entry whatever it resolves
    /// to**, even where the plan moves no document's presence: what the
    /// rewrite wrote is checked where it lands. The set is computed here from
    /// the bytes the rewrite leaves, alone.
    #[test]
    fn a_link_a_rewrite_writes_is_an_entry_whatever_it_resolves_to() {
        let normalizer = PathNormalizer::for_sensitivity(CaseSensitivity::Sensitive);
        let rewrite = Operation::new(OperationKind::rewrite_link(
            path("b.md"),
            LinkFamily::Wikilink,
            "old",
            "a",
        ));
        let operations = [rewrite];
        let lineage = Lineage::of(&operations, &[0], &normalizer);
        let holder = path("b.md");
        let links = EmptyStore::new();
        let set = change_set(
            &[Target {
                path: &holder,
                before: true,
                after: Some(b"[[a]] and [[other]]\n"),
            }],
            &lineage,
            &normalizer,
            &operations,
            &[],
            &[],
            &links.index(),
        )
        .expect("an empty store's index answers");
        assert_eq!(
            set,
            ChangeSet {
                entries: vec![PlanCondition::link_resolution(
                    key("b.md", "a"),
                    Resolves::none(),
                    Resolves::none(),
                )],
                advisories: Vec::new(),
            }
        );
    }

    /// **A link a cascade rewrites is a written entry keyed at the
    /// after-state**: the holder is named where it stands after the plan —
    /// here where another move of the plan carries it — and the link as the
    /// rewrite leaves it, with what it resolves to from where the holder's
    /// content stood before and from where it stands after.
    #[test]
    fn a_cascade_rewrite_is_a_written_entry_keyed_at_the_after_state() {
        let normalizer = PathNormalizer::for_sensitivity(CaseSensitivity::Sensitive);
        let operations = [
            Operation::new(OperationKind::move_document(path("a.md"), path("x/b.md")))
                .with_cascade(vec![norn_wire::LinkRewrite::new(
                    path("y/h.md"),
                    LinkFamily::Wikilink,
                    "a",
                    "b",
                )]),
            Operation::new(OperationKind::move_document(path("h.md"), path("y/h.md"))),
        ];
        let lineage = Lineage::of(&operations, &[0, 1], &normalizer);
        let (a, b, h, moved_h) = (path("a.md"), path("x/b.md"), path("h.md"), path("y/h.md"));
        let links = EmptyStore::new();
        let set = change_set(
            &[
                Target {
                    path: &a,
                    before: true,
                    after: None,
                },
                Target {
                    path: &b,
                    before: false,
                    after: Some(b"A\n"),
                },
                Target {
                    path: &h,
                    before: true,
                    after: None,
                },
                Target {
                    path: &moved_h,
                    before: false,
                    after: Some(b"[[b]]\n"),
                },
            ],
            &lineage,
            &normalizer,
            &operations,
            &[],
            &[],
            &links.index(),
        )
        .expect("an empty store's index answers");
        assert_eq!(
            set,
            ChangeSet {
                entries: vec![PlanCondition::link_resolution(
                    key("y/h.md", "b"),
                    Resolves::none(),
                    Resolves::one(path("x/b.md")),
                )],
                advisories: Vec::new(),
            }
        );
        assert!(super::reads_links_over([(true, true)], &operations));
    }

    /// **A link to an attachment is advised on as link health judges it**:
    /// resolving to no document it is not judged at all, so going there from
    /// one document leaves nothing broken, while going from none to several
    /// makes it ambiguous.
    #[test]
    fn an_attachment_address_resolving_to_nothing_is_never_left_broken() {
        let judged = |before: Resolves, after: Resolves| {
            Judged {
                key: key("b.md", "v1.2"),
                address: LinkAddressKind::Attachment,
                before,
                after,
                written: false,
                unwritten: true,
                members_moved: true,
                named_removed: false,
                left_behind: false,
                ambiguous_among_moved: false,
            }
            .advisory(None)
        };
        assert_eq!(
            judged(Resolves::one(path("v1.2.md")), Resolves::none()),
            None
        );
        assert_eq!(judged(Resolves::several(), Resolves::none()), None);
        assert_eq!(
            judged(Resolves::none(), Resolves::several()),
            Some(LinkAdvisory::made_ambiguous(key("b.md", "v1.2")))
        );
    }

    /// **The forecast advises on what a caller should look at.** A link that
    /// resolved and resolves to none is left broken; one that did not resolve
    /// to several and does is made ambiguous; an ambiguous link the plan does
    /// not write is retargeted where it resolves to one after, or to several
    /// whose members moved. Nothing else is advised on.
    #[test]
    fn the_forecast_advises_on_broken_ambiguous_and_retargeted_links() {
        let one = || Resolves::one(path("a.md"));
        let judged = |before: Resolves, after: Resolves, written: bool, members_moved: bool| {
            Judged {
                key: key("b.md", "a"),
                address: LinkAddressKind::Document,
                before,
                after,
                written,
                unwritten: !written,
                members_moved,
                named_removed: false,
                left_behind: false,
                ambiguous_among_moved: false,
            }
            .advisory(None)
        };
        let link = key("b.md", "a");
        for (before, after, written, moved, advised) in [
            (
                one(),
                Resolves::none(),
                false,
                true,
                Some(LinkAdvisory::left_broken(link.clone())),
            ),
            (
                Resolves::several(),
                Resolves::none(),
                false,
                true,
                Some(LinkAdvisory::left_broken(link.clone())),
            ),
            (
                Resolves::none(),
                Resolves::several(),
                false,
                true,
                Some(LinkAdvisory::made_ambiguous(link.clone())),
            ),
            (
                one(),
                Resolves::several(),
                false,
                true,
                Some(LinkAdvisory::made_ambiguous(link.clone())),
            ),
            (
                Resolves::several(),
                one(),
                false,
                true,
                Some(LinkAdvisory::retargeted(link.clone())),
            ),
            (
                Resolves::several(),
                Resolves::several(),
                false,
                true,
                Some(LinkAdvisory::retargeted(link.clone())),
            ),
            (Resolves::several(), Resolves::several(), false, false, None),
            (Resolves::several(), one(), true, true, None),
            (Resolves::none(), one(), false, true, None),
            (one(), Resolves::one(path("c.md")), false, true, None),
        ] {
            assert_eq!(
                judged(before.clone(), after.clone(), written, moved),
                advised,
                "{before:?} -> {after:?}, written {written}, moved {moved}"
            );
        }
    }
}
