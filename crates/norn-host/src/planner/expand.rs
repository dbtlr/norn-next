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
//! **A match resting on facts the store has not taken in is not written.**
//! Each matched document carries the content hash the store indexed it
//! under; where the file's bytes — the before-state its expanded operation
//! is composed from, read once through the same view — hash otherwise, the
//! file changed since the index saw it, perhaps by the very edit that makes
//! it stop matching, and the before-state guard alone cannot tell, since it
//! already carries that edit. That document's expanded operation is left
//! unresolved, saying the plan should be re-sent once the vault has indexed
//! the change; the others resolve. A preview and an apply judge alike.
//!
//! **A folder move expands the same way, from the files.** A `move_folder`
//! is turned into one `move_document` per document beneath its folder, at
//! any depth, in path order, each to the same place beneath its destination
//! and keeping the original's conditions, read through the view's own walk
//! of the folder ([`VaultView::folder_contents`]): a folder's contents are
//! the files', as a move's before-states are. Every other file beneath it,
//! and every place there the walk does not enter, is left behind, and the
//! forecast names each. A folder moved onto itself, to another spelling of
//! itself, or beneath itself, and one naming no folder or a folder holding no
//! document, is left unresolved in words.
//!
//! **Expansion keeps what an operation says beyond its target.** Each
//! expanded operation carries the original's kind and author conditions, in
//! the order the matcher answers, which is path order, at the original's
//! place in the plan. An authored `where` operation or folder move carrying an
//! identifier or a requirement is a fault in the plan's shape
//! ([`AuthoredPlan::ordered_expanded_targets`]), so no expansion is ever
//! required by, or orders after, another operation. A fault found in the
//! expanded plan names the authored positions of the operations it concerns.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use norn_wire::{
    AuthoredPlan, DocumentPath, FilePath, FolderPath, Operation, OperationKind, PlanFault,
    Predicate, RootIdentity, UnresolvedReason, WriteTarget,
};

use super::links::LinkIndex;
use super::resolve::{PlanningFailure, Resolution, resolve_leaving_out};
use super::rule::{self, Rules};
use super::view::{Entry, Remembered, VaultView};

/// What a `where` target's predicates match: the documents, in path order,
/// or why the builder could not apply them as asked, in words.
pub(crate) type Matched = Result<Vec<MatchedDocument>, String>;

/// One document a `where` target matched, with the content hash the index
/// holds it under: the bytes the facts it matched on were derived from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MatchedDocument {
    pub(crate) path: DocumentPath,
    /// The 64 lowercase hexadecimal digits of the indexed content hash.
    pub(crate) indexed: String,
}

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
/// the matcher or the link index could not read.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ExpandingFailure<V, M> {
    /// The plan's own shape, or the vault's files.
    Planning(PlanningFailure<V>),
    /// The one snapshot the `where` targets are matched on and the plan's
    /// resolution change set is judged on.
    Snapshot(M),
}

/// Plan `authored` against what `view` holds, its `where` targets expanded
/// through `matcher` first and its creations by rule made by `rules`, its
/// resolution change set judged through `links`.
///
/// **The matcher and the link index read one snapshot**, so a link index
/// that cannot be read refuses as the matcher's snapshot does.
pub(crate) fn resolve_expanding<V, M, I>(
    authored: AuthoredPlan,
    root: RootIdentity,
    view: &V,
    matcher: &M,
    links: &I,
    rules: &Rules<'_>,
) -> Result<Resolution, ExpandingFailure<V::Error, M::Error>>
where
    V: VaultView,
    M: Matcher,
    I: LinkIndex<Error = M::Error> + ?Sized,
{
    if let Some(fault) = authored
        .ordered_expanded_targets()
        .or_else(|| authored.misplaced_cascades())
    {
        return Err(ExpandingFailure::Planning(PlanningFailure::Fault(fault)));
    }
    // One observation of each file: the bytes a match is judged against
    // here are the bytes the planner composes each expanded operation from.
    let view = &Remembered::over(view);
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
    let mut left_behind = Vec::new();
    for (position, operation) in operations.into_iter().enumerate() {
        if let OperationKind::MoveFolder { from, to } = &operation.kind {
            let moved = folder_moves(&operation, from, to, view)
                .map_err(|error| ExpandingFailure::Planning(PlanningFailure::View(error)))?;
            match moved {
                Ok((moves, left)) => {
                    origin.extend(std::iter::repeat_n(position, moves.len()));
                    expanded.extend(moves);
                    left_behind.extend(left);
                }
                Err(detail) => {
                    left_out.insert(expanded.len(), UnresolvedReason::no_longer_resolves(detail));
                    expanded.push(operation);
                    origin.push(position);
                }
            }
            continue;
        }
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
                .map_err(ExpandingFailure::Snapshot)?
            {
                Ok(documents) if documents.is_empty() => Err(format!(
                    "no document matches the `where` target {}",
                    told(predicates)
                )),
                Ok(documents) => Ok(documents),
                Err(words) => Err(format!(
                    "the `where` target {} matches no document as asked: {words}",
                    told(predicates)
                )),
            }
        };
        match matched {
            Ok(documents) => {
                for document in documents {
                    if let Some(changed) = changed_since_indexed(&document, view)
                        .map_err(|error| ExpandingFailure::Planning(PlanningFailure::View(error)))?
                    {
                        left_out.insert(expanded.len(), changed);
                    }
                    expanded.push(at_path(&operation, document.path));
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
    rule::expand(&mut expanded, &mut left_out, rules, view)
        .map_err(|error| ExpandingFailure::Planning(PlanningFailure::View(error)))?;
    let mut authored = AuthoredPlan::new(vault, expanded).with_force(force);
    authored.plan = plan;
    authored.footnote = footnote;
    let mut resolution =
        resolve_leaving_out(authored, root, &BTreeSet::new(), view, links, left_out).map_err(
            |failure| match failure {
                PlanningFailure::Fault(fault) => ExpandingFailure::Planning(
                    PlanningFailure::Fault(at_authored_positions(fault, &origin)),
                ),
                PlanningFailure::View(error) => {
                    ExpandingFailure::Planning(PlanningFailure::View(error))
                }
                PlanningFailure::Links(refused) => ExpandingFailure::Snapshot(refused),
            },
        )?;
    resolution.forecast = resolution.forecast.with_left_behind(left_behind);
    Ok(resolution)
}

/// What a folder move expanded into: its document moves, and every file it
/// leaves behind.
type FolderMoved = (Vec<Operation>, Vec<FilePath>);

/// The document moves the folder move `operation`, from `from` to `to`,
/// expands into — one per document `view` lists beneath `from`, in path
/// order, each to the same place beneath `to` and carrying what `operation`
/// carries beyond its kind — and every file it leaves behind; or why it
/// expands into none, in words.
///
/// **What a folder move does not plan.** A folder moved onto itself names no
/// change; one moved to another spelling of itself is a folder's change of
/// case, which is not planned; one moved beneath itself — beneath any
/// spelling the root takes for the same folder — would move each document
/// under a name inside the folder being emptied; and one naming no
/// folder, or a folder holding no document, moves nothing. Each leaves the
/// operation unresolved. A destination beneath which documents already stand
/// is merged into: each document's own destination must still be vacant, or
/// vacated by the plan, as any move's is.
fn folder_moves<V: VaultView>(
    operation: &Operation,
    from: &FolderPath,
    to: &FolderPath,
    view: &V,
) -> Result<Result<FolderMoved, String>, V::Error> {
    let normalizer = view.normalizer();
    let (from_folder, to_folder) = match (
        normalizer.normalize(Path::new(from.as_str())),
        normalizer.normalize(Path::new(to.as_str())),
    ) {
        (Ok(from_folder), Ok(to_folder)) => (from_folder, to_folder),
        (Err(error), _) => {
            return Ok(Err(format!(
                "`{from}` names no folder in the vault: {error}"
            )));
        }
        (_, Err(error)) => return Ok(Err(format!("`{to}` names no folder in the vault: {error}"))),
    };
    if from_folder == to_folder {
        let spelled = |path: &FolderPath| {
            normalizer
                .normalize(Path::new(path.as_str()))
                .map(|folder| folder.as_path().to_owned())
                .ok()
        };
        return Ok(Err(if spelled(from) == spelled(to) {
            format!("the folder `{from}` would be moved onto itself")
        } else {
            format!(
                "`{to}` differs from `{from}` only in case, and a folder's change of case is not planned"
            )
        }));
    }
    // Inside is a matter of identity, as onto is: a folder above `to` that
    // is `from` under the root's case rule, whole components only.
    let inside = to_folder
        .as_path()
        .ancestors()
        .skip(1)
        .filter(|above| !above.as_os_str().is_empty())
        .filter_map(|above| normalizer.normalize(above).ok())
        .any(|above| above == from_folder);
    if inside {
        return Ok(Err(format!(
            "`{to}` lies inside `{from}`, and a folder cannot be moved into itself"
        )));
    }
    let Some(contents) = view.folder_contents(&from_folder)? else {
        return Ok(Err(format!("no folder stands at `{from}`")));
    };
    if contents.documents.is_empty() {
        return Ok(Err(format!("the folder `{from}` holds no document")));
    }
    let depth = from_folder.as_path().components().count();
    let mut moves = Vec::with_capacity(contents.documents.len());
    for document in contents.documents {
        let rest: PathBuf = Path::new(document.as_str())
            .components()
            .skip(depth)
            .collect();
        let Some(destination) = to_folder.as_path().join(rest).to_str().map(str::to_string) else {
            return Ok(Err(format!(
                "`{to}` names no folder a document can be moved to"
            )));
        };
        let destination = match DocumentPath::new(destination) {
            Ok(destination) => destination,
            Err(refusal) => {
                return Ok(Err(format!(
                    "`{to}` names no folder a document can be moved to: {refusal}"
                )));
            }
        };
        let mut moved = operation.clone();
        moved.kind = OperationKind::move_document(document, destination);
        moves.push(moved);
    }
    Ok(Ok((moves, contents.left)))
}

/// Why `document`'s expanded operation is left unresolved where the file
/// holds other bytes than the index derived its facts from, and `None` where
/// it holds the same ones, or no document at all — which composition leaves
/// unresolved as it leaves any path naming no document.
///
/// **A match resting on facts the index has not taken in is not written.**
/// The store may not have taken in a foreign edit the watcher has not yet
/// delivered, and that edit may be the one that makes the document stop
/// matching; the file's before-state carries the edit, so the write guard
/// alone would not see it.
fn changed_since_indexed<V: VaultView>(
    document: &MatchedDocument,
    view: &V,
) -> Result<Option<UnresolvedReason>, V::Error> {
    let Ok(identity) = view
        .normalizer()
        .normalize(Path::new(document.path.as_str()))
    else {
        return Ok(None);
    };
    Ok(match view.entry(&identity)? {
        Entry::Document { hash, .. } if hash.hex() != document.indexed => {
            Some(UnresolvedReason::no_longer_resolves(format!(
                "`{}` changed since the vault's index saw it, so the `where` target's match \
                 of it rests on facts the index has not taken in; re-send the plan once the \
                 vault has indexed the change",
                document.path
            )))
        }
        _ => None,
    })
}

/// `predicates` named for a person reading why an operation did not
/// resolve, in the wire's own names; the operation itself carries them as
/// the wire spells them.
fn told(predicates: &[Predicate]) -> String {
    listed(predicates)
}

/// `parts` as a bracketed list of their human spellings, `[a, b]`: how a
/// `where` target's predicates, and the parts a find could not apply, are
/// named in a reason a person reads.
pub(crate) fn listed<T: std::fmt::Display>(parts: &[T]) -> String {
    let spelled: Vec<String> = parts.iter().map(ToString::to_string).collect();
    format!("[{}]", spelled.join(", "))
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
    use super::super::links::testing::{EmptyStore, Untouched};
    use super::super::rule::testing::no_rules;
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

        /// `paths`, each indexed under the bytes of a draft, which is what
        /// every document the cases match holds.
        fn paths(paths: &[&str]) -> Self {
            Answering::indexed(&paths.iter().map(|at| (*at, draft())).collect::<Vec<_>>())
        }

        /// Each path, indexed under the hash of the text beside it.
        fn indexed(documents: &[(&str, &str)]) -> Self {
            Answering::with(Ok(documents
                .iter()
                .map(|(at, text)| MatchedDocument {
                    path: path(at),
                    indexed: content_hash(text.as_bytes()).hex().to_string(),
                })
                .collect()))
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
        let links = (EmptyStore::new(), std::marker::PhantomData);
        match resolve_expanding(
            authored(operations),
            root(),
            vault,
            matcher,
            &links,
            &no_rules(),
        ) {
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
                r#"no document matches the `where` target [{op: eq, key: "status", value: "draft"}]"#
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
                r#"the `where` target [{op: eq, key: "status", value: "draft"}] matches no document as asked: no document carries the key `status`"#
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

    /// **A matched document whose bytes are not the ones the index saw is
    /// left unresolved alone, saying the plan should be re-sent**: the match
    /// rests on facts the index has not taken in, and the document may no
    /// longer match. The others resolve.
    #[test]
    fn a_matched_document_changed_since_the_index_saw_it_is_unresolved_alone() {
        let published = "---\nstatus: published\n---\n";
        let vault = MemoryVault::with(&[("a.md", draft()), ("b.md", published)]);
        let resolution = planned(
            &vault,
            vec![set_done(WriteTarget::matching(drafts()))],
            &Answering::indexed(&[("a.md", draft()), ("b.md", draft())]),
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
        assert_eq!(left.operation, set_done(WriteTarget::path(path("b.md"))));
        let UnresolvedReason::NoLongerResolves { detail, .. } = &left.reason else {
            panic!("left out for {:?}", left.reason);
        };
        assert!(
            detail.contains("changed since the vault's index saw it") && detail.contains("re-send"),
            "{detail}"
        );
        assert_eq!(
            vault.reads.borrow().get("a.md"),
            Some(&1),
            "a matched document was judged on other bytes than it was composed from"
        );
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
            &Untouched::new(),
            &no_rules(),
        )
        .expect_err("an identified where is planned");

        assert_eq!(
            failure,
            ExpandingFailure::Planning(PlanningFailure::Fault(PlanFault::expanded_target_ordered(
                vec![0]
            )))
        );
        assert_eq!(matcher.asked.get(), 0);
    }

    /// **An authored operation carrying a link cascade is a fault in the
    /// plan**, answered before anything is matched: planning writes a
    /// cascade from the links the vault holds, and an author does not.
    #[test]
    fn an_authored_cascade_is_a_fault() {
        let matcher = Answering::paths(&["a.md"]);
        let cascade = vec![norn_wire::LinkRewrite::new(
            path("b.md"),
            norn_wire::LinkFamily::Wikilink,
            "a",
            "c",
        )];
        let failure = resolve_expanding(
            authored(vec![
                set_done(WriteTarget::matching(drafts())),
                Operation::new(OperationKind::move_document(path("a.md"), path("c.md")))
                    .with_cascade(cascade),
            ]),
            root(),
            &MemoryVault::with(&[("a.md", draft())]),
            &matcher,
            &Untouched::new(),
            &no_rules(),
        )
        .expect_err("an authored cascade is planned");

        assert_eq!(
            failure,
            ExpandingFailure::Planning(PlanningFailure::Fault(PlanFault::misplaced_cascade(vec![
                1
            ])))
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
            &Untouched::new(),
            &no_rules(),
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
            &Untouched::new(),
            &no_rules(),
        )
        .expect_err("an unreadable snapshot planned");
        assert_eq!(
            failure,
            ExpandingFailure::Snapshot("the store refused".to_string())
        );
    }

    fn folder(text: &str) -> FolderPath {
        FolderPath::new(text).expect("a legal folder path")
    }

    fn moving_folder(from: &str, to: &str) -> Operation {
        Operation::new(OperationKind::move_folder(folder(from), folder(to)))
    }

    fn moving(from: &str, to: &str) -> Operation {
        Operation::new(OperationKind::move_document(path(from), path(to)))
    }

    /// The one operation of `resolution` left unresolved, and its words.
    fn left_in_words(resolution: &Resolution) -> String {
        match &resolution.unresolved[..] {
            [unresolved] => match &unresolved.reason {
                UnresolvedReason::NoLongerResolves { detail, .. } => detail.clone(),
                other => panic!("no longer resolves: {other:?}"),
            },
            other => panic!("one operation is unresolved: {other:?}"),
        }
    }

    /// **A folder move expands into one document move per document the
    /// folder holds, at any depth, in path order, at its place in the
    /// plan**: each moves its document to the same place beneath the
    /// destination and keeps what the folder move carries beyond its kind.
    /// Every other file the folder holds is left behind, and the forecast
    /// names it.
    #[test]
    fn a_folder_move_expands_per_document_in_path_order() {
        let vault = MemoryVault::with(&[
            ("notes/b.md", "b\n"),
            ("notes/a.md", "a\n"),
            ("notes/deep/c.MD", "c\n"),
            ("notes/img.png", "png"),
            ("other.md", "o\n"),
        ]);
        let guard = AuthorCondition::content_hash(path("other.md"), content_hash(b"o\n"));
        let before = Operation::new(OperationKind::create_document(path("z.md"), "z"));
        let bulk = moving_folder("notes", "archive/notes")
            .with_conditions(vec![guard.clone()])
            .with_footnote("tidy");
        let resolution = planned(&vault, vec![before.clone(), bulk], &Answering::paths(&[]));
        assert!(
            resolution.unresolved.is_empty(),
            "{:?}",
            resolution.unresolved
        );
        let moved = |from: &str, to: &str| {
            moving(from, to)
                .with_conditions(vec![guard.clone()])
                .with_footnote("tidy")
        };
        assert_eq!(
            resolution.plan.operations,
            vec![
                before,
                moved("notes/a.md", "archive/notes/a.md"),
                moved("notes/b.md", "archive/notes/b.md"),
                moved("notes/deep/c.MD", "archive/notes/deep/c.MD"),
            ]
        );
        assert_eq!(
            resolution.forecast.left_behind,
            vec![FilePath::new("notes/img.png").expect("a file path")]
        );
    }

    /// **A folder move with an identifier or a requirement is a fault in the
    /// plan**, as a `where` operation with one is: it expands into several
    /// operations, from the vault as it stands before the plan.
    #[test]
    fn an_ordered_folder_move_is_a_fault() {
        let failure = resolve_expanding(
            authored(vec![
                moving_folder("notes", "archive").with_id(OperationId::new("all").unwrap()),
            ]),
            root(),
            &MemoryVault::with(&[("notes/a.md", "a\n")]),
            &Answering::paths(&[]),
            &Untouched::new(),
            &no_rules(),
        )
        .expect_err("an identified folder move is planned");
        assert_eq!(
            failure,
            ExpandingFailure::Planning(PlanningFailure::Fault(PlanFault::expanded_target_ordered(
                vec![0]
            )))
        );
    }

    /// **A folder's change of case is not planned**: on a root that folds
    /// case, a folder moved to another spelling of itself is left unresolved,
    /// in words, and so is one moved onto exactly itself.
    #[test]
    fn a_folder_case_change_is_unresolved() {
        let vault = MemoryVault::with(&[("notes/a.md", "a\n")]).folding_case();
        let respelled = planned(
            &vault,
            vec![moving_folder("notes", "Notes")],
            &Answering::paths(&[]),
        );
        let detail = left_in_words(&respelled);
        assert!(detail.contains("change of case"), "{detail}");
        assert!(respelled.plan.transitions.is_empty());
        let onto_itself = planned(
            &vault,
            vec![moving_folder("notes", "notes")],
            &Answering::paths(&[]),
        );
        let detail = left_in_words(&onto_itself);
        assert!(detail.contains("onto itself"), "{detail}");
    }

    /// **A folder moved into itself is unresolved**, in words, and moves
    /// nothing: each document would be moved beneath the folder it is
    /// emptied from. Whether the destination lies inside is the root's own
    /// question of identity, so on a root that folds case a destination
    /// beneath another spelling of the folder lies inside it too.
    #[test]
    fn a_folder_moved_into_itself_is_unresolved() {
        let sensitive = MemoryVault::with(&[("notes/a.md", "a\n")]);
        let folding = MemoryVault::with(&[("notes/a.md", "a\n")]).folding_case();
        for (vault, to) in [
            (&sensitive, "notes/sub"),
            (&folding, "notes/sub"),
            (&folding, "Notes/sub"),
            (&folding, "NOTES/deep/sub"),
        ] {
            let resolution = planned(
                vault,
                vec![moving_folder("notes", to)],
                &Answering::paths(&[]),
            );
            let detail = left_in_words(&resolution);
            assert!(detail.contains("into itself"), "{to}: {detail}");
            assert!(resolution.plan.transitions.is_empty(), "{to}");
        }
    }

    /// **A destination is inside a folder only beneath the same folder**: on
    /// a case-sensitive root `Notes/sub` lies in another folder than
    /// `notes`, and on either root a sibling sharing the folder's name as a
    /// prefix lies beside it, so each is an ordinary folder move.
    #[test]
    fn a_destination_beside_the_folder_is_moved_into() {
        let sensitive = MemoryVault::with(&[("notes/a.md", "a\n")]);
        let folding = MemoryVault::with(&[("notes/a.md", "a\n")]).folding_case();
        for (vault, to) in [
            (&sensitive, "Notes/sub"),
            (&sensitive, "notesy/x"),
            (&folding, "notesy/x"),
            (&folding, "Notesy/x"),
        ] {
            let resolution = planned(
                vault,
                vec![moving_folder("notes", to)],
                &Answering::paths(&[]),
            );
            assert!(
                resolution.unresolved.is_empty(),
                "{to}: {:?}",
                resolution.unresolved
            );
            assert_eq!(
                resolution.plan.operations,
                vec![moving("notes/a.md", &format!("{to}/a.md"))],
                "{to}"
            );
        }
    }

    /// **A folder path keeps only its floor, so a destination can name a
    /// folder no document path stands beneath**: a backslash or a control
    /// character in it is refused by the document-path grammar each document
    /// would land at, and the move is left unresolved saying why.
    #[test]
    fn a_folder_move_to_a_folder_no_document_can_stand_in_is_unresolved() {
        let vault = MemoryVault::with(&[("notes/a.md", "a\n")]);
        for (to, words) in [
            ("a\\b", "carries a backslash"),
            ("a\u{1}b", "control character"),
        ] {
            let resolution = planned(
                &vault,
                vec![moving_folder("notes", to)],
                &Answering::paths(&[]),
            );
            let detail = left_in_words(&resolution);
            assert!(
                detail.contains("names no folder a document can be moved to")
                    && detail.contains(words),
                "{to:?}: {detail}"
            );
            assert!(resolution.plan.transitions.is_empty(), "{to:?}");
        }
    }

    /// **A folder move naming no folder, or a folder holding no document,
    /// moves nothing**, and is left unresolved saying which.
    #[test]
    fn a_folder_move_with_nothing_to_move_is_unresolved() {
        let vault = MemoryVault::with(&[("files/img.png", "png"), ("a.md", "a\n")]);
        for (from, words) in [
            ("missing", "no folder stands"),
            ("a.md", "no folder stands"),
            ("files", "holds no document"),
        ] {
            let resolution = planned(
                &vault,
                vec![moving_folder(from, "archive")],
                &Answering::paths(&[]),
            );
            let detail = left_in_words(&resolution);
            assert!(detail.contains(words), "{from}: {detail}");
        }
    }
}
