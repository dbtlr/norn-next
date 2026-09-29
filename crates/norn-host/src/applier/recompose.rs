//! Recomposition: every target's after-bytes rebuilt from the before-states
//! and the operations, in the order the resolved plan records them.
//!
//! **The plan carries no bytes it did not author**, so what the applier
//! publishes is composed again here, through the planner's own
//! [`compose`](crate::planner::compose::compose), and published only where it
//! hashes to the after-state the plan carries. The order is the plan's
//! recorded one, which [`crate::planner::order`] settled at planning; it is
//! never derived again, so the same operations compose the same bytes.

use std::collections::BTreeMap;
use std::sync::Arc;

use norn_fs::{NormalizedPath, PathNormalizer};
use norn_wire::{FileState, ResolvedPlan};

use super::observe::{TargetState, identity};
use crate::planner::compose::{Composition, compose};
use crate::planner::view::{Entry, VaultView};

/// Compose `plan`'s operations, in its recorded order, over the before-state
/// of every target, as `states` observed them.
pub(super) fn recompose<V: VaultView>(
    plan: &ResolvedPlan,
    states: &[TargetState],
    view: &V,
) -> Result<Composition, V::Error> {
    let before = BeforeStates::over(plan, states, view);
    let order: Vec<usize> = (0..plan.operations.len()).collect();
    compose(&plan.operations, &order, &before)
}

/// The vault as it stood before the plan, where the plan touches it.
///
/// **A target reads as its before-state**: absent where the plan found
/// nothing, and the bytes it holds where it still holds the before-state. A
/// target whose before-state is gone — a document a landed move already took
/// away, or one another writer changed — reads as a place no document is
/// read from, so an operation drawing on it does not compose and the target
/// it writes does not hash to its after-state. Nothing else is read: every
/// name a resolved plan's operations carry is one of its targets.
struct BeforeStates<'a, V> {
    view: &'a V,
    plan: &'a ResolvedPlan,
    states: &'a [TargetState],
    by_identity: BTreeMap<NormalizedPath, Vec<usize>>,
}

impl<'a, V: VaultView> BeforeStates<'a, V> {
    fn over(plan: &'a ResolvedPlan, states: &'a [TargetState], view: &'a V) -> Self {
        let mut by_identity: BTreeMap<NormalizedPath, Vec<usize>> = BTreeMap::new();
        for (index, transition) in plan.transitions.iter().enumerate() {
            if let Some(identity) = identity(view.normalizer(), transition.path.as_str()) {
                by_identity.entry(identity).or_default().push(index);
            }
        }
        BeforeStates {
            view,
            plan,
            states,
            by_identity,
        }
    }
}

impl<V: VaultView> VaultView for BeforeStates<'_, V> {
    type Error = V::Error;

    fn normalizer(&self) -> &PathNormalizer {
        self.view.normalizer()
    }

    fn entry(&self, path: &NormalizedPath) -> Result<Entry, V::Error> {
        let Some(indices) = self.by_identity.get(path) else {
            return self.view.entry(path);
        };
        // A case-only rename's identity has two transitions: the document
        // stood at the one whose before-state is present.
        let index = indices
            .iter()
            .copied()
            .find(|&index| self.plan.transitions[index].before != FileState::absent())
            .unwrap_or(indices[0]);
        let transition = &self.plan.transitions[index];
        let FileState::Present { hash } = &transition.before else {
            return Ok(Entry::Absent {
                at: transition.path.clone(),
            });
        };
        Ok(match before_bytes(&self.states[index]) {
            Some(bytes) => Entry::Document {
                at: transition.path.clone(),
                bytes,
                hash: hash.clone(),
            },
            None => Entry::Blocked {
                detail: format!("`{}` no longer holds its before-state", transition.path),
            },
        })
    }

    fn folder_stands(&self, folder: &NormalizedPath) -> Result<bool, V::Error> {
        self.view.folder_stands(folder)
    }

    fn folder_names(&self, folder: &NormalizedPath) -> Result<Vec<std::ffi::OsString>, V::Error> {
        self.view.folder_names(folder)
    }
}

/// The before-state's bytes, where the target still holds them.
fn before_bytes(state: &TargetState) -> Option<Arc<[u8]>> {
    match state {
        TargetState::AtBefore(bytes) => bytes.clone(),
        TargetState::Landed(_) | TargetState::Halfway(_) | TargetState::Drifted(_) => None,
    }
}
