//! The plan's shape: which operation runs after which, and the faults that
//! make a plan's shape wrong whatever vault it is for.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};

use norn_wire::{DocumentPath, Operation, OperationId, OperationKind, PlanFault};

/// Which operations each operation runs after, by position.
///
/// **Two kinds of requirement.** An operation runs after the operations its
/// `requires` names. A move also runs after every other operation that
/// vacates its destination — a move away from it, or its removal — because
/// ADR 0031 lets a move's destination be one another operation of the same
/// plan vacates, and the move then requires it. The second kind is read from
/// the plan's shape alone, so a plan's order and its faults are the same
/// whatever vault it is planned against.
pub(crate) struct Dependencies {
    /// For each position, the positions it runs after, ascending.
    after: Vec<Vec<usize>>,
}

/// The dependencies of `operations`, or the first fault in their shape.
///
/// Faults are looked for in the order [`PlanFault`] lists them: an identifier
/// carried twice, a requirement nothing carries, a cycle of `requires`, and
/// last a cycle only a vacated destination closes, which is a content cycle.
pub(crate) fn dependencies(operations: &[Operation]) -> Result<Dependencies, PlanFault> {
    let carriers = carriers(operations)?;
    let explicit = explicit_requirements(operations, &carriers)?;
    if let Some(cycle) = first_cycle(&explicit) {
        return Err(PlanFault::requires_cycle(cycle));
    }
    let after: Vec<Vec<usize>> = explicit
        .into_iter()
        .zip(vacating_requirements(operations))
        .map(|(mut explicit, vacating)| {
            explicit.extend(vacating);
            explicit.sort_unstable();
            explicit.dedup();
            explicit
        })
        .collect();
    if let Some(cycle) = first_cycle(&after) {
        return Err(PlanFault::content_cycle(cycle));
    }
    Ok(Dependencies { after })
}

impl Dependencies {
    /// The positions `included` keeps, each after every one it runs after
    /// and otherwise in plan order.
    ///
    /// A requirement on a position `included` drops is dropped with it: an
    /// operation left out of a plan is left out with everything that requires
    /// it, so what remains is what runs.
    pub(crate) fn order(&self, included: impl Fn(usize) -> bool) -> Vec<usize> {
        let count = self.after.len();
        let mut waiting_on = vec![0usize; count];
        let mut required_by = vec![Vec::new(); count];
        for (position, after) in self.after.iter().enumerate() {
            if !included(position) {
                continue;
            }
            for &required in after.iter().filter(|&&required| included(required)) {
                waiting_on[position] += 1;
                required_by[required].push(position);
            }
        }
        let mut ready: BinaryHeap<Reverse<usize>> = (0..count)
            .filter(|&position| included(position) && waiting_on[position] == 0)
            .map(Reverse)
            .collect();
        let mut order = Vec::with_capacity(count);
        while let Some(Reverse(position)) = ready.pop() {
            order.push(position);
            for &dependent in &required_by[position] {
                waiting_on[dependent] -= 1;
                if waiting_on[dependent] == 0 {
                    ready.push(Reverse(dependent));
                }
            }
        }
        order
    }
}

/// Where each identifier is carried, refusing one carried twice.
fn carriers(operations: &[Operation]) -> Result<BTreeMap<&OperationId, usize>, PlanFault> {
    let mut positions: BTreeMap<&OperationId, Vec<usize>> = BTreeMap::new();
    for (position, operation) in operations.iter().enumerate() {
        if let Some(id) = &operation.id {
            positions.entry(id).or_default().push(position);
        }
    }
    if let Some((id, carried)) = positions
        .iter()
        .filter(|(_, carried)| carried.len() > 1)
        .min_by_key(|(_, carried)| carried[1])
    {
        return Err(PlanFault::duplicate_id((*id).clone(), carried.clone()));
    }
    Ok(positions
        .into_iter()
        .map(|(id, carried)| (id, carried[0]))
        .collect())
}

/// The positions each operation's `requires` names, refusing a name nothing
/// carries.
fn explicit_requirements(
    operations: &[Operation],
    carriers: &BTreeMap<&OperationId, usize>,
) -> Result<Vec<Vec<usize>>, PlanFault> {
    operations
        .iter()
        .enumerate()
        .map(|(position, operation)| {
            operation
                .requires
                .iter()
                .map(|required| {
                    carriers
                        .get(required)
                        .copied()
                        .ok_or_else(|| PlanFault::unknown_requirement(position, required.clone()))
                })
                .collect()
        })
        .collect()
}

/// For each move, every other operation that vacates its destination.
fn vacating_requirements(operations: &[Operation]) -> Vec<Vec<usize>> {
    let mut vacaters: BTreeMap<&DocumentPath, Vec<usize>> = BTreeMap::new();
    for (position, operation) in operations.iter().enumerate() {
        if let Some(vacated) = vacates(&operation.kind) {
            vacaters.entry(vacated).or_default().push(position);
        }
    }
    operations
        .iter()
        .enumerate()
        .map(|(position, operation)| match &operation.kind {
            OperationKind::MoveDocument { to, .. } => vacaters
                .get(to)
                .into_iter()
                .flatten()
                .copied()
                .filter(|&vacater| vacater != position)
                .collect(),
            OperationKind::CreateDocument { .. }
            | OperationKind::StrReplace { .. }
            | OperationKind::DeleteDocument { .. } => Vec::new(),
        })
        .collect()
}

/// The path an operation leaves empty: a move's source, or a removal's path.
fn vacates(kind: &OperationKind) -> Option<&DocumentPath> {
    match kind {
        OperationKind::MoveDocument { from, .. } => Some(from),
        OperationKind::DeleteDocument { path } => Some(path),
        OperationKind::CreateDocument { .. } | OperationKind::StrReplace { .. } => None,
    }
}

/// The first cycle among `after`, searched from the lowest position and each
/// position's requirements in ascending order, as the positions in the order
/// each runs after the next; `None` where there is none.
fn first_cycle(after: &[Vec<usize>]) -> Option<Vec<usize>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Unvisited,
        OnPath,
        Done,
    }
    let mut marks = vec![Mark::Unvisited; after.len()];
    for start in 0..after.len() {
        if marks[start] != Mark::Unvisited {
            continue;
        }
        // The path from `start`, each entry a position and how many of its
        // requirements have been followed.
        let mut path: Vec<(usize, usize)> = vec![(start, 0)];
        marks[start] = Mark::OnPath;
        while let Some((position, followed)) = path.last_mut() {
            let position = *position;
            let Some(&next) = after[position].get(*followed) else {
                marks[position] = Mark::Done;
                path.pop();
                continue;
            };
            *followed += 1;
            match marks[next] {
                Mark::OnPath => {
                    let from = path
                        .iter()
                        .position(|&(on, _)| on == next)
                        .expect("a position marked on the path is on it");
                    return Some(path[from..].iter().map(|&(on, _)| on).collect());
                }
                Mark::Unvisited => {
                    marks[next] = Mark::OnPath;
                    path.push((next, 0));
                }
                Mark::Done => {}
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use norn_wire::{DocumentPath, OperationId, OperationKind};

    use super::*;

    fn path(text: &str) -> DocumentPath {
        DocumentPath::new(text).expect("a legal document path")
    }

    fn id(text: &str) -> OperationId {
        OperationId::new(text).expect("a legal identifier")
    }

    fn delete(at: &str) -> Operation {
        Operation::new(OperationKind::delete_document(path(at)))
    }

    fn moving(from: &str, to: &str) -> Operation {
        Operation::new(OperationKind::move_document(path(from), path(to)))
    }

    fn fault(operations: &[Operation]) -> PlanFault {
        match dependencies(operations) {
            Ok(_) => panic!("the plan's shape is faulty"),
            Err(fault) => fault,
        }
    }

    fn order(operations: &[Operation]) -> Vec<usize> {
        match dependencies(operations) {
            Ok(dependencies) => dependencies.order(|_| true),
            Err(fault) => panic!("the plan's shape is sound: {fault:?}"),
        }
    }

    #[test]
    fn an_identifier_carried_twice_is_a_fault_naming_every_carrier() {
        let operations = [
            delete("a.md").with_id(id("x")),
            delete("b.md"),
            delete("c.md").with_id(id("x")),
        ];
        assert_eq!(
            fault(&operations),
            PlanFault::duplicate_id(id("x"), vec![0, 2])
        );
    }

    #[test]
    fn a_requirement_no_operation_carries_is_a_fault() {
        let operations = [
            delete("a.md").with_id(id("x")),
            delete("b.md").with_requires(vec![id("x"), id("nobody")]),
        ];
        assert_eq!(
            fault(&operations),
            PlanFault::unknown_requirement(1, id("nobody"))
        );
    }

    #[test]
    fn operations_requiring_each_other_are_a_cycle_in_requirement_order() {
        let operations = [
            delete("a.md").with_id(id("a")).with_requires(vec![id("c")]),
            delete("b.md").with_id(id("b")).with_requires(vec![id("a")]),
            delete("c.md").with_id(id("c")).with_requires(vec![id("b")]),
        ];
        assert_eq!(fault(&operations), PlanFault::requires_cycle(vec![0, 2, 1]));
    }

    #[test]
    fn an_operation_requiring_itself_is_a_cycle() {
        let operations = [delete("a.md").with_id(id("a")).with_requires(vec![id("a")])];
        assert_eq!(fault(&operations), PlanFault::requires_cycle(vec![0]));
    }

    #[test]
    fn two_documents_exchanging_places_are_a_content_cycle() {
        let operations = [moving("a.md", "b.md"), moving("b.md", "a.md")];
        assert_eq!(fault(&operations), PlanFault::content_cycle(vec![0, 1]));
    }

    #[test]
    fn a_longer_chain_of_moves_back_to_its_start_is_a_content_cycle() {
        let operations = [
            moving("a.md", "b.md"),
            moving("b.md", "c.md"),
            moving("c.md", "a.md"),
        ];
        assert_eq!(fault(&operations), PlanFault::content_cycle(vec![0, 1, 2]));
    }

    #[test]
    fn a_move_runs_after_the_operation_vacating_its_destination() {
        let operations = [moving("a.md", "b.md"), moving("b.md", "c.md")];
        assert_eq!(order(&operations), vec![1, 0]);
        let operations = [moving("a.md", "b.md"), delete("b.md")];
        assert_eq!(order(&operations), vec![1, 0]);
    }

    #[test]
    fn an_operation_runs_after_what_it_requires_and_otherwise_in_plan_order() {
        let operations = [
            delete("a.md").with_requires(vec![id("c")]),
            delete("b.md"),
            delete("c.md").with_id(id("c")),
            delete("d.md"),
        ];
        assert_eq!(order(&operations), vec![1, 2, 0, 3]);
    }

    #[test]
    fn an_order_keeps_only_the_included_operations() {
        let operations = [
            moving("a.md", "b.md"),
            moving("b.md", "c.md"),
            delete("d.md"),
        ];
        let dependencies = dependencies(&operations).expect("a sound shape");
        assert_eq!(dependencies.order(|position| position != 1), vec![0, 2]);
    }
}
