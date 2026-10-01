//! Refuse-and-refresh: a refused plan answers with a plan resolved afresh
//! against what the vault holds now.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

use norn_fs::{NormalizedPath, PathNormalizer};
use norn_wire::{
    AuthoredPlan, DocumentPath, Forecast, OperationId, RefusedCheck, ResolvedPlan,
    UnresolvedOperation, UnresolvedReason,
};

use super::observe::{TargetState, identity, observe, units};
use super::outcome::{ApplyOutcome, Refused};
use super::stage::{Links, check, drifted_checks};
use crate::derivation::Declared;
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
/// **What the fresh plan holds** (ADR 0032): an operation whose targets all
/// hold their after-states is dropped; an operation none of whose targets
/// holds its after-state is resolved again against what the vault holds now,
/// through the one planner; and an operation with one target landed and
/// another not, a move whose source another writer changed before its
/// destination landed, an operation that no longer resolves, and one that
/// requires an unresolved operation or touches a file one touches — since
/// operations on one file stand or fall together — are listed as unresolved,
/// never resolved again or dropped. Every drifted target is marked in the
/// forecast, because a hash cannot tell whether it already carries this
/// plan's change. Every fresh plan carries the refused plan's footnote and
/// its force, an empty one whose operations no longer plan included, and a
/// forced fresh plan's forecast lists the schema violations its force lets
/// through, judged under `declared` by the applier's one judgment, so it is
/// what a preview of the fresh plan lists. The fresh plan records its own
/// resolution change set, judged through `links`, and its forecast advises on
/// the links that set reaches. Nothing is rebased: applying the fresh plan is
/// the caller's decision.
pub(super) fn refuse_and_refresh(
    plan: ResolvedPlan,
    view: &TreeView,
    declared: &Declared,
    mut checks: Vec<RefusedCheck>,
    links: Links<'_>,
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
    let fates = fates(&plan, &states, &drifted_moves, view.normalizer());
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
    // The fresh plan is forced as the refused one was, so sending it back
    // applies it the way the refused plan would have applied.
    authored.force = plan.force;
    let mut unresolved: Vec<(usize, UnresolvedOperation)> = Vec::new();
    for (position, fate) in fates.into_iter().enumerate() {
        if let Fate::Unresolved(reason) = fate {
            unresolved.push((
                position,
                UnresolvedOperation::new(plan.operations[position].clone(), reason),
            ));
        }
    }
    let (fresh, forecast) = match resolve(authored, plan.root.clone(), &met, view, links) {
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
            let mut empty = ResolvedPlan::new(
                plan.vault.clone(),
                plan.root.clone(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            );
            empty.footnote = plan.footnote.clone();
            empty.force = plan.force;
            (empty, Forecast::new(Vec::new(), Vec::new(), Vec::new()))
        }
        Err(PlanningFailure::View(error)) => {
            return ApplyOutcome::WriteFailed {
                plan,
                detail: error.to_string(),
            };
        }
        Err(PlanningFailure::Links(refused)) => return ApplyOutcome::Unread(refused),
    };
    unresolved.sort_by_key(|(position, _)| *position);
    let forced = forced_through(&fresh, view, declared, links);
    ApplyOutcome::Refused(Box::new(Refused {
        plan: fresh,
        forecast: Forecast::new(drifted, forecast.folders_made, forecast.folders_removed)
            .with_forced(forced)
            .with_links(forecast.links),
        checks,
        unresolved: unresolved.into_iter().map(|(_, left)| left).collect(),
    }))
}

/// The schema violations the forced `fresh` plan lets through, as a preview
/// of it would list them: the applier's one judgment ([`check`]), run over
/// the vault the plan was just resolved against. None for a plan that is not
/// forced, and none where the fresh plan fails its own checks, which its
/// preview answers as a refusal rather than a forecast.
fn forced_through(
    fresh: &ResolvedPlan,
    view: &TreeView,
    declared: &Declared,
    links: Links<'_>,
) -> Vec<norn_wire::SchemaViolation> {
    if !fresh.force {
        return Vec::new();
    }
    check(fresh, view, declared, links).map_or_else(|_| Vec::new(), |checked| checked.forced)
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

/// Where an operation falls: the sweep, then the position.
type Fall = (usize, usize);

/// What becomes of each of `plan`'s operations, by position.
///
/// **What falls with an unresolved operation** is one requiring it, and one
/// touching a file it touches, each directly or through others. The fates are
/// those of a sweep over the positions in order, repeated until none falls,
/// in which an operation falls when it meets an unresolved requirement or
/// file-sharer and names the first requirement in its own order that is
/// unresolved by then, else the lowest position sharing a file that is.
///
/// **`O(n log n)` in operations, with each file normalized once.** The sweep
/// is not run: each operation's fall is placed at the sweep and position it
/// would fall at, `(sweep, position)`, earliest first through one queue, as a
/// shortest path. One unresolved to begin with stands at sweep 0 and is seen
/// by every position of sweep 1; one falling at `(k, q)` is seen by a later
/// position of sweep `k` and an earlier one of sweep `k + 1`. A file's
/// operations are reached once, from the first of them to fall, since every
/// later fall on that file is no earlier for any of them. Each reason is read
/// afterwards against what had fallen before the operation's own place.
fn fates(
    plan: &ResolvedPlan,
    states: &[TargetState],
    drifted_moves: &[usize],
    normalizer: &PathNormalizer,
) -> Vec<Fate> {
    let count = plan.operations.len();
    let mut by_identity: BTreeMap<NormalizedPath, Vec<usize>> = BTreeMap::new();
    for (index, transition) in plan.transitions.iter().enumerate() {
        if let Some(identity) = identity(normalizer, transition.path.as_str()) {
            by_identity.entry(identity).or_default().push(index);
        }
    }
    let files: Vec<Vec<NormalizedPath>> = plan
        .operations
        .iter()
        .map(|operation| {
            touches(&operation.kind)
                .filter_map(|path| identity(normalizer, path.as_str()))
                .collect()
        })
        .collect();
    let drifted_moves: BTreeSet<usize> = drifted_moves.iter().copied().collect();
    let mut fates: Vec<Fate> = plan
        .operations
        .iter()
        .enumerate()
        .map(|(position, _)| {
            if drifted_moves.contains(&position) {
                return Fate::Unresolved(UnresolvedReason::no_longer_resolves(
                    "its source was changed by another writer before its destination landed",
                ));
            }
            let targets: Vec<&TargetState> = files[position]
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
    let carriers: BTreeMap<&OperationId, usize> = plan
        .operations
        .iter()
        .enumerate()
        .filter_map(|(position, operation)| Some((operation.id.as_ref()?, position)))
        .collect();
    // Who requires each operation, by the carrier its identifier names.
    let mut required_by: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (position, operation) in plan.operations.iter().enumerate() {
        for id in &operation.requires {
            if let Some(&carrier) = carriers.get(id) {
                required_by[carrier].push(position);
            }
        }
    }
    let mut sharing: BTreeMap<&NormalizedPath, Vec<usize>> = BTreeMap::new();
    for (position, mine) in files.iter().enumerate() {
        for file in mine {
            let positions = sharing.entry(file).or_default();
            if positions.last() != Some(&position) {
                positions.push(position);
            }
        }
    }
    // Where each unresolved operation fell: `(sweep, position)`.
    let mut fell_at: Vec<Option<Fall>> = vec![None; count];
    let mut queue: BinaryHeap<Reverse<Fall>> = BinaryHeap::new();
    for (position, fate) in fates.iter().enumerate() {
        if matches!(fate, Fate::Unresolved(_)) {
            queue.push(Reverse((0, position)));
        }
    }
    let mut reached: BTreeSet<&NormalizedPath> = BTreeSet::new();
    while let Some(Reverse(at)) = queue.pop() {
        count_fate_step();
        let (sweep, fallen) = at;
        if fell_at[fallen].is_some() {
            continue;
        }
        fell_at[fallen] = Some(at);
        let seen_at = |position: usize| match sweep {
            0 => (1, position),
            _ if fallen < position => (sweep, position),
            _ => (sweep + 1, position),
        };
        let falls_with = required_by[fallen].iter().copied().chain(
            files[fallen]
                .iter()
                .filter(|file| reached.insert(*file))
                .flat_map(|file| sharing[file].iter().copied()),
        );
        for position in falls_with {
            count_fate_step();
            if matches!(fates[position], Fate::Resolved) && fell_at[position].is_none() {
                queue.push(Reverse(seen_at(position)));
            }
        }
    }
    // Each file's fallen operations in the order they fell, with the lowest
    // position among each prefix.
    let mut fallen_on: BTreeMap<&NormalizedPath, Vec<(Fall, usize)>> = BTreeMap::new();
    for (file, positions) in &sharing {
        let mut fell: Vec<Fall> = positions
            .iter()
            .filter_map(|&position| fell_at[position])
            .collect();
        fell.sort_unstable();
        let mut lowest = usize::MAX;
        let prefix = fell
            .into_iter()
            .map(|at| {
                lowest = lowest.min(at.1);
                (at, lowest)
            })
            .collect();
        fallen_on.insert(file, prefix);
    }
    for position in 0..count {
        if !matches!(fates[position], Fate::Resolved) {
            continue;
        }
        let Some(at) = fell_at[position] else {
            continue;
        };
        let before = |carrier: usize| fell_at[carrier].is_some_and(|fell| fell < at);
        let required = plan.operations[position].requires.iter().find(|id| {
            count_fate_step();
            carriers.get(id).is_some_and(|&carrier| before(carrier))
        });
        let reason = match required {
            Some(id) => UnresolvedReason::requires_unresolved(id.clone()),
            None => {
                let other = files[position]
                    .iter()
                    .filter_map(|file| {
                        count_fate_step();
                        let fell = &fallen_on[file];
                        let earlier = fell.partition_point(|(fell, _)| *fell < at);
                        earlier.checked_sub(1).map(|last| fell[last].1)
                    })
                    .min()
                    .expect("an operation falls only with one fallen before it");
                UnresolvedReason::no_longer_resolves(format!(
                    "it touches a file the unresolved operation at position {other} touches, and operations on one file stand or fall together"
                ))
            }
        };
        fates[position] = Fate::Unresolved(reason);
    }
    fates
}

#[cfg(test)]
thread_local! {
    /// How many steps judging a refused plan's fates has taken on this
    /// thread. The count, not the clock, shows the work per operation is
    /// bounded however the unresolved operations fall.
    static FATE_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn count_fate_step() {
    FATE_STEPS.with(|steps| steps.set(steps.get() + 1));
}

#[cfg(not(test))]
fn count_fate_step() {}

#[cfg(test)]
mod tests {
    use norn_wire::{
        DocumentPath, FileState, Operation, OperationId, OperationKind, ResolvedPlan, RootIdentity,
        Transition, UnresolvedReason, VaultAddress, VaultName,
    };

    use super::super::observe::TargetState;
    use super::{FATE_STEPS, Fate, fates};
    use crate::planner::view::VaultView;
    use crate::planner::view::memory::MemoryVault;

    fn path(text: &str) -> DocumentPath {
        DocumentPath::new(text).expect("a legal document path")
    }

    fn id(text: &str) -> OperationId {
        OperationId::new(text).expect("a legal operation id")
    }

    fn editing(at: &str) -> Operation {
        Operation::new(OperationKind::str_replace(path(at), "draft", "final"))
    }

    /// `operations`, each with one untouched target per file it names, in
    /// order, and the fates of those at `drifted` a drifted move's.
    fn judged(operations: Vec<Operation>, drifted: &[usize]) -> Vec<Fate> {
        let vault = MemoryVault::with(&[]);
        let mut seen = std::collections::BTreeSet::new();
        let transitions: Vec<Transition> = operations
            .iter()
            .filter_map(|operation| match &operation.kind {
                OperationKind::StrReplace { path, .. } if seen.insert(path.clone()) => Some(
                    Transition::new(path.clone(), FileState::absent(), FileState::absent()),
                ),
                _ => None,
            })
            .collect();
        let states = vec![TargetState::AtBefore(None); transitions.len()];
        let plan = ResolvedPlan::new(
            VaultAddress::name(VaultName::new("notes").expect("a legal vault name")),
            RootIdentity::from_device_and_inode(1, 2),
            operations,
            transitions,
            Vec::new(),
        );
        fates(&plan, &states, drifted, VaultView::normalizer(&vault))
    }

    fn reason(fate: &Fate) -> Option<&UnresolvedReason> {
        match fate {
            Fate::Unresolved(reason) => Some(reason),
            _ => None,
        }
    }

    fn touching(position: usize) -> UnresolvedReason {
        UnresolvedReason::no_longer_resolves(format!(
            "it touches a file the unresolved operation at position {position} touches, and operations on one file stand or fall together"
        ))
    }

    /// What falls with an unresolved operation, and the reason it names:
    /// the first requirement already unresolved, else the lowest position
    /// sharing a file that is, each judged as the operations fall in turn.
    #[test]
    fn what_falls_with_an_unresolved_operation_names_what_it_fell_with() {
        let fates = judged(
            vec![
                editing("x.md").with_requires(vec![id("c")]),
                editing("y.md").with_id(id("b")),
                editing("x.md")
                    .with_id(id("c"))
                    .with_requires(vec![id("b")]),
                editing("y.md"),
                editing("z.md"),
                editing("z.md"),
                editing("z.md"),
                editing("w.md"),
            ],
            &[3, 5, 6],
        );
        let drifted = UnresolvedReason::no_longer_resolves(
            "its source was changed by another writer before its destination landed",
        );
        assert_eq!(
            fates.iter().map(reason).collect::<Vec<_>>(),
            vec![
                Some(&UnresolvedReason::requires_unresolved(id("c"))),
                Some(&touching(3)),
                Some(&UnresolvedReason::requires_unresolved(id("b"))),
                Some(&drifted),
                Some(&touching(5)),
                Some(&drifted),
                Some(&drifted),
                None,
            ]
        );
    }

    /// Judging fates takes a bounded number of steps per operation, even
    /// where the unresolved operations fall in reverse position order: each
    /// requires the next, and only the last is unresolved to begin with.
    #[test]
    fn judging_fates_takes_a_bounded_number_of_steps_per_operation() {
        const OPERATIONS: usize = 400;
        let operations: Vec<Operation> = (0..OPERATIONS)
            .map(|position| {
                let operation =
                    editing(&format!("{position}.md")).with_id(id(&format!("op-{position}")));
                if position + 1 < OPERATIONS {
                    operation.with_requires(vec![id(&format!("op-{}", position + 1))])
                } else {
                    operation
                }
            })
            .collect();
        FATE_STEPS.with(|steps| steps.set(0));
        let fates = judged(operations, &[OPERATIONS - 1]);
        let steps = FATE_STEPS.with(std::cell::Cell::get);
        assert!(fates.iter().all(|fate| matches!(fate, Fate::Unresolved(_))));
        assert_eq!(
            reason(&fates[0]),
            Some(&UnresolvedReason::requires_unresolved(id("op-1")))
        );
        assert!(
            steps <= 16 * OPERATIONS,
            "{steps} steps to judge {OPERATIONS} operations"
        );
    }

    /// Judging fates takes a bounded number of steps per operation where
    /// every operation touches one file, and so each fall reaches the whole
    /// plan, once, not once per operation.
    #[test]
    fn judging_fates_takes_a_bounded_number_of_steps_where_all_operations_share_a_file() {
        const OPERATIONS: usize = 400;
        let operations: Vec<Operation> = (0..OPERATIONS).map(|_| editing("shared.md")).collect();
        FATE_STEPS.with(|steps| steps.set(0));
        let fates = judged(operations, &[OPERATIONS - 1]);
        let steps = FATE_STEPS.with(std::cell::Cell::get);
        assert!(fates.iter().all(|fate| matches!(fate, Fate::Unresolved(_))));
        assert!(
            steps <= 16 * OPERATIONS,
            "{steps} steps to judge {OPERATIONS} operations"
        );
    }

    /// An operation requiring several operations names the first of them
    /// that had already fallen when it fell, not one that fell after it.
    #[test]
    fn a_requirement_that_fell_after_the_operation_is_not_the_one_it_names() {
        let fates = judged(
            vec![
                editing("x.md").with_requires(vec![id("b"), id("c")]),
                editing("x.md").with_id(id("b")),
                editing("y.md").with_id(id("c")),
            ],
            &[2],
        );
        assert_eq!(
            reason(&fates[0]),
            Some(&UnresolvedReason::requires_unresolved(id("c")))
        );
        assert_eq!(reason(&fates[1]), Some(&touching(0)));
    }

    /// An operation sharing different files with several unresolved
    /// operations names the lowest position among them.
    #[test]
    fn an_operation_sharing_files_with_several_unresolved_names_the_lowest_position() {
        let fates = judged(
            vec![
                editing("b.md"),
                editing("a.md"),
                Operation::new(OperationKind::move_document(path("a.md"), path("b.md"))),
            ],
            &[0, 1],
        );
        assert_eq!(reason(&fates[2]), Some(&touching(0)));
    }
}
