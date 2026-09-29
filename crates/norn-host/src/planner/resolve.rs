//! Planning: operations resolved against the vault into a resolved plan, or
//! refused for a fault in their shape.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use norn_wire::{
    AuthorCondition, AuthoredPlan, ContentHash, DocumentPath, FileState, Forecast, Operation,
    OperationId, OperationsTag, PlanCondition, PlanFault, ResolvedPlan, RootIdentity, Transition,
    UnresolvedOperation, UnresolvedReason,
};

use super::compose::{Composition, compose, content_hash, touches};
use super::forecast::forecast;
use super::order::dependencies;
use super::view::{self, Remembered, VaultView};

/// What planning a plan came to.
#[derive(Debug)]
pub(crate) struct Resolution {
    /// The operations that resolved, their transitions and the conditions
    /// their planning read.
    pub(crate) plan: ResolvedPlan,
    /// What the plan does beyond its transitions.
    pub(crate) forecast: Forecast,
    /// Each operation left out of the plan, and why, in plan order.
    pub(crate) unresolved: Vec<UnresolvedOperation>,
}

/// Why a plan was not planned at all.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum PlanningFailure<E> {
    /// The plan's own shape is wrong: `request/plan-invalid`.
    Fault(PlanFault),
    /// The vault could not be read.
    View(E),
}

/// Plan `authored` against what `view` holds, under the root `root`.
///
/// **`met` is what a refresh already landed.** A refused apply's fresh plan
/// drops every operation whose targets all hold their after-states and
/// re-resolves the rest (ADR 0031), so an operation that remains may require
/// one that was dropped. `met` names those: a requirement on one is
/// satisfied, orders nothing, and is left off the operation the resolved plan
/// carries, so the fresh plan is a whole plan that can be sent back as it is.
/// An authored plan and a write verb's operation plan with nothing met.
pub(crate) fn resolve<V: VaultView>(
    authored: AuthoredPlan,
    root: RootIdentity,
    met: &BTreeSet<OperationId>,
    view: &V,
) -> Result<Resolution, PlanningFailure<V::Error>> {
    let AuthoredPlan {
        plan: OperationsTag,
        vault,
        operations,
        footnote,
    } = authored;
    let view = &Remembered::over(view);
    let dependencies = dependencies(&operations, met, view)?;
    let mut left_out: BTreeMap<usize, UnresolvedReason> = BTreeMap::new();
    let (order, composition) = loop {
        let order = dependencies.order(|position| !left_out.contains_key(&position));
        let composition = compose(&operations, &order, view).map_err(PlanningFailure::View)?;
        let failed =
            failures(&operations, &order, &composition, view).map_err(PlanningFailure::View)?;
        if failed.is_empty() {
            break (order, composition);
        }
        left_out.extend(failed);
        leave_out_what_falls_with(&operations, &mut left_out, view);
    };
    let conditions =
        plan_conditions(&operations, &order, &composition, view).map_err(PlanningFailure::View)?;
    let transitions = composition
        .targets
        .into_iter()
        .map(|(path, target)| {
            let after = match &target.after {
                Some(bytes) => FileState::present(content_hash(bytes)),
                None => FileState::absent(),
            };
            Transition::new(path, target.before, after)
        })
        .collect::<Vec<_>>();
    let carried: BTreeSet<&OperationId> = operations
        .iter()
        .filter_map(|operation| operation.id.as_ref())
        .collect();
    let mut plan = ResolvedPlan::new(
        vault,
        root,
        order
            .iter()
            .map(|&position| {
                let mut operation = operations[position].clone();
                operation
                    .requires
                    .retain(|id| carried.contains(id) || !met.contains(id));
                operation
            })
            .collect(),
        transitions,
        conditions,
    );
    plan.footnote = footnote;
    let unresolved = left_out
        .into_iter()
        .map(|(position, reason)| UnresolvedOperation::new(operations[position].clone(), reason))
        .collect();
    Ok(Resolution {
        forecast: forecast(&plan.transitions, view).map_err(PlanningFailure::View)?,
        plan,
        unresolved,
    })
}

/// The operations of this pass that did not resolve: each that met a state it
/// cannot act on, or, where every operation acted, each whose author observed
/// a file holding what it no longer holds.
///
/// **What fails because what it requires did not act names that
/// requirement.** An operation composed after one it requires that failed met
/// the state that one left, so its own failure says nothing its requirement's
/// does not: it is unresolved for requiring an unresolved operation.
///
/// **Author conditions wait for a pass where everything acted.** Until then
/// the pass's files include some only an operation that is about to be left
/// out touches, and a condition on one of those is judged against the file as
/// it stands rather than as a before-state.
///
/// **A condition is judged as the vault stands at the plan's after-state**
/// (ADR 0031): on a file the plan writes, against the before-state the plan
/// reads there, which it checks; on a file it does not write, against the file
/// as it stands now, which the plan does not change.
fn failures<V: VaultView>(
    operations: &[Operation],
    order: &[usize],
    composition: &Composition,
    view: &V,
) -> Result<BTreeMap<usize, UnresolvedReason>, V::Error> {
    if !composition.unresolvable.is_empty() {
        let failed: BTreeSet<usize> = composition
            .unresolvable
            .iter()
            .map(|unresolvable| unresolvable.position)
            .collect();
        let falling = requiring_closure(operations, &failed);
        let carriers = carriers(operations);
        return Ok(composition
            .unresolvable
            .iter()
            .map(|unresolvable| {
                let operation = &operations[unresolvable.position];
                let unmet = operation.requires.iter().find(|required| {
                    carriers
                        .get(required)
                        .is_some_and(|carrier| falling.contains(carrier))
                });
                let reason = match unmet {
                    Some(required) => UnresolvedReason::requires_unresolved(required.clone()),
                    None => UnresolvedReason::no_longer_resolves(unresolvable.detail.clone()),
                };
                (unresolvable.position, reason)
            })
            .collect());
    }
    let mut failed = BTreeMap::new();
    for &position in order {
        for AuthorCondition::ContentHash { path, hash } in &operations[position].conditions {
            if let Some(detail) = unmet_condition(path, hash, composition, view)? {
                failed
                    .entry(position)
                    .or_insert_with(|| UnresolvedReason::no_longer_resolves(detail));
            }
        }
    }
    Ok(failed)
}

/// Why the vault does not meet an author's condition that `path` holds
/// `hash`, or `None` where it does.
fn unmet_condition<V: VaultView>(
    path: &DocumentPath,
    hash: &ContentHash,
    composition: &Composition,
    view: &V,
) -> Result<Option<String>, V::Error> {
    let Ok(identity) = view.normalizer().normalize(Path::new(path.as_str())) else {
        return Ok(Some(format!("`{path}` names no document in the vault")));
    };
    if let Some(detail) = view::document_path(identity.as_path())
        .as_ref()
        .and_then(view::unholdable)
    {
        return Ok(Some(detail));
    }
    let holds = match composition.before(&identity) {
        Some(before) => *before == FileState::present(hash.clone()),
        None => match view.entry(&identity)? {
            // The plan records the condition at the spelling the tree lists,
            // so that spelling is the one the index must be able to hold.
            view::Entry::Document { at, .. } if let Some(detail) = view::unholdable(&at) => {
                return Ok(Some(detail));
            }
            view::Entry::Document { hash: held, .. } => held == *hash,
            _ => false,
        },
    };
    Ok((!holds).then(|| format!("`{path}` no longer holds the content its author observed")))
}

/// Where each identifier is carried.
fn carriers(operations: &[Operation]) -> BTreeMap<&OperationId, usize> {
    operations
        .iter()
        .enumerate()
        .filter_map(|(position, operation)| Some((operation.id.as_ref()?, position)))
        .collect()
}

/// `failed`, with every operation that requires one of them, directly or
/// through others.
fn requiring_closure(operations: &[Operation], failed: &BTreeSet<usize>) -> BTreeSet<usize> {
    let carriers = carriers(operations);
    let mut falling = failed.clone();
    loop {
        let more: Vec<usize> = operations
            .iter()
            .enumerate()
            .filter(|(position, operation)| {
                !falling.contains(position)
                    && operation.requires.iter().any(|required| {
                        carriers
                            .get(required)
                            .is_some_and(|carrier| falling.contains(carrier))
                    })
            })
            .map(|(position, _)| position)
            .collect();
        if more.is_empty() {
            return falling;
        }
        falling.extend(more);
    }
}

/// Leave out, with the operations already left out, every operation that
/// falls with one of them: one requiring a left-out operation, and one
/// touching a file a left-out operation touches, since operations on one file
/// stand or fall together — each directly or through others. A file is its
/// identity, so two spellings of one file are one file here too.
fn leave_out_what_falls_with<V: VaultView>(
    operations: &[Operation],
    left_out: &mut BTreeMap<usize, UnresolvedReason>,
    view: &V,
) {
    let identity = |path: &DocumentPath| view.normalizer().normalize(Path::new(path.as_str())).ok();
    let mut touching: BTreeMap<_, Vec<usize>> = BTreeMap::new();
    let mut requiring: BTreeMap<&OperationId, Vec<usize>> = BTreeMap::new();
    for (position, operation) in operations.iter().enumerate() {
        for path in touches(&operation.kind) {
            touching.entry(identity(path)).or_default().push(position);
        }
        for required in &operation.requires {
            requiring.entry(required).or_default().push(position);
        }
    }
    let mut fallen: Vec<usize> = left_out.keys().copied().collect();
    while let Some(position) = fallen.pop() {
        let operation = &operations[position];
        let by_requirement = operation
            .id
            .iter()
            .flat_map(|id| requiring.get(id).into_iter().flatten())
            .map(|&requirer| {
                let id = operation
                    .id
                    .clone()
                    .expect("a requirement names an identifier");
                (requirer, UnresolvedReason::requires_unresolved(id))
            });
        let by_file = touches(&operation.kind).flat_map(|path| {
            let sharers = match identity(path) {
                // A name with no identity is no file anybody else touches.
                None => None,
                some => touching.get(&some),
            };
            sharers.into_iter().flatten().map(move |&sharer| {
                (
                    sharer,
                    UnresolvedReason::no_longer_resolves(format!(
                        "it touches `{path}`, as the unresolved operation at position {position} does, and operations on one file stand or fall together"
                    )),
                )
            })
        });
        for (falling, reason) in by_requirement.chain(by_file).collect::<Vec<_>>() {
            if let Entry::Vacant(entry) = left_out.entry(falling) {
                entry.insert(reason);
                fallen.push(falling);
            }
        }
    }
}

/// The author conditions of the operations that resolved on files the plan
/// does not write, each once, at the spelling the tree lists, in the order
/// the operations compose. Each holds: [`failures`] left out every operation
/// whose condition does not.
fn plan_conditions<V: VaultView>(
    operations: &[Operation],
    order: &[usize],
    composition: &Composition,
    view: &V,
) -> Result<Vec<PlanCondition>, V::Error> {
    let mut conditions: Vec<PlanCondition> = Vec::new();
    for &position in order {
        for AuthorCondition::ContentHash { path, hash } in &operations[position].conditions {
            let Ok(identity) = view.normalizer().normalize(Path::new(path.as_str())) else {
                continue;
            };
            if composition.before(&identity).is_some() {
                continue;
            }
            let at = match view.entry(&identity)? {
                view::Entry::Document { at, .. } => at,
                _ => path.clone(),
            };
            let condition = PlanCondition::content_hash(at, hash.clone());
            if !conditions.contains(&condition) {
                conditions.push(condition);
            }
        }
    }
    Ok(conditions)
}

#[cfg(test)]
mod tests {
    use norn_wire::{
        AuthorCondition, DocumentPath, FileState, Operation, OperationId, OperationKind,
        PlanCondition, Transition, UnresolvedReason, VaultAddress, VaultName,
    };

    use super::super::compose::content_hash;
    use super::super::view::memory::MemoryVault;
    use super::*;

    fn path(text: &str) -> DocumentPath {
        DocumentPath::new(text).expect("a legal document path")
    }

    fn id(text: &str) -> OperationId {
        OperationId::new(text).expect("a legal identifier")
    }

    fn present(text: &str) -> FileState {
        FileState::present(content_hash(text.as_bytes()))
    }

    fn root() -> RootIdentity {
        RootIdentity::from_device_and_inode(1, 2)
    }

    fn authored(operations: Vec<Operation>) -> AuthoredPlan {
        let name = VaultName::new("notes").expect("a legal vault name");
        AuthoredPlan::new(VaultAddress::name(name), operations)
    }

    fn planned(vault: &MemoryVault, operations: Vec<Operation>) -> Resolution {
        match resolve(authored(operations), root(), &BTreeSet::new(), vault) {
            Ok(resolution) => resolution,
            Err(failure) => panic!("the plan resolves: {failure:?}"),
        }
    }

    fn transition(at: &str, before: FileState, after: FileState) -> Transition {
        Transition::new(path(at), before, after)
    }

    fn unresolved_positions(resolution: &Resolution, operations: &[Operation]) -> Vec<usize> {
        resolution
            .unresolved
            .iter()
            .map(|unresolved| {
                operations
                    .iter()
                    .position(|operation| *operation == unresolved.operation)
                    .expect("an unresolved operation is one of the plan's")
            })
            .collect()
    }

    #[test]
    fn a_create_is_a_transition_from_absent() {
        let resolution = planned(
            &MemoryVault::default(),
            vec![Operation::new(OperationKind::create_document(
                path("a.md"),
                "new",
            ))],
        );
        assert_eq!(
            resolution.plan.transitions,
            vec![transition("a.md", FileState::absent(), present("new"))]
        );
        assert!(resolution.unresolved.is_empty());
        assert_eq!(resolution.plan.root, root());
    }

    #[test]
    fn an_edit_is_a_transition_between_two_hashes() {
        let vault = MemoryVault::with(&[("a.md", "draft")]);
        let resolution = planned(
            &vault,
            vec![Operation::new(OperationKind::str_replace(
                path("a.md"),
                "draft",
                "final",
            ))],
        );
        assert_eq!(
            resolution.plan.transitions,
            vec![transition("a.md", present("draft"), present("final"))]
        );
    }

    #[test]
    fn a_delete_is_a_transition_to_absent() {
        let vault = MemoryVault::with(&[("a.md", "gone")]);
        let resolution = planned(
            &vault,
            vec![Operation::new(OperationKind::delete_document(path("a.md")))],
        );
        assert_eq!(
            resolution.plan.transitions,
            vec![transition("a.md", present("gone"), FileState::absent())]
        );
    }

    #[test]
    fn a_move_is_a_create_at_its_destination_and_a_removal_at_its_source() {
        let vault = MemoryVault::with(&[("a.md", "moved")]);
        let resolution = planned(
            &vault,
            vec![Operation::new(OperationKind::move_document(
                path("a.md"),
                path("b.md"),
            ))],
        );
        assert_eq!(
            resolution.plan.transitions,
            vec![
                transition("a.md", present("moved"), FileState::absent()),
                transition("b.md", FileState::absent(), present("moved")),
            ]
        );
    }

    #[test]
    fn a_move_into_a_destination_another_move_vacates_replaces_it() {
        let vault = MemoryVault::with(&[("a.md", "A"), ("b.md", "B")]);
        let operations = vec![
            Operation::new(OperationKind::move_document(path("a.md"), path("b.md"))),
            Operation::new(OperationKind::move_document(path("b.md"), path("c.md"))),
        ];
        let resolution = planned(&vault, operations.clone());
        assert!(resolution.unresolved.is_empty());
        // In the order they compose: the move vacating `b.md` first.
        assert_eq!(
            resolution.plan.operations,
            vec![operations[1].clone(), operations[0].clone()]
        );
        assert_eq!(
            resolution.plan.transitions,
            vec![
                transition("a.md", present("A"), FileState::absent()),
                transition("b.md", present("B"), present("A")),
                transition("c.md", FileState::absent(), present("B")),
            ]
        );
    }

    #[test]
    fn operations_on_one_file_stand_or_fall_together() {
        let vault = MemoryVault::with(&[("a.md", "one two"), ("b.md", "other")]);
        let operations = vec![
            Operation::new(OperationKind::str_replace(path("a.md"), "one", "1")),
            Operation::new(OperationKind::str_replace(path("a.md"), "gone", "x")),
            Operation::new(OperationKind::str_replace(path("b.md"), "other", "b")),
        ];
        let resolution = planned(&vault, operations.clone());
        assert_eq!(unresolved_positions(&resolution, &operations), vec![0, 1]);
        assert_eq!(resolution.plan.operations, vec![operations[2].clone()]);
        assert_eq!(
            resolution.plan.transitions,
            vec![transition("b.md", present("other"), present("b"))]
        );
    }

    #[test]
    fn a_move_falls_with_an_operation_on_either_of_its_files() {
        let vault = MemoryVault::with(&[("a.md", "A")]);
        let operations = vec![
            Operation::new(OperationKind::move_document(path("a.md"), path("b.md"))),
            Operation::new(OperationKind::str_replace(path("b.md"), "missing", "x")),
            Operation::new(OperationKind::str_replace(path("a.md"), "A", "a")),
        ];
        let resolution = planned(&vault, operations.clone());
        assert_eq!(
            unresolved_positions(&resolution, &operations),
            vec![0, 1, 2]
        );
        assert!(resolution.plan.transitions.is_empty());
    }

    #[test]
    fn a_missing_anchor_leaves_its_operation_unresolved_rather_than_refusing_the_plan() {
        let vault = MemoryVault::with(&[("a.md", "text")]);
        let operations = vec![Operation::new(OperationKind::str_replace(
            path("a.md"),
            "anchor",
            "x",
        ))];
        let resolution = planned(&vault, operations.clone());
        assert_eq!(resolution.unresolved.len(), 1);
        assert!(matches!(
            resolution.unresolved[0].reason,
            UnresolvedReason::NoLongerResolves { .. }
        ));
        assert!(resolution.plan.operations.is_empty());
    }

    #[test]
    fn an_operation_requiring_an_unresolved_one_is_unresolved_naming_it() {
        let vault = MemoryVault::with(&[("a.md", "text"), ("b.md", "b")]);
        let operations = vec![
            Operation::new(OperationKind::str_replace(path("a.md"), "anchor", "x"))
                .with_id(id("edit")),
            Operation::new(OperationKind::delete_document(path("b.md")))
                .with_requires(vec![id("edit")]),
        ];
        let resolution = planned(&vault, operations);
        assert_eq!(
            resolution.unresolved[1].reason,
            UnresolvedReason::requires_unresolved(id("edit"))
        );
        assert!(resolution.plan.transitions.is_empty());
    }

    #[test]
    fn a_fault_in_the_plan_shape_refuses_the_plan() {
        let vault = MemoryVault::with(&[("a.md", "A"), ("b.md", "B")]);
        let operations = vec![
            Operation::new(OperationKind::move_document(path("a.md"), path("b.md"))),
            Operation::new(OperationKind::move_document(path("b.md"), path("a.md"))),
        ];
        assert_eq!(
            resolve(authored(operations), root(), &BTreeSet::new(), &vault).map(|_| ()),
            Err(PlanningFailure::Fault(PlanFault::content_cycle(vec![0, 1])))
        );
    }

    #[test]
    fn an_author_condition_on_a_written_file_is_its_before_state_and_no_plan_condition() {
        let vault = MemoryVault::with(&[("a.md", "draft")]);
        let hash = content_hash(b"draft");
        let resolution = planned(
            &vault,
            vec![
                Operation::new(OperationKind::str_replace(path("a.md"), "draft", "final"))
                    .with_conditions(vec![AuthorCondition::content_hash(path("a.md"), hash)]),
            ],
        );
        assert!(resolution.unresolved.is_empty());
        assert!(resolution.plan.conditions.is_empty());
        assert_eq!(resolution.plan.transitions[0].before, present("draft"));
    }

    #[test]
    fn an_author_condition_a_written_file_no_longer_meets_leaves_the_operation_unresolved() {
        let vault = MemoryVault::with(&[("a.md", "draft, edited since")]);
        let resolution = planned(
            &vault,
            vec![
                Operation::new(OperationKind::str_replace(path("a.md"), "draft", "final"))
                    .with_conditions(vec![AuthorCondition::content_hash(
                        path("a.md"),
                        content_hash(b"draft"),
                    )]),
            ],
        );
        assert_eq!(resolution.unresolved.len(), 1);
        assert!(resolution.plan.transitions.is_empty());
    }

    #[test]
    fn an_author_condition_on_a_file_the_plan_does_not_write_is_a_plan_condition() {
        let vault = MemoryVault::with(&[("a.md", "draft"), ("c.md", "context")]);
        let hash = content_hash(b"context");
        let resolution = planned(
            &vault,
            vec![
                Operation::new(OperationKind::str_replace(path("a.md"), "draft", "final"))
                    .with_conditions(vec![AuthorCondition::content_hash(
                        path("c.md"),
                        hash.clone(),
                    )]),
            ],
        );
        assert_eq!(
            resolution.plan.conditions,
            vec![PlanCondition::content_hash(path("c.md"), hash)]
        );
        assert_eq!(resolution.plan.transitions.len(), 1);
    }

    #[test]
    fn a_resolved_plan_carries_no_bytes_its_operations_did_not_author() {
        let unauthored = "UNAUTHORED-BODY-THAT-MUST-NOT-TRAVEL";
        let vault = MemoryVault::with(&[
            ("a.md", &format!("{unauthored} anchor {unauthored}")),
            ("b.md", unauthored),
        ]);
        let resolution = planned(
            &vault,
            vec![
                Operation::new(OperationKind::str_replace(path("a.md"), "anchor", "edited")),
                Operation::new(OperationKind::move_document(path("b.md"), path("c.md"))),
            ],
        );
        // Every field of the plan, rendered: what it carries is what it shows.
        let carried = format!("{:?}", resolution.plan);
        assert!(!carried.contains(unauthored), "{carried}");
        assert!(carried.contains("edited"));
        assert_eq!(resolution.plan.transitions.len(), 3);
    }

    #[test]
    fn a_plan_is_forecast_with_the_folders_it_makes_and_removes() {
        let vault = MemoryVault::with(&[("inbox/a.md", "A")]);
        let resolution = planned(
            &vault,
            vec![Operation::new(OperationKind::move_document(
                path("inbox/a.md"),
                path("archive/a.md"),
            ))],
        );
        let folder = |text| norn_wire::FolderPath::new(text).expect("a legal folder path");
        assert_eq!(resolution.forecast.folders_made, vec![folder("archive")]);
        assert_eq!(resolution.forecast.folders_removed, vec![folder("inbox")]);
        assert!(resolution.forecast.drifted.is_empty());
    }

    #[test]
    fn an_author_condition_is_judged_against_the_files_the_resolved_plan_writes() {
        let vault = MemoryVault::with(&[("a.md", "draft"), ("c.md", "context")]);
        let observed = content_hash(b"context");
        let operations = vec![
            Operation::new(OperationKind::str_replace(path("c.md"), "missing", "x")),
            Operation::new(OperationKind::str_replace(path("a.md"), "draft", "final"))
                .with_conditions(vec![AuthorCondition::content_hash(
                    path("c.md"),
                    observed.clone(),
                )]),
        ];
        let resolution = planned(&vault, operations.clone());
        assert_eq!(unresolved_positions(&resolution, &operations), vec![0]);
        assert_eq!(
            resolution.plan.conditions,
            vec![PlanCondition::content_hash(path("c.md"), observed)]
        );
    }

    #[test]
    fn a_resolved_plan_planned_again_is_the_same_plan() {
        let vault = MemoryVault::with(&[("a.md", "A"), ("b.md", "B"), ("d.md", "D")]);
        let operations = vec![
            Operation::new(OperationKind::move_document(path("a.md"), path("b.md"))),
            Operation::new(OperationKind::str_replace(path("d.md"), "D", "d")).with_id(id("d")),
            Operation::new(OperationKind::move_document(path("b.md"), path("c.md")))
                .with_requires(vec![id("d")]),
        ];
        let first = planned(&vault, operations);
        let again = planned(&vault, first.plan.operations.clone());
        assert_eq!(again.plan, first.plan);
    }

    fn moving(from: &str, to: &str) -> Operation {
        Operation::new(OperationKind::move_document(path(from), path(to)))
    }

    fn creating(at: &str, content: &str) -> Operation {
        Operation::new(OperationKind::create_document(path(at), content))
    }

    fn deleting(at: &str) -> Operation {
        Operation::new(OperationKind::delete_document(path(at)))
    }

    fn detail_of(resolution: &Resolution, index: usize) -> String {
        match &resolution.unresolved[index].reason {
            UnresolvedReason::NoLongerResolves { detail, .. } => detail.clone(),
            other => panic!("the operation no longer resolves: {other:?}"),
        }
    }

    #[test]
    fn a_move_onto_an_absent_name_waits_for_nothing_that_vacates_it() {
        let vault = MemoryVault::with(&[("a.md", "A")]);
        let resolution = planned(&vault, vec![moving("a.md", "b.md"), moving("b.md", "c.md")]);
        assert!(
            resolution.unresolved.is_empty(),
            "{:?}",
            resolution.unresolved
        );
        assert_eq!(
            resolution.plan.transitions,
            vec![
                transition("a.md", present("A"), FileState::absent()),
                transition("b.md", FileState::absent(), FileState::absent()),
                transition("c.md", FileState::absent(), present("A")),
            ]
        );
    }

    #[test]
    fn a_move_onto_an_absent_name_a_later_removal_names_resolves() {
        let vault = MemoryVault::with(&[("a.md", "A")]);
        let resolution = planned(&vault, vec![moving("a.md", "b.md"), deleting("b.md")]);
        assert!(
            resolution.unresolved.is_empty(),
            "{:?}",
            resolution.unresolved
        );
    }

    #[test]
    fn two_documents_exchange_places_through_an_absent_name() {
        let vault = MemoryVault::with(&[("a.md", "A"), ("b.md", "B")]);
        let resolution = planned(
            &vault,
            vec![
                moving("a.md", "t.md"),
                moving("b.md", "a.md"),
                moving("t.md", "b.md"),
            ],
        );
        assert!(
            resolution.unresolved.is_empty(),
            "{:?}",
            resolution.unresolved
        );
        assert_eq!(
            resolution.plan.transitions,
            vec![
                transition("a.md", present("A"), present("B")),
                transition("b.md", present("B"), present("A")),
                transition("t.md", FileState::absent(), FileState::absent()),
            ]
        );
    }

    #[test]
    fn a_create_at_a_name_a_move_vacates_runs_after_the_move() {
        let vault = MemoryVault::with(&[("a.md", "old")]);
        let operations = vec![creating("a.md", "new"), moving("a.md", "b.md")];
        let resolution = planned(&vault, operations.clone());
        assert!(
            resolution.unresolved.is_empty(),
            "{:?}",
            resolution.unresolved
        );
        assert_eq!(
            resolution.plan.operations,
            vec![operations[1].clone(), operations[0].clone()]
        );
        assert_eq!(
            resolution.plan.transitions,
            vec![
                transition("a.md", present("old"), present("new")),
                transition("b.md", FileState::absent(), present("old")),
            ]
        );
    }

    #[test]
    fn a_move_away_from_a_name_a_move_fills_moves_the_document_that_arrived() {
        let vault = MemoryVault::with(&[("a.md", "A"), ("b.md", "B")]);
        let operations = vec![
            moving("b.md", "x.md"),
            moving("a.md", "b.md"),
            moving("b.md", "y.md"),
        ];
        let resolution = planned(&vault, operations.clone());
        assert!(
            resolution.unresolved.is_empty(),
            "{:?}",
            resolution.unresolved
        );
        assert_eq!(resolution.plan.operations, operations);
        assert_eq!(
            resolution.plan.transitions,
            vec![
                transition("a.md", present("A"), FileState::absent()),
                transition("b.md", present("B"), FileState::absent()),
                transition("x.md", FileState::absent(), present("B")),
                transition("y.md", FileState::absent(), present("A")),
            ]
        );
    }

    #[test]
    fn a_removal_after_a_create_over_a_removed_name_removes_the_created_document() {
        let vault = MemoryVault::with(&[("a.md", "A")]);
        let operations = vec![deleting("a.md"), creating("a.md", "new"), deleting("a.md")];
        let resolution = planned(&vault, operations.clone());
        assert!(
            resolution.unresolved.is_empty(),
            "{:?}",
            resolution.unresolved
        );
        assert_eq!(resolution.plan.operations, operations);
        assert_eq!(
            resolution.plan.transitions,
            vec![transition("a.md", present("A"), FileState::absent())]
        );
    }

    #[test]
    fn each_vacater_of_a_name_vacates_the_occupant_its_place_in_plan_order_pairs_it_with() {
        let vault = MemoryVault::with(&[("a.md", "old")]);
        let operations = vec![
            creating("a.md", "new"),
            moving("a.md", "b.md"),
            moving("a.md", "c.md"),
        ];
        let resolution = planned(&vault, operations.clone());
        assert!(
            resolution.unresolved.is_empty(),
            "{:?}",
            resolution.unresolved
        );
        // The first move vacates the document standing at planning, the
        // create fills the name, and the second move carries what it made.
        assert_eq!(
            resolution.plan.operations,
            vec![
                operations[1].clone(),
                operations[0].clone(),
                operations[2].clone()
            ]
        );
        assert_eq!(
            resolution.plan.transitions,
            vec![
                transition("a.md", present("old"), FileState::absent()),
                transition("b.md", FileState::absent(), present("old")),
                transition("c.md", FileState::absent(), present("new")),
            ]
        );
    }

    #[test]
    fn a_removal_authored_before_a_create_at_an_empty_name_does_not_resolve() {
        let operations = vec![deleting("e.md"), creating("e.md", "new")];
        let resolution = planned(&MemoryVault::default(), operations.clone());
        assert_eq!(unresolved_positions(&resolution, &operations), vec![0, 1]);
        assert!(resolution.plan.transitions.is_empty());
    }

    #[test]
    fn a_removal_of_an_arrived_document_waits_for_the_arrival_it_removes() {
        let operations = vec![
            creating("e.md", "new").with_requires(vec![id("z")]),
            deleting("e.md"),
            creating("z.md", "z").with_id(id("z")),
        ];
        let resolution = planned(&MemoryVault::default(), operations.clone());
        assert_eq!(
            unresolved_positions(&resolution, &operations),
            Vec::<usize>::new()
        );
        assert_eq!(
            resolution.plan.transitions,
            vec![
                transition("e.md", FileState::absent(), FileState::absent()),
                transition("z.md", FileState::absent(), present("z")),
            ]
        );
    }

    #[test]
    fn a_move_onto_itself_is_unresolved_rather_than_a_cycle() {
        let vault = MemoryVault::with(&[("a.md", "A")]);
        let resolution = planned(&vault, vec![moving("a.md", "a.md")]);
        assert_eq!(resolution.unresolved.len(), 1);
        assert!(
            detail_of(&resolution, 0).contains("onto itself"),
            "{}",
            detail_of(&resolution, 0)
        );
        assert!(resolution.plan.transitions.is_empty());
    }

    #[test]
    fn a_plan_condition_two_operations_carry_is_carried_once() {
        let vault = MemoryVault::with(&[("a.md", "A"), ("b.md", "B"), ("c.md", "C")]);
        let observed = AuthorCondition::content_hash(path("c.md"), content_hash(b"C"));
        let resolution = planned(
            &vault,
            vec![
                Operation::new(OperationKind::str_replace(path("a.md"), "A", "a"))
                    .with_conditions(vec![observed.clone()]),
                Operation::new(OperationKind::str_replace(path("b.md"), "B", "b"))
                    .with_conditions(vec![observed]),
            ],
        );
        assert_eq!(
            resolution.plan.conditions,
            vec![PlanCondition::content_hash(
                path("c.md"),
                content_hash(b"C")
            )]
        );
    }

    #[test]
    fn a_plan_condition_the_vault_no_longer_meets_leaves_its_operation_unresolved() {
        let vault = MemoryVault::with(&[("a.md", "A"), ("c.md", "changed since")]);
        let operations = vec![
            Operation::new(OperationKind::str_replace(path("a.md"), "A", "a")).with_conditions(
                vec![AuthorCondition::content_hash(
                    path("c.md"),
                    content_hash(b"C"),
                )],
            ),
        ];
        let resolution = planned(&vault, operations.clone());
        assert_eq!(unresolved_positions(&resolution, &operations), vec![0]);
        assert!(resolution.plan.conditions.is_empty());
        assert!(resolution.plan.transitions.is_empty());
    }

    #[test]
    fn of_two_conditions_on_one_unwritten_file_only_the_one_it_meets_resolves() {
        let vault = MemoryVault::with(&[("a.md", "A"), ("b.md", "B"), ("c.md", "C")]);
        let operations = vec![
            Operation::new(OperationKind::str_replace(path("a.md"), "A", "a")).with_conditions(
                vec![AuthorCondition::content_hash(
                    path("c.md"),
                    content_hash(b"old C"),
                )],
            ),
            Operation::new(OperationKind::str_replace(path("b.md"), "B", "b")).with_conditions(
                vec![AuthorCondition::content_hash(
                    path("c.md"),
                    content_hash(b"C"),
                )],
            ),
        ];
        let resolution = planned(&vault, operations.clone());
        assert_eq!(unresolved_positions(&resolution, &operations), vec![0]);
        assert_eq!(
            resolution.plan.conditions,
            vec![PlanCondition::content_hash(
                path("c.md"),
                content_hash(b"C")
            )]
        );
    }

    #[test]
    fn an_operation_that_fails_because_what_it_requires_did_not_act_names_that_requirement() {
        let vault = MemoryVault::with(&[("a.md", "A")]);
        let operations = vec![
            Operation::new(OperationKind::str_replace(path("a.md"), "missing", "x"))
                .with_id(id("edit")),
            Operation::new(OperationKind::str_replace(path("a.md"), "x", "y"))
                .with_requires(vec![id("edit")]),
        ];
        let resolution = planned(&vault, operations);
        assert_eq!(
            resolution.unresolved[1].reason,
            UnresolvedReason::requires_unresolved(id("edit"))
        );
    }

    #[test]
    fn a_create_onto_a_folder_does_not_resolve() {
        let vault = MemoryVault::with(&[("a/x.md", "x")]);
        let resolution = planned(&vault, vec![creating("a", "new")]);
        assert_eq!(resolution.unresolved.len(), 1);
        assert!(
            detail_of(&resolution, 0).contains("folder"),
            "{}",
            detail_of(&resolution, 0)
        );
    }

    #[test]
    fn a_move_onto_a_folder_does_not_resolve() {
        let vault = MemoryVault::with(&[("a/x.md", "x"), ("b.md", "B")]);
        let resolution = planned(&vault, vec![moving("b.md", "a")]);
        assert_eq!(resolution.unresolved.len(), 1);
        assert!(
            detail_of(&resolution, 0).contains("folder"),
            "{}",
            detail_of(&resolution, 0)
        );
    }

    #[test]
    fn a_create_beneath_a_document_does_not_resolve() {
        let vault = MemoryVault::with(&[("a.md", "A")]);
        let resolution = planned(&vault, vec![creating("a.md/b.md", "new")]);
        assert_eq!(resolution.unresolved.len(), 1);
        assert!(
            detail_of(&resolution, 0).contains("beneath"),
            "{}",
            detail_of(&resolution, 0)
        );
    }

    #[test]
    fn a_create_beneath_a_document_the_plan_removes_does_not_resolve() {
        // Creates publish before removals, so the document still stands when
        // the create would make a folder of its name.
        let vault = MemoryVault::with(&[("a.md", "A")]);
        let operations = vec![deleting("a.md"), creating("a.md/b.md", "new")];
        let resolution = planned(&vault, operations.clone());
        assert_eq!(unresolved_positions(&resolution, &operations), vec![1]);
        assert_eq!(
            resolution.plan.transitions,
            vec![transition("a.md", present("A"), FileState::absent())]
        );
    }

    #[test]
    fn a_create_beneath_a_document_the_plan_creates_does_not_resolve() {
        let vault = MemoryVault::default();
        for operations in [
            vec![creating("a.md", "A"), creating("a.md/b.md", "B")],
            vec![creating("a.md/b.md", "B"), creating("a.md", "A")],
        ] {
            let resolution = planned(&vault, operations.clone());
            assert_eq!(resolution.unresolved.len(), 1, "{operations:?}");
        }
    }

    #[test]
    fn a_requirement_an_earlier_apply_met_orders_nothing_and_leaves_a_whole_plan() {
        let vault = MemoryVault::with(&[("b.md", "B")]);
        let operations = vec![
            Operation::new(OperationKind::str_replace(path("b.md"), "B", "b"))
                .with_requires(vec![id("landed")]),
        ];
        assert_eq!(
            resolve(
                authored(operations.clone()),
                root(),
                &BTreeSet::new(),
                &vault
            )
            .map(|_| ()),
            Err(PlanningFailure::Fault(PlanFault::unknown_requirement(
                0,
                id("landed")
            )))
        );
        let met = BTreeSet::from([id("landed")]);
        let fresh = match resolve(authored(operations), root(), &met, &vault) {
            Ok(fresh) => fresh,
            Err(failure) => panic!("a met requirement is no fault: {failure:?}"),
        };
        assert!(fresh.unresolved.is_empty());
        assert!(fresh.plan.operations[0].requires.is_empty());
        // Sent back as it is, the fresh plan plans as itself.
        assert_eq!(
            planned(&vault, fresh.plan.operations.clone()).plan,
            fresh.plan
        );
    }

    #[test]
    fn a_case_only_rename_on_a_folding_root_is_a_move_between_two_spellings() {
        let vault = MemoryVault::with(&[("Notes.md", "N")]).folding_case();
        let resolution = planned(&vault, vec![moving("Notes.md", "notes.md")]);
        assert!(
            resolution.unresolved.is_empty(),
            "{:?}",
            resolution.unresolved
        );
        assert_eq!(
            resolution.plan.transitions,
            vec![
                transition("Notes.md", present("N"), FileState::absent()),
                transition("notes.md", FileState::absent(), present("N")),
            ]
        );
    }

    #[test]
    fn an_operation_after_a_case_only_rename_acts_on_the_new_spelling() {
        let vault = MemoryVault::with(&[("A.md", "x")]).folding_case();
        let resolution = planned(
            &vault,
            vec![
                moving("A.md", "a.md"),
                Operation::new(OperationKind::str_replace(path("A.md"), "x", "y")),
            ],
        );
        assert!(
            resolution.unresolved.is_empty(),
            "{:?}",
            resolution.unresolved
        );
        assert_eq!(
            resolution.plan.transitions,
            vec![
                transition("A.md", present("x"), FileState::absent()),
                transition("a.md", FileState::absent(), present("y")),
            ]
        );
    }

    #[test]
    fn after_a_case_only_rename_the_document_can_be_removed_at_its_new_spelling() {
        let vault = MemoryVault::with(&[("Notes.md", "N")]).folding_case();
        let resolution = planned(
            &vault,
            vec![moving("Notes.md", "notes.md"), deleting("notes.md")],
        );
        assert!(
            resolution.unresolved.is_empty(),
            "{:?}",
            resolution.unresolved
        );
        assert_eq!(
            resolution.plan.transitions,
            vec![
                transition("Notes.md", present("N"), FileState::absent()),
                transition("notes.md", FileState::absent(), FileState::absent()),
            ]
        );
    }

    #[test]
    fn after_a_case_only_rename_the_document_can_be_moved_on() {
        let vault = MemoryVault::with(&[("Notes.md", "N")]).folding_case();
        let resolution = planned(
            &vault,
            vec![moving("Notes.md", "notes.md"), moving("notes.md", "b.md")],
        );
        assert!(
            resolution.unresolved.is_empty(),
            "{:?}",
            resolution.unresolved
        );
        assert_eq!(
            resolution.plan.transitions,
            vec![
                transition("Notes.md", present("N"), FileState::absent()),
                transition("b.md", FileState::absent(), present("N")),
                transition("notes.md", FileState::absent(), FileState::absent()),
            ]
        );
    }

    #[test]
    fn a_case_only_rename_and_its_rename_back_resolve_as_a_chain_through_an_absent_name() {
        let vault = MemoryVault::with(&[("Notes.md", "N")]).folding_case();
        let resolution = planned(
            &vault,
            vec![
                moving("Notes.md", "notes.md"),
                moving("notes.md", "Notes.md"),
            ],
        );
        assert!(
            resolution.unresolved.is_empty(),
            "{:?}",
            resolution.unresolved
        );
        assert_eq!(
            resolution.plan.transitions,
            vec![
                transition("Notes.md", present("N"), present("N")),
                transition("notes.md", FileState::absent(), FileState::absent()),
            ]
        );
    }

    #[test]
    fn on_a_folding_root_two_spellings_of_one_document_compose_as_one_target() {
        let vault = MemoryVault::with(&[("A.md", "one two")]).folding_case();
        let resolution = planned(
            &vault,
            vec![
                Operation::new(OperationKind::str_replace(path("A.md"), "one", "1")),
                Operation::new(OperationKind::str_replace(path("a.md"), "two", "2")),
            ],
        );
        assert!(
            resolution.unresolved.is_empty(),
            "{:?}",
            resolution.unresolved
        );
        assert_eq!(
            resolution.plan.transitions,
            vec![transition("A.md", present("one two"), present("1 2"))]
        );
    }

    #[test]
    fn on_a_folding_root_a_create_at_another_spelling_of_a_document_does_not_resolve() {
        let vault = MemoryVault::with(&[("A.md", "A")]).folding_case();
        let resolution = planned(&vault, vec![creating("a.md", "new")]);
        assert!(
            detail_of(&resolution, 0).contains("a document stands"),
            "{}",
            detail_of(&resolution, 0)
        );
    }

    #[test]
    fn on_a_root_that_tells_case_apart_two_spellings_are_two_documents() {
        let vault = MemoryVault::with(&[("A.md", "A"), ("a.md", "a")]);
        let resolution = planned(&vault, vec![deleting("a.md")]);
        assert_eq!(
            resolution.plan.transitions,
            vec![transition("a.md", present("a"), FileState::absent())]
        );
        let resolution = planned(&vault, vec![moving("A.md", "a.md")]);
        assert!(
            detail_of(&resolution, 0).contains("a document stands"),
            "{}",
            detail_of(&resolution, 0)
        );
    }

    #[test]
    fn a_folder_s_change_of_case_is_left_unresolved() {
        let vault = MemoryVault::with(&[("Notes/a.md", "A")]).folding_case();
        for operation in [
            moving("Notes/a.md", "notes/a.md"),
            creating("notes/b.md", "B"),
        ] {
            let resolution = planned(&vault, vec![operation]);
            assert_eq!(resolution.unresolved.len(), 1);
            assert!(
                detail_of(&resolution, 0).contains("a folder's change of case is not planned"),
                "{}",
                detail_of(&resolution, 0)
            );
        }
    }

    #[test]
    fn a_folder_the_plan_makes_spelled_a_second_way_is_left_unresolved() {
        let vault = MemoryVault::with(&[("a.md", "A")]).folding_case();
        for second in [creating("dir/y.md", "Y"), moving("a.md", "dir/a.md")] {
            let resolution = planned(&vault, vec![creating("Dir/x.md", "X"), second]);
            assert_eq!(
                resolution.unresolved.len(),
                1,
                "{:?}",
                resolution.unresolved
            );
            assert!(
                detail_of(&resolution, 0).contains("a folder's change of case is not planned"),
                "{}",
                detail_of(&resolution, 0)
            );
            assert_eq!(
                resolution.plan.transitions,
                vec![transition("Dir/x.md", FileState::absent(), present("X"))]
            );
        }
    }

    #[test]
    fn a_new_folder_beneath_a_plan_made_folder_spelled_a_second_way_is_left_unresolved() {
        let vault = MemoryVault::with(&[]).folding_case();
        let resolution = planned(
            &vault,
            vec![creating("D/x.md", "X"), creating("d/E/y.md", "Y")],
        );
        assert_eq!(
            resolution.unresolved.len(),
            1,
            "{:?}",
            resolution.unresolved
        );
        assert!(
            detail_of(&resolution, 0).contains("a folder's change of case is not planned"),
            "{}",
            detail_of(&resolution, 0)
        );
    }

    #[test]
    fn a_plan_over_a_tree_on_disk_reads_it_through_the_vault_s_own_descent() {
        use super::super::view::TreeView;
        let scratch = norn_testkit::scratch::Scratch::new("planner-tree");
        #[allow(clippy::disallowed_methods)] // Harness scaffolding: the tree a case plans over.
        let place = |at: &str, content: &str| {
            let full = scratch.join(at);
            std::fs::create_dir_all(full.parent().expect("a parent")).expect("folders");
            std::fs::write(full, content).expect("a file");
        };
        place("inbox/a.md", "A");
        place("b.md", "B");
        place("folder/x.md", "X");
        let view = TreeView::open(scratch.root(), &[]).expect("a vault");
        let operations = vec![
            moving("inbox/a.md", "archive/a.md"),
            Operation::new(OperationKind::str_replace(path("b.md"), "B", "b")),
            creating("b.md/c.md", "beneath a document"),
            creating("folder", "onto a folder"),
        ];
        let resolution = match resolve(
            authored(operations.clone()),
            root(),
            &BTreeSet::new(),
            &view,
        ) {
            Ok(resolution) => resolution,
            Err(failure) => panic!("the plan resolves: {failure:?}"),
        };
        assert_eq!(unresolved_positions(&resolution, &operations), vec![2, 3]);
        assert_eq!(
            resolution.plan.transitions,
            vec![
                transition("archive/a.md", FileState::absent(), present("A")),
                transition("b.md", present("B"), present("b")),
                transition("inbox/a.md", present("A"), FileState::absent()),
            ]
        );
        let folder = |text| norn_wire::FolderPath::new(text).expect("a legal folder path");
        assert_eq!(resolution.forecast.folders_made, vec![folder("archive")]);
        assert_eq!(resolution.forecast.folders_removed, vec![folder("inbox")]);
    }

    #[test]
    fn each_file_is_read_once_however_many_passes_planning_takes() {
        let vault = MemoryVault::with(&[("a.md", "A"), ("x.md", "X")]);
        let operations = vec![
            Operation::new(OperationKind::str_replace(path("x.md"), "missing", "y")),
            Operation::new(OperationKind::str_replace(path("a.md"), "A", "a")),
        ];
        let resolution = planned(&vault, operations);
        assert_eq!(resolution.unresolved.len(), 1);
        let reads = vault.reads.borrow();
        assert!(reads.values().all(|&count| count == 1), "{reads:?}");
    }

    /// Spellings the wire reads as document paths and the index cannot hold:
    /// a backslash, a control byte, and a leaf whose stem is `.`.
    const UNHOLDABLE: [&str; 3] = ["a\\b.md", "a\u{1}.md", "..md"];

    fn is_unholdable_detail(resolution: &Resolution, index: usize, at: &str) {
        let detail = detail_of(resolution, index);
        assert!(
            detail.contains(at) && detail.contains("is not a document path"),
            "{detail}"
        );
    }

    #[test]
    fn a_create_at_a_path_the_index_cannot_hold_does_not_resolve() {
        for at in UNHOLDABLE {
            let resolution = planned(&MemoryVault::default(), vec![creating(at, "new")]);
            assert_eq!(resolution.unresolved.len(), 1, "{at}");
            is_unholdable_detail(&resolution, 0, at);
            assert!(resolution.plan.transitions.is_empty(), "{at}");
        }
    }

    #[test]
    fn an_edit_at_a_path_the_index_cannot_hold_does_not_resolve() {
        for at in UNHOLDABLE {
            let vault = MemoryVault::with(&[(at, "text")]);
            let resolution = planned(
                &vault,
                vec![Operation::new(OperationKind::str_replace(
                    path(at),
                    "text",
                    "edited",
                ))],
            );
            assert_eq!(resolution.unresolved.len(), 1, "{at}");
            is_unholdable_detail(&resolution, 0, at);
        }
    }

    #[test]
    fn a_removal_at_a_path_the_index_cannot_hold_does_not_resolve() {
        for at in UNHOLDABLE {
            let vault = MemoryVault::with(&[(at, "text")]);
            let resolution = planned(&vault, vec![deleting(at)]);
            assert_eq!(resolution.unresolved.len(), 1, "{at}");
            is_unholdable_detail(&resolution, 0, at);
        }
    }

    #[test]
    fn a_move_from_a_path_the_index_cannot_hold_does_not_resolve() {
        for at in UNHOLDABLE {
            let vault = MemoryVault::with(&[(at, "text")]);
            let resolution = planned(&vault, vec![moving(at, "b.md")]);
            assert_eq!(resolution.unresolved.len(), 1, "{at}");
            is_unholdable_detail(&resolution, 0, at);
            assert!(resolution.plan.transitions.is_empty(), "{at}");
        }
    }

    #[test]
    fn a_move_to_a_path_the_index_cannot_hold_does_not_resolve() {
        for at in UNHOLDABLE {
            let vault = MemoryVault::with(&[("a.md", "text")]);
            let resolution = planned(&vault, vec![moving("a.md", at)]);
            assert_eq!(resolution.unresolved.len(), 1, "{at}");
            is_unholdable_detail(&resolution, 0, at);
            assert!(resolution.plan.transitions.is_empty(), "{at}");
        }
    }

    #[test]
    fn a_condition_on_a_path_the_index_cannot_hold_does_not_resolve() {
        for at in UNHOLDABLE {
            let vault = MemoryVault::with(&[("a.md", "draft"), (at, "context")]);
            let resolution = planned(
                &vault,
                vec![
                    Operation::new(OperationKind::str_replace(path("a.md"), "draft", "final"))
                        .with_conditions(vec![AuthorCondition::content_hash(
                            path(at),
                            content_hash(b"context"),
                        )]),
                ],
            );
            assert_eq!(resolution.unresolved.len(), 1, "{at}");
            is_unholdable_detail(&resolution, 0, at);
            assert!(resolution.plan.conditions.is_empty(), "{at}");
        }
    }

    /// A condition is recorded at the spelling the tree lists, so that
    /// spelling, not the one the author asked with, is the one judged.
    #[test]
    fn a_condition_on_a_document_listed_at_a_spelling_the_index_cannot_hold_does_not_resolve() {
        let vault = MemoryVault::with(&[("a.md", "draft"), ("c/./d.md", "context")]);
        let resolution = planned(
            &vault,
            vec![
                Operation::new(OperationKind::str_replace(path("a.md"), "draft", "final"))
                    .with_conditions(vec![AuthorCondition::content_hash(
                        path("c/d.md"),
                        content_hash(b"context"),
                    )]),
            ],
        );
        assert_eq!(resolution.unresolved.len(), 1, "{:?}", resolution.plan);
        assert!(resolution.plan.conditions.is_empty());
    }

    /// The index keys a document by its normalized spelling, so a spelling
    /// the index refuses as written resolves where it normalizes to one the
    /// index holds.
    #[test]
    fn a_path_that_normalizes_to_one_the_index_holds_resolves() {
        let vault = MemoryVault::with(&[("a/c.md", "C")]);
        let resolution = planned(
            &vault,
            vec![
                creating("./a//b.md", "new"),
                Operation::new(OperationKind::str_replace(path("a//c.md"), "C", "c")),
            ],
        );
        assert!(
            resolution.unresolved.is_empty(),
            "{:?}",
            resolution.unresolved
        );
        assert_eq!(
            resolution.plan.transitions,
            vec![
                transition("a/b.md", FileState::absent(), present("new")),
                transition("a/c.md", present("C"), present("c")),
            ]
        );
    }
}
