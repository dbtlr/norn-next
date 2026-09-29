//! Planning: operations resolved against the vault into a resolved plan, or
//! refused for a fault in their shape.

use std::collections::BTreeMap;

use norn_wire::{
    AuthorCondition, AuthoredPlan, DocumentPath, FileState, Forecast, Operation, OperationsTag,
    PlanCondition, PlanFault, ResolvedPlan, RootIdentity, Transition, UnresolvedOperation,
    UnresolvedReason,
};

use super::compose::{Composition, compose, content_hash, touches};
use super::forecast::forecast;
use super::order::dependencies;
use super::view::VaultView;

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
pub(crate) fn resolve<V: VaultView>(
    authored: AuthoredPlan,
    root: RootIdentity,
    view: &V,
) -> Result<Resolution, PlanningFailure<V::Error>> {
    let AuthoredPlan {
        plan: OperationsTag,
        vault,
        operations,
        footnote,
    } = authored;
    let dependencies = dependencies(&operations).map_err(PlanningFailure::Fault)?;
    let mut left_out: BTreeMap<usize, UnresolvedReason> = BTreeMap::new();
    let (order, composition) = loop {
        let order = dependencies.order(|position| !left_out.contains_key(&position));
        let composition = compose(&operations, &order, view).map_err(PlanningFailure::View)?;
        let failed = failures(&operations, &order, &composition);
        if failed.is_empty() {
            break (order, composition);
        }
        left_out.extend(failed);
        leave_out_what_falls_with(&operations, &mut left_out);
    };
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
    let conditions = plan_conditions(&operations, &order, &transitions);
    let mut plan = ResolvedPlan::new(
        vault,
        root,
        kept(&operations, &left_out),
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
/// cannot act on, and each whose author observed a file the plan writes
/// holding what it no longer holds.
fn failures(
    operations: &[Operation],
    order: &[usize],
    composition: &Composition,
) -> BTreeMap<usize, UnresolvedReason> {
    let mut failed: BTreeMap<usize, UnresolvedReason> = composition
        .unresolvable
        .iter()
        .map(|unresolvable| {
            (
                unresolvable.position,
                UnresolvedReason::no_longer_resolves(unresolvable.detail.clone()),
            )
        })
        .collect();
    for &position in order {
        for AuthorCondition::ContentHash { path, hash } in &operations[position].conditions {
            let Some(target) = composition.targets.get(path) else {
                continue;
            };
            if target.before != FileState::present(hash.clone()) && !failed.contains_key(&position)
            {
                failed.insert(
                    position,
                    UnresolvedReason::no_longer_resolves(format!(
                        "`{path}` no longer holds the content its author observed"
                    )),
                );
            }
        }
    }
    failed
}

/// Leave out, with the operations already left out, every operation that
/// falls with one of them: one touching a file a left-out operation touches,
/// since operations on one file stand or fall together, and one requiring a
/// left-out operation, directly or through others.
fn leave_out_what_falls_with(
    operations: &[Operation],
    left_out: &mut BTreeMap<usize, UnresolvedReason>,
) {
    loop {
        let fallen_files: BTreeMap<&DocumentPath, usize> = left_out
            .keys()
            .flat_map(|&position| {
                touches(&operations[position].kind).map(move |path| (path, position))
            })
            .collect();
        let mut falling = BTreeMap::new();
        for (position, operation) in operations.iter().enumerate() {
            if left_out.contains_key(&position) {
                continue;
            }
            let required = operation.requires.iter().find(|required| {
                left_out
                    .keys()
                    .any(|&fallen| operations[fallen].id.as_ref() == Some(*required))
            });
            if let Some(required) = required {
                falling.insert(
                    position,
                    UnresolvedReason::requires_unresolved((*required).clone()),
                );
            } else if let Some((path, fallen)) = touches(&operation.kind)
                .find_map(|path| fallen_files.get(path).map(|&fallen| (path, fallen)))
            {
                falling.insert(
                    position,
                    UnresolvedReason::no_longer_resolves(format!(
                        "it touches `{path}`, as the unresolved operation at position {fallen} does, and operations on one file stand or fall together"
                    )),
                );
            }
        }
        if falling.is_empty() {
            return;
        }
        left_out.extend(falling);
    }
}

/// The author conditions of the operations that resolved on files the plan
/// does not write, each once, in the order the operations compose.
fn plan_conditions(
    operations: &[Operation],
    order: &[usize],
    transitions: &[Transition],
) -> Vec<PlanCondition> {
    let mut conditions: Vec<PlanCondition> = Vec::new();
    for &position in order {
        for AuthorCondition::ContentHash { path, hash } in &operations[position].conditions {
            let written = transitions
                .iter()
                .any(|transition| &transition.path == path);
            let condition = PlanCondition::content_hash(path.clone(), hash.clone());
            if !written && !conditions.contains(&condition) {
                conditions.push(condition);
            }
        }
    }
    conditions
}

/// The operations not left out, in plan order.
fn kept(operations: &[Operation], left_out: &BTreeMap<usize, UnresolvedReason>) -> Vec<Operation> {
    operations
        .iter()
        .enumerate()
        .filter(|(position, _)| !left_out.contains_key(position))
        .map(|(_, operation)| operation.clone())
        .collect()
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
        match resolve(authored(operations), root(), vault) {
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
        assert_eq!(resolution.plan.operations, operations);
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
            resolve(authored(operations), root(), &vault).map(|_| ()),
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
}
