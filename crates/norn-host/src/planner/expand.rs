//! `where` expansion: a frontmatter operation's predicate list turned, at
//! planning, into one operation per document it matches, each naming its
//! document by path.
//!
//! **The match is the store's, read through the find builder.** A `where`
//! target is the conjunction a `find` filters by, so it is matched by the one
//! Layer 3 builder that answers a find, in process, on the snapshot the
//! request already holds: a preview's one snapshot, and for an apply the
//! snapshot its job takes inside the entry's claim. A [`Matcher`] is that
//! seam, so this module holds no query of its own.
//!
//! **The match set is not a condition.** Each expanded operation is an
//! ordinary operation with a path target: the planner reads its document's
//! bytes and guards it by the before-state hash of what it composed from, as
//! it guards every write. A resolved plan carries only those operations, so
//! sending it back writes exactly the documents it was previewed with,
//! whatever the vault would match by then.
//!
//! **A `where` that expands to nothing does not resolve**, in words, never as
//! a silent no-op: a list of no predicates, which would match every
//! document, is left unresolved without being matched; one matching no
//! document names its predicates; and one the builder cannot apply as asked
//! — a part it reports unsatisfied, such as a key the vault's field universe
//! does not hold, or a request it refuses, such as a bound that does not
//! read as its key's declared type — names what it reported. A read answers
//! an unsatisfied part in-band and matches nothing; a write goes no further
//! than that nothing.
//!
//! **The match is the store's and the bytes are the files'.** The snapshot
//! can list a document the files no longer hold — deleted or moved since the
//! store last took in the watcher's facts. Its expanded operation is then an
//! edit of a document that does not stand, which composition leaves
//! unresolved, as it leaves any path target naming no document; the other
//! matched documents resolve. A document the files hold and the snapshot
//! does not list yet is not matched: the snapshot is what a read at the same
//! instant would have answered.
//!
//! **Expansion keeps what an operation says beyond its target.** Each
//! expanded operation carries the original's kind and author conditions, in
//! the order the matcher answers, which is path order, at the original's
//! place in the plan. An authored `where` operation carrying an identifier or
//! a requirement is a fault in the plan's shape
//! ([`AuthoredPlan::ordered_where_targets`]), so no expansion is ever
//! required by, or orders after, another operation. A fault found in the
//! expanded plan names the authored positions of the operations it concerns.

use std::collections::{BTreeMap, BTreeSet};

use norn_wire::{
    AuthoredPlan, DocumentPath, Operation, OperationKind, PlanFault, Predicate, RootIdentity,
    UnresolvedReason, WriteTarget,
};

use super::resolve::{PlanningFailure, Resolution, resolve_leaving_out};
use super::view::VaultView;

/// What a `where` target's predicates match: the documents, in path order,
/// or why the builder could not apply them as asked, in words.
pub(crate) type Matched = Result<Vec<DocumentPath>, String>;

/// The documents a predicate list matches, on the one snapshot a request
/// plans against.
pub(crate) trait Matcher {
    /// Why the snapshot could not be read at all.
    type Error;

    /// The documents every one of `predicates` matches, or why they match
    /// none as asked.
    fn matching(&self, predicates: &[Predicate]) -> Result<Matched, Self::Error>;
}

/// Why a plan was not planned: a failure of planning itself, or a snapshot
/// the matcher could not read.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ExpandingFailure<V, M> {
    /// The plan's own shape, or the vault's files.
    Planning(PlanningFailure<V>),
    /// The snapshot the `where` targets are matched on.
    Match(M),
}

/// Plan `authored` against what `view` holds, its `where` targets expanded
/// through `matcher` first.
pub(crate) fn resolve_expanding<V: VaultView, M: Matcher>(
    authored: AuthoredPlan,
    root: RootIdentity,
    view: &V,
    matcher: &M,
) -> Result<Resolution, ExpandingFailure<V::Error, M::Error>> {
    if let Some(fault) = authored.ordered_where_targets() {
        return Err(ExpandingFailure::Planning(PlanningFailure::Fault(fault)));
    }
    let AuthoredPlan {
        plan,
        vault,
        operations,
        force,
        footnote,
    } = authored;
    let mut expanded = Vec::with_capacity(operations.len());
    let mut origin = Vec::with_capacity(operations.len());
    let mut left_out = BTreeMap::new();
    for (position, operation) in operations.into_iter().enumerate() {
        let Some(WriteTarget::Where(predicates)) = operation.kind.target() else {
            expanded.push(operation);
            origin.push(position);
            continue;
        };
        let matched = if predicates.is_empty() {
            Err("a `where` target names at least one predicate, since a conjunction of none matches every document".to_string())
        } else {
            match matcher
                .matching(predicates)
                .map_err(ExpandingFailure::Match)?
            {
                Ok(paths) if paths.is_empty() => Err(format!(
                    "no document matches the `where` target {}",
                    told(predicates)
                )),
                Ok(paths) => Ok(paths),
                Err(words) => Err(format!(
                    "the `where` target {} matches no document as asked: {words}",
                    told(predicates)
                )),
            }
        };
        match matched {
            Ok(paths) => {
                for path in paths {
                    expanded.push(at_path(&operation, path));
                    origin.push(position);
                }
            }
            Err(detail) => {
                left_out.insert(expanded.len(), UnresolvedReason::no_longer_resolves(detail));
                expanded.push(operation);
                origin.push(position);
            }
        }
    }
    let mut authored = AuthoredPlan::new(vault, expanded).with_force(force);
    authored.plan = plan;
    authored.footnote = footnote;
    resolve_leaving_out(authored, root, &BTreeSet::new(), view, left_out).map_err(|failure| {
        ExpandingFailure::Planning(match failure {
            PlanningFailure::Fault(fault) => {
                PlanningFailure::Fault(at_authored_positions(fault, &origin))
            }
            other => other,
        })
    })
}

/// `predicates` named for a person reading why an operation did not
/// resolve; the operation itself carries them as the wire spells them.
fn told(predicates: &[Predicate]) -> String {
    format!("{predicates:?}")
}

/// `operation`, its `where` target replaced by the one document `path`.
fn at_path(operation: &Operation, path: DocumentPath) -> Operation {
    let mut expanded = operation.clone();
    let target = WriteTarget::path(path);
    match &mut expanded.kind {
        OperationKind::SetFrontmatter { target: at, .. }
        | OperationKind::RemoveFrontmatter { target: at, .. }
        | OperationKind::PushFrontmatter { target: at, .. }
        | OperationKind::PopFrontmatter { target: at, .. } => *at = target,
        _ => unreachable!("only a frontmatter kind carries a `where` target"),
    }
    expanded
}

/// `fault`, found in the expanded plan, naming the authored positions of the
/// operations it concerns, each once.
fn at_authored_positions(fault: PlanFault, origin: &[usize]) -> PlanFault {
    let authored = |positions: Vec<usize>| {
        let mut named: Vec<usize> = Vec::with_capacity(positions.len());
        for position in positions {
            let at = origin[position];
            if !named.contains(&at) {
                named.push(at);
            }
        }
        named
    };
    match fault {
        PlanFault::DuplicateId { id, positions, .. } => {
            PlanFault::duplicate_id(id, authored(positions))
        }
        PlanFault::UnknownRequirement {
            position, requires, ..
        } => PlanFault::unknown_requirement(origin[position], requires),
        PlanFault::RequiresCycle { positions, .. } => {
            PlanFault::requires_cycle(authored(positions))
        }
        PlanFault::ContentCycle { positions, .. } => PlanFault::content_cycle(authored(positions)),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use norn_wire::{
        AuthorCondition, AuthoredValue, ExpectedField, FileState, OperationId, Transition,
        VaultAddress, VaultName,
    };

    use super::super::compose::content_hash;
    use super::super::view::memory::MemoryVault;
    use super::*;

    fn path(text: &str) -> DocumentPath {
        DocumentPath::new(text).expect("a legal document path")
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

    fn drafts() -> Vec<Predicate> {
        vec![Predicate::equal_to("status", "draft")]
    }

    fn set_done(target: WriteTarget) -> Operation {
        Operation::new(OperationKind::set_frontmatter(
            target,
            "status",
            AuthoredValue::string("done"),
        ))
    }

    /// A matcher answering every predicate list with `answer`, counting how
    /// often it is asked.
    struct Answering {
        answer: Matched,
        asked: Cell<usize>,
    }

    impl Answering {
        fn with(answer: Matched) -> Self {
            Answering {
                answer,
                asked: Cell::new(0),
            }
        }

        fn paths(paths: &[&str]) -> Self {
            Answering::with(Ok(paths.iter().map(|at| path(at)).collect()))
        }
    }

    impl Matcher for Answering {
        type Error = String;

        fn matching(&self, _: &[Predicate]) -> Result<Matched, String> {
            self.asked.set(self.asked.get() + 1);
            Ok(self.answer.clone())
        }
    }

    /// A matcher whose snapshot cannot be read.
    struct Unreadable;

    impl Matcher for Unreadable {
        type Error = String;

        fn matching(&self, _: &[Predicate]) -> Result<Matched, String> {
            Err("the store refused".to_string())
        }
    }

    fn planned(
        vault: &MemoryVault,
        operations: Vec<Operation>,
        matcher: &impl Matcher<Error = String>,
    ) -> Resolution {
        match resolve_expanding(authored(operations), root(), vault, matcher) {
            Ok(resolution) => resolution,
            Err(failure) => panic!("the plan resolves: {failure:?}"),
        }
    }

    fn draft() -> &'static str {
        "---\nstatus: draft\n---\n"
    }

    fn done() -> &'static str {
        "---\nstatus: done\n---\n"
    }

    /// **A `where` target expands into one path operation per matched
    /// document, in the order matched, at its place in the plan**: each keeps
    /// the original's kind and conditions, and each document is one
    /// transition guarded by the before-state it was composed from.
    #[test]
    fn a_where_target_expands_into_one_path_operation_per_matched_document() {
        let vault = MemoryVault::with(&[("a.md", draft()), ("b.md", draft()), ("c.md", draft())]);
        let guard = AuthorCondition::expected_value(
            path("c.md"),
            "status",
            ExpectedField::present(AuthoredValue::string("draft")),
        );
        let before = Operation::new(OperationKind::create_document(path("z.md"), "z"));
        let bulk = set_done(WriteTarget::matching(drafts())).with_conditions(vec![guard.clone()]);
        let resolution = planned(
            &vault,
            vec![before.clone(), bulk],
            &Answering::paths(&["a.md", "b.md"]),
        );

        assert!(resolution.unresolved.is_empty());
        assert_eq!(
            resolution.plan.operations,
            vec![
                before,
                set_done(WriteTarget::path(path("a.md"))).with_conditions(vec![guard.clone()]),
                set_done(WriteTarget::path(path("b.md"))).with_conditions(vec![guard]),
            ]
        );
        assert_eq!(
            resolution.plan.transitions,
            vec![
                Transition::new(path("a.md"), present(draft()), present(done())),
                Transition::new(path("b.md"), present(draft()), present(done())),
                Transition::new(path("z.md"), FileState::absent(), present("z")),
            ]
        );
    }

    /// **A `where` matching no document is left unresolved naming its
    /// predicates**, never a silent no-op, and the plan's other operations
    /// still resolve.
    #[test]
    fn a_where_matching_no_document_is_unresolved_naming_its_predicates() {
        let vault = MemoryVault::with(&[("a.md", draft())]);
        let bulk = set_done(WriteTarget::matching(drafts()));
        let single = set_done(WriteTarget::path(path("a.md")));
        let resolution = planned(
            &vault,
            vec![bulk.clone(), single.clone()],
            &Answering::paths(&[]),
        );

        assert_eq!(resolution.plan.operations, vec![single]);
        let [left] = resolution.unresolved.as_slice() else {
            panic!("one operation is left out: {:?}", resolution.unresolved);
        };
        assert_eq!(left.operation, bulk);
        assert_eq!(
            left.reason,
            UnresolvedReason::no_longer_resolves(
                r#"no document matches the `where` target [Eq { key: "status", value: "draft" }]"#
            )
        );
    }

    /// **A `where` the builder cannot apply as asked is left unresolved
    /// naming what it reported.**
    #[test]
    fn a_where_the_builder_cannot_apply_is_unresolved_naming_its_report() {
        let resolution = planned(
            &MemoryVault::default(),
            vec![set_done(WriteTarget::matching(drafts()))],
            &Answering::with(Err("no document carries the key `status`".to_string())),
        );

        let [left] = resolution.unresolved.as_slice() else {
            panic!("one operation is left out: {:?}", resolution.unresolved);
        };
        assert_eq!(
            left.reason,
            UnresolvedReason::no_longer_resolves(
                r#"the `where` target [Eq { key: "status", value: "draft" }] matches no document as asked: no document carries the key `status`"#
            )
        );
    }

    /// **An empty `where` is left unresolved without being matched**, since
    /// a conjunction of no predicates would match every document.
    #[test]
    fn an_empty_where_is_unresolved_without_being_matched() {
        let matcher = Answering::paths(&["a.md"]);
        let resolution = planned(
            &MemoryVault::with(&[("a.md", draft())]),
            vec![set_done(WriteTarget::matching(Vec::new()))],
            &matcher,
        );

        assert_eq!(matcher.asked.get(), 0);
        assert!(resolution.plan.transitions.is_empty());
        assert_eq!(resolution.unresolved.len(), 1);
    }

    /// **A matched document the files no longer hold leaves its expanded
    /// operation unresolved, and the other matched documents resolve**: the
    /// snapshot's match can trail the files, and the files decide.
    #[test]
    fn a_matched_document_the_files_no_longer_hold_is_unresolved_alone() {
        let vault = MemoryVault::with(&[("a.md", draft())]);
        let resolution = planned(
            &vault,
            vec![set_done(WriteTarget::matching(drafts()))],
            &Answering::paths(&["a.md", "gone.md"]),
        );

        assert_eq!(
            resolution.plan.transitions,
            vec![Transition::new(
                path("a.md"),
                present(draft()),
                present(done())
            )]
        );
        let [left] = resolution.unresolved.as_slice() else {
            panic!("one operation is left out: {:?}", resolution.unresolved);
        };
        assert_eq!(left.operation, set_done(WriteTarget::path(path("gone.md"))));
    }

    /// **A `where` operation carrying an identifier or a requirement is a
    /// fault in the plan**, answered before anything is matched.
    #[test]
    fn a_where_operation_with_an_id_or_a_requirement_is_a_fault() {
        let matcher = Answering::paths(&["a.md"]);
        let id = OperationId::new("bulk").unwrap();
        let failure = resolve_expanding(
            authored(vec![set_done(WriteTarget::matching(drafts())).with_id(id)]),
            root(),
            &MemoryVault::with(&[("a.md", draft())]),
            &matcher,
        )
        .expect_err("an identified where is planned");

        assert_eq!(
            failure,
            ExpandingFailure::Planning(PlanningFailure::Fault(PlanFault::where_target_ordered(
                vec![0]
            )))
        );
        assert_eq!(matcher.asked.get(), 0);
    }

    /// **A fault in the expanded plan names authored positions**: an
    /// identifier carried twice, after a `where` that expanded to two
    /// operations, is named where the author wrote it.
    #[test]
    fn a_fault_in_the_expanded_plan_names_authored_positions() {
        let id = OperationId::new("twice").unwrap();
        let create = |at: &str| {
            Operation::new(OperationKind::create_document(path(at), "x")).with_id(id.clone())
        };
        let failure = resolve_expanding(
            authored(vec![
                set_done(WriteTarget::matching(drafts())),
                create("y.md"),
                create("z.md"),
            ]),
            root(),
            &MemoryVault::with(&[("a.md", draft()), ("b.md", draft())]),
            &Answering::paths(&["a.md", "b.md"]),
        )
        .expect_err("a duplicate identifier is planned");

        assert_eq!(
            failure,
            ExpandingFailure::Planning(PlanningFailure::Fault(PlanFault::duplicate_id(
                id,
                vec![1, 2]
            )))
        );
    }

    /// **A snapshot the matcher cannot read fails planning**, as a vault the
    /// planner cannot read does: nothing is planned.
    #[test]
    fn a_snapshot_the_matcher_cannot_read_fails_planning() {
        let failure = resolve_expanding(
            authored(vec![set_done(WriteTarget::matching(drafts()))]),
            root(),
            &MemoryVault::default(),
            &Unreadable,
        )
        .expect_err("an unreadable snapshot planned");
        assert_eq!(
            failure,
            ExpandingFailure::Match("the store refused".to_string())
        );
    }
}
