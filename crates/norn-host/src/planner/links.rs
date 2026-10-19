//! The resolution change set: every link whose resolution a plan changes, and
//! every link whose text it writes, each recorded as a condition naming the
//! link as the plan leaves it with what it resolves to before the plan and
//! after it — and the advisories a forecast carries about them.
//!
//! **One computation, two callers.** The planner records the set a plan
//! resolves to, and the applier computes it again, after its intake, from the
//! resolved plan alone and refuses on any difference (ADR 0037): an entry the
//! plan records that the set computed again does not hold, or holds with
//! other values, and an entry the set computed again holds that the plan
//! does not record. Both compute it here, from the same inputs — the plan's
//! targets with whether a document stands at each before and after, the bytes
//! each composed document holds after, the plan's content lineage and its
//! operations — so the two can only differ where the vault outside the plan
//! moved.
//!
//! **A document stands where a file's bytes decode as one.** A file whose
//! bytes do not is quarantined: the store derives no document from it, so no
//! link resolves to it, and the set reads it as no document — deleting or
//! moving one changes no link's resolution, and bytes that start or stop
//! decoding put a document where none stood, or take one away, though a file
//! stands there throughout. Whether a target is a document on each side is
//! read from the plan's file states ([`Target::new`]), which record it for
//! every present side by the derivation's own rule, never from bytes here: a
//! target that already holds its change has no before-bytes left to decode,
//! so planning and the applier read the recorded flag alike, and the applier
//! holds the flag to every side whose bytes it does hold.
//!
//! **What is judged where.** A document the plan writes is read from the
//! bytes the plan composed, since the store holds its before-state — a
//! document a move carries unread from the links the index holds for its
//! source at the hash it carries, which are those bytes' links
//! ([`vouched`]), or from the source read whole at that hash where the index
//! does not vouch for them: each link
//! it holds is keyed at its after-state, and read before the plan from where
//! the document's content stood — a moved document's source, whether or not
//! its bytes decoded there — so a relative link a move breaks is seen
//! breaking. Every other link is the store's, read
//! through the [`LinkIndex`] by the keys that could name a target whose
//! presence the plan changes, or whose document it replaces
//! (`norn_store::Snapshot::resolution_changes`). A link in a document the
//! plan removes is no entry: its disappearance is the plan's own transition.
//!
//! **An entry is a link whose resolution changes, whose text the plan
//! writes, that is left behind** ([`left_behind`]), **or that named a document
//! the plan removes.** The vocabulary an
//! entry records a resolution in names paths, so a link left naming a path
//! the plan vacates and refills is recorded naming that path on both sides:
//! what changed is the document there, which the entry's presence in the set
//! says, and the forecast says why the cascade left it. A backlink to such a
//! path another writer adds after planning is then an entry the plan does
//! not record, and refuses it as one to a vacated path does.
//!
//! **A plan that changes no document's presence, deletes none and writes no
//! link is answered without reading anything.** No link's resolution moves
//! unless a document appears or disappears somewhere a key names, the
//! document at a path a key names is replaced, or the plan writes the link's
//! text, so such a plan — every edit-only plan, a frontmatter set among them —
//! records nothing, and the index is never asked: nothing is minted for it.
//! A plan replaces the document at a path while every presence stays as it
//! was only by deleting it and refilling the path: a document a move carries
//! away lands somewhere nothing stood or leaves somewhere nothing stands
//! after, since moves closing a cycle are refused, and a create with no
//! delete adds a document. So a plan holding a delete is read — a delete
//! whose path the plan refills among them, whose backlinks are judged as any
//! other delete's.
//!
//! **Which link addresses a plan writes is one predicate**
//! ([`WrittenLinks::holds`]):
//! a link a rewrite of the plan writes — a `rewrite_link` operation's, or
//! one of a cascade's — held in its document under the syntax and the
//! address the rewrite writes. An anchor rewrite is recorded by the forecast
//! separately, keeping its address subject to cascades. Every such link is an
//! entry whatever it resolves to, so a rewrite is checked where it lands. Whether it is a
//! backlink of a document a delete removes is read from the text it had —
//! the address the rewrite matched, probed from where its holder's content
//! stood ([`Reached::originals`]) — never from its new text, unless the
//! rewrite is the one its author said it names by: an authored link
//! rewrite's or a wikilink rewrite's ([`decider`]), which clears a backlink
//! by respelling it.
//!
//! **What the forecast says of a link a cascade did not follow.** A link
//! that named a document a move carries away, and does not name it where it
//! lands, a document a delete removes rewriting the links naming it, and
//! does not name the delete's target, or a wikilink a wikilink rewrite
//! retargets that does not name its `new` ([`respells`]), is advised on
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

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use norn_fs::{NormalizedPath, PathNormalizer};
use norn_store::{
    HeldLinks, LinkChange, LinkFact, PathOverlay, PlanSide, ProbedLink, TargetNaming,
};
use norn_text::RewriteSkip;
use norn_wire::{
    Backlinks, DocumentPath, FileState, LinkAddress, LinkAddressKind, LinkAdvisory, LinkFamily,
    LinkHealth, LinkKey, Operation, OperationKind, PlanCondition, Resolves,
};

use super::compose::{After, Composition, Kept, Skipped};
use super::control::role_at;
use super::lineage::{Drawn, Lineage, Relink, Removal, Retarget};
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

    /// What each of `addresses` names over the plan `overlay`, as
    /// [`LinkIndex::target`] reads one, in their order: one judgment for
    /// them all where the index answers so, so the plan's targets are read
    /// once, not once per address.
    fn targets(
        &self,
        overlay: &PathOverlay,
        addresses: &[&str],
        headed: PlanSide,
    ) -> Result<Vec<TargetNaming>, Self::Error> {
        addresses
            .iter()
            .map(|address| self.target(overlay, address, headed))
            .collect()
    }

    /// The links the index holds for the document at `holder`, beside the
    /// content hash of the bytes it derived them from; `None` where it holds
    /// no document at that spelling.
    ///
    /// **A reading of a document's links without its body.** Where the hash
    /// answered is the hash of the file a caller streamed, the links are
    /// exactly what [`document_links`] reads from the file's bytes, in its
    /// order; where it is not, they are another file's, and the caller reads
    /// the file itself or leaves the question unresolved.
    ///
    /// **What a carried document's links are read from.** A move whose
    /// document the plan carries byte for byte holds no body to parse, so
    /// planning and the applier read its links here, vouched for by the hash
    /// ([`vouched`]).
    fn held_links(&self, holder: &DocumentPath) -> Result<Option<HeldLinks>, Self::Error>;

    /// The frontmatter block the index holds for the document at `holder`,
    /// beside the content hash of the bytes it derived it from; `None` where
    /// it holds no document at that spelling.
    ///
    /// **What a carried document is judged again by.** A document a move
    /// carries byte for byte is judged at its destination under the schema
    /// rules that read where a document stands, and its block is read here,
    /// vouched for by the hash ([`vouched_block`]), so the move holds the
    /// store's projection and no copy of the document.
    ///
    /// An index answering for no store vouches for no block, and the applier
    /// reads the file instead: the default.
    fn held_frontmatter(
        &self,
        holder: &DocumentPath,
    ) -> Result<Option<norn_store::HeldFrontmatter>, Self::Error> {
        let _ = holder;
        Ok(None)
    }

    /// The indexed headings and recorded hash of one target, without its
    /// body. An index holding no store has no heading evidence to offer.
    fn held_headings(
        &self,
        path: &DocumentPath,
    ) -> Result<Option<norn_store::HeldHeadings>, Self::Error> {
        let _ = path;
        Ok(None)
    }

    /// Say the index will not be read again for the plan at hand, so a handle
    /// it holds for that plan alone may be given back. A later read may take
    /// another.
    fn release(&self) {}
}

/// **The one fast-path predicate**: whether a plan whose targets each stand
/// as `presence` — whether a document stands there before the plan, and
/// after it — and whose operations are `operations` reads the link index:
/// some target holds a document on one side and not the other; some
/// operation deletes a document, which is how a plan replaces the document
/// at a path it refills, every presence staying as it was ([`replaces`]); or
/// some operation rewrites a link, as a `rewrite_link`, a `rewrite_wikilink`
/// — whose wikilinks are read whether or not it rewrote one — or through its
/// cascade. A file whose bytes start or stop decoding holds a document on
/// one side only, though a file stands there on both. A plan of which none
/// holds records no entry and reads nothing, so [`change_set`] answers it
/// without asking, and an apply job whose index is asked by nothing else
/// mints no handle for it.
fn reads_links_over<'o>(
    presence: impl IntoIterator<Item = (bool, bool)>,
    operations: impl IntoIterator<Item = &'o Operation>,
) -> bool {
    presence.into_iter().any(|(before, after)| before != after)
        || operations.into_iter().any(|operation| {
            matches!(
                operation.kind,
                OperationKind::RewriteLink { .. }
                    | OperationKind::RewriteAnchor { .. }
                    | OperationKind::RewriteWikilink { .. }
                    | OperationKind::DeleteDocument { .. }
            ) || !operation.cascade.is_empty()
        })
}

/// The links each document a plan carries byte for byte holds, by the
/// spelling it stood at before the plan ([`After::Carried`]): what the change
/// set and a move's cascade read of it in place of its bytes.
#[derive(Debug, Default)]
pub(crate) struct CarriedLinks {
    held: BTreeMap<DocumentPath, Vec<LinkFact>>,
}

impl CarriedLinks {
    /// The links the document carried from `from` holds, where they were
    /// read.
    pub(crate) fn of(&self, from: &DocumentPath) -> Option<&[LinkFact]> {
        self.held.get(from).map(Vec::as_slice)
    }

    /// Take in `links` as the links the document carried from `from` holds.
    pub(crate) fn insert(&mut self, from: DocumentPath, links: Vec<LinkFact>) {
        self.held.insert(from, links);
    }
}

/// **The one guard on a carried document's links**: the links the document
/// at `from`, standing at `state`, holds, read through `index` without its
/// body, or `None` where the index cannot vouch for them.
///
/// **The index vouches only for the bytes it read.** Its links are the ones
/// [`document_links`] reads from a file's bytes only where the hash it
/// derived them from is the file's, so a document whose state hashes
/// otherwise — changed since the vault's index saw it, or never indexed — is
/// vouched for by nothing. The caller then reads the file whole at that
/// state and its links from its bytes, one copy, as planning read every
/// moved document before it carried any: planning where it composes
/// (`super::resolve`), the applier where it checks (`crate::applier`). So a
/// carried move holds no copy of its document once the vault has indexed
/// it, and one copy where the index lags the file.
///
/// **A document whose bytes do not decode holds no link**, the answer
/// [`document_links`] gives any bytes that do not decode, so it is vouched
/// for with none and no index is asked: the store derives no document from
/// it, so the index holds no row to compare.
pub(crate) fn vouched<I: LinkIndex + ?Sized>(
    from: &DocumentPath,
    state: &FileState,
    index: &I,
) -> Result<Option<Vec<LinkFact>>, I::Error> {
    if !state.is_document() {
        return Ok(Some(Vec::new()));
    }
    let Some(hash) = state.hash() else {
        return Ok(None);
    };
    Ok(index
        .held_links(from)?
        .filter(|held| held.content_hash == hash.hex())
        .map(|held| held.links))
}

/// **The one guard on a carried document's frontmatter**: what the block of
/// the document at `from`, standing at `state`, came to, read through
/// `index` without its body, or `None` where the index cannot vouch for it —
/// the hash its row records is not `state`'s, or it holds no document there.
/// The caller then reads the file whole at that state, as [`vouched`]'s does
/// for the links.
pub(crate) fn vouched_block<I: LinkIndex + ?Sized>(
    from: &DocumentPath,
    state: &FileState,
    index: &I,
) -> Result<Option<norn_store::HeldBlock>, I::Error> {
    let Some(hash) = state.hash() else {
        return Ok(None);
    };
    Ok(index
        .held_frontmatter(from)?
        .filter(|held| held.content_hash == hash.hex())
        .map(|held| held.block))
}

/// The source of every document `composition` carries whose links `known`
/// does not yet hold, read into it through `index` ([`vouched`]); and each
/// source the index could not vouch for, at the spelling it stood at and the
/// state it is carried at.
pub(crate) fn vouch_for_carried<I: LinkIndex + ?Sized>(
    composition: &Composition,
    known: &mut CarriedLinks,
    index: &I,
) -> Result<Vec<(DocumentPath, FileState)>, I::Error> {
    let mut unvouched: Vec<(DocumentPath, FileState)> = Vec::new();
    for target in composition.targets.values() {
        let After::Carried { state, from } = &target.after else {
            continue;
        };
        if known.of(from).is_some() || unvouched.iter().any(|(source, _)| source == from) {
            continue;
        }
        match vouched(from, state, index)? {
            Some(links) => known.insert(from.clone(), links),
            None => unvouched.push((from.clone(), state.clone())),
        }
    }
    Ok(unvouched)
}

/// What a file holds after a plan, as the change set reads it: its bytes,
/// or the links a document carried unread holds ([`CarriedLinks`]).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Holding<'a> {
    Bytes(&'a [u8]),
    Links(&'a [LinkFact]),
}

/// One file a plan writes, as the change set reads it.
///
/// **A document stands where a file's bytes decode as one**, which is what
/// makes it a link's candidate: a quarantined file is none, so the change
/// set reads one as no document, before the plan and after it. Built only by
/// [`Target::new`], from the plan's file states, so planning and the applier
/// read a side alike whether or not its bytes are still there to decode.
///
/// **A control file is no document on either side** ([`super::control`]),
/// whatever its bytes: the vault derives none from it, so it is no link's
/// candidate and holds no link the vault reads, and a plan writing one reads
/// no link.
///
/// **A document's links are read once per target.** Built from bytes, a
/// target reads their links here, once ([`document_links`]); built from a
/// carried document, it takes the links the index vouched for. Every reader
/// of a target's links — the change set's probes, and a move's own relative
/// links in its cascade — reads them off the target.
pub(crate) struct Target<'a> {
    /// The file, at the spelling the plan writes it.
    path: &'a DocumentPath,
    /// Whether a file stands there before the plan, a document or not: where
    /// content the plan carries elsewhere stood.
    stood: bool,
    /// Whether a document stands there before the plan.
    before: bool,
    /// The links the document there holds after the plan, or `None` where
    /// none stands.
    after: Option<Cow<'a, [LinkFact]>>,
}

impl<'a> Target<'a> {
    /// The file at `path`, going from `before` to `after`, holding `holding`
    /// after the plan where a file stands there then: a document on each
    /// side only where that side's state says its bytes decode.
    pub(crate) fn new(
        path: &'a DocumentPath,
        before: &FileState,
        after: &FileState,
        holding: Option<Holding<'a>>,
    ) -> Self {
        let document = role_at(path.as_str()).is_none();
        Target {
            path,
            stood: before.hash().is_some(),
            before: document && before.is_document(),
            after: holding.filter(|_| document && after.is_document()).map(
                |holding| match holding {
                    Holding::Bytes(bytes) => Cow::Owned(document_links(bytes)),
                    Holding::Links(links) => Cow::Borrowed(links),
                },
            ),
        }
    }

    /// Every file `composition` writes, as the change set reads it: its
    /// before-state as composition read it, and what it composed for after —
    /// bytes, or a document carried unread, whose links `carried` holds —
    /// whose state is the one the plan records ([`After::state`]).
    pub(crate) fn of(composition: &'a Composition, carried: &'a CarriedLinks) -> Vec<Target<'a>> {
        composition
            .targets
            .iter()
            .map(|(path, target)| {
                let holding = match &target.after {
                    After::Absent => None,
                    After::Bytes(bytes) => Some(crate::planner::links::Holding::Bytes(bytes)),
                    After::Carried { from, .. } => Some(Holding::Links(carried.of(from).expect(
                        "planning vouches for every carried document's links before reading them",
                    ))),
                };
                Target::new(path, &target.before, &target.after.state(), holding)
            })
            .collect()
    }

    /// The file, at the spelling the plan writes it.
    pub(crate) fn path(&self) -> &DocumentPath {
        self.path
    }

    /// The links the document there holds after the plan, where one stands.
    pub(crate) fn links(&self) -> Option<&[LinkFact]> {
        self.after.as_deref()
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
    /// The position of each delete whose link choice the set contradicts
    /// ([`Removal::kept_by`]), in order. Planning leaves every such delete
    /// unresolved by the same rule, so a plan it resolved holds none; the applier refuses a
    /// resolved plan holding one as one that is not what its operations do.
    pub(crate) unkept: Vec<usize>,
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
    let Reached {
        overlay,
        mut probed,
        originals,
    } = reach(targets, lineage, normalizer, &written);
    // The forecast must retain anchor-only text writes before the index drops
    // unchanged address keys. Cascade probes keep only address authorship.
    for probe in &mut probed {
        probe.written |= normalizer
            .normalize(Path::new(probe.after_holder.as_str()))
            .ok()
            .is_some_and(|holder| written.holds_anchor(&holder, &probe.link));
    }
    let rewritten_to = rewrite_targets(lineage, &overlay, index)?;
    let retargeted = retarget_namings(lineage, &overlay, index)?;
    let indexed = Retargets::of(lineage, &retargeted, normalizer);
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
                        .is_some_and(|removal| removal.rewrite_to().is_some())
            })
    };

    let kept = left_as_written(skipped, kept);
    let mut judged: BTreeMap<EntryKey, Judged> = BTreeMap::new();
    // Each delete a link naming its document contradicts.
    let mut named: BTreeSet<usize> = BTreeSet::new();
    // Whether a cascade follows a place an ambiguous link could name, read
    // once per key's list of places.
    let mut among_followed = AmongFollowed::default();
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
        let address_written = change.written
            && normalizer
                .normalize(Path::new(change.holder.as_str()))
                .ok()
                .is_some_and(|holder| written.holds(&holder, &change.link));
        let unwritten = !address_written || kept.contains(&entry);
        // A backlink of a document a delete removes, a link left behind by
        // a cascade — a move's, a delete's rewriting the links naming its
        // document, or a wikilink rewrite's — and an ambiguous link that
        // could name a document either follows, which is left as written
        // since which it names is not known — each read here from a link the
        // plan does not write. A link a rewrite writes is a backlink by the
        // text it had, read below from its original, never by what its new
        // text names before the plan; one respelled to a path the plan
        // refills reads as left behind from its new address.
        let removal = unwritten
            .then(|| removed_by(&change.before, lineage, normalizer))
            .flatten();
        if let Some(removal) = removal {
            named.insert(removal.position);
        }
        // The operation deciding what the link names after the plan, and
        // whether its cascade respells the link for it.
        let decided = unwritten
            .then(|| {
                decider(
                    &change.holder,
                    &change.link,
                    Named::of(&change),
                    lineage,
                    &indexed,
                    normalizer,
                )
            })
            .flatten();
        // Whether the plan's after-state respells the link its decider
        // decides, probed at most once: `fulfilled` and `left_behind` both
        // ask it.
        let mut respelled_once: Option<bool> = None;
        let mut respelled = |decider| {
            *respelled_once.get_or_insert_with(|| {
                respells(
                    decider,
                    &change.before,
                    &change.after,
                    lineage,
                    &rewritten_to,
                    &indexed,
                    normalizer,
                )
            })
        };
        // A link a delete rewriting its links, or a wikilink rewrite, would
        // respell and already names what that operation names: the plan
        // took it where its author said.
        let fulfilled = decided.is_some_and(|decided| {
            let names_a_destination = match decided {
                Decider::Removal(removal) => removal.rewrite_to().is_some(),
                Decider::Retarget(_) => true,
                Decider::Authored(_) | Decider::Move { .. } => false,
            };
            names_a_destination && !respelled(decided)
        });
        // A link the plan leaves naming another document than the one it
        // named, and not that document where a move carries it. Where its
        // holder moved and it is a relative path, the move alone changed what
        // it names, and the cascade had no spelling to keep it: a wikilink
        // written as a path is given none yet, and a Markdown one whose
        // spelling does not read back is left too.
        let names_another = unwritten
            && !fulfilled
            && names_another(&change.before, &change.after, lineage, normalizer);
        let holder_moved = normalizer
            .normalize(Path::new(change.holder.as_str()))
            .ok()
            .is_some_and(|holder| {
                lineage
                    .source(&holder)
                    .is_some_and(|drawn| drawn.from != holder)
            });
        let relative = matches!(
            LinkAddress::of(
                wire_family(change.link.family),
                change.link.protocol.as_deref(),
                &change.link.target
            ),
            LinkAddress::Relative(_)
        );
        let relative_left = names_another && holder_moved && relative;
        let left_behind = relative_left || decided.is_some_and(&mut respelled);
        // Links held under one key share its places, so the places are read
        // once per key, however many documents the key names and however
        // the links of several keys interleave.
        let ambiguous_among_moved = unwritten
            && matches!(change.before, Resolves::Several {})
            && among_followed.any(change.before_targets.lists(), &followed);
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
            names_another: false,
        });
        held.written |= change.written;
        held.unwritten |= unwritten;
        held.members_moved |= change.members_moved;
        held.named_removed |= removal.is_some();
        held.left_behind |= left_behind;
        held.ambiguous_among_moved |= ambiguous_among_moved;
        held.names_another |= names_another;
    })?;
    // A link a rewrite writes names the document a delete removes where the
    // text it had resolved before the plan to exactly that document, read
    // from where its holder's content stood: a backlink respelled away, or
    // respelled from where its moved holder lands, is still one, and a link
    // respelled toward the deleted document's name is none. A backlink an
    // authored link rewrite or a wikilink rewrite decides is that rewrite's,
    // whose author said what it names, so respelled it is none. Only a delete forbidding the links
    // naming its document reads whether one does ([`Removal::kept_by`]), so
    // the originals are read only where one does.
    if !originals.is_empty()
        && lineage
            .removals()
            .any(|removal| removal.backlinks == Backlinks::Forbidden)
    {
        index.changes(&overlay, &originals, &mut |change| {
            let Some(removal) = change
                .written
                .then(|| removed_by(&change.before, lineage, normalizer))
                .flatten()
            else {
                return;
            };
            let authored = matches!(
                decider(
                    &change.holder,
                    &change.link,
                    Named::of(&change),
                    lineage,
                    &indexed,
                    normalizer
                ),
                Some(Decider::Authored(_) | Decider::Retarget(_))
            );
            if !authored {
                named.insert(removal.position);
            }
        })?;
    }

    // A key whose links were left for several reasons is advised once, by
    // the reason ranked first, so the advisory does not depend on where in
    // its holder each of them stands.
    let mut skips: BTreeMap<EntryKey, RewriteSkip> = BTreeMap::new();
    for skip in skipped {
        let key = (
            skip.holder.as_str().to_string(),
            family_name(skip.syntax),
            skip.address.clone(),
        );
        skips
            .entry(key)
            .and_modify(|held| {
                if skip_rank(skip.reason) < skip_rank(*held) {
                    *held = skip.reason;
                }
            })
            .or_insert(skip.reason);
    }
    let mut set = ChangeSet {
        unkept: lineage
            .removals()
            .filter(|removal| {
                !removal.kept_by(
                    named.contains(&removal.position),
                    rewritten_to.get(&removal.position),
                    normalizer,
                )
            })
            .map(|removal| removal.position)
            .collect(),
        ..ChangeSet::default()
    };
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
        let reason = skips[&entry];
        advised.entry(entry).or_insert_with(|| {
            skip_advisory(
                LinkKey::new(skip.holder.clone(), skip.syntax, skip.address.clone()),
                reason,
            )
        });
    }
    set.advisories = advised.into_values().collect();
    Ok(set)
}

/// Every address resolution among `conditions` that does not hold at the
/// plan's after-state, in the order they are recorded: from the condition's
/// holder, its address resolves to something other than what it records.
///
/// **The address is probed, not looked up.** An address resolution names the
/// link by holder, syntax and address, and the link need not stand in the
/// holder after the plan: an address a rewrite replaced is judged as well as
/// one it writes. Each is handed to `index` as a link the plan writes, so it
/// is judged and handed back whatever it resolves to, from the holder path
/// the condition records, over an overlay that draws every target of the plan at
/// its after-state alone. The overlay changes no target's presence, so the
/// judgment reads only the keys the probes name, in the one call every
/// condition shares, and none of the links the store holds under the keys a
/// target could be named by. A plan recording no address resolution asks
/// `index` nothing.
///
/// A condition whose syntax the store holds no link of (`probe_of` is `None`)
/// is reported failed: no resolution could confirm it.
///
/// A written address is probed from where its holder lands; a follow's
/// replaced address from where the finding read it, even when the plan
/// moves that holder away. Only the after side of the judgment is read,
/// over the plan's targets; no holder need stand at the recorded path and
/// no lineage source is needed.
pub(crate) fn failed_address_resolutions<I: LinkIndex + ?Sized>(
    targets: &[Target<'_>],
    conditions: &[PlanCondition],
    index: &I,
) -> Result<Vec<PlanCondition>, I::Error> {
    let recorded: Vec<(&PlanCondition, &LinkKey, &Resolves)> = conditions
        .iter()
        .filter_map(|condition| match condition {
            PlanCondition::AddressResolution { link, after } => Some((condition, link, after)),
            PlanCondition::ContentHash { .. } | PlanCondition::LinkResolution { .. } => None,
        })
        .collect();
    if recorded.is_empty() {
        return Ok(Vec::new());
    }
    let overlay = targets.iter().fold(PathOverlay::new(), |overlay, target| {
        let standing = target.after.is_some();
        overlay.with(stored_path(target.path), standing, standing)
    });
    let links: Vec<&LinkKey> = recorded.iter().map(|(_, link, _)| *link).collect();
    let resolved = resolved_addresses(index, &overlay, links)?;
    Ok(recorded
        .into_iter()
        .filter(|(_, link, after)| resolved.get(&entry_key(link)) != Some(*after))
        .map(|(condition, _, _)| condition.clone())
        .collect())
}

/// What each of `links` resolves to from its holder with every target of the
/// plan `overlay` describes at its after-state, by the link's key
/// ([`entry_key`]), in one judgment of the index: the one probe an address
/// resolution is recorded by — a repair's planner, for every link its fixes
/// write — and checked by ([`failed_address_resolutions`]), so what a
/// planner records is by construction what the applier computes again. A
/// link of a syntax the store holds no link of has no entry.
pub(crate) fn resolved_addresses<'l, I: LinkIndex + ?Sized>(
    index: &I,
    overlay: &PathOverlay,
    links: impl IntoIterator<Item = &'l LinkKey>,
) -> Result<BTreeMap<EntryKey, Resolves>, I::Error> {
    let probed: Vec<ProbedLink> = links.into_iter().filter_map(probe_of).collect();
    let mut resolved: BTreeMap<EntryKey, Resolves> = BTreeMap::new();
    if probed.is_empty() {
        return Ok(resolved);
    }
    index.changes(overlay, &probed, &mut |change| {
        let key = LinkKey::new(
            wire_path(&change.holder),
            wire_family(change.link.family),
            address(&change.link),
        );
        resolved.insert(entry_key(&key), change.after);
    })?;
    Ok(resolved)
}

/// The overlay of a plan whose only effect on the vault's documents is
/// `moves`, each `(from, to)`: a document gone from `from` and standing at
/// `to`, drawn as the applier draws every target of a plan at its after-state
/// alone ([`failed_address_resolutions`]).
pub(crate) fn moved_overlay<'m>(
    moves: impl IntoIterator<Item = (&'m DocumentPath, &'m DocumentPath)>,
) -> PathOverlay {
    moves
        .into_iter()
        .fold(PathOverlay::new(), |overlay, (from, to)| {
            overlay
                .with(stored_path(from), false, false)
                .with(stored_path(to), true, true)
        })
}

/// `link` as a link its holder's after-state writes, to be judged wherever it
/// resolves; `None` for a syntax the store holds no link of.
pub(crate) fn probe_of(link: &LinkKey) -> Option<ProbedLink> {
    let family = match link.syntax {
        LinkFamily::Wikilink => norn_store::LinkFamily::Wikilink,
        LinkFamily::Markdown => norn_store::LinkFamily::Markdown,
        _ => return None,
    };
    let (protocol, stem) = link.protocol_and_stem();
    let holder = stored_path(&link.holder);
    Some(ProbedLink {
        before_holder: holder.clone(),
        after_holder: holder,
        link: norn_store::LinkFact {
            field: None,
            family,
            embed: false,
            protocol: protocol.map(str::to_string),
            target: stem.to_string(),
            title: None,
            anchor: None,
            span: None,
        },
        written: true,
    })
}

/// Each key holding a link a plan's rewrites left as written, matched or
/// not: a link a rewrite matched and the text layer left (`skipped`), and a
/// link no rewrite matched under an address one writes (`kept`). Every link
/// under a key a rewrite writes reads as written, so one left as written
/// there is known by its key alone.
pub(crate) fn left_as_written(skipped: &[Skipped], kept: &[Kept]) -> BTreeSet<EntryKey> {
    kept.iter()
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
        .collect()
}

/// What a plan reaches of the links the store holds: the overlay of every
/// target the plan writes, present before where a document stands there
/// before the plan and after where one stands after; every link a document
/// the plan writes holds at its after-state, read from the bytes the plan
/// composed and probed from where its content stood before the plan; and
/// each link the plan writes as it was written before a rewrite respelled
/// it ([`Reached::originals`]).
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
/// content from, so a relative link a move breaks is read breaking.
pub(crate) fn reach(
    targets: &[Target<'_>],
    lineage: &Lineage,
    normalizer: &PathNormalizer,
    written: &WrittenLinks,
) -> Reached {
    let identity = |path: &DocumentPath| normalizer.normalize(Path::new(path.as_str())).ok();
    // The spelling each file standing before the plan is written at, which
    // a moved document's links are read from before the plan. Where its
    // content stood is a fact of the file's path and lineage, so a file
    // whose bytes did not decode there counts: one repaired and moved is
    // read from where it stood, not from where it lands.
    let stood: BTreeMap<NormalizedPath, &DocumentPath> = targets
        .iter()
        .filter(|target| target.stood)
        .filter_map(|target| Some((identity(target.path)?, target.path)))
        .collect();
    let mut overlay = PathOverlay::new();
    let mut probed = Vec::new();
    let mut originals = Vec::new();
    for target in targets {
        let stored = stored_path(target.path);
        let file = identity(target.path);
        overlay = if replaces(target, file.as_ref(), lineage) {
            overlay.replacing(stored.clone())
        } else {
            overlay.with(stored.clone(), target.before, target.after.is_some())
        };
        let Some(links) = target.links() else {
            continue;
        };
        let before_holder = file
            .as_ref()
            .and_then(|file| lineage.source(file))
            .and_then(|drawn| stood.get(&drawn.from).copied())
            .map(stored_path)
            .unwrap_or_else(|| stored.clone());
        for link in links.iter().cloned() {
            let rewritten = file.as_ref().is_some_and(|file| written.holds(file, &link));
            if let (true, Some(file)) = (rewritten, file.as_ref()) {
                originals.extend(written.originals(file, &link).map(|original| ProbedLink {
                    before_holder: before_holder.clone(),
                    after_holder: stored.clone(),
                    link: original,
                    written: true,
                }));
            }
            probed.push(ProbedLink {
                before_holder: before_holder.clone(),
                after_holder: stored.clone(),
                link,
                written: rewritten,
            });
        }
    }
    Reached {
        overlay,
        probed,
        originals,
    }
}

/// What [`reach`] draws of a plan.
pub(crate) struct Reached {
    /// Every target the plan writes, on each side of it.
    pub(crate) overlay: PathOverlay,
    /// Every link a document the plan writes holds at its after-state.
    pub(crate) probed: Vec<ProbedLink>,
    /// **Each link the plan writes as it was written before**: for every
    /// link a rewrite of the plan writes, the link with each address a
    /// rewrite writing it respelled it from, held where it is held after the
    /// plan and read before the plan from where its holder's content stood
    /// — where the link a rewrite matched stood, so what it resolves to
    /// before the plan is what the link itself named. Marked written, so
    /// each is judged and handed back whatever it resolves to.
    pub(crate) originals: Vec<ProbedLink>,
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

/// **Whether a link names another document after the plan**: it resolved
/// before the plan to exactly one document, and after it resolves to anything
/// but that document, where it stood or where a move of the plan carries it
/// — none, several, or one other. With the exact path first (ADR 0040), a
/// document appearing at a suffix link's exact path, or leaving it, moves the
/// link between documents without breaking it; a link that still names its
/// document, followed or left in place, does not.
///
/// What is compared is the document, not the path: a link resolving to a
/// path the plan vacates and refills — a delete and a create at one path —
/// names another document though its path is unchanged. Where a move carried
/// the named document away, [`left_behind`] advises first.
pub(crate) fn names_another(
    before: &Resolves,
    after: &Resolves,
    lineage: &Lineage,
    normalizer: &PathNormalizer,
) -> bool {
    let identity = |path: &DocumentPath| normalizer.normalize(Path::new(path.as_str())).ok();
    let Resolves::One { path } = before else {
        return false;
    };
    let Some(named) = identity(path) else {
        return false;
    };
    match after {
        Resolves::One { path } => {
            let Some(now) = identity(path) else {
                return false;
            };
            lineage.source(&now).map(|drawn| drawn.from) != Some(named)
        }
        Resolves::None {} | Resolves::Several {} => true,
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
    let requests: Vec<_> = lineage
        .removals()
        .filter_map(|removal| Some((removal.position, removal.rewrite_to()?)))
        .collect();
    if requests.is_empty() {
        return Ok(BTreeMap::new());
    }
    let addresses: Vec<_> = requests.iter().map(|(_, address)| *address).collect();
    let named = index.targets(overlay, &addresses, PlanSide::After)?;
    Ok(requests
        .into_iter()
        .map(|(position, _)| position)
        .zip(named)
        .collect())
}

/// The one document the target `named` names where the plan leaves the
/// vault, as the file the vault's rule reads and the path the store names:
/// what a rewriting delete's backlinks, or a wikilink rewrite's wikilinks,
/// are respelled toward. `None` where it names no one document. The
/// document-path grammar refuses every spelling the vault's rule refuses, so
/// a document it names always normalizes; the arm for one that does not is
/// the conservative answer, never a panic, and no target reaches it.
pub(crate) fn rewrite_destination(
    named: &TargetNaming,
    normalizer: &PathNormalizer,
) -> Option<(NormalizedPath, norn_store::DocumentPath)> {
    let Resolves::One { path } = &named.after else {
        return None;
    };
    let file = normalizer.normalize(Path::new(path.as_str())).ok()?;
    Some((file, stored_path(path)))
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
    if removal.rewrite_to().is_none() {
        return false;
    }
    let target = targets
        .get(&removal.position)
        .and_then(|target| rewrite_destination(target, normalizer));
    match (after, target) {
        (Resolves::One { path }, Some((target, _))) => {
            normalizer.normalize(Path::new(path.as_str())).ok().as_ref() != Some(&target)
        }
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

impl RetargetNaming {
    /// **The one rule a wikilink rewrite resolves by**: the one document its
    /// wikilinks are retargeted to, its ends naming as this says — the one
    /// document its `new` names where the plan leaves the vault, at a path a
    /// link can be respelled toward ([`rewrite_destination`]), where its
    /// `old` names one document or none before the plan, and the document
    /// `old` names is not the one `new` names where the plan leaves it
    /// ([`Lineage::landing`]). `None` otherwise: planning leaves such a
    /// rewrite unresolved, and it selects no wikilink ([`selecting`]).
    pub(crate) fn destination(
        &self,
        lineage: &Lineage,
        normalizer: &PathNormalizer,
    ) -> Option<(NormalizedPath, norn_store::DocumentPath)> {
        if matches!(self.old.before, Resolves::Several {}) {
            return None;
        }
        let (file, at) = rewrite_destination(&self.new, normalizer)?;
        (self.old_lands(lineage, normalizer).as_ref() != Some(&file)).then_some((file, at))
    }

    /// The file the one document `old` names before the plan stands at
    /// after it, where `old` names one and a file holds it then.
    pub(crate) fn old_lands(
        &self,
        lineage: &Lineage,
        normalizer: &PathNormalizer,
    ) -> Option<NormalizedPath> {
        let Resolves::One { path } = &self.old.before else {
            return None;
        };
        lineage.landing(&normalizer.normalize(Path::new(path.as_str())).ok()?)
    }
}

/// What the two ends of each wikilink rewrite of the plan name, by the
/// rewrite's position, on each side of the plan `overlay` describes, read
/// through `index`: the one reading of them planning resolves the rewrite by
/// and the change set judges its cascade by.
///
/// **`old` is read before the plan, `new` after it.** The wikilinks a rewrite
/// retargets are the ones naming `old`'s document as the vault stands before
/// the plan — the same side every link's own resolution is read from by
/// [`selecting`], as by a move's and a delete's rules — so the document
/// meant is the one the author saw, wherever the plan carries it, and the
/// plan's own documents cannot make it ambiguous. What they are retargeted to
/// must stand where the plan leaves the vault, as a delete's `rewrite_to`
/// must, so a `new` the plan creates is named.
pub(crate) fn retarget_namings<I: LinkIndex + ?Sized>(
    lineage: &Lineage,
    overlay: &PathOverlay,
    index: &I,
) -> Result<BTreeMap<usize, RetargetNaming>, I::Error> {
    let retargets: Vec<_> = lineage.retargets().collect();
    if retargets.is_empty() {
        return Ok(BTreeMap::new());
    }
    let old: Vec<_> = retargets
        .iter()
        .map(|retarget| retarget.old.as_str())
        .collect();
    let new: Vec<_> = retargets
        .iter()
        .map(|retarget| retarget.new.as_str())
        .collect();
    let old = index.targets(overlay, &old, PlanSide::Before)?;
    let new = index.targets(overlay, &new, PlanSide::After)?;
    Ok(retargets
        .into_iter()
        .zip(old)
        .zip(new)
        .map(|((retarget, old), new)| (retarget.position, RetargetNaming { old, new }))
        .collect())
}

/// `overlay`, reaching the wikilinks each wikilink rewrite of the plan could
/// retarget, its ends named as `namings` says: every link naming the one
/// document its `old` names before the plan, or, where `old` names none,
/// every link that could name a document standing at the place `old` spells
/// ([`norn_store::spelled_place`]), though none stands there. An `old`
/// naming several reaches nothing, as its rewrite does not resolve.
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
            Some(Resolves::One { path }) => overlay.reaching(stored_path(path)),
            Some(Resolves::None {}) => match norn_store::spelled_place(&retarget.old) {
                Some(place) => overlay.reaching(place),
                None => overlay,
            },
            _ => overlay,
        }
    })
}

/// Whether a cascade follows any place a link could name before the plan
/// ([`LinkChange::before_targets`]), remembered per list of places: a key's
/// list is one allocation for every link held under it in a judgment, so each
/// key's places are read once, however many links hold the key and however
/// the links of several keys interleave. What it keeps is one answer per
/// key whose list was read, each list held so no other takes its address.
#[derive(Default)]
struct AmongFollowed {
    read: BTreeMap<usize, (Arc<[norn_store::DocumentPath]>, bool)>,
}

impl AmongFollowed {
    /// Whether `followed` holds of a place one of `lists` names, each list
    /// read once over this memo's life.
    fn any(
        &mut self,
        lists: &[Arc<[norn_store::DocumentPath]>],
        followed: &impl Fn(&str) -> bool,
    ) -> bool {
        let mut among = false;
        for list in lists {
            let at = Arc::as_ptr(list).cast::<norn_store::DocumentPath>() as usize;
            let (_, follows) = self.read.entry(at).or_insert_with(|| {
                let follows = list.iter().any(|place| followed(place.as_str()));
                (Arc::clone(list), follows)
            });
            among |= *follows;
        }
        among
    }
}

/// What a link named before the plan, as [`decider`] reads it.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Named<'a> {
    /// Exactly the file at this path.
    One(&'a str),
    /// No document, the link broken as link health judges it; it could name
    /// a document standing at each of these places the plan reaches
    /// ([`LinkChange::before_targets`]).
    Broken(&'a norn_store::BeforeTargets),
    /// Several documents, or no document without the link breaking.
    Other,
}

impl<'a> Named<'a> {
    /// What `change`'s link named before the plan.
    pub(crate) fn of(change: &'a LinkChange) -> Self {
        match &change.before {
            Resolves::One { path } => Named::One(path.as_str()),
            Resolves::None {}
                if LinkHealth::of_address(change.address, 0) == LinkHealth::Broken =>
            {
                Named::Broken(&change.before_targets)
            }
            _ => Named::Other,
        }
    }
}

/// The operation of a plan that decides what a link names after it
/// ([`decider`]).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Decider<'l> {
    /// An authored link rewrite naming the link.
    Authored(&'l Relink),
    /// A wikilink rewrite whose `old` names the link.
    Retarget(&'l Retarget),
    /// A delete removing the document the link named.
    Removal(&'l Removal),
    /// The moves carrying the document the link named away: the file they
    /// land it at, and the last of them.
    Move {
        /// The file the document lands at.
        to: &'l NormalizedPath,
        /// The position of the move landing it there.
        owner: usize,
    },
}

impl Decider<'_> {
    /// The position of the operation whose cascade carries a rewrite of a
    /// link it decides.
    pub(crate) fn owner(&self) -> usize {
        match self {
            Decider::Authored(relink) => relink.position,
            Decider::Retarget(retarget) => retarget.position,
            Decider::Removal(removal) => removal.position,
            Decider::Move { owner, .. } => *owner,
        }
    }
}

/// Every wikilink rewrite of the plan whose `old` names `link`, which named
/// `named` before the plan, in operation position order, its ends named as
/// `namings` says: one whose `old` names exactly one document before the plan names a
/// wikilink resolving to exactly that document then, whatever its spelling;
/// one whose `old` names no document names a broken wikilink that would
/// resolve to a document standing at the place `old` spells
/// ([`norn_store::spelled_place`]), its keys read as the root reads them —
/// so `Old Note` names `[[Old Note]]`, `[[Old Note.md]]` and
/// `[[vault://Old Note]]`, `[[old note]]` too where the root folds ASCII
/// case, and `v1.2` never names `[[v1]]`. A Markdown link, and a wikilink resolving to several
/// documents, is named by none; and a rewrite with no document to retarget
/// to ([`RetargetNaming::destination`]), which planning leaves unresolved,
/// names none, so it never takes a wikilink from a rewrite that has one.
pub(crate) fn selecting<'l>(
    link: &LinkFact,
    named: Named<'_>,
    indexed: &Retargets<'l>,
    normalizer: &PathNormalizer,
) -> Vec<&'l Retarget> {
    if link.family != norn_store::LinkFamily::Wikilink {
        return Vec::new();
    }
    let mut selected = Vec::new();
    match named {
        Named::One(path) => {
            crate::evidence::count_rewrite_selection_step();
            if let Ok(file) = normalizer.normalize(Path::new(path)) {
                selected.extend(indexed.resolved.get(&file).into_iter().flatten().copied());
            }
        }
        Named::Broken(could) => {
            // Lists are shared by every link under a key in one judgment.
            // Retain their allocations so their pointer identities cannot be reused.
            let mut read = indexed.broken_lists.borrow_mut();
            for list in could.lists() {
                crate::evidence::count_rewrite_selection_step();
                let at = Arc::as_ptr(list).cast::<norn_store::DocumentPath>() as usize;
                let (_, matches) = read.entry(at).or_insert_with(|| {
                    let mut matches = BTreeMap::new();
                    for place in list.iter() {
                        crate::evidence::count_rewrite_selection_step();
                        if let Ok(file) = normalizer.normalize(Path::new(place.as_str())) {
                            for retarget in indexed.broken.get(&file).into_iter().flatten() {
                                matches.insert(retarget.position, *retarget);
                            }
                        }
                    }
                    (Arc::clone(list), matches.into_values().collect::<Vec<_>>())
                });
                selected.extend(matches.iter().copied());
            }
        }
        Named::Other => {}
    }
    for _ in &selected {
        crate::evidence::count_rewrite_selection_step();
    }
    // Dependencies can reverse composition order; link ownership still follows
    // the rewrites' authored positions.
    selected.sort_by_key(|retarget| retarget.position);
    selected.dedup_by_key(|retarget| retarget.position);
    selected
}

/// The eligible rewrites under the identity their `old` selects. Resolved
/// documents and empty places are separate keys: a broken link selects only
/// rewrites whose `old` names no document. Destinations are checked once per
/// rewrite, before any link is selected.
#[derive(Default)]
pub(crate) struct Retargets<'l> {
    resolved: BTreeMap<NormalizedPath, Vec<&'l Retarget>>,
    broken: BTreeMap<NormalizedPath, Vec<&'l Retarget>>,
    pub(crate) destinations: BTreeMap<usize, (NormalizedPath, norn_store::DocumentPath)>,
    broken_lists: std::cell::RefCell<BTreeMap<usize, SelectedList<'l>>>,
}

type SelectedList<'l> = (Arc<[norn_store::DocumentPath]>, Vec<&'l Retarget>);

impl<'l> Retargets<'l> {
    pub(crate) fn of(
        lineage: &'l Lineage,
        namings: &BTreeMap<usize, RetargetNaming>,
        normalizer: &PathNormalizer,
    ) -> Self {
        let mut indexed = Self::default();
        for retarget in lineage.retargets() {
            crate::evidence::count_rewrite_selection_step();
            let Some(named) = namings.get(&retarget.position) else {
                continue;
            };
            let Some(destination) = named.destination(lineage, normalizer) else {
                continue;
            };
            indexed.destinations.insert(retarget.position, destination);
            let (keys, place) = match &named.old.before {
                Resolves::One { path } => (&mut indexed.resolved, Some(stored_path(path))),
                Resolves::None {} => (
                    &mut indexed.broken,
                    norn_store::spelled_place(&retarget.old),
                ),
                _ => continue,
            };
            if let Some(file) =
                place.and_then(|path| normalizer.normalize(Path::new(path.as_str())).ok())
            {
                keys.entry(file).or_default().push(retarget);
            }
        }
        indexed
    }
}

/// **The one rule which operation of a plan decides what a link names after
/// it**, the link written as `link` in the document held at `holder` after
/// the plan, and naming `named` before it: an authored link rewrite naming
/// the link — its holder, its syntax and the address it is written with —
/// the first in composition order; else a wikilink rewrite whose `old` names it
/// ([`selecting`]), the first by operation position — each whatever a move or a
/// delete would do with it, since its author said what it names; else a
/// delete removing the one document it named; else the moves carrying that
/// document away. `None` where the plan leaves what it named where it stood.
///
/// A link cascade reads the document a link must name by this
/// (`Cascade::final_document`), and the change set which links a cascade
/// left as written ([`respells`]) and which links a rewrite writes are a
/// deleted document's backlinks still.
pub(crate) fn decider<'l>(
    holder: &norn_store::DocumentPath,
    link: &LinkFact,
    named: Named<'_>,
    lineage: &'l Lineage,
    indexed: &Retargets<'l>,
    normalizer: &PathNormalizer,
) -> Option<Decider<'l>> {
    let held = normalizer.normalize(Path::new(holder.as_str())).ok();
    if let Some(relink) = held
        .as_ref()
        .and_then(|held| lineage.relink(held, wire_family(link.family), &address(link)))
    {
        return Some(Decider::Authored(relink));
    }
    if let Some(&first) = selecting(link, named, indexed, normalizer).first() {
        return Some(Decider::Retarget(first));
    }
    let Named::One(path) = named else {
        return None;
    };
    let file = normalizer.normalize(Path::new(path)).ok()?;
    if let Some(removal) = lineage.removed_by(&file) {
        return Some(Decider::Removal(removal));
    }
    let (to, drawn) = lineage.carried_to(&file)?;
    Some(Decider::Move {
        to,
        owner: *drawn.moves.last()?,
    })
}

/// **Whether the operation `decider` decides a link by respells it**, the
/// link resolving to `before` before the plan and `after` after it: never an
/// authored link rewrite, which writes the link itself rather than through a
/// cascade; a wikilink rewrite, where the link does not already name the one document
/// its `new` names after the plan (`indexed`); a delete, by its own rule
/// ([`rewritten_for`], its target named as `rewritten_to` says); the moves,
/// by theirs ([`left_behind`]).
///
/// A cascade rewrites exactly the links this names; the change set records
/// every link it names and advises on each the cascade left as written.
pub(crate) fn respells(
    decider: Decider<'_>,
    before: &Resolves,
    after: &Resolves,
    lineage: &Lineage,
    rewritten_to: &BTreeMap<usize, TargetNaming>,
    indexed: &Retargets<'_>,
    normalizer: &PathNormalizer,
) -> bool {
    let identity = |path: &DocumentPath| normalizer.normalize(Path::new(path.as_str())).ok();
    let one = |resolves: &Resolves| match resolves {
        Resolves::One { path } => identity(path),
        _ => None,
    };
    match decider {
        Decider::Authored(_) => false,
        Decider::Retarget(retarget) => {
            let new = indexed
                .destinations
                .get(&retarget.position)
                .map(|(file, _)| file.clone());
            new.is_none() || one(after) != new
        }
        Decider::Removal(removal) => rewritten_for(removal, after, rewritten_to, normalizer),
        Decider::Move { .. } => left_behind(before, after, lineage, normalizer).is_some(),
    }
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
    /// not name its `new`; or it is a relative path its holder's move leaves
    /// naming another document: its cascade left it as written.
    left_behind: bool,
    /// The link was ambiguous before the plan, and one of the documents it
    /// could name is one a move of the plan carries away, a delete removes
    /// rewriting the links naming it, or a wikilink rewrite's `old` names.
    ambiguous_among_moved: bool,
    /// The link, which the plan does not write, named one document before
    /// the plan and names something else after it, neither that document nor
    /// that document where a move carries it ([`names_another`]).
    names_another: bool,
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
    /// made ambiguous; an ambiguous link the plan does not write — the
    /// key holding one, whatever else it holds — is retargeted where it is healthy after, or ambiguous after with members
    /// the plan moved — the last having no entry, since several on both
    /// sides is no change the set records; and a healthy link the plan does
    /// not write is retargeted where it names another document after
    /// ([`names_another`]), as a document appearing at or leaving its exact
    /// path makes it. A side link health does not judge
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
            (LinkHealth::Healthy, LinkHealth::Healthy) if self.names_another => {
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

/// Where `reason` ranks among the reasons one key's links can be left for,
/// the first ranked lowest: the one whose remedy reaches least is named. A
/// new address the link's syntax cannot carry there holds whatever is done
/// to the value around it, so it outranks a frontmatter value that cannot
/// hold it, which outranks a token with no place for any address, which
/// outranks a link with no place in the source, whose field's whole value
/// can still be set.
fn skip_rank(reason: RewriteSkip) -> u8 {
    match reason {
        RewriteSkip::Unrepresentable | RewriteSkip::ConflictingRewrites => 0,
        RewriteSkip::WouldCorruptFrontmatter => 1,
        RewriteSkip::LinkNotRewritable => 2,
        RewriteSkip::Unplaced => 3,
    }
}

/// The advisory on `link`, which the text layer left as written for
/// `reason`.
///
/// A link two rewrites of one batch name with two different targets cannot
/// carry one new address, and is unrepresentable: planning writes one
/// rewrite per holder, syntax and address, so only a plan no planning wrote
/// meets it.
pub(crate) fn skip_advisory(link: LinkKey, reason: RewriteSkip) -> LinkAdvisory {
    match reason {
        RewriteSkip::Unrepresentable | RewriteSkip::ConflictingRewrites => {
            LinkAdvisory::skipped_unrepresentable(link)
        }
        RewriteSkip::WouldCorruptFrontmatter => {
            LinkAdvisory::skipped_would_corrupt_frontmatter(link)
        }
        RewriteSkip::LinkNotRewritable => LinkAdvisory::skipped_not_rewritable(link),
        RewriteSkip::Unplaced => LinkAdvisory::skipped_unplaced(link),
    }
}

/// The links a plan writes: each a link of one syntax, at one address, in
/// one document's after-state, which a `rewrite_link` of the plan or a
/// rewrite of one of its cascades writes — with each address a rewrite
/// writing it there respelled it from.
///
/// Ordinary frontmatter sets and body edits record no resolution condition for
/// links they write. A target can disappear or gain a competitor before apply
/// without refusing that write. Repair's link-valued fixes record their own
/// address-resolution conditions.
#[derive(Default)]
pub(crate) struct WrittenLinks {
    rewritten: BTreeMap<(NormalizedPath, &'static str, String), BTreeSet<String>>,
    /// Heading fragments the plan writes, read by their syntax to match the forecast's facts.
    anchors: BTreeSet<(NormalizedPath, &'static str, String, String)>,
}

impl WrittenLinks {
    /// The links `operations` write: each `rewrite_link`'s and each
    /// cascade rewrite's document identity, syntax, and the address it
    /// writes, with the address it respells.
    pub(crate) fn of<'o>(
        operations: impl IntoIterator<Item = &'o Operation>,
        normalizer: &PathNormalizer,
    ) -> Self {
        let mut rewritten: BTreeMap<_, BTreeSet<String>> = BTreeMap::new();
        let mut anchors = BTreeSet::new();
        for operation in operations {
            if let OperationKind::RewriteAnchor {
                path,
                syntax,
                address,
                to,
                ..
            } = &operation.kind
                && let (Ok(file), Some(family)) = (
                    normalizer.normalize(Path::new(path.as_str())),
                    family_name(*syntax),
                )
            {
                let anchor = if *syntax == LinkFamily::Markdown {
                    norn_text::decode_markdown_fragment(to)
                } else {
                    to.clone()
                };
                anchors.insert((file, family, address.clone(), anchor));
            }
            let authored = match &operation.kind {
                OperationKind::RewriteLink {
                    path,
                    syntax,
                    from,
                    to,
                } => Some((path, *syntax, from, to)),
                _ => None,
            };
            let cascaded = operation
                .cascade
                .iter()
                .map(|rewrite| (&rewrite.path, rewrite.syntax, &rewrite.from, &rewrite.to));
            for (path, syntax, from, to) in authored.into_iter().chain(cascaded) {
                if let (Ok(file), Some(syntax)) = (
                    normalizer.normalize(Path::new(path.as_str())),
                    family_name(syntax),
                ) {
                    rewritten
                        .entry((file, syntax, to.clone()))
                        .or_default()
                        .insert(from.clone());
                }
            }
        }
        WrittenLinks { rewritten, anchors }
    }

    /// Whether the plan writes `link`'s address in `holder`'s after-state.
    /// An anchor rewrite keeps the address subject to the move cascade.
    pub(crate) fn holds(&self, holder: &NormalizedPath, link: &norn_store::LinkFact) -> bool {
        self.rewritten
            .contains_key(&(holder.clone(), link.family.as_str(), address(link)))
    }

    /// Whether the plan writes the heading fragment, recorded by the forecast
    /// without treating its address as authored for a cascade.
    fn holds_anchor(&self, holder: &NormalizedPath, link: &norn_store::LinkFact) -> bool {
        match &link.anchor {
            Some(norn_store::LinkAnchor::Heading { written, .. }) => self.anchors.contains(&(
                holder.clone(),
                link.family.as_str(),
                address(link),
                written.clone(),
            )),
            _ => false,
        }
    }

    /// `link`, which the plan writes in the document `holder`, as it was
    /// written before each rewrite writing it there respelled it: the link
    /// with the address that rewrite matched. A rewrite never changes a
    /// link's protocol, so an address under another protocol than the
    /// link's is none it was respelled from.
    fn originals<'w>(
        &'w self,
        holder: &NormalizedPath,
        link: &'w norn_store::LinkFact,
    ) -> impl Iterator<Item = norn_store::LinkFact> + 'w {
        self.rewritten
            .get(&(holder.clone(), link.family.as_str(), address(link)))
            .into_iter()
            .flatten()
            .filter_map(move |from| {
                let target = match &link.protocol {
                    Some(protocol) => from.strip_prefix(protocol.as_str())?.strip_prefix("://")?,
                    None => from.as_str(),
                };
                Some(spelled(link, target))
            })
    }
}

/// `link` written with the target `target`, its protocol kept and nothing
/// else of it: how a link is probed under another spelling of its address.
pub(crate) fn spelled(link: &norn_store::LinkFact, target: &str) -> norn_store::LinkFact {
    norn_store::LinkFact {
        field: None,
        family: link.family,
        embed: false,
        protocol: link.protocol.clone(),
        target: target.to_string(),
        title: None,
        anchor: None,
        span: None,
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

/// `path` as the store names it.
pub(crate) fn stored_path(path: &DocumentPath) -> norn_store::DocumentPath {
    norn_store::DocumentPath::from(path)
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
    use norn_wire::{DocumentPath, LinkKey, VaultAddress, VaultName};

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

        fn held_links(
            &self,
            holder: &norn_wire::DocumentPath,
        ) -> Result<Option<norn_store::HeldLinks>, E> {
            panic!(
                "a plan that moves no document's presence and writes no link read the links `{}` holds",
                holder.as_str()
            )
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

        fn held_links(
            &self,
            holder: &norn_wire::DocumentPath,
        ) -> Result<Option<norn_store::HeldLinks>, E> {
            self.0
                .index()
                .held_links(holder)
                .map_err(|refused| panic!("an empty store's index refused: {refused:?}"))
        }
    }

    impl<E> crate::planner::repair::Following for (EmptyStore, std::marker::PhantomData<E>) {
        fn tombstones(&self, _: &LinkKey) -> Result<Vec<norn_store::MovedTombstone>, Self::Error> {
            Ok(Vec::new())
        }
        fn candidate_head(
            &self,
            _: &[DocumentPath],
            total: u64,
        ) -> Result<norn_wire::CandidateHead, Self::Error> {
            Ok(norn_wire::CandidateHead::new([], total).expect("an empty head"))
        }
        fn similar_stems(&self, stem: &str) -> Result<norn_store::SimilarStems, Self::Error> {
            Ok(norn_store::SimilarStems {
                matches: norn_store::StemMatches::Candidates(Vec::new()),
                query_trigrams: norn_store::similarity::trigrams(stem).len(),
                work: norn_store::SimilarStemsWork::default(),
            })
        }
        fn live_holders(&self, _: &str) -> Result<Vec<DocumentPath>, Self::Error> {
            Ok(Vec::new())
        }
    }

    /// The empty store's index, answering the links each of `files` holds as
    /// an index that took every one of them in would: what a planning case
    /// over files in memory vouches for a carried document by, reading none
    /// of the vault's files to answer.
    pub(crate) struct IndexedFiles {
        store: EmptyStore,
        held: std::collections::BTreeMap<String, norn_store::HeldLinks>,
    }

    impl IndexedFiles {
        pub(crate) fn of(files: &[(&str, &str)]) -> Self {
            IndexedFiles {
                store: EmptyStore::new(),
                held: files
                    .iter()
                    .map(|(at, content)| {
                        (
                            at.to_string(),
                            norn_store::HeldLinks {
                                content_hash: norn_fs::ContentHash::of(content.as_bytes()).to_hex(),
                                links: crate::derivation::document_links(content.as_bytes()),
                            },
                        )
                    })
                    .collect(),
            }
        }
    }

    impl LinkIndex for IndexedFiles {
        type Error = std::convert::Infallible;

        fn changes(
            &self,
            overlay: &norn_store::PathOverlay,
            probed: &[norn_store::ProbedLink],
            each: &mut dyn FnMut(norn_store::LinkChange),
        ) -> Result<(), Self::Error> {
            self.store
                .index()
                .changes(overlay, probed, each)
                .map_err(|refused| panic!("an empty store's index refused: {refused:?}"))
        }

        fn target(
            &self,
            overlay: &norn_store::PathOverlay,
            address: &str,
            headed: norn_store::PlanSide,
        ) -> Result<norn_store::TargetNaming, Self::Error> {
            self.store
                .index()
                .target(overlay, address, headed)
                .map_err(|refused| panic!("an empty store's index refused: {refused:?}"))
        }

        fn held_links(
            &self,
            holder: &norn_wire::DocumentPath,
        ) -> Result<Option<norn_store::HeldLinks>, Self::Error> {
            Ok(self.held.get(holder.as_str()).cloned())
        }
    }

    /// A store derived from the tree at `root`, pinning no schema, and a
    /// snapshot over it: the index a planning case over a tree on disk
    /// judges its links on, holding every document the tree holds as the
    /// vault's own index would — what a move's carried document is vouched
    /// for by.
    pub(crate) struct TreeStore {
        snapshot: Snapshot,
        declared: ContentModel,
        _store: Store,
        _scratch: Scratch,
    }

    impl TreeStore {
        pub(crate) fn over(root: &std::path::Path) -> Self {
            let scratch = Scratch::new("planner-links-tree");
            let mut store = Store::open_throwaway(
                scratch.join("store.sqlite3"),
                StoredPathOrder::Sensitive,
                crate::DERIVATION_VERSION,
            )
            .expect("a throwaway store");
            crate::production::heal_from_zero(&mut store, root, &[]).expect("a heal");
            TreeStore {
                snapshot: snapshot_of(&store),
                declared: ContentModel::none(),
                _store: store,
                _scratch: scratch,
            }
        }

        /// The index over the derived store.
        pub(crate) fn index(&self) -> PlanSnapshot<'_> {
            PlanSnapshot::held(vault(), &self.snapshot, &self.declared)
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
        AuthoredPlan, DocumentPath, FileState, LinkAddressKind, LinkAdvisory, LinkFamily, LinkKey,
        Operation, OperationKind, PlanCondition, Resolves, RootIdentity,
    };

    use super::testing::{EmptyStore, Untouched, vault};
    use super::{AmongFollowed, ChangeSet, Judged, Target, change_set};
    use crate::planner::compose::content_hash;
    use crate::planner::lineage::Lineage;
    use crate::planner::resolve::resolve;
    use crate::planner::view::memory::MemoryVault;

    fn path(text: &str) -> DocumentPath {
        DocumentPath::new(text).expect("a legal document path")
    }

    /// Every link reads only its own authored rewrite key as the number of
    /// holders and unrelated authored rewrites doubles.
    #[test]
    fn authored_rewrite_selection_work_grows_with_links() {
        fn run(size: usize) -> u64 {
            let normalizer = PathNormalizer::for_sensitivity(CaseSensitivity::Sensitive);
            let operations: Vec<_> = (0..size)
                .map(|at| {
                    Operation::new(OperationKind::rewrite_link(
                        path(&format!("h{at}.md")),
                        LinkFamily::Wikilink,
                        "old",
                        "new",
                    ))
                })
                .collect();
            let account = std::sync::Arc::new(crate::evidence::JobEvidence::default());
            {
                let _attribution = account.attributing();
                let lineage = Lineage::of(&operations, &(0..size).collect::<Vec<_>>(), &normalizer);
                for at in 0..size {
                    let holder = super::stored_path(&path(&format!("h{at}.md")));
                    for text in ["[[old]]", "[[unrelated]]"] {
                        let link = crate::derivation::document_links(text.as_bytes()).remove(0);
                        let owner = super::decider(
                            &holder,
                            &link,
                            super::Named::Other,
                            &lineage,
                            &Default::default(),
                            &normalizer,
                        )
                        .map(|by| by.owner());
                        assert_eq!(owner, (text == "[[old]]").then_some(at));
                    }
                }
            }
            account.read().rewrite_selection_steps
        }
        let (small, large) = (run(16), run(32));
        assert!(small > 0, "selection must move the instrument");
        assert!(
            large <= 2 * small,
            "doubling links and rewrites: {small} -> {large}"
        );
    }

    /// A link resolving to one document reads only rewrites of that
    /// document, including the cost of checking their destinations once.
    #[test]
    fn retarget_selection_work_grows_with_links() {
        fn run(size: usize) -> u64 {
            use norn_store::TargetNaming;
            use norn_wire::ResolutionTarget;
            let normalizer = PathNormalizer::for_sensitivity(CaseSensitivity::Sensitive);
            let target = |text: String| ResolutionTarget::new(text).expect("a target");
            let operations: Vec<_> = (0..size)
                .map(|at| {
                    Operation::new(OperationKind::rewrite_wikilink(
                        target(format!("old{at}")),
                        target("new".into()),
                    ))
                })
                .collect();
            let lineage = Lineage::of(&operations, &(0..size).collect::<Vec<_>>(), &normalizer);
            let namings = (0..size)
                .map(|at| {
                    (
                        at,
                        super::RetargetNaming {
                            old: TargetNaming::new(
                                Resolves::one(path(&format!("old{at}.md"))),
                                Resolves::none(),
                                None,
                            ),
                            new: TargetNaming::new(
                                Resolves::none(),
                                Resolves::one(path("new.md")),
                                None,
                            ),
                        },
                    )
                })
                .collect();
            let account = std::sync::Arc::new(crate::evidence::JobEvidence::default());
            {
                let _attribution = account.attributing();
                let indexed = super::Retargets::of(&lineage, &namings, &normalizer);
                let link = crate::derivation::document_links(b"[[old]]").remove(0);
                for at in 0..size {
                    let name = format!("old{at}.md");
                    let selected =
                        super::selecting(&link, super::Named::One(&name), &indexed, &normalizer);
                    assert_eq!(
                        selected
                            .iter()
                            .map(|rewrite| rewrite.position)
                            .collect::<Vec<_>>(),
                        [at]
                    );
                }
            }
            account.read().rewrite_selection_steps
        }
        let (small, large) = (run(16), run(32));
        assert!(small > 0);
        assert!(
            large <= 2 * small,
            "doubling links and retargets: {small} -> {large}"
        );
    }

    /// Broken links read their reached places once per shared key, rather
    /// than testing every rewrite against every possible place.
    #[test]
    fn broken_retarget_selection_work_grows_with_rewrites_and_reached_places() {
        fn run(size: usize) -> u64 {
            use norn_store::{PathOverlay, ProbedLink, TargetNaming};
            use norn_wire::ResolutionTarget;
            let normalizer = PathNormalizer::for_sensitivity(CaseSensitivity::Sensitive);
            let operations: Vec<_> = (0..size)
                .map(|at| {
                    Operation::new(OperationKind::rewrite_wikilink(
                        ResolutionTarget::new(format!("area/{at}/lost")).unwrap(),
                        ResolutionTarget::new("new").unwrap(),
                    ))
                })
                .collect();
            let lineage = Lineage::of(
                &operations,
                &(0..size).rev().collect::<Vec<_>>(),
                &normalizer,
            );
            let namings = (0..size)
                .map(|at| {
                    (
                        at,
                        super::RetargetNaming {
                            old: TargetNaming::new(Resolves::none(), Resolves::none(), None),
                            new: TargetNaming::new(
                                Resolves::none(),
                                Resolves::one(path("new.md")),
                                None,
                            ),
                        },
                    )
                })
                .collect();
            let overlay = (0..size).fold(PathOverlay::new(), |overlay, at| {
                overlay.reaching(super::stored_path(&path(&format!("area/{at}/lost.md"))))
            });
            let store = EmptyStore::new();
            let index = store.index();
            let link = crate::derivation::document_links(b"[[lost]]").remove(0);
            let mut could = None;
            super::LinkIndex::changes(
                &index,
                &overlay,
                &[ProbedLink {
                    before_holder: super::stored_path(&path("h.md")),
                    after_holder: super::stored_path(&path("h.md")),
                    link: link.clone(),
                    written: true,
                }],
                &mut |change| {
                    assert_eq!(change.before, Resolves::none());
                    could = Some(change.before_targets);
                },
            )
            .expect("the empty store answers");
            let could = could.expect("one broken link is judged");
            assert_eq!(could.iter().count(), size);
            let account = std::sync::Arc::new(crate::evidence::JobEvidence::default());
            {
                let _attribution = account.attributing();
                let indexed = super::Retargets::of(&lineage, &namings, &normalizer);
                for _ in 0..3 {
                    let selected = super::selecting(
                        &link,
                        super::Named::Broken(&could),
                        &indexed,
                        &normalizer,
                    );
                    assert_eq!(
                        selected
                            .iter()
                            .map(|rewrite| rewrite.position)
                            .collect::<Vec<_>>(),
                        (0..size).collect::<Vec<_>>()
                    );
                }
            }
            account.read().rewrite_selection_steps
        }
        let (small, large) = (run(16), run(32));
        assert!(small > 0);
        assert!(
            large <= 2 * small,
            "doubling broken rewrite keys and places: {small} -> {large}"
        );
    }

    /// Many broken links under one key reuse the key's candidate places,
    /// including places that match no rewrite.
    #[test]
    fn broken_links_share_candidate_selection_work_when_most_places_match_no_rewrite() {
        fn run(size: usize) -> u64 {
            use norn_store::{PathOverlay, ProbedLink, TargetNaming};
            use norn_wire::ResolutionTarget;
            let normalizer = PathNormalizer::for_sensitivity(CaseSensitivity::Sensitive);
            let operations = [Operation::new(OperationKind::rewrite_wikilink(
                ResolutionTarget::new("area/0/lost").unwrap(),
                ResolutionTarget::new("new").unwrap(),
            ))];
            let lineage = Lineage::of(&operations, &[0], &normalizer);
            let namings = [(
                0,
                super::RetargetNaming {
                    old: TargetNaming::new(Resolves::none(), Resolves::none(), None),
                    new: TargetNaming::new(Resolves::none(), Resolves::one(path("new.md")), None),
                },
            )]
            .into_iter()
            .collect();
            let overlay = (0..size).fold(PathOverlay::new(), |overlay, at| {
                overlay.reaching(super::stored_path(&path(&format!("area/{at}/lost.md"))))
            });
            let link = crate::derivation::document_links(b"[[lost]]").remove(0);
            let probes: Vec<_> = (0..size)
                .map(|at| {
                    let holder = super::stored_path(&path(&format!("h{at}.md")));
                    ProbedLink {
                        before_holder: holder.clone(),
                        after_holder: holder,
                        link: link.clone(),
                        written: true,
                    }
                })
                .collect();
            let store = EmptyStore::new();
            let mut changes = Vec::new();
            super::LinkIndex::changes(&store.index(), &overlay, &probes, &mut |change| {
                changes.push(change)
            })
            .unwrap();
            assert_eq!(changes.len(), size);
            let shared = changes[0]
                .before_targets
                .lists()
                .first()
                .expect("one shared candidate list");
            for change in &changes {
                assert_eq!(change.before, Resolves::none());
                assert_eq!(change.before_targets.iter().count(), size);
                assert!(std::sync::Arc::ptr_eq(
                    shared,
                    &change.before_targets.lists()[0]
                ));
            }
            let account = std::sync::Arc::new(crate::evidence::JobEvidence::default());
            {
                let _attribution = account.attributing();
                let indexed = super::Retargets::of(&lineage, &namings, &normalizer);
                for change in &changes {
                    let selected = super::selecting(
                        &change.link,
                        super::Named::of(change),
                        &indexed,
                        &normalizer,
                    );
                    assert_eq!(
                        selected
                            .iter()
                            .map(|rewrite| rewrite.position)
                            .collect::<Vec<_>>(),
                        [0]
                    );
                }
            }
            account.read().rewrite_selection_steps
        }
        let (small, large) = (run(16), run(32));
        assert!(small > 0);
        assert!(
            large <= 2 * small,
            "doubling broken links and shared candidate places: {small} -> {large}"
        );
    }

    /// Dependencies can reverse composition order without changing which
    /// retarget owns a link: the earliest operation position wins.
    #[test]
    fn retarget_precedence_follows_position_when_composition_order_is_reversed() {
        use norn_store::TargetNaming;
        use norn_wire::ResolutionTarget;
        let normalizer = PathNormalizer::for_sensitivity(CaseSensitivity::Sensitive);
        let operations: Vec<_> = ["a", "x/a"]
            .into_iter()
            .map(|old| {
                Operation::new(OperationKind::rewrite_wikilink(
                    ResolutionTarget::new(old).unwrap(),
                    ResolutionTarget::new("new").unwrap(),
                ))
            })
            .collect();
        let lineage = Lineage::of(&operations, &[1, 0], &normalizer);
        let namings = (0..2)
            .map(|at| {
                (
                    at,
                    super::RetargetNaming {
                        old: TargetNaming::new(
                            Resolves::one(path("x/a.md")),
                            Resolves::none(),
                            None,
                        ),
                        new: TargetNaming::new(
                            Resolves::none(),
                            Resolves::one(path("new.md")),
                            None,
                        ),
                    },
                )
            })
            .collect();
        let indexed = super::Retargets::of(&lineage, &namings, &normalizer);
        let link = crate::derivation::document_links(b"[[a]]").remove(0);
        let selected = super::selecting(&link, super::Named::One("x/a.md"), &indexed, &normalizer);
        assert_eq!(
            selected
                .iter()
                .map(|rewrite| rewrite.position)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        let by = super::decider(
            &super::stored_path(&path("h.md")),
            &link,
            super::Named::One("x/a.md"),
            &lineage,
            &indexed,
            &normalizer,
        )
        .expect("a retarget decides the link");
        assert_eq!(by.owner(), 0);
    }

    /// An earlier rewrite without a destination cannot take ownership from
    /// a later rewrite that can retarget the link.
    #[test]
    fn an_invalid_destination_does_not_precede_a_valid_retarget_of_the_same_link() {
        use norn_store::TargetNaming;
        use norn_wire::ResolutionTarget;
        let normalizer = PathNormalizer::for_sensitivity(CaseSensitivity::Sensitive);
        let operations: Vec<_> = ["gone", "new"]
            .into_iter()
            .map(|new| {
                Operation::new(OperationKind::rewrite_wikilink(
                    ResolutionTarget::new("a").unwrap(),
                    ResolutionTarget::new(new).unwrap(),
                ))
            })
            .collect();
        let lineage = Lineage::of(&operations, &[0, 1], &normalizer);
        let namings = [
            (
                0,
                super::RetargetNaming {
                    old: TargetNaming::new(Resolves::one(path("a.md")), Resolves::none(), None),
                    new: TargetNaming::new(Resolves::none(), Resolves::none(), None),
                },
            ),
            (
                1,
                super::RetargetNaming {
                    old: TargetNaming::new(Resolves::one(path("a.md")), Resolves::none(), None),
                    new: TargetNaming::new(Resolves::none(), Resolves::one(path("new.md")), None),
                },
            ),
        ]
        .into_iter()
        .collect();
        let indexed = super::Retargets::of(&lineage, &namings, &normalizer);
        let link = crate::derivation::document_links(b"[[a]]").remove(0);
        let selected = super::selecting(&link, super::Named::One("a.md"), &indexed, &normalizer);
        assert_eq!(
            selected
                .iter()
                .map(|rewrite| rewrite.position)
                .collect::<Vec<_>>(),
            [1]
        );
        let by = super::decider(
            &super::stored_path(&path("h.md")),
            &link,
            super::Named::One("a.md"),
            &lineage,
            &indexed,
            &normalizer,
        )
        .expect("the valid retarget decides the link");
        assert_eq!(by.owner(), 1);
    }

    #[test]
    fn authored_rewrite_precedence_follows_composition_order() {
        let normalizer = PathNormalizer::for_sensitivity(CaseSensitivity::Insensitive);
        let operations = [
            Operation::new(OperationKind::rewrite_link(
                path("H.md"),
                LinkFamily::Wikilink,
                "old",
                "one",
            )),
            Operation::new(OperationKind::rewrite_link(
                path("h.md"),
                LinkFamily::Wikilink,
                "old",
                "two",
            )),
        ];
        let lineage = Lineage::of(&operations, &[1, 0], &normalizer);
        let holder = super::stored_path(&path("h.md"));
        let link = crate::derivation::document_links(b"[[old]]").remove(0);
        let by = super::decider(
            &holder,
            &link,
            super::Named::One("old.md"),
            &lineage,
            &Default::default(),
            &normalizer,
        )
        .expect("an authored rewrite");
        assert!(matches!(by, super::Decider::Authored(_)));
        assert_eq!(by.owner(), 1);
    }

    /// Rewriting deletes share a judgment over the overlay, so doubling
    /// deletes and overlay targets doubles the construction work.
    #[test]
    fn rewriting_delete_namings_index_the_overlay_once() {
        fn run(size: usize) -> u64 {
            use norn_store::PathOverlay;
            use norn_wire::ResolutionTarget;
            let normalizer = PathNormalizer::for_sensitivity(CaseSensitivity::Sensitive);
            let operations: Vec<_> = (0..size)
                .map(|at| {
                    Operation::new(OperationKind::delete_document_rewriting(
                        path(&format!("old{at}.md")),
                        ResolutionTarget::new("new").unwrap(),
                    ))
                })
                .collect();
            let lineage = Lineage::of(&operations, &(0..size).collect::<Vec<_>>(), &normalizer);
            let overlay = (0..size).fold(
                PathOverlay::new().with(super::stored_path(&path("new.md")), false, true),
                |overlay, at| {
                    overlay.with(
                        super::stored_path(&path(&format!("old{at}.md"))),
                        true,
                        false,
                    )
                },
            );
            let store = EmptyStore::new();
            let index = store.index();
            let named = super::rewrite_targets(&lineage, &overlay, &index).unwrap();
            assert_eq!(named.len(), size);
            for naming in named.values() {
                assert_eq!(naming.before, Resolves::none());
                assert_eq!(naming.after, Resolves::one(path("new.md")));
                assert!(naming.candidates.is_none());
            }
            index.link_judgment_cost().overlay_targets_indexed
        }
        let (small, large) = (run(16), run(32));
        assert!(small > 0);
        assert!(
            large <= 2 * small,
            "doubling deletes and overlay targets: {small} -> {large}"
        );
    }

    /// Retarget ends share two judgments. The old end's head belongs to
    /// Before and the new end's to After even when the other side is ambiguous.
    #[test]
    fn retarget_namings_index_the_overlay_twice_with_heads_on_the_correct_sides() {
        fn run(size: usize) -> u64 {
            use norn_store::PathOverlay;
            use norn_wire::ResolutionTarget;
            let normalizer = PathNormalizer::for_sensitivity(CaseSensitivity::Sensitive);
            let operations: Vec<_> = (0..size)
                .map(|at| {
                    Operation::new(OperationKind::rewrite_wikilink(
                        ResolutionTarget::new(format!("old{at}")).unwrap(),
                        ResolutionTarget::new(format!("new{at}")).unwrap(),
                    ))
                })
                .collect();
            let lineage = Lineage::of(&operations, &(0..size).collect::<Vec<_>>(), &normalizer);
            let overlay = (0..size).fold(PathOverlay::new(), |overlay, at| {
                [
                    (format!("before/old{at}.md"), true, false),
                    (format!("after-a/old{at}.md"), false, true),
                    (format!("after-b/old{at}.md"), false, true),
                    (format!("before-a/new{at}.md"), true, false),
                    (format!("before-b/new{at}.md"), true, false),
                    (format!("after/new{at}.md"), false, true),
                ]
                .into_iter()
                .fold(overlay, |overlay, (at, before, after)| {
                    overlay.with(super::stored_path(&path(&at)), before, after)
                })
            });
            let store = EmptyStore::new();
            let index = store.index();
            let named = super::retarget_namings(&lineage, &overlay, &index).unwrap();
            assert_eq!(named.len(), size);
            for (at, ends) in named {
                assert_eq!(
                    ends.old.before,
                    Resolves::one(path(&format!("before/old{at}.md")))
                );
                assert!(matches!(ends.old.after, Resolves::Several {}));
                assert!(ends.old.candidates.is_none(), "old is unique on Before");
                assert!(matches!(ends.new.before, Resolves::Several {}));
                assert_eq!(
                    ends.new.after,
                    Resolves::one(path(&format!("after/new{at}.md")))
                );
                assert!(ends.new.candidates.is_none(), "new is unique on After");
            }
            index.link_judgment_cost().overlay_targets_indexed
        }
        let (small, large) = (run(16), run(32));
        assert!(small > 0);
        assert!(
            large <= 2 * small,
            "doubling retargets and overlay targets: {small} -> {large}"
        );
    }

    /// An encoded Markdown rewrite is recognized by the anchor the store derives.
    #[test]
    fn a_written_markdown_anchor_is_held_by_its_percent_decoded_replacement() {
        let normalizer = PathNormalizer::for_sensitivity(CaseSensitivity::Sensitive);
        let holder = normalizer
            .normalize(std::path::Path::new("h.md"))
            .expect("a document identity");
        let operation = Operation::new(OperationKind::rewrite_anchor(
            path("h.md"),
            LinkFamily::Markdown,
            "t.md",
            "a%20b",
            "c%20d",
        ));
        let written = super::WrittenLinks::of([&operation], &normalizer);
        let links = crate::derivation::document_links(b"[x](t.md#c%20d)");
        let [link] = links.as_slice() else {
            panic!("one written link");
        };
        assert!(matches!(
            &link.anchor,
            Some(norn_store::LinkAnchor::Heading { written, .. }) if written == "c d"
        ));
        assert!(written.holds_anchor(&holder, link));
    }

    /// **Whether a cascade follows a place an ambiguous link could name is
    /// read once per key's list of places**, however the links of two keys
    /// interleave: links alternating between a stem's list and another's,
    /// and one link holding both, read each list's places once.
    #[test]
    fn a_keys_places_are_read_once_however_links_of_two_keys_interleave() {
        let list = |stem: &str| -> std::sync::Arc<[norn_store::DocumentPath]> {
            (0..10)
                .map(|at| {
                    norn_store::DocumentPath::new(&format!("a/{at:02}/{stem}.md"))
                        .expect("a legal document path")
                })
                .collect()
        };
        let (t, u) = (list("t"), list("u"));
        let read = std::cell::Cell::new(0usize);
        let followed = |place: &str| {
            read.set(read.get() + 1);
            place == "a/09/u.md"
        };
        let mut among = AmongFollowed::default();

        let mut answers = Vec::new();
        for _ in 0..50 {
            answers.push(among.any(std::slice::from_ref(&t), &followed));
            answers.push(among.any(std::slice::from_ref(&u), &followed));
        }
        answers.push(among.any(&[t.clone(), u.clone()], &followed));

        assert_eq!(read.get(), 20, "each list's places, read once");
        assert!(answers.iter().step_by(2).take(50).all(|among| !among));
        assert!(answers.iter().skip(1).step_by(2).all(|among| *among));
        assert_eq!(answers.last(), Some(&true), "a link holding both keys");
    }

    fn key(holder: &str, address: &str) -> LinkKey {
        LinkKey::new(path(holder), LinkFamily::Wikilink, address)
    }

    /// The state of a file holding the document `bytes`.
    fn present(bytes: &[u8]) -> FileState {
        FileState::present(content_hash(bytes))
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

    /// **A plan holding a delete reads the link index, though every
    /// presence stays as it was.** Deleting `a.md` and creating another
    /// document there replaces the document a link naming the path follows,
    /// so the job takes a read handle for it as the change set reads one; an
    /// edit in place reads none.
    #[test]
    fn a_delete_whose_path_the_plan_refills_reads_the_link_index() {
        // A document stands at `a.md` before the plan and after it.
        let refilled = [(true, true)];
        let deleted = Operation::new(OperationKind::delete_document(path("a.md")));
        let created = Operation::new(OperationKind::create_document(path("a.md"), "New\n"));
        assert!(super::reads_links_over(refilled, &[deleted, created]));
        let edited = Operation::new(OperationKind::str_replace(path("a.md"), "Old", "New"));
        assert!(!super::reads_links_over(refilled, &[edited]));
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
        let (before, after): (&[u8], &[u8]) =
            (b"[[old]] and [[other]]\n", b"[[a]] and [[other]]\n");
        let links = EmptyStore::new();
        let set = change_set(
            &[Target::new(
                &holder,
                &FileState::present(crate::planner::compose::content_hash(before)),
                &FileState::present(crate::planner::compose::content_hash(after)),
                Some(crate::planner::links::Holding::Bytes(after)),
            )],
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
                unkept: Vec::new(),
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
                Target::new(&a, &present(b"A\n"), &FileState::absent(), None),
                Target::new(
                    &b,
                    &FileState::absent(),
                    &present(b"A\n"),
                    Some(crate::planner::links::Holding::Bytes(b"A\n")),
                ),
                Target::new(&h, &present(b"[[a]]\n"), &FileState::absent(), None),
                Target::new(
                    &moved_h,
                    &FileState::absent(),
                    &present(b"[[b]]\n"),
                    Some(crate::planner::links::Holding::Bytes(b"[[b]]\n")),
                ),
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
                unkept: Vec::new(),
            }
        );
        assert!(super::reads_links_over([(true, true)], &operations));
    }

    /// Bytes that do not decode as a vault document: a quarantined file.
    const UNDECODABLE: &[u8] = b"\xff\xfe not utf-8\n";

    /// The plan `operations` makes over `files` with the linker `l.md` —
    /// holding `[[q]]` and `[[d]]` — edited, so its links are read from the
    /// bytes planning composed: an empty store holds no link of its own.
    fn planned_with_the_linker(
        files: &[(&str, &[u8])],
        mut operations: Vec<Operation>,
    ) -> crate::planner::resolve::Resolution {
        let mut held = vec![("l.md", &b"See [[q]] and [[d]].\n"[..])];
        held.extend_from_slice(files);
        operations.push(Operation::new(OperationKind::str_replace(
            path("l.md"),
            "See",
            "Read",
        )));
        let resolution = planned(
            &MemoryVault::with_bytes(&held),
            operations,
            &EmptyStore::new(),
        );
        assert!(
            resolution.unresolved.is_empty(),
            "{:?}",
            resolution.unresolved
        );
        resolution
    }

    /// The transition the plan carries at `at`.
    fn transition_at<'p>(
        resolution: &'p crate::planner::resolve::Resolution,
        at: &str,
    ) -> &'p norn_wire::Transition {
        resolution
            .plan
            .transitions
            .iter()
            .find(|transition| transition.path.as_str() == at)
            .unwrap_or_else(|| panic!("the plan writes {at}"))
    }

    /// **A quarantined file is no link's candidate, so deleting one changes
    /// no link's resolution.** `[[q]]` names nothing before the plan, since
    /// the bytes at `q.md` do not decode, and nothing after it: the plan
    /// records the file it removes as quarantined, and no entry and no
    /// advisory.
    #[test]
    fn deleting_a_quarantined_document_records_no_link_change() {
        let resolution = planned_with_the_linker(
            &[("q.md", UNDECODABLE)],
            vec![Operation::new(OperationKind::delete_document(path("q.md")))],
        );
        assert_eq!(
            transition_at(&resolution, "q.md").before,
            FileState::quarantined(content_hash(UNDECODABLE))
        );
        assert_eq!(entries(&resolution.plan.conditions), []);
        assert!(resolution.forecast.links.is_empty());
    }

    /// **Moving a quarantined file retargets no link**: it is a document on
    /// neither side, at its source or at its destination, which the plan
    /// records as quarantined too.
    #[test]
    fn moving_a_quarantined_document_records_no_link_change() {
        let resolution = planned_with_the_linker(
            &[("q.md", UNDECODABLE)],
            vec![Operation::new(OperationKind::move_document(
                path("q.md"),
                path("elsewhere/q.md"),
            ))],
        );
        assert_eq!(
            transition_at(&resolution, "elsewhere/q.md").after,
            FileState::quarantined(content_hash(UNDECODABLE))
        );
        assert_eq!(entries(&resolution.plan.conditions), []);
        assert!(resolution.forecast.links.is_empty());
    }

    /// **Bytes that come to decode put a document where none stood**, though
    /// a file stood there throughout: replacing the quarantined `q.md` with a
    /// document mends `[[q]]`, from none to `q.md`.
    #[test]
    fn a_quarantined_file_replaced_by_a_document_mends_the_links_naming_it() {
        let resolution = planned_with_the_linker(
            &[("q.md", UNDECODABLE), ("n.md", b"now a document\n")],
            vec![
                Operation::new(OperationKind::delete_document(path("q.md"))),
                Operation::new(OperationKind::move_document(path("n.md"), path("q.md"))),
            ],
        );
        let replaced = transition_at(&resolution, "q.md");
        assert!(!replaced.before.is_document() && replaced.after.is_document());
        assert_eq!(
            entries(&resolution.plan.conditions),
            [PlanCondition::link_resolution(
                key("l.md", "q"),
                Resolves::none(),
                Resolves::one(path("q.md")),
            )]
        );
        assert!(resolution.forecast.links.is_empty());
    }

    /// **Bytes that stop decoding take a document away**, though a file
    /// stands there throughout: replacing the document `d.md` with
    /// quarantined bytes, by a delete leaving the links naming it broken,
    /// leaves `[[d]]` broken.
    #[test]
    fn a_document_replaced_by_quarantined_bytes_leaves_the_links_naming_it_broken() {
        let resolution = planned_with_the_linker(
            &[("d.md", b"a document\n"), ("q.md", UNDECODABLE)],
            vec![
                Operation::new(OperationKind::delete_document_breaking_links(path("d.md"))),
                Operation::new(OperationKind::move_document(path("q.md"), path("d.md"))),
            ],
        );
        let replaced = transition_at(&resolution, "d.md");
        assert!(replaced.before.is_document() && !replaced.after.is_document());
        assert_eq!(
            entries(&resolution.plan.conditions),
            [PlanCondition::link_resolution(
                key("l.md", "d"),
                Resolves::one(path("d.md")),
                Resolves::none(),
            )]
        );
        assert_eq!(
            resolution.forecast.links,
            [LinkAdvisory::left_broken(key("l.md", "d"))]
        );
    }

    /// **The fast path reads a change of whether a document stands, not of
    /// whether a file does**: a transition between quarantined bytes and a
    /// document reads the index, and one between two quarantined states, or
    /// from quarantined bytes to nothing, does not.
    #[test]
    fn the_fast_path_reads_whether_a_document_stands() {
        let quarantined = FileState::quarantined(content_hash(UNDECODABLE));
        let document = FileState::present(content_hash(b"text\n"));
        let reads = |before: &FileState, after: &FileState| {
            super::reads_links_over([(before.is_document(), after.is_document())], &[])
        };
        assert!(reads(&quarantined, &document));
        assert!(reads(&document, &quarantined));
        assert!(!reads(&quarantined, &FileState::absent()));
        assert!(!reads(&FileState::absent(), &quarantined));
        assert!(!reads(
            &quarantined,
            &FileState::quarantined(content_hash(b"\xff"))
        ));
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
                names_another: false,
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
                names_another: false,
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

    /// **A wikilink rewrite resolves only where its ends name a document to
    /// retarget to that is not the one `old` named, where the plan leaves
    /// it.** An `old` naming one document or none and a `new` naming one
    /// document resolve; an `old` naming several,
    /// and the two naming one document — at one path, or across the move
    /// carrying `old`'s document to where `new` names — do not. A `new`
    /// naming the path `old`'s document leaves names whatever stands there
    /// after, never that document.
    #[test]
    fn a_wikilink_rewrite_resolves_only_to_an_addressable_other_document() {
        use norn_store::TargetNaming;

        use super::RetargetNaming;

        let normalizer = PathNormalizer::for_sensitivity(CaseSensitivity::Sensitive);
        let one = |at: &str| Resolves::one(path(at));
        let ends = |old: Resolves, new: Resolves| RetargetNaming {
            old: TargetNaming::new(old, Resolves::none(), None),
            new: TargetNaming::new(Resolves::none(), new, None),
        };
        let still = Lineage::default();
        let resolves = |named: &RetargetNaming, lineage: &Lineage| {
            named
                .destination(lineage, &normalizer)
                .map(|(_, at)| at.as_str().to_string())
        };
        assert_eq!(
            resolves(&ends(one("a.md"), one("x/c.md")), &still).as_deref(),
            Some("x/c.md")
        );
        assert_eq!(
            resolves(&ends(Resolves::none(), one("x/c.md")), &still).as_deref(),
            Some("x/c.md")
        );
        assert_eq!(
            resolves(&ends(Resolves::several(), one("c.md")), &still),
            None
        );
        assert_eq!(resolves(&ends(one("a.md"), Resolves::none()), &still), None);
        assert_eq!(resolves(&ends(one("a.md"), one("a.md")), &still), None);

        let moved = Lineage::of(
            &[Operation::new(OperationKind::move_document(
                path("a.md"),
                path("b.md"),
            ))],
            &[0],
            &normalizer,
        );
        assert_eq!(resolves(&ends(one("a.md"), one("b.md")), &moved), None);
        assert_eq!(
            resolves(&ends(one("a.md"), one("a.md")), &moved).as_deref(),
            Some("a.md")
        );
    }

    /// **The index's links of a document are the links its bytes hold.** For
    /// documents holding wikilinks with heading and block anchors and an
    /// alias, Markdown links with a title, a protocol and a relative path,
    /// embeds of both syntaxes, frontmatter wikilinks placed and unplaced, a
    /// link-shaped span inside code, and none at all, the planner's snapshot answers the links
    /// [`document_links`] reads from the file's bytes, in its order, beside
    /// the hex of the hash a streamed read of the file answers. A file that
    /// does not decode is no document, and the index holds nothing for it.
    ///
    /// The forbidden shape is a second reading of a document's links: a plan
    /// that took its moved document's links from the index would otherwise
    /// plan against links its bytes do not hold.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the vault the store is derived from.
    fn the_indexed_links_of_a_document_are_the_links_its_bytes_hold() {
        use super::LinkIndex;
        use crate::derivation::document_links;
        use crate::planner::links::testing::snapshot_of;

        let scratch = norn_testkit::scratch::Scratch::new("held-links");
        let root = scratch.join("vault");
        let documents: [(&str, &[u8]); 6] = [
            (
                "notes/wiki.md",
                b"# Wiki\n\nSee [[target#Some Heading]], [[target#^block-1|alias]] and [[sub/deep]].\n",
            ),
            (
                "notes/markdown.md",
                b"A [relative](../other/target.md \"title\"), a [site](https://example.com/x#y) \
                  and [here](#local).\n",
            ),
            (
                "notes/embeds.md",
                b"![[diagram.png]]\n\n![alt](images/photo.jpg)\n\n![[target#Heading]]\n",
            ),
            (
                "notes/frontmatter.md",
                b"---\nrelated: \"[[target]]\"\nalso:\n  - \"[[other|Other]]\"\nflow: [\"[[flowed]]\"]\n\
                  nested:\n  at: \"[[nested#Heading]]\"\n---\n\nBody [[body-link]].\n",
            ),
            (
                "notes/code.md",
                b"`[[not-a-link]]`\n\n```\n[also](not-a-link.md)\n```\n\nBut [[real]].\n",
            ),
            ("notes/none.md", b"# Nothing\n\nNo links at all.\n"),
        ];
        for (at, bytes) in documents {
            let full = root.join(at);
            std::fs::create_dir_all(full.parent().unwrap()).expect("a folder");
            std::fs::write(&full, bytes).expect("a document");
        }
        std::fs::write(root.join("notes/binary.md"), b"[[target]] \xff\n").expect("a file");
        let mut store = norn_store::Store::open(
            scratch.join("store.sqlite3"),
            norn_store::StoredPathOrder::Sensitive,
            crate::DERIVATION_VERSION,
        )
        .expect("a store");
        crate::production::heal_from_zero(&mut store, &root, &[]).expect("a heal");
        let snapshot = snapshot_of(&store);
        let declared = norn_store::ContentModel::none();
        let index = crate::apply::PlanSnapshot::held(vault(), &snapshot, &declared);

        for (at, bytes) in documents {
            let streamed = norn_fs::stream_optional_and_hash(&root, std::path::Path::new(at))
                .expect("a streamed read")
                .expect("the document is there");
            let held = index
                .held_links(&path(at))
                .unwrap_or_else(|refused| panic!("the links {at} holds: {refused:?}"))
                .unwrap_or_else(|| panic!("the index holds no document at {at}"));
            assert_eq!(held.content_hash, streamed.content_hash().to_hex(), "{at}");
            assert_eq!(held.links, document_links(bytes), "{at}");
        }
        // The Markdown image beside the wikilink embeds is a held link of its
        // own, an embedding Markdown one.
        let embeds = document_links(documents[2].1);
        assert!(
            embeds.iter().any(|link| link.embed
                && link.family == norn_store::LinkFamily::Markdown
                && link.target == "images/photo.jpg"),
            "the image is not held as a Markdown embed: {embeds:?}"
        );
        assert!(
            documents
                .iter()
                .filter(|(at, _)| *at != "notes/none.md" && *at != "notes/code.md")
                .all(|(_, bytes)| document_links(bytes).len() >= 2),
            "a case meant to hold links holds fewer than two"
        );
        for nothing in ["notes/binary.md", "notes/absent.md"] {
            assert!(
                matches!(index.held_links(&path(nothing)), Ok(None)),
                "the index holds a document at {nothing}"
            );
        }
    }
}
