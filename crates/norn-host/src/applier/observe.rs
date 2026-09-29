//! What a resolved plan's targets hold now: each at its before-state, at its
//! after-state, or at neither, read at the exact spelling the plan writes.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use norn_fs::{CaseSensitivity, NormalizedPath, PathNormalizer};
use norn_wire::{DocumentPath, FileState, OperationKind, PlanCondition, ResolvedPlan, Transition};

use crate::planner::view::{Entry, VaultView};

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
/// **A move's source is drift once its destination is not at its
/// after-state.** The applier replaces or removes a source only after every
/// target drawing on it durably landed, so a source at its after-state beside
/// a destination that is not was changed by another writer (ADR 0031): the
/// source is marked drifted, holding what it holds, and the move is named.
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
    let sources = drifted_sources(plan, &mut states, view.normalizer());
    Ok((states, sources))
}

/// What one transition's target holds.
fn one<V: VaultView>(transition: &Transition, view: &V) -> Result<TargetState, V::Error> {
    let Some(identity) = canonical(view.normalizer(), &transition.path) else {
        return Ok(TargetState::Drifted(FileState::absent()));
    };
    let (holds, bytes) = match view.entry(&identity)? {
        Entry::Document { at, bytes, hash } if at == transition.path => {
            (FileState::present(hash), Some(bytes))
        }
        Entry::Document { .. } | Entry::Absent { .. } => (FileState::absent(), None),
        // Nothing a document can be read from stands there: a create's name is
        // left for staging to judge, which refuses it as taken; for any other
        // transition it is drift.
        Entry::Folder | Entry::Blocked { .. } => {
            return Ok(if transition.before == FileState::absent() {
                TargetState::AtBefore(None)
            } else {
                TargetState::Drifted(FileState::absent())
            });
        }
    };
    Ok(judged(transition, holds, bytes))
}

/// `holds` judged against `transition`'s two states. The after-state is asked
/// first, so a transition whose two states agree is landed.
fn judged(transition: &Transition, holds: FileState, bytes: Option<Arc<[u8]>>) -> TargetState {
    if holds == transition.after {
        TargetState::Landed(bytes)
    } else if holds == transition.before {
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
    let entry = match canonical(view.normalizer(), &old.path)
        .filter(|_| canonical(view.normalizer(), &new.path).is_some())
    {
        Some(identity) => view.entry(&identity)?,
        None => Entry::Blocked {
            detail: String::new(),
        },
    };
    let untouched = TargetState::AtBefore(None);
    Ok(match entry {
        Entry::Document { at, bytes, hash } if at == old.path => {
            let holds = FileState::present(hash);
            if holds == old.before {
                (TargetState::AtBefore(Some(bytes)), untouched)
            } else if holds == new.after {
                (
                    TargetState::Halfway(bytes.clone()),
                    TargetState::Halfway(bytes),
                )
            } else {
                (TargetState::Drifted(holds), untouched)
            }
        }
        Entry::Document { at, bytes, hash } if at == new.path => {
            let holds = FileState::present(hash);
            if holds == new.after {
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

/// Mark each move's source drifted where it holds its after-state and its
/// destination does not, and answer those moves' positions.
fn drifted_sources(
    plan: &ResolvedPlan,
    states: &mut [TargetState],
    normalizer: &PathNormalizer,
) -> Vec<usize> {
    let index_of = transition_index(plan, normalizer);
    let mut moves = Vec::new();
    for (position, operation) in plan.operations.iter().enumerate() {
        let OperationKind::MoveDocument { from, to } = &operation.kind else {
            continue;
        };
        let (Some(from_identity), Some(to_identity)) = (
            identity(normalizer, from.as_str()),
            identity(normalizer, to.as_str()),
        ) else {
            continue;
        };
        // A case-only rename is its own source, published as one respell.
        if from_identity == to_identity {
            continue;
        }
        let (Some(&source), Some(&destination)) =
            (index_of.get(&from_identity), index_of.get(&to_identity))
        else {
            continue;
        };
        let transition = &plan.transitions[source];
        if transition.before != transition.after
            && states[source].landed()
            && !states[destination].landed()
        {
            states[source] = TargetState::Drifted(transition.after.clone());
            moves.push(position);
        }
    }
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

/// Every condition the plan carries that the vault no longer meets.
///
/// A condition is on a file the plan does not write, so it reads the same
/// whatever of the plan has landed: it is judged against the file as it
/// stands.
pub(super) fn failed_conditions<V: VaultView>(
    plan: &ResolvedPlan,
    view: &V,
) -> Result<Vec<PlanCondition>, V::Error> {
    let mut failed = Vec::new();
    for condition in &plan.conditions {
        let PlanCondition::ContentHash { path, hash } = condition;
        let holds = match identity(view.normalizer(), path.as_str()) {
            Some(identity) => matches!(
                view.entry(&identity)?,
                Entry::Document { at, hash: held, .. } if at == *path && held == *hash
            ),
            None => false,
        };
        if !holds {
            failed.push(condition.clone());
        }
    }
    Ok(failed)
}

/// `path`'s identity under the vault's one rule, where it names a file.
pub(super) fn identity(normalizer: &PathNormalizer, path: &str) -> Option<NormalizedPath> {
    normalizer.normalize(Path::new(path)).ok()
}

/// `path`'s identity, where `path` is spelled as the vault's one rule spells
/// it.
///
/// **A plan names each target at one spelling.** The planner writes every
/// transition at a normalized spelling, and the kernel keeps a path as it is
/// given — a `./` component included — so a target spelled otherwise, as a
/// plan edited by hand can be, would be published, recorded and derived at a
/// second spelling of one file. It is judged as drift instead, holding
/// nothing, and the fresh plan the refusal answers spells it once.
fn canonical(normalizer: &PathNormalizer, path: &DocumentPath) -> Option<NormalizedPath> {
    identity(normalizer, path.as_str())
        .filter(|identity| identity.as_path() == Path::new(path.as_str()))
}

/// Whether a transition puts a document where none stood.
pub(super) fn is_create(transition: &Transition) -> bool {
    matches!(
        (&transition.before, &transition.after),
        (FileState::Absent {}, FileState::Present { .. })
    )
}

/// Whether a transition takes away a document that stood.
pub(super) fn is_removal(transition: &Transition) -> bool {
    matches!(
        (&transition.before, &transition.after),
        (FileState::Present { .. }, FileState::Absent {})
    )
}
