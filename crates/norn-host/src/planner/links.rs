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
//! presence the plan changes (`norn_store::Snapshot::resolution_changes`). A
//! link in a document the plan removes is no entry: its disappearance is the
//! plan's own transition.
//!
//! **A plan that changes no document's presence and writes no link is
//! answered without reading anything.** No link's resolution moves unless a
//! document appears or disappears somewhere a key names, or the plan writes
//! the link's text, so such a plan — every edit-only plan, a frontmatter set
//! among them — records nothing, and the index is never asked: nothing is
//! minted for it.
//!
//! **Which links a plan writes is one predicate** ([`writes`]): a link a
//! `rewrite_link` operation of the plan writes, held in its document under
//! the syntax and the address the rewrite writes. Every such link is an
//! entry whatever it resolves to, so a cascade's rewrite is checked where it
//! lands. NORN-297: no planning resolves a `rewrite_link` yet, so the
//! predicate holds of no link a resolved plan carries until link cascades are
//! planned.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use norn_fs::{NormalizedPath, PathNormalizer};
use norn_store::{LinkChange, PathOverlay, ProbedLink};
use norn_wire::{
    DocumentPath, FileState, LinkAdvisory, LinkFamily, LinkKey, Operation, OperationKind,
    PlanCondition, Resolves, Transition,
};

use super::lineage::Lineage;
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
/// `operations` reads the link index at all: whether some target holds a
/// document on one side and not the other, or some operation rewrites a
/// link. A plan of which neither holds records no entry and reads nothing.
pub(crate) fn reads_links(transitions: &[Transition], operations: &[Operation]) -> bool {
    transitions.iter().any(|transition| {
        matches!(transition.before, FileState::Present { .. })
            != matches!(transition.after, FileState::Present { .. })
    }) || operations
        .iter()
        .any(|operation| matches!(operation.kind, OperationKind::RewriteLink { .. }))
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

/// A plan's resolution change set, and the advisories its forecast carries
/// about the links it reaches.
#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct ChangeSet {
    /// One condition per entry, in the order of the link's key: its holder,
    /// its syntax, then its address.
    pub(crate) entries: Vec<PlanCondition>,
    /// What the plan does to a link a caller should look at, in the same
    /// order.
    pub(crate) advisories: Vec<LinkAdvisory>,
}

/// The resolution change set of the plan writing `targets`, whose content
/// follows `lineage` and whose operations are `operations`, every name read
/// through `normalizer`; the links the plan does not write are judged through
/// `index`.
pub(crate) fn change_set<'o, I: LinkIndex + ?Sized>(
    targets: &[Target<'_>],
    lineage: &Lineage,
    normalizer: &PathNormalizer,
    operations: impl IntoIterator<Item = &'o Operation>,
    index: &I,
) -> Result<ChangeSet, I::Error> {
    let rewrites = rewrites(operations, normalizer);
    let moves_presence = targets
        .iter()
        .any(|target| target.before != target.after.is_some());
    if !moves_presence && rewrites.is_empty() {
        return Ok(ChangeSet::default());
    }
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
        overlay = overlay.with(stored.clone(), target.before, target.after.is_some());
        let Some(bytes) = target.after else {
            continue;
        };
        let file = identity(target.path);
        let before_holder = file
            .as_ref()
            .and_then(|file| lineage.source(file))
            .and_then(|drawn| stood.get(&drawn.from).copied())
            .and_then(stored_path)
            .unwrap_or_else(|| stored.clone());
        for link in document_links(bytes) {
            let written = file
                .as_ref()
                .is_some_and(|file| writes(&rewrites, file, &link));
            probed.push(ProbedLink {
                before_holder: before_holder.clone(),
                after_holder: stored.clone(),
                link,
                written,
            });
        }
    }

    let mut judged: BTreeMap<(String, &'static str, String), Judged> = BTreeMap::new();
    index.changes(&overlay, &probed, &mut |change| {
        let key = LinkKey::new(
            wire_path(&change.holder),
            wire_family(change.link.family),
            address(&change.link),
        );
        let held = judged
            .entry((
                key.holder.as_str().to_string(),
                change.link.family.as_str(),
                key.address.clone(),
            ))
            .or_insert_with(|| Judged {
                key,
                before: change.before,
                after: change.after,
                written: false,
                members_moved: false,
            });
        held.written |= change.written;
        held.members_moved |= change.members_moved;
    })?;

    let mut set = ChangeSet::default();
    for (_, judged) in judged {
        if let Some(advisory) = judged.advisory() {
            set.advisories.push(advisory);
        }
        if judged.before != judged.after || judged.written {
            set.entries.push(PlanCondition::link_resolution(
                judged.key,
                judged.before,
                judged.after,
            ));
        }
    }
    Ok(set)
}

/// One link the change set judged, keyed as the plan leaves it.
struct Judged {
    key: LinkKey,
    before: Resolves,
    after: Resolves,
    written: bool,
    members_moved: bool,
}

impl Judged {
    /// What the forecast says about the link, where it says anything: a link
    /// that resolved and resolves to none after is left broken; one that did
    /// not resolve to several and does after is made ambiguous; and an
    /// ambiguous link the plan does not write is retargeted where it resolves
    /// to one document after, or to several whose members the plan moved —
    /// the last having no entry, since several on both sides is no change the
    /// set records.
    fn advisory(&self) -> Option<LinkAdvisory> {
        let key = self.key.clone();
        match (&self.before, &self.after) {
            (Resolves::One { .. } | Resolves::Several {}, Resolves::None {}) => {
                Some(LinkAdvisory::left_broken(key))
            }
            (Resolves::None {} | Resolves::One { .. }, Resolves::Several {}) => {
                Some(LinkAdvisory::made_ambiguous(key))
            }
            (Resolves::Several {}, Resolves::One { .. }) if !self.written => {
                Some(LinkAdvisory::retargeted(key))
            }
            (Resolves::Several {}, Resolves::Several {}) if !self.written && self.members_moved => {
                Some(LinkAdvisory::retargeted(key))
            }
            _ => None,
        }
    }
}

/// The links each `rewrite_link` of `operations` writes: its document's
/// identity, the syntax, and the address it writes.
fn rewrites<'o>(
    operations: impl IntoIterator<Item = &'o Operation>,
    normalizer: &PathNormalizer,
) -> BTreeSet<(NormalizedPath, &'static str, String)> {
    operations
        .into_iter()
        .filter_map(|operation| match &operation.kind {
            OperationKind::RewriteLink {
                path, syntax, to, ..
            } => Some((
                normalizer.normalize(Path::new(path.as_str())).ok()?,
                family_name(*syntax)?,
                to.clone(),
            )),
            _ => None,
        })
        .collect()
}

/// Whether the plan writes `link`, as its after-state holds it in the
/// document `holder`: the one predicate "a link whose text the plan writes"
/// is read by.
fn writes(
    rewrites: &BTreeSet<(NormalizedPath, &'static str, String)>,
    holder: &NormalizedPath,
    link: &norn_store::LinkFact,
) -> bool {
    rewrites.contains(&(holder.clone(), link.family.as_str(), address(link)))
}

/// A link's address as written: its protocol prefix, then its target, with
/// no anchor.
fn address(link: &norn_store::LinkFact) -> String {
    match &link.protocol {
        Some(protocol) => format!("{protocol}://{}", link.target),
        None => link.target.clone(),
    }
}

/// `path` as the store names it, where its grammar holds it; planning leaves
/// an operation naming any other unresolved.
fn stored_path(path: &DocumentPath) -> Option<norn_store::DocumentPath> {
    norn_store::DocumentPath::new(path.as_str()).ok()
}

/// A stored path as the wire names it.
fn wire_path(path: &norn_store::DocumentPath) -> DocumentPath {
    DocumentPath::new(path.as_str()).expect("a stored document path is a wire document path")
}

fn wire_family(family: norn_store::LinkFamily) -> LinkFamily {
    match family {
        norn_store::LinkFamily::Wikilink => LinkFamily::Wikilink,
        norn_store::LinkFamily::Markdown => LinkFamily::Markdown,
    }
}

/// A syntax's name as the store spells it, which orders the change set's
/// keys; `None` for a syntax the store holds no link of.
fn family_name(family: LinkFamily) -> Option<&'static str> {
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
        AuthoredPlan, DocumentPath, LinkAdvisory, LinkFamily, LinkKey, Operation, OperationKind,
        PlanCondition, Resolves, RootIdentity,
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
                before,
                after,
                written,
                members_moved,
            }
            .advisory()
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
