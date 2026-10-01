//! Recomposition: the plan's operations run again from its before-states,
//! through the planner's own ordering and composition, and held to the
//! transitions the plan carries.
//!
//! **The plan carries no bytes it did not author**, so what the applier
//! publishes is composed again here, through the planner's own
//! [`dependencies`] and [`compose`], the one path from operations to
//! transitions (invariant 4). A plan is sound only where that path, run over a
//! vault holding every target at its recorded before-state, gives back exactly
//! the plan's transitions: one per file, each with the same before- and
//! after-state, none missing and none added, every operation acting and every
//! author condition the operations carry held. Anything else — a transition
//! dropped, added, repeated or changed, an order the operations' requirements
//! do not allow — describes something the operations do not do, and publishing
//! it could write content no operation produced or remove a document no
//! operation removes. Such a plan's own shape is wrong: it is
//! `request/plan-invalid` with [`PlanFault::TransitionsDisagree`], naming every
//! file it disagrees at, never a refusal answered with a fresh plan, since no
//! target drifted and the caller's fix is to preview the operations again.
//!
//! **The order is the plan's recorded one, and it must be one the
//! dependencies allow.** Planning records its operations in the order they
//! compose, which is the dependencies' own order; run again over the recorded
//! operations and the before-states, the dependencies either give that same
//! order back or find the plan's shape wrong — a cycle of content or of
//! requirements, which is `request/plan-invalid` here as at planning.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use norn_fs::{NormalizedPath, PathNormalizer};
use norn_wire::{
    AuthorCondition, DocumentPath, ExpectedField, FileState, PlanCondition, PlanFault, ResolvedPlan,
};

use super::observe::{TargetState, identity};
use crate::planner::compose::{Composition, compose, content_hash, edits_in_place, touched};
use crate::planner::edit;
use crate::planner::lineage::Lineage;
use crate::planner::order::dependencies;
use crate::planner::resolve::PlanningFailure;
use crate::planner::view::{Entry, VaultView};

/// What running a plan's operations again from its before-states found.
pub(super) enum Recomposed {
    /// The operations do what the plan says, and this is what they compose.
    Sound(Composition),
    /// The plan's own shape is wrong: its operations', or its transitions
    /// disagree with them.
    Invalid(PlanFault),
}

/// The fault of a plan whose transitions disagree with its operations at
/// `paths`.
pub(super) fn disagreement(paths: impl IntoIterator<Item = DocumentPath>) -> PlanFault {
    PlanFault::transitions_disagree(paths.into_iter().collect())
}

/// Run `plan`'s operations again over the before-state of every target, as
/// `states` observed them, and hold the result to the plan's transitions.
///
/// **A before-state this apply cannot see** — a target already holding its
/// change — is composed from bytes standing in for it. What the plan's moves
/// carry from one reaches only targets that landed, since a target that has
/// not landed and draws on one is drift (see [`super::observe`]), so the
/// stand-in reaches nothing published; such a landed target's after-state is
/// the one the vault already holds, and is not recomposed. An edit the
/// stand-in does not let act is one whose target already holds its change.
///
/// **A target already holding its after-state is landed, whoever wrote it**
/// (ADR 0031's landed rule). So a plan whose after-states were edited by hand
/// to what the vault already holds is not caught here: every target is
/// judged landed, nothing is recomposed against those after-states, and the
/// plan answers applied with every target found, writing nothing. A target
/// whose after-state equals its before-state is the exception: its held bytes
/// are its before-bytes, so it is recomposed from them, and an operation that
/// would change it refuses the plan.
pub(super) fn recompose<V: VaultView>(
    plan: &ResolvedPlan,
    states: &[TargetState],
    lineage: &Lineage,
    view: &V,
) -> Result<Recomposed, V::Error> {
    let before = BeforeStates::over(plan, states, view);
    let count = plan.operations.len();
    let dependencies = match dependencies(&plan.operations, &BTreeSet::new(), &before) {
        Ok(dependencies) => dependencies,
        Err(PlanningFailure::Fault(fault)) => return Ok(Recomposed::Invalid(fault)),
        Err(PlanningFailure::View(error)) => return Err(error),
    };
    let recorded: Vec<usize> = (0..count).collect();
    let allowed = dependencies.order(|_| true);
    if allowed != recorded {
        // Composing in an order the operations cannot run in says nothing
        // about them, so the files the misplaced operations touch are named
        // and nothing is composed.
        let misplaced = recorded
            .iter()
            .zip(&allowed)
            .filter(|(recorded, allowed)| recorded != allowed)
            .flat_map(|(&recorded, &allowed)| [recorded, allowed])
            .flat_map(|position| touched(&plan.operations[position]).cloned());
        return Ok(Recomposed::Invalid(disagreement(misplaced)));
    }
    if let Some(cycle) = lineage.content_cycle() {
        return Ok(Recomposed::Invalid(PlanFault::content_cycle(cycle)));
    }
    let composition = compose(&plan.operations, &recorded, &before)?;
    let unseen = |file: &NormalizedPath| before.unseen.contains(file);
    let mut disagreeing: Vec<DocumentPath> = Vec::new();
    // An operation that does not act refuses the plan whatever files it
    // names: one naming none — a wikilink rewrite without a cascade — would
    // otherwise add nothing here and be dropped while the rest landed.
    let mut inactive = false;
    for unresolvable in &composition.unresolvable {
        let operation = &plan.operations[unresolvable.position];
        let stood_in = edits_in_place(&operation.kind)
            && lineage.edited(unresolvable.position).is_some_and(unseen);
        if !stood_in {
            inactive = true;
            disagreeing.extend(touched(operation).cloned());
        }
    }
    disagreeing.extend(transitions_differ(
        plan,
        states,
        lineage,
        &composition,
        &before,
    ));
    disagreeing.extend(conditions_differ(plan, &composition, &before)?);
    if inactive || !disagreeing.is_empty() {
        return Ok(Recomposed::Invalid(disagreement(disagreeing)));
    }
    Ok(Recomposed::Sound(composition))
}

/// Every file whose transition is not the composed one: named by more than
/// one transition, written by the operations with no transition, carried
/// with none of the operations writing it, or at another before- or
/// after-state than the operations give it.
fn transitions_differ<V>(
    plan: &ResolvedPlan,
    states: &[TargetState],
    lineage: &Lineage,
    composition: &Composition,
    before: &BeforeStates<'_, V>,
) -> Vec<DocumentPath> {
    let mut differing = Vec::new();
    let mut carried = BTreeMap::new();
    for (index, transition) in plan.transitions.iter().enumerate() {
        if carried.insert(&transition.path, index).is_some() {
            differing.push(transition.path.clone());
        }
    }
    differing.extend(
        composition
            .targets
            .keys()
            .filter(|path| !carried.contains_key(path))
            .cloned(),
    );
    for (path, &index) in &carried {
        let transition = &plan.transitions[index];
        let Some(composed) = composition.targets.get(*path) else {
            differing.push((*path).clone());
            continue;
        };
        if composed.before != transition.before {
            differing.push((*path).clone());
            continue;
        }
        let after = match &composed.after {
            Some(bytes) => FileState::present(content_hash(bytes)),
            None => FileState::absent(),
        };
        if after == transition.after {
            continue;
        }
        let drawn_from_unseen = before.identities[index]
            .as_ref()
            .and_then(|file| lineage.source(file))
            .is_some_and(|source| before.unseen.contains(&source.from));
        if !(states[index].partly_landed() && drawn_from_unseen) {
            differing.push((*path).clone());
        }
    }
    differing
}

/// Every file an author condition the operations carry names that the plan
/// does not check it at: a condition on a file the plan writes is that
/// file's before-state, and one on any other file travels as a plan
/// condition.
///
/// **An expected value is judged on the bytes its check stands for.** On a
/// file the plan writes, the recorded before-state must hold the expected
/// value, judged on the bytes the target holds there — unless this apply
/// cannot see them, a target already holding its change, which is landed
/// whatever it held. On any other file a plan condition must name the file,
/// and where the file holds the content that condition names — which every
/// file does here, since a failed condition refused before recomposition —
/// that content must hold the expected value. Either way a plan whose
/// recorded state does not hold its own operation's expectation is not what
/// its operations do.
fn conditions_differ<V: VaultView>(
    plan: &ResolvedPlan,
    composition: &Composition,
    before: &BeforeStates<'_, V>,
) -> Result<Vec<DocumentPath>, V::Error> {
    let normalizer = before.normalizer();
    let carried: Vec<(NormalizedPath, &norn_wire::ContentHash)> = plan
        .conditions
        .iter()
        .filter_map(|condition| match condition {
            PlanCondition::ContentHash { path, hash } => {
                Some((identity(normalizer, path.as_str())?, hash))
            }
            // A link entry says how a link resolves, not what a file holds,
            // so no author condition is carried by one.
            PlanCondition::LinkResolution { .. } => None,
        })
        .collect();
    let carries = |file: &NormalizedPath, hash: Option<&norn_wire::ContentHash>| {
        carried.iter().any(|(condition, carried)| {
            condition == file && hash.is_none_or(|hash| *carried == hash)
        })
    };
    let mut differing = Vec::new();
    for operation in &plan.operations {
        for condition in &operation.conditions {
            let held = match condition {
                AuthorCondition::ContentHash { path, hash } => identity(normalizer, path.as_str())
                    .is_some_and(|file| match composition.before(&file) {
                        Some(before) => *before == FileState::present(hash.clone()),
                        None => carries(&file, Some(hash)),
                    }),
                AuthorCondition::ExpectedValue {
                    path,
                    field,
                    expect,
                } => match identity(normalizer, path.as_str()) {
                    None => false,
                    Some(file) => {
                        expectation_held(&file, path, field, expect, composition, before, &carries)?
                    }
                },
            };
            if !held {
                differing.push(condition_path(condition).clone());
            }
        }
    }
    Ok(differing)
}

/// Whether the expected value on `file` holds where the plan checks it: on
/// the recorded before-state of a file the plan writes, and on the content a
/// plan condition names for any other file (see [`conditions_differ`]).
fn expectation_held<V: VaultView>(
    file: &NormalizedPath,
    path: &DocumentPath,
    field: &str,
    expect: &ExpectedField,
    composition: &Composition,
    before: &BeforeStates<'_, V>,
    carries: &impl Fn(&NormalizedPath, Option<&norn_wire::ContentHash>) -> bool,
) -> Result<bool, V::Error> {
    let written = composition.before(file).is_some();
    if written && before.unseen.contains(file) {
        return Ok(true);
    }
    if !written && !carries(file, None) {
        return Ok(false);
    }
    Ok(match before.entry(file)? {
        Entry::Document { bytes, hash, .. } if written || carries(file, Some(&hash)) => {
            edit::expectation_unmet(path, &bytes, field, expect).is_none()
        }
        // A carried condition the file does not meet refused before
        // recomposition, so it is not judged again here.
        _ => !written,
    })
}

/// The file an author condition names.
fn condition_path(condition: &AuthorCondition) -> &DocumentPath {
    match condition {
        AuthorCondition::ContentHash { path, .. } | AuthorCondition::ExpectedValue { path, .. } => {
            path
        }
    }
}

/// The vault as it stood before the plan, where the plan touches it, and the
/// vault as it stands everywhere else.
///
/// **A target reads as its recorded before-state**: absent where the plan
/// found nothing, and a document with the before-state's hash where it found
/// one — holding the bytes the target still holds where it holds that
/// before-state, and the stand-in where this apply cannot see them. A place
/// the vault reads no documents at never reaches here: observing refuses a
/// target named at one, and any other name an operation carries is read from
/// the vault as it stands, which reads it as what it is.
struct BeforeStates<'a, V> {
    view: &'a V,
    plan: &'a ResolvedPlan,
    states: &'a [TargetState],
    by_identity: BTreeMap<NormalizedPath, Vec<usize>>,
    /// Each transition's identity, by index.
    identities: Vec<Option<NormalizedPath>>,
    /// Every target whose before-state this apply cannot see.
    unseen: BTreeSet<NormalizedPath>,
}

impl<'a, V: VaultView> BeforeStates<'a, V> {
    fn over(plan: &'a ResolvedPlan, states: &'a [TargetState], view: &'a V) -> Self {
        let identities: Vec<Option<NormalizedPath>> = plan
            .transitions
            .iter()
            .map(|transition| identity(view.normalizer(), transition.path.as_str()))
            .collect();
        let mut by_identity: BTreeMap<NormalizedPath, Vec<usize>> = BTreeMap::new();
        let mut unseen = BTreeSet::new();
        for (index, identity) in identities.iter().enumerate() {
            let Some(identity) = identity else { continue };
            if states[index].before_unseen(&plan.transitions[index]) {
                unseen.insert(identity.clone());
            }
            by_identity.entry(identity.clone()).or_default().push(index);
        }
        BeforeStates {
            view,
            plan,
            states,
            by_identity,
            identities,
            unseen,
        }
    }
}

impl<V: VaultView> VaultView for BeforeStates<'_, V> {
    type Error = V::Error;

    fn normalizer(&self) -> &PathNormalizer {
        self.view.normalizer()
    }

    fn entry(&self, path: &NormalizedPath) -> Result<Entry, V::Error> {
        let Some(indices) = self.by_identity.get(path) else {
            return self.view.entry(path);
        };
        // A case-only rename's identity has two transitions: the document
        // stood at the one whose before-state is present.
        let index = indices
            .iter()
            .copied()
            .find(|&index| self.plan.transitions[index].before != FileState::absent())
            .unwrap_or(indices[0]);
        let transition = &self.plan.transitions[index];
        let FileState::Present { hash } = &transition.before else {
            return Ok(Entry::Absent {
                at: transition.path.clone(),
            });
        };
        let bytes = match &self.states[index] {
            TargetState::AtBefore(Some(bytes)) => bytes.clone(),
            TargetState::Landed(Some(bytes)) if transition.after == transition.before => {
                bytes.clone()
            }
            _ => stand_in(),
        };
        Ok(Entry::Document {
            at: transition.path.clone(),
            bytes,
            hash: hash.clone(),
        })
    }

    fn folder_stands(&self, folder: &NormalizedPath) -> Result<bool, V::Error> {
        self.view.folder_stands(folder)
    }

    fn folder_names(&self, folder: &NormalizedPath) -> Result<Vec<std::ffi::OsString>, V::Error> {
        self.view.folder_names(folder)
    }
}

/// The bytes standing in for a before-state this apply cannot see. Nothing
/// composed from them is published or compared.
fn stand_in() -> Arc<[u8]> {
    Arc::from(&b""[..])
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use norn_wire::{
        AuthoredPlan, DocumentPath, FileState, Operation, OperationKind, ResolvedPlan,
        RootIdentity, Transition, VaultAddress, VaultName,
    };

    use norn_wire::PlanFault;

    use super::super::observe::{observe, recorded_lineage, units};
    use super::{Recomposed, recompose};
    use crate::planner::compose::content_hash;
    use crate::planner::links::testing::resolve_over_files as resolve;
    use crate::planner::view::VaultView;
    use crate::planner::view::memory::MemoryVault;

    fn path(text: &str) -> DocumentPath {
        DocumentPath::new(text).expect("a legal document path")
    }

    fn vault_name() -> VaultAddress {
        VaultAddress::name(VaultName::new("notes").expect("a legal vault name"))
    }

    fn root() -> RootIdentity {
        RootIdentity::from_device_and_inode(1, 2)
    }

    fn recomposed(plan: &ResolvedPlan, vault: &MemoryVault) -> Recomposed {
        let units = units(plan, VaultView::normalizer(vault));
        let (states, _) = observe(plan, &units, vault).expect("an infallible view");
        let lineage = recorded_lineage(plan, VaultView::normalizer(vault));
        recompose(plan, &states, &lineage, vault).expect("an infallible view")
    }

    /// On a root that folds case, two transitions sharing one identity — the
    /// old spelling taken away, the new one written — are published as one
    /// respell only where the operations make that case-only rename: a plan
    /// carrying the pair beside a removal is refused.
    #[test]
    fn two_spellings_of_one_file_the_operations_do_not_rename_are_refused() {
        let vault = MemoryVault::with(&[("Note.md", "note")]).folding_case();
        let hash = FileState::present(content_hash(b"note"));
        let plan = ResolvedPlan::new(
            vault_name(),
            root(),
            vec![Operation::new(OperationKind::delete_document(path(
                "Note.md",
            )))],
            vec![
                Transition::new(path("Note.md"), hash.clone(), FileState::absent()),
                Transition::new(path("note.md"), FileState::absent(), hash),
            ],
            Vec::new(),
        );
        match recomposed(&plan, &vault) {
            Recomposed::Invalid(PlanFault::TransitionsDisagree { paths, .. }) => {
                assert_eq!(paths, vec![path("note.md")]);
            }
            _ => panic!("the plan's transitions disagree with its operations"),
        }
    }

    /// The case-only rename the planner resolves recomposes as itself.
    #[test]
    fn a_case_only_rename_the_planner_resolved_is_sound() {
        let vault = MemoryVault::with(&[("Note.md", "note draft")]).folding_case();
        let authored = AuthoredPlan::new(
            vault_name(),
            vec![
                Operation::new(OperationKind::str_replace(
                    path("Note.md"),
                    "draft",
                    "final",
                )),
                Operation::new(OperationKind::move_document(
                    path("Note.md"),
                    path("note.md"),
                )),
            ],
        );
        let plan = resolve(authored, root(), &BTreeSet::new(), &vault)
            .expect("the plan resolves")
            .plan;
        assert_eq!(plan.transitions.len(), 2);
        assert!(matches!(recomposed(&plan, &vault), Recomposed::Sound(_)));
    }
}
