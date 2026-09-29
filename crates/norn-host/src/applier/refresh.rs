//! Refuse-and-refresh: a refused plan answers with a plan resolved afresh
//! against what the vault holds now.

use std::collections::{BTreeMap, BTreeSet};

use norn_fs::NormalizedPath;
use norn_wire::{
    AuthoredPlan, DocumentPath, Forecast, OperationId, OperationKind, RefusedCheck, ResolvedPlan,
    UnresolvedOperation, UnresolvedReason,
};

use super::observe::{TargetState, identity, observe, units};
use super::outcome::{ApplyOutcome, Refused, Unsound};
use super::stage::drifted_checks;
use crate::planner::compose::touches;
use crate::planner::resolve::{PlanningFailure, resolve};
use crate::planner::view::{TreeView, VaultView};

/// What becomes of one operation of a refused plan.
enum Fate {
    /// Every target it writes holds its after-state: it is left out, and a
    /// requirement on it is met.
    Dropped,
    /// Left for the caller, for this reason.
    Unresolved(UnresolvedReason),
    /// None of its targets holds its after-state: it is resolved again.
    Resolved,
}

/// Refuse `plan` for `checks`, answering with a plan resolved afresh through
/// `view`.
///
/// **What the fresh plan holds** (ADR 0031): an operation whose targets all
/// hold their after-states is dropped; an operation none of whose targets
/// holds its after-state is resolved again against what the vault holds now,
/// through the one planner; and an operation with one target landed and
/// another not, a move whose source another writer changed before its
/// destination landed, an operation that no longer resolves, and one that
/// requires an unresolved operation or touches a file one touches — since
/// operations on one file stand or fall together — are listed as unresolved,
/// never resolved again or dropped. Every drifted target is marked in the
/// forecast, because a hash cannot tell whether it already carries this
/// plan's change. Nothing is rebased: applying the fresh plan is the
/// caller's decision.
pub(super) fn refuse_and_refresh(
    plan: ResolvedPlan,
    view: &TreeView,
    mut checks: Vec<RefusedCheck>,
) -> ApplyOutcome {
    let normalizer = view.normalizer();
    let units = units(&plan, normalizer);
    let (states, drifted_moves) = match observe(&plan, &units, view) {
        Ok(observed) => observed,
        Err(error) => {
            return ApplyOutcome::WriteFailed {
                plan,
                detail: error.to_string(),
            };
        }
    };
    // A target a check already names — drifted, or a create whose name was
    // taken — is not named twice.
    let named: BTreeSet<DocumentPath> = checks.iter().filter_map(named_path).cloned().collect();
    for check in drifted_checks(&plan, &states) {
        if drifted_path(&check).is_some_and(|path| !named.contains(path)) {
            checks.push(check);
        }
    }
    let drifted: Vec<DocumentPath> = checks
        .iter()
        .filter_map(drifted_path)
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let fates = fates(&plan, &states, &drifted_moves, view);
    let met: BTreeSet<OperationId> = plan
        .operations
        .iter()
        .zip(&fates)
        .filter(|(_, fate)| matches!(fate, Fate::Dropped))
        .filter_map(|(operation, _)| operation.id.clone())
        .collect();
    let again: Vec<usize> = fates
        .iter()
        .enumerate()
        .filter(|(_, fate)| matches!(fate, Fate::Resolved))
        .map(|(position, _)| position)
        .collect();
    let mut authored = AuthoredPlan::new(
        plan.vault.clone(),
        again
            .iter()
            .map(|&position| plan.operations[position].clone())
            .collect(),
    );
    authored.footnote = plan.footnote.clone();
    let mut unresolved: Vec<(usize, UnresolvedOperation)> = Vec::new();
    for (position, fate) in fates.into_iter().enumerate() {
        if let Fate::Unresolved(reason) = fate {
            unresolved.push((
                position,
                UnresolvedOperation::new(plan.operations[position].clone(), reason),
            ));
        }
    }
    let (fresh, forecast) = match resolve(authored, plan.root.clone(), &met, view) {
        Ok(resolution) => {
            for left in resolution.unresolved {
                let position = again
                    .iter()
                    .copied()
                    .find(|&position| {
                        plan.operations[position] == left.operation
                            && !unresolved.iter().any(|(taken, _)| *taken == position)
                    })
                    .unwrap_or(usize::MAX);
                unresolved.push((position, left));
            }
            (resolution.plan, resolution.forecast)
        }
        Err(PlanningFailure::Fault(fault)) => {
            for &position in &again {
                unresolved.push((
                    position,
                    UnresolvedOperation::new(
                        plan.operations[position].clone(),
                        UnresolvedReason::no_longer_resolves(format!(
                            "resolved again against what the vault holds, the operations left are no plan: {fault:?}"
                        )),
                    ),
                ));
            }
            (
                ResolvedPlan::new(
                    plan.vault.clone(),
                    plan.root.clone(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                ),
                Forecast::new(Vec::new(), Vec::new(), Vec::new()),
            )
        }
        Err(PlanningFailure::View(error)) => {
            return ApplyOutcome::WriteFailed {
                plan,
                detail: error.to_string(),
            };
        }
    };
    unresolved.sort_by_key(|(position, _)| *position);
    ApplyOutcome::Refused(Box::new(Refused {
        plan: fresh,
        forecast: Forecast::new(drifted, forecast.folders_made, forecast.folders_removed),
        checks,
        unresolved: unresolved.into_iter().map(|(_, left)| left).collect(),
    }))
}

/// Refuse `plan`, whose transitions are not what its operations do,
/// answering with every one of its operations resolved afresh through `view`.
///
/// **Nothing the plan says of its targets is read**: its transitions are what
/// is wrong with it, so no operation is dropped as landed and no target is
/// marked drifted. The fresh plan is what the operations do against the vault
/// as it stands, through the one planner, with each operation that does not
/// resolve listed for the caller.
pub(super) fn refuse_unsound(plan: ResolvedPlan, view: &TreeView, detail: String) -> ApplyOutcome {
    let mut authored = AuthoredPlan::new(plan.vault.clone(), plan.operations.clone());
    authored.footnote = plan.footnote.clone();
    match resolve(authored, plan.root.clone(), &BTreeSet::new(), view) {
        Ok(resolution) => ApplyOutcome::Unsound(Box::new(Unsound {
            refused: Refused {
                plan: resolution.plan,
                forecast: resolution.forecast,
                checks: Vec::new(),
                unresolved: resolution.unresolved,
            },
            detail,
        })),
        Err(PlanningFailure::Fault(fault)) => ApplyOutcome::Invalid(fault),
        Err(PlanningFailure::View(error)) => ApplyOutcome::WriteFailed {
            plan,
            detail: error.to_string(),
        },
    }
}

/// The target a drifted or a taken-name check names.
fn named_path(check: &RefusedCheck) -> Option<&DocumentPath> {
    match check {
        RefusedCheck::Drifted { path, .. } | RefusedCheck::NameTaken { path, .. } => Some(path),
        _ => None,
    }
}

/// The target a drifted check names.
fn drifted_path(check: &RefusedCheck) -> Option<&DocumentPath> {
    match check {
        RefusedCheck::Drifted { path, .. } => Some(path),
        _ => None,
    }
}

/// What becomes of each of `plan`'s operations, by position.
fn fates(
    plan: &ResolvedPlan,
    states: &[TargetState],
    drifted_moves: &[usize],
    view: &TreeView,
) -> Vec<Fate> {
    let normalizer = view.normalizer();
    let mut by_identity: BTreeMap<NormalizedPath, Vec<usize>> = BTreeMap::new();
    for (index, transition) in plan.transitions.iter().enumerate() {
        if let Some(identity) = identity(normalizer, transition.path.as_str()) {
            by_identity.entry(identity).or_default().push(index);
        }
    }
    let files = |kind: &OperationKind| -> Vec<NormalizedPath> {
        touches(kind)
            .filter_map(|path| identity(normalizer, path.as_str()))
            .collect()
    };
    let mut fates: Vec<Fate> = plan
        .operations
        .iter()
        .enumerate()
        .map(|(position, operation)| {
            if drifted_moves.contains(&position) {
                return Fate::Unresolved(UnresolvedReason::no_longer_resolves(
                    "its source was changed by another writer before its destination landed",
                ));
            }
            let targets: Vec<&TargetState> = files(&operation.kind)
                .iter()
                .flat_map(|file| by_identity.get(file).into_iter().flatten())
                .map(|&index| &states[index])
                .collect();
            if !targets.is_empty() && targets.iter().all(|state| state.landed()) {
                Fate::Dropped
            } else if targets.iter().any(|state| state.partly_landed()) {
                Fate::Unresolved(UnresolvedReason::part_landed())
            } else {
                Fate::Resolved
            }
        })
        .collect();
    // What falls with an unresolved operation: one requiring it, and one
    // touching a file it touches, each directly or through others.
    let carriers: BTreeMap<&OperationId, usize> = plan
        .operations
        .iter()
        .enumerate()
        .filter_map(|(position, operation)| Some((operation.id.as_ref()?, position)))
        .collect();
    loop {
        let mut fell = false;
        for position in 0..plan.operations.len() {
            if !matches!(fates[position], Fate::Resolved) {
                continue;
            }
            let operation = &plan.operations[position];
            let required = operation.requires.iter().find(|id| {
                carriers
                    .get(id)
                    .is_some_and(|&carrier| matches!(fates[carrier], Fate::Unresolved(_)))
            });
            let reason = if let Some(id) = required {
                Some(UnresolvedReason::requires_unresolved(id.clone()))
            } else {
                let mine = files(&operation.kind);
                (0..plan.operations.len())
                    .find(|&other| {
                        matches!(fates[other], Fate::Unresolved(_))
                            && files(&plan.operations[other].kind)
                                .iter()
                                .any(|file| mine.contains(file))
                    })
                    .map(|other| {
                        UnresolvedReason::no_longer_resolves(format!(
                            "it touches a file the unresolved operation at position {other} touches, and operations on one file stand or fall together"
                        ))
                    })
            };
            if let Some(reason) = reason {
                fates[position] = Fate::Unresolved(reason);
                fell = true;
            }
        }
        if !fell {
            return fates;
        }
    }
}
