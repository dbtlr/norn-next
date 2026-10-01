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
use crate::planner::compose::touched;

/// Every path at which `plan`'s transitions do not name exactly the files its
/// operations touch — each holder an operation's link cascade rewrites
/// among them ([`touched`]): a transition for a file no operation touches or that the
/// vault's rule names no file, a spelling named by more than one transition,
/// a second spelling of a file no operation renames to itself, and each
/// spelling an operation names a file by that no transition covers.
pub(super) fn shape_disagrees(
    plan: &ResolvedPlan,
    normalizer: &PathNormalizer,
) -> Vec<DocumentPath> {
    let mut touching: BTreeMap<NormalizedPath, BTreeSet<&DocumentPath>> = BTreeMap::new();
    let mut respelled: BTreeSet<NormalizedPath> = BTreeSet::new();
    for operation in &plan.operations {
        for path in touched(operation) {
            if let Some(file) = identity(normalizer, path.as_str()) {
                touching.entry(file).or_default().insert(path);
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
            Some(file) if touching.contains_key(&file) => {
                if !carried.entry(file).or_default().insert(path) {
                    disagreeing.push(path.clone());
                }
            }
            _ => disagreeing.push(path.clone()),
        }
    }
    for (file, spellings) in &carried {
        // A case-only rename carries its file at the two spellings it moves
        // between; no plan carries one file at more.
        let allowed = if respelled.contains(file) { 2 } else { 1 };
        if spellings.len() > allowed {
            disagreeing.extend(spellings.iter().map(|path| (*path).clone()));
        }
    }
    for (file, spellings) in &touching {
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

    fn cascading(from: &str, to: &str, holder: &str) -> Operation {
        moving(from, to).with_cascade(vec![norn_wire::LinkRewrite::new(
            path(holder),
            norn_wire::LinkFamily::Wikilink,
            "a",
            "b",
        )])
    }

    /// **A cascade's holder is a file its operation touches**, so a plan
    /// carrying a transition at it is shaped as its operations are.
    #[test]
    fn a_cascade_holder_is_a_touched_file() {
        let vault = MemoryVault::with(&[]);
        let state = || FileState::absent();
        let plan = plan(
            vec![cascading("a.md", "b.md", "h.md")],
            vec![
                Transition::new(path("a.md"), state(), state()),
                Transition::new(path("b.md"), state(), state()),
                Transition::new(path("h.md"), state(), state()),
            ],
        );
        assert!(shape_disagrees(&plan, VaultView::normalizer(&vault)).is_empty());
    }

    /// **A cascade's holder with no transition disagrees**, named by the
    /// spelling the cascade gives it: the rewrite would be composed and never
    /// published.
    #[test]
    fn a_cascade_holder_without_a_transition_is_plan_invalid() {
        let vault = MemoryVault::with(&[]);
        let state = || FileState::absent();
        let plan = plan(
            vec![cascading("a.md", "b.md", "h.md")],
            vec![
                Transition::new(path("a.md"), state(), state()),
                Transition::new(path("b.md"), state(), state()),
            ],
        );
        assert_eq!(
            shape_disagrees(&plan, VaultView::normalizer(&vault)),
            vec![path("h.md")]
        );
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

    /// A case-only rename carries its file at two spellings, never three.
    #[test]
    fn a_third_spelling_of_a_renamed_file_disagrees() {
        let vault = MemoryVault::with(&[]).folding_case();
        let plan = plan(
            vec![moving("Note.md", "note.md")],
            vec![
                Transition::new(path("Note.md"), FileState::absent(), FileState::absent()),
                Transition::new(path("note.md"), FileState::absent(), FileState::absent()),
                Transition::new(path("NOTE.md"), FileState::absent(), FileState::absent()),
            ],
        );
        assert_eq!(
            shape_disagrees(&plan, VaultView::normalizer(&vault)),
            vec![path("NOTE.md"), path("Note.md"), path("note.md")]
        );
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
