//! The plan's shape: which operation runs after which, and the faults that
//! make a plan's shape wrong.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::path::Path;

use norn_fs::NormalizedPath;
use norn_wire::{DocumentPath, Operation, OperationId, OperationKind, PlanFault};

use super::resolve::PlanningFailure;
use super::view::{Entry, VaultView};

/// Which operations each operation runs after, by position.
///
/// **Two kinds of requirement.** An operation runs after the operations its
/// `requires` names. A move or a create whose name a document stands at also
/// runs after every other operation that vacates that name — a move away from
/// it, or its removal — because ADR 0031 lets a move's destination be absent
/// at planning or vacated by another operation of the same plan, which the
/// move then requires, and a create over a vacated name is the same act. A
/// name nothing stands at needs nothing vacated, so an operation vacating it
/// later in the plan is no requirement. The second kind is read against what
/// the vault holds at planning, by identity, so it is the vault that decides
/// whether a chain of moves closes a cycle.
pub(crate) struct Dependencies {
    /// For each position, the positions it runs after, ascending.
    after: Vec<Vec<usize>>,
}

/// The dependencies of `operations` over what `view` holds, or the first
/// fault in their shape.
///
/// **A requirement already met is no requirement.** `met` names operations an
/// earlier apply of this plan already landed, which a refresh drops before it
/// re-resolves what remains (ADR 0031); a requirement on one of them is
/// satisfied rather than unknown, and orders nothing.
///
/// Faults are looked for in the order [`PlanFault`] lists them: an identifier
/// carried twice, a requirement nothing carries, a cycle of `requires` alone,
/// and last a cycle that a vacated name closes, alone or with `requires`,
/// which is a content cycle.
pub(crate) fn dependencies<V: VaultView>(
    operations: &[Operation],
    met: &BTreeSet<OperationId>,
    view: &V,
) -> Result<Dependencies, PlanningFailure<V::Error>> {
    let carriers = carriers(operations).map_err(PlanningFailure::Fault)?;
    let explicit =
        explicit_requirements(operations, &carriers, met).map_err(PlanningFailure::Fault)?;
    if let Some(cycle) = first_cycle(&explicit) {
        return Err(PlanningFailure::Fault(PlanFault::requires_cycle(cycle)));
    }
    let vacating = vacating_requirements(operations, view).map_err(PlanningFailure::View)?;
    let after: Vec<Vec<usize>> = explicit
        .into_iter()
        .zip(vacating)
        .map(|(mut explicit, vacating)| {
            explicit.extend(vacating);
            explicit.sort_unstable();
            explicit.dedup();
            explicit
        })
        .collect();
    if let Some(cycle) = first_cycle(&after) {
        return Err(PlanningFailure::Fault(PlanFault::content_cycle(cycle)));
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
/// carries and leaving out a name `met` holds that nothing carries.
fn explicit_requirements(
    operations: &[Operation],
    carriers: &BTreeMap<&OperationId, usize>,
    met: &BTreeSet<OperationId>,
) -> Result<Vec<Vec<usize>>, PlanFault> {
    operations
        .iter()
        .enumerate()
        .map(|(position, operation)| {
            let mut required = Vec::new();
            for id in &operation.requires {
                match carriers.get(id) {
                    Some(&carrier) => required.push(carrier),
                    None if met.contains(id) => {}
                    None => return Err(PlanFault::unknown_requirement(position, id.clone())),
                }
            }
            Ok(required)
        })
        .collect()
}

/// For each move and create whose name a document stands at, every other
/// operation that vacates that name.
fn vacating_requirements<V: VaultView>(
    operations: &[Operation],
    view: &V,
) -> Result<Vec<Vec<usize>>, V::Error> {
    let identity = |path: &DocumentPath| view.normalizer().normalize(Path::new(path.as_str())).ok();
    let mut vacaters: BTreeMap<NormalizedPath, Vec<usize>> = BTreeMap::new();
    for (position, operation) in operations.iter().enumerate() {
        if let Some(vacated) = vacates(&operation.kind).and_then(identity) {
            vacaters.entry(vacated).or_default().push(position);
        }
    }
    let mut after = Vec::with_capacity(operations.len());
    for (position, operation) in operations.iter().enumerate() {
        let mut vacated_first = Vec::new();
        if let Some(name) = arrives_at(&operation.kind).and_then(identity)
            && let Some(vacating) = vacaters.get(&name)
            && matches!(view.entry(&name)?, Entry::Document { .. })
        {
            vacated_first.extend(vacating.iter().copied().filter(|&at| at != position));
        }
        after.push(vacated_first);
    }
    Ok(after)
}

/// The name an operation puts a document at: a move's destination, or a
/// create's path.
fn arrives_at(kind: &OperationKind) -> Option<&DocumentPath> {
    match kind {
        OperationKind::MoveDocument { to, .. } => Some(to),
        OperationKind::CreateDocument { path, .. } => Some(path),
        OperationKind::StrReplace { .. } | OperationKind::DeleteDocument { .. } => None,
    }
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

    use super::super::view::memory::MemoryVault;
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

    /// A vault where every document the tests name stands.
    fn vault() -> MemoryVault {
        MemoryVault::with(&[("a.md", "a"), ("b.md", "b"), ("c.md", "c"), ("d.md", "d")])
    }

    fn fault(operations: &[Operation]) -> PlanFault {
        match dependencies(operations, &BTreeSet::new(), &vault()) {
            Err(PlanningFailure::Fault(fault)) => fault,
            _ => panic!("the plan's shape is faulty"),
        }
    }

    fn order(operations: &[Operation]) -> Vec<usize> {
        match dependencies(operations, &BTreeSet::new(), &vault()) {
            Ok(dependencies) => dependencies.order(|_| true),
            Err(failure) => panic!("the plan's shape is sound: {failure:?}"),
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
    fn a_create_runs_after_the_operation_vacating_its_name() {
        let operations = [
            Operation::new(OperationKind::create_document(path("a.md"), "new")),
            moving("a.md", "e.md"),
        ];
        assert_eq!(order(&operations), vec![1, 0]);
    }

    #[test]
    fn a_name_nothing_stands_at_waits_for_nothing_that_vacates_it() {
        let operations = [moving("a.md", "e.md"), moving("e.md", "f.md")];
        assert_eq!(order(&operations), vec![0, 1]);
        let operations = [moving("a.md", "e.md"), moving("e.md", "a.md")];
        assert_eq!(order(&operations), vec![0, 1]);
    }

    #[test]
    fn a_requirement_already_met_orders_nothing() {
        let operations = [delete("a.md").with_requires(vec![id("landed")])];
        let met = BTreeSet::from([id("landed")]);
        let dependencies = dependencies(&operations, &met, &vault()).expect("a sound shape");
        assert_eq!(dependencies.order(|_| true), vec![0]);
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
        let dependencies =
            dependencies(&operations, &BTreeSet::new(), &vault()).expect("a sound shape");
        assert_eq!(dependencies.order(|position| position != 1), vec![0, 2]);
    }
}
