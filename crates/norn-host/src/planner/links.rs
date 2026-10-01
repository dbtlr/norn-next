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
//! resolves to, so a cascade's rewrite is checked where it lands. NORN-297:
//! no planning resolves an authored `rewrite_link` yet, so the links a
//! resolved plan writes are its cascades' until it does.
//!
//! **What the forecast says of a link a cascade did not follow.** A link
//! that named a document a move carries away, and does not name it where it
//! lands, is advised on as the cascade's skip — the text layer's reason, or
//! unrepresentable where no spelling read back — and an ambiguous link that
//! could name a moved document as skipped for its ambiguity, each in place
//! of what its resolution alone would say. Both are read here, from the plan
//! alone, so the planner and the applier forecast alike.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use norn_fs::{NormalizedPath, PathNormalizer};
use norn_store::{LinkChange, PathOverlay, ProbedLink};
use norn_text::RewriteSkip;
use norn_wire::{
    DocumentPath, FileState, LinkAddressKind, LinkAdvisory, LinkFamily, LinkHealth, LinkKey,
    Operation, OperationKind, PlanCondition, Resolves, Transition,
};

use super::compose::{Composition, Skipped};
use super::lineage::{Drawn, Lineage};
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
/// operation rewrites a link, as a `rewrite_link` or through its cascade. A
/// plan of which neither holds records no entry and reads nothing, so
/// [`change_set`] answers it without asking and an apply job mints no handle
/// for it ([`reads_links`]).
fn reads_links_over<'o>(
    presence: impl IntoIterator<Item = (bool, bool)>,
    operations: impl IntoIterator<Item = &'o Operation>,
) -> bool {
    presence.into_iter().any(|(before, after)| before != after)
        || operations.into_iter().any(|operation| {
            matches!(operation.kind, OperationKind::RewriteLink { .. })
                || !operation.cascade.is_empty()
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
/// why, which the forecast says of that link.
pub(crate) fn change_set<'o, I: LinkIndex + ?Sized>(
    targets: &[Target<'_>],
    lineage: &Lineage,
    normalizer: &PathNormalizer,
    operations: impl IntoIterator<Item = &'o Operation> + Clone,
    skipped: &[Skipped],
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
    let moved = |path: &str| {
        normalizer
            .normalize(Path::new(path))
            .ok()
            .and_then(|file| lineage.carried_to(&file))
            .is_some()
    };

    let mut judged: BTreeMap<EntryKey, Judged> = BTreeMap::new();
    index.changes(&overlay, &probed, &mut |change| {
        let key = LinkKey::new(
            wire_path(&change.holder),
            wire_family(change.link.family),
            address(&change.link),
        );
        // A link left behind by the cascade, and an ambiguous link that could
        // name a document a move carries away, which is left as written since
        // which it names is not known.
        let left_behind = left_behind(&change.before, &change.after, lineage, normalizer).is_some();
        let ambiguous_among_moved = matches!(change.before, Resolves::Several {})
            && change
                .before_targets
                .iter()
                .any(|target| moved(target.as_str()));
        let held = judged.entry(entry_key(&key)).or_insert_with(|| Judged {
            key,
            address: change.address,
            before: change.before,
            after: change.after,
            written: false,
            members_moved: false,
            left_behind: false,
            ambiguous_among_moved: false,
        });
        held.written |= change.written;
        held.members_moved |= change.members_moved;
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
    members_moved: bool,
    /// The link named a document a move of the plan carries away, and does
    /// not name it where it lands: its cascade left it as written.
    left_behind: bool,
    /// The link was ambiguous before the plan, and one of the documents it
    /// could name is one a move of the plan carries away.
    ambiguous_among_moved: bool,
}

impl Judged {
    /// Whether the change set records the link: what it resolves to changes,
    /// the plan writes its text, or it is left behind — which, where the
    /// path it resolves to is the same on both sides, says the document
    /// there is not the one it named.
    fn records(&self) -> bool {
        self.before != self.after || self.written || self.left_behind
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
    /// link's resolution alone would say of it.
    ///
    /// **Otherwise each side is read as link health judges it**
    /// ([`LinkHealth::of_address`]): a link healthy or ambiguous before and
    /// broken after is left broken; one ambiguous after and not before is
    /// made ambiguous; and an ambiguous link the plan does not write is
    /// retargeted where it is healthy after, or ambiguous after with members
    /// the plan moved — the last having no entry, since several on both
    /// sides is no change the set records. A side link health does not judge
    /// — an attachment's address resolving to no document — is never broken,
    /// so a link going there is recorded and not advised on.
    fn advisory(&self, skip: Option<RewriteSkip>) -> Option<LinkAdvisory> {
        let key = self.key.clone();
        if !self.written {
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
            (LinkHealth::Ambiguous, LinkHealth::Healthy) if !self.written => {
                Some(LinkAdvisory::retargeted(key))
            }
            (LinkHealth::Ambiguous, LinkHealth::Ambiguous)
                if !self.written && self.members_moved =>
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
    /// rewrite wrote is checked where it lands. Planning composes no rewrite
    /// yet, so the set is computed here from the bytes a rewrite would leave.
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
                members_moved: true,
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
                members_moved,
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
