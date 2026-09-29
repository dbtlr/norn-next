//! The shape check: whether a plan's transitions name exactly the files its
//! operations touch, judged before anything is read from the vault.
//!
//! **A transition no operation accounts for is the plan's fault, never the
//! vault's.** Planning writes one transition per file an operation touches, so
//! a transition for a file no operation touches, a file an operation touches
//! with no transition, and a file named twice are wrong whatever the vault
//! holds. Judged after the vault is read, such a transition would be reported
//! by what the file holds — drift where its before-state was guessed wrong, an
//! invalid plan where it was guessed right — so it is judged here first. A
//! transition on a touched file whose before-state is not what the file holds
//! is left to the drift check: it cannot be told apart from a foreign edit.
//!
//! **Files are compared by the vault's own identity rule**, the normalizer
//! the planner reads through, so no second spelling rule exists. One identity
//! carries two transitions only where an operation renames a file to another
//! spelling of itself, a case-only rename on a root that folds case, which
//! planning writes as the old spelling taken away and the new one written.

use std::collections::{BTreeMap, BTreeSet};

use norn_fs::{NormalizedPath, PathNormalizer};
use norn_wire::{DocumentPath, OperationKind, ResolvedPlan};

use super::observe::identity;
use crate::planner::compose::touches;

/// Every path at which `plan`'s transitions do not name exactly the files its
/// operations touch: a transition for a file no operation touches or that the
/// vault's rule names no file, a spelling named by more than one transition,
/// a second spelling of a file no operation renames to itself, and each
/// spelling an operation names a file by that no transition covers.
pub(super) fn shape_disagrees(
    plan: &ResolvedPlan,
    normalizer: &PathNormalizer,
) -> Vec<DocumentPath> {
    let mut touched: BTreeMap<NormalizedPath, BTreeSet<&DocumentPath>> = BTreeMap::new();
    let mut respelled: BTreeSet<NormalizedPath> = BTreeSet::new();
    for operation in &plan.operations {
        for path in touches(&operation.kind) {
            if let Some(file) = identity(normalizer, path.as_str()) {
                touched.entry(file).or_default().insert(path);
            }
        }
        if let OperationKind::MoveDocument { from, to } = &operation.kind
            && from != to
            && let Some(file) = identity(normalizer, from.as_str())
            && identity(normalizer, to.as_str()).as_ref() == Some(&file)
        {
            respelled.insert(file);
        }
    }
    let mut disagreeing = Vec::new();
    let mut carried: BTreeMap<NormalizedPath, BTreeSet<&DocumentPath>> = BTreeMap::new();
    for transition in &plan.transitions {
        let path = &transition.path;
        match identity(normalizer, path.as_str()) {
            Some(file) if touched.contains_key(&file) => {
                if !carried.entry(file).or_default().insert(path) {
                    disagreeing.push(path.clone());
                }
            }
            _ => disagreeing.push(path.clone()),
        }
    }
    for (file, spellings) in &carried {
        if spellings.len() > 1 && !respelled.contains(file) {
            disagreeing.extend(spellings.iter().map(|path| (*path).clone()));
        }
    }
    for (file, spellings) in &touched {
        if !carried.contains_key(file) {
            disagreeing.extend(spellings.iter().map(|path| (*path).clone()));
        }
    }
    disagreeing
}

#[cfg(test)]
mod tests {
    use norn_wire::{
        DocumentPath, FileState, Operation, OperationKind, ResolvedPlan, RootIdentity, Transition,
        VaultAddress, VaultName,
    };

    use super::shape_disagrees;
    use crate::planner::view::VaultView;
    use crate::planner::view::memory::MemoryVault;

    fn path(text: &str) -> DocumentPath {
        DocumentPath::new(text).expect("a legal document path")
    }

    fn plan(operations: Vec<Operation>, transitions: Vec<Transition>) -> ResolvedPlan {
        ResolvedPlan::new(
            VaultAddress::name(VaultName::new("notes").expect("a legal vault name")),
            RootIdentity::from_device_and_inode(1, 2),
            operations,
            transitions,
            Vec::new(),
        )
    }

    fn moving(from: &str, to: &str) -> Operation {
        Operation::new(OperationKind::move_document(path(from), path(to)))
    }

    /// A case-only rename on a folding root is one file at two spellings,
    /// and carries a transition at each.
    #[test]
    fn a_case_only_rename_carries_both_spellings_of_one_file() {
        let vault = MemoryVault::with(&[]).folding_case();
        let plan = plan(
            vec![moving("Note.md", "note.md")],
            vec![
                Transition::new(path("Note.md"), FileState::absent(), FileState::absent()),
                Transition::new(path("note.md"), FileState::absent(), FileState::absent()),
            ],
        );
        assert!(shape_disagrees(&plan, VaultView::normalizer(&vault)).is_empty());
    }

    /// Two spellings of one file no operation renames to itself disagree at
    /// both.
    #[test]
    fn two_spellings_of_a_file_nothing_renames_disagree() {
        let vault = MemoryVault::with(&[]).folding_case();
        let plan = plan(
            vec![Operation::new(OperationKind::delete_document(path(
                "Note.md",
            )))],
            vec![
                Transition::new(path("Note.md"), FileState::absent(), FileState::absent()),
                Transition::new(path("note.md"), FileState::absent(), FileState::absent()),
            ],
        );
        assert_eq!(
            shape_disagrees(&plan, VaultView::normalizer(&vault)),
            vec![path("Note.md"), path("note.md")]
        );
    }
}
