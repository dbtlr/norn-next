//! What a resolved plan's targets hold now: each at its before-state, at its
//! after-state, or at neither, read at the exact spelling the plan writes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use norn_fs::{CaseSensitivity, NormalizedPath, PathNormalizer};
use norn_wire::{ContentHash, DocumentPath, FileState, PlanCondition, ResolvedPlan, Transition};

use crate::derivation::decodes;
use crate::planner::compose::holding;
use crate::planner::lineage::Lineage;
use crate::planner::view::{Barrier, Entry, VaultView};

/// One publication the kernel makes: a transition on its own, or the two
/// transitions of a case-only rename on a root that folds case, which publish
/// together as one respell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Unit {
    /// One transition, by its index in the plan.
    One(usize),
    /// A case-only rename: the old spelling from present to absent and the
    /// new one from absent to present, by their indices in the plan.
    Respell { old: usize, new: usize },
}

impl Unit {
    /// The transitions this unit publishes.
    pub(super) fn transitions(self) -> impl Iterator<Item = usize> {
        let (first, second) = match self {
            Unit::One(index) => (index, None),
            Unit::Respell { old, new } => (old, Some(new)),
        };
        std::iter::once(first).chain(second)
    }
}

/// What one target holds now, judged against its two states.
#[derive(Clone, Debug)]
pub(super) enum TargetState {
    /// It holds its before-state: the bytes where it is a document.
    AtBefore(Option<Arc<[u8]>>),
    /// It holds its after-state, whichever writer put it there: landed. The
    /// bytes where it is a document, which hash to the after-state.
    Landed(Option<Arc<[u8]>>),
    /// A respell whose content landed under the old spelling and whose rename
    /// did not. The bytes are the after-state's.
    Halfway(Arc<[u8]>),
    /// It holds neither: drift, and what it holds, where absence stands for
    /// anything that is not a document.
    Drifted(FileState),
    /// It names a place the vault reads no documents at: the plan is not
    /// what its operations do, whatever the place holds.
    Unplaced,
}

impl TargetState {
    /// Whether the target holds its after-state.
    pub(super) fn landed(&self) -> bool {
        matches!(self, TargetState::Landed(_))
    }

    /// Whether some of the target's change landed: its after-state, or a
    /// respell's content under the old spelling.
    pub(super) fn partly_landed(&self) -> bool {
        matches!(self, TargetState::Landed(_) | TargetState::Halfway(_))
    }

    /// Whether the document `transition` found before the plan is gone from
    /// view: the target holds its change, whole or halfway, and so no longer
    /// the before-state's bytes. Where the two states agree, the bytes it
    /// holds are the before-state's.
    pub(super) fn before_unseen(&self, transition: &Transition) -> bool {
        matches!(transition.before, FileState::Present { .. })
            && match self {
                TargetState::Landed(_) => !transition.before.same_content(&transition.after),
                TargetState::Halfway(_) => true,
                TargetState::AtBefore(_) | TargetState::Drifted(_) | TargetState::Unplaced => false,
            }
    }
}

/// The plan's publication units: every pair of transitions that is one
/// case-only rename on a folding root, and every other transition alone.
///
/// **Only a case-only rename gives one identity two transitions.** The planner
/// holds a file at one spelling, and the one act that gives it a second is
/// that rename (see the planner's `compose`), so two transitions sharing an
/// identity, one taking a document away and one putting it at the other
/// spelling, are that rename.
pub(super) fn units(plan: &ResolvedPlan, normalizer: &PathNormalizer) -> Vec<Unit> {
    let mut by_identity: BTreeMap<NormalizedPath, Vec<usize>> = BTreeMap::new();
    if normalizer.case_sensitivity() == CaseSensitivity::Insensitive {
        for (index, transition) in plan.transitions.iter().enumerate() {
            if let Some(identity) = identity(normalizer, transition.path.as_str()) {
                by_identity.entry(identity).or_default().push(index);
            }
        }
    }
    let mut paired: BTreeMap<usize, Unit> = BTreeMap::new();
    for indices in by_identity.values() {
        let [first, second] = indices[..] else {
            continue;
        };
        let pair = match (
            is_removal(&plan.transitions[first]),
            is_removal(&plan.transitions[second]),
        ) {
            (true, false) if is_create(&plan.transitions[second]) => Unit::Respell {
                old: first,
                new: second,
            },
            (false, true) if is_create(&plan.transitions[first]) => Unit::Respell {
                old: second,
                new: first,
            },
            _ => continue,
        };
        paired.insert(first, pair);
        paired.insert(second, pair);
    }
    let mut units = Vec::new();
    for index in 0..plan.transitions.len() {
        match paired.get(&index) {
            None => units.push(Unit::One(index)),
            Some(pair @ Unit::Respell { old, .. }) if *old == index => units.push(*pair),
            Some(_) => {}
        }
    }
    units
}

/// What every target of `plan` holds, by transition index, read through
/// `view`; and the positions of the moves whose source another writer changed
/// before their destination landed.
///
/// **A name is judged at its exact spelling.** On a root that folds case, a
/// document listed under another spelling of a target's name is not at that
/// name: a case-only rename that landed leaves its old spelling absent, not
/// drifted.
///
/// **A source is drift once a target drawing on it is not at its
/// after-state.** The applier replaces or removes a source only after every
/// target drawing on it durably landed, so a source whose before-state is gone
/// while a target whose content the plan's moves carry from it has not landed
/// was changed by another writer (ADR 0031): the source is marked drifted,
/// holding what it holds, and the moves that carry it are named. A chain is
/// followed whole, through names the plan makes and takes away again.
pub(super) fn observe<V: VaultView>(
    plan: &ResolvedPlan,
    units: &[Unit],
    view: &V,
) -> Result<(Vec<TargetState>, Vec<usize>), V::Error> {
    let mut states: Vec<Option<TargetState>> = vec![None; plan.transitions.len()];
    for unit in units {
        match *unit {
            Unit::One(index) => {
                states[index] = Some(one(&plan.transitions[index], view)?);
            }
            Unit::Respell { old, new } => {
                let (old_state, new_state) =
                    respell(&plan.transitions[old], &plan.transitions[new], view)?;
                states[old] = Some(old_state);
                states[new] = Some(new_state);
            }
        }
    }
    let mut states: Vec<TargetState> = states
        .into_iter()
        .map(|state| state.expect("every transition is in one unit"))
        .collect();
    let lineage = recorded_lineage(plan, view.normalizer());
    let sources = drifted_sources(plan, &mut states, &lineage, view.normalizer());
    Ok((states, sources))
}

/// What one transition's target holds.
fn one<V: VaultView>(transition: &Transition, view: &V) -> Result<TargetState, V::Error> {
    let Some(identity) = identity(view.normalizer(), transition.path.as_str()) else {
        return Ok(TargetState::Unplaced);
    };
    let (holds, bytes) = match view.entry(&identity)? {
        Entry::Document { at, bytes, hash } if at == transition.path => {
            (holding(&bytes, hash), Some(bytes))
        }
        Entry::Document { .. } | Entry::Absent { .. } => (FileState::absent(), None),
        Entry::Blocked {
            barrier: Barrier::Closed,
            ..
        } => return Ok(TargetState::Unplaced),
        // Something that is no document stands in the way — a folder, or an
        // entry at or above the name that is not a folder — which another
        // writer can take away: a create's name is left for staging to judge,
        // which refuses it as taken; for any other transition it is drift.
        Entry::Folder
        | Entry::Blocked {
            barrier: Barrier::Occupied,
            ..
        } => {
            return Ok(if transition.before == FileState::absent() {
                TargetState::AtBefore(None)
            } else {
                TargetState::Drifted(FileState::absent())
            });
        }
    };
    Ok(judged(transition, holds, bytes))
}

/// `holds` judged against `transition`'s two states by the bytes each holds
/// ([`FileState::same_content`]): whether the bytes decode as a document is
/// judged against the plan's record of them apart ([`misread`]). The
/// after-state is asked first, so a transition whose two states agree is
/// landed.
fn judged(transition: &Transition, holds: FileState, bytes: Option<Arc<[u8]>>) -> TargetState {
    if holds.same_content(&transition.after) {
        TargetState::Landed(bytes)
    } else if holds.same_content(&transition.before) {
        TargetState::AtBefore(bytes)
    } else {
        TargetState::Drifted(holds)
    }
}

/// What a case-only rename's two spellings hold: before (the old spelling at
/// its before-state), halfway (the old spelling at the new one's
/// after-state), landed (the new spelling at its after-state), or drift.
fn respell<V: VaultView>(
    old: &Transition,
    new: &Transition,
    view: &V,
) -> Result<(TargetState, TargetState), V::Error> {
    let Some(identity) = identity(view.normalizer(), old.path.as_str()) else {
        return Ok((TargetState::Unplaced, TargetState::Unplaced));
    };
    let entry = view.entry(&identity)?;
    let untouched = TargetState::AtBefore(None);
    Ok(match entry {
        Entry::Blocked {
            barrier: Barrier::Closed,
            ..
        } => (TargetState::Unplaced, TargetState::Unplaced),
        Entry::Document { at, bytes, hash } if at == old.path => {
            let holds = holding(&bytes, hash);
            if holds.same_content(&old.before) {
                (TargetState::AtBefore(Some(bytes)), untouched)
            } else if holds.same_content(&new.after) {
                (
                    TargetState::Halfway(bytes.clone()),
                    TargetState::Halfway(bytes),
                )
            } else {
                (TargetState::Drifted(holds), untouched)
            }
        }
        Entry::Document { at, bytes, hash } if at == new.path => {
            let holds = holding(&bytes, hash);
            if holds.same_content(&new.after) {
                (TargetState::Landed(None), TargetState::Landed(Some(bytes)))
            } else {
                (TargetState::Landed(None), TargetState::Drifted(holds))
            }
        }
        // Gone from both spellings, or at a third: the source was taken away
        // while the rename had not landed.
        _ => (TargetState::Drifted(FileState::absent()), untouched),
    })
}

/// Every target carrying bytes whose decoding the plan records wrongly: bytes
/// it records as decoding as a vault document at one state and not at
/// another, or otherwise than the bytes a target holds of its change decode.
///
/// **Whether bytes decode is a fact of the bytes**, so every present state
/// sharing a hash records it alike, and bytes the applier holds pin every
/// state recording their hash, by the derivation's own rule ([`decodes`]).
/// The bytes a target holds of its change — its after-state, landed, or a
/// respell's content halfway under the old spelling — are judged here,
/// against every state with their hash, a gone before-state among them. The
/// bytes a target holds at its before-state are judged where recomposition
/// composes from them and compares the before-state it reads with the
/// recorded one. Bytes no held bytes share a hash with — a landed target's
/// gone before-state — are read as the plan records them, which is all an
/// apply can know of them. A plan disagreeing with itself or with bytes it
/// names is not what its operations do, so it is invalid rather than drift,
/// and every transition recording the bytes is named.
pub(super) fn misread(plan: &ResolvedPlan, states: &[TargetState]) -> Vec<DocumentPath> {
    let mut recorded: BTreeMap<&ContentHash, bool> = BTreeMap::new();
    let mut misread: BTreeSet<&ContentHash> = BTreeSet::new();
    for transition in &plan.transitions {
        for side in [&transition.before, &transition.after] {
            if let FileState::Present { hash, quarantined } = side
                && *recorded.entry(hash).or_insert(*quarantined) != *quarantined
            {
                misread.insert(hash);
            }
        }
    }
    for (transition, state) in plan.transitions.iter().zip(states) {
        if let TargetState::Landed(Some(bytes)) | TargetState::Halfway(bytes) = state
            && let FileState::Present { hash, quarantined } = &transition.after
            && *quarantined == decodes(bytes)
        {
            misread.insert(hash);
        }
    }
    plan.transitions
        .iter()
        .filter(|transition| {
            [&transition.before, &transition.after]
                .into_iter()
                .any(|side| side.hash().is_some_and(|hash| misread.contains(hash)))
        })
        .map(|transition| transition.path.clone())
        .collect()
}

/// Mark drifted each source whose before-state is gone while a target drawing
/// on it has not landed, and answer the positions of the moves that carry it.
fn drifted_sources(
    plan: &ResolvedPlan,
    states: &mut [TargetState],
    lineage: &Lineage,
    normalizer: &PathNormalizer,
) -> Vec<usize> {
    let index_of = transition_index(plan, normalizer);
    let mut drifted: BTreeMap<usize, FileState> = BTreeMap::new();
    let mut moves = Vec::new();
    for (index, transition) in plan.transitions.iter().enumerate() {
        if !matches!(states[index], TargetState::AtBefore(_))
            || !matches!(transition.after, FileState::Present { .. })
        {
            continue;
        }
        let Some(file) = identity(normalizer, transition.path.as_str()) else {
            continue;
        };
        let Some(source) = lineage.source(&file) else {
            continue;
        };
        let Some(&from) = index_of.get(&source.from) else {
            continue;
        };
        if source.from != file && states[from].before_unseen(&plan.transitions[from]) {
            drifted.insert(from, plan.transitions[from].after.clone());
            moves.extend(source.moves);
        }
    }
    for (index, holds) in drifted {
        states[index] = TargetState::Drifted(holds);
    }
    moves.sort_unstable();
    moves.dedup();
    moves
}

/// Each target's transition index by its identity. A case-only rename's two
/// transitions share one identity; the old spelling's is kept, which is the
/// one a move naming either spelling draws from.
pub(super) fn transition_index(
    plan: &ResolvedPlan,
    normalizer: &PathNormalizer,
) -> BTreeMap<NormalizedPath, usize> {
    let mut index = BTreeMap::new();
    for (position, transition) in plan.transitions.iter().enumerate() {
        if let Some(identity) = identity(normalizer, transition.path.as_str()) {
            let slot = index.entry(identity).or_insert(position);
            if is_removal(transition) {
                *slot = position;
            }
        }
    }
    index
}

/// Every content condition the plan carries that the vault no longer meets.
///
/// A content-hash condition is on a file the plan does not write, so it reads
/// the same whatever of the plan has landed: it is judged against the file as
/// it stands.
///
/// **A link-resolution entry is not judged here.** The entries are one
/// resolution change set, judged whole against the set computed again from
/// the plan's composed results, once those are known (`stage::check`).
pub(super) fn failed_conditions<V: VaultView>(
    plan: &ResolvedPlan,
    view: &V,
) -> Result<Vec<PlanCondition>, V::Error> {
    let mut failed = Vec::new();
    for condition in &plan.conditions {
        let holds = match condition {
            PlanCondition::ContentHash { path, hash } => {
                match identity(view.normalizer(), path.as_str()) {
                    Some(identity) => matches!(
                        view.entry(&identity)?,
                        Entry::Document { at, hash: held, .. } if at == *path && held == *hash
                    ),
                    None => false,
                }
            }
            PlanCondition::LinkResolution { .. } => true,
        };
        if !holds {
            failed.push(condition.clone());
        }
    }
    Ok(failed)
}

/// Where each file's content in `plan` is drawn from, following its
/// operations in their recorded order: the planner's one lineage derivation,
/// over the order a resolved plan composes in.
pub(super) fn recorded_lineage(plan: &ResolvedPlan, normalizer: &PathNormalizer) -> Lineage {
    let recorded: Vec<usize> = (0..plan.operations.len()).collect();
    Lineage::of(&plan.operations, &recorded, normalizer)
}

/// `path`'s identity under the vault's one rule, where it names a file.
pub(super) fn identity(normalizer: &PathNormalizer, path: &str) -> Option<NormalizedPath> {
    normalizer.normalize(Path::new(path)).ok()
}

/// Whether a transition puts a file where none stood, whether or not its
/// bytes decode as a document.
pub(super) fn is_create(transition: &Transition) -> bool {
    matches!(
        (&transition.before, &transition.after),
        (FileState::Absent {}, FileState::Present { .. })
    )
}

/// Whether a transition takes away a file that stood, whether or not its
/// bytes decoded as a document.
pub(super) fn is_removal(transition: &Transition) -> bool {
    matches!(
        (&transition.before, &transition.after),
        (FileState::Present { .. }, FileState::Absent {})
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use norn_wire::{
        AuthoredPlan, DocumentPath, FileState, Operation, OperationKind, ResolvedPlan,
        RootIdentity, VaultAddress, VaultName,
    };

    use super::{misread, observe, units};
    use crate::planner::links::testing::resolve_over_files as resolve;
    use crate::planner::view::VaultView;
    use crate::planner::view::memory::MemoryVault;

    /// Bytes that do not decode as a vault document until the `X` between
    /// the two halves of a character is taken out.
    const UNDECODABLE: &[u8] = b"\xc2X\xa0 note\n";

    fn path(text: &str) -> DocumentPath {
        DocumentPath::new(text).expect("a legal document path")
    }

    /// The plan the planner resolves for `operations` over `vault`.
    fn planned(vault: &MemoryVault, operations: Vec<OperationKind>) -> ResolvedPlan {
        let authored = AuthoredPlan::new(
            VaultAddress::name(VaultName::new("notes").expect("a legal vault name")),
            operations.into_iter().map(Operation::new).collect(),
        );
        resolve(
            authored,
            RootIdentity::from_device_and_inode(1, 2),
            &BTreeSet::new(),
            vault,
        )
        .expect("the plan resolves")
        .plan
    }

    /// The files `plan` is misread at over `vault`, as it stands.
    fn misread_over(plan: &ResolvedPlan, vault: &MemoryVault) -> Vec<DocumentPath> {
        let units = units(plan, VaultView::normalizer(vault));
        let (states, _) = observe(plan, &units, vault).expect("an infallible view");
        let mut misread = misread(plan, &states);
        misread.sort();
        misread
    }

    /// `plan` with the flag on `at`'s state on one side turned over.
    fn flipped(plan: &ResolvedPlan, at: &str, after: bool) -> ResolvedPlan {
        let mut plan = plan.clone();
        let transition = plan
            .transitions
            .iter_mut()
            .find(|transition| transition.path == path(at))
            .expect("a transition at the path");
        let side = if after {
            &mut transition.after
        } else {
            &mut transition.before
        };
        let FileState::Present { quarantined, .. } = side else {
            panic!("a present side");
        };
        *quarantined = !*quarantined;
        plan
    }

    /// **A case-only rename records its unchanged bytes at both spellings,
    /// and records them one way.** Its old spelling before the plan and its
    /// new one after it hold the same bytes, so a plan saying they decode at
    /// one and not the other is misread at both spellings, whether the
    /// rename has landed or not; the honest plan is misread nowhere.
    #[test]
    fn a_respells_two_records_of_its_unchanged_bytes_must_agree() {
        let before = MemoryVault::with_bytes(&[("Note.md", UNDECODABLE)]).folding_case();
        let landed = MemoryVault::with_bytes(&[("note.md", UNDECODABLE)]).folding_case();
        let plan = planned(
            &before,
            vec![OperationKind::move_document(
                path("Note.md"),
                path("note.md"),
            )],
        );
        assert_eq!(plan.transitions.len(), 2);
        for vault in [&before, &landed] {
            assert_eq!(misread_over(&plan, vault), Vec::<DocumentPath>::new());
            for (at, after) in [("Note.md", false), ("note.md", true)] {
                assert_eq!(
                    misread_over(&flipped(&plan, at, after), vault),
                    vec![path("Note.md"), path("note.md")],
                    "{at} flipped"
                );
            }
        }
    }

    /// **A respell halfway — its content landed under the old spelling — is
    /// judged on the bytes the old spelling holds**, which are the new
    /// spelling's after-state: a plan saying they do not decode where they
    /// do is misread there, and the honest plan is misread nowhere.
    #[test]
    fn a_respell_halfway_is_judged_on_the_bytes_its_old_spelling_holds() {
        let before = MemoryVault::with_bytes(&[("Note.md", UNDECODABLE)]).folding_case();
        let halfway = MemoryVault::with_bytes(&[("Note.md", b"\xc2\xa0 note\n")]).folding_case();
        let plan = planned(
            &before,
            vec![
                OperationKind::str_replace(path("Note.md"), "X", ""),
                OperationKind::move_document(path("Note.md"), path("note.md")),
            ],
        );
        assert_eq!(misread_over(&plan, &halfway), Vec::<DocumentPath>::new());
        assert_eq!(
            misread_over(&flipped(&plan, "note.md", true), &halfway),
            vec![path("note.md")]
        );
    }
}
