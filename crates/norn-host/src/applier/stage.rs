//! The check-and-stage phase: every target's state, every condition, every
//! composed result's schema, and a shadow for every written target, before
//! anything is published.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use norn_fs::{PathNormalizer, Refusal, ShadowHome, Staging};
use norn_wire::{FileState, OperationKind, RefusedCheck, ResolvedPlan, Transition};

use super::observe::{
    TargetState, Unit, failed_conditions, identity, is_create, is_removal, observe,
    transition_index, units,
};
use super::recompose::recompose;
use super::schema;
use crate::derivation::Declared;
use crate::planner::compose::Composition;
use crate::planner::view::{TreeView, VaultView, wire_hash};

/// A plan every target of which is checked and staged: one fixed-size record
/// per publication, in the order they publish, and nothing that holds a file
/// open or a file's bytes.
#[derive(Debug)]
pub(super) struct StagedPlan {
    pub(super) targets: Vec<StagedTarget>,
}

/// One publication waiting for its turn.
///
/// **Plain data.** The unit names the plan's transitions by index, and the
/// kernel's record names the target, its root, its transition's hashes and
/// its shadow; neither holds a handle or a byte of content (ADR 0031).
#[derive(Debug)]
pub(super) struct StagedTarget {
    pub(super) unit: Unit,
    pub(super) phase: Phase,
    pub(super) held: Held,
}

/// What staging left for publication to act on.
#[derive(Debug)]
pub(super) enum Held {
    /// A shadow, or a removal, waiting to publish.
    Staged(norn_fs::Staged),
    /// A target already at its after-state, which publication confirms.
    Landed(norn_fs::Landed),
    /// A target whose two states are absence, standing absent: nothing to
    /// publish or confirm.
    Nothing,
}

/// When a publication runs: creates first, then replaces (a respell among
/// them), then removals (ADR 0031).
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum Phase {
    Create,
    Replace,
    Remove,
    Nothing,
}

/// Why the plan did not reach publication.
#[derive(Debug)]
pub(super) enum Stop {
    /// A check refused: drift, a condition, a schema violation, a taken name.
    Refused(Vec<RefusedCheck>),
    /// The vault root is not the directory the plan's identity names.
    RootReplaced,
    /// The machine failed, in words.
    Failed(String),
}

/// What a kernel refusal means for a plan.
pub(super) enum Classified {
    /// The target is not what the plan was checked against, and holds this.
    Drift(FileState),
    /// A create's name is taken.
    NameTaken,
    /// The vault root was replaced.
    RootReplaced,
    /// The machine failed, in words: a staged shadow gone or changed among
    /// them, which is an I/O failure for that target and never drift.
    Io(String),
}

/// What `refusal` means for the target it was about.
///
/// A create refused beside folders it could not take back is classified by
/// the refusal it wraps. A request the kernel cannot act on — a path that is
/// not a name below the root, a respell on a folder not proven to fold — is
/// the planner's fault, never the vault's: it is asserted in a debug build and
/// answered as a failure otherwise, since nothing was published.
pub(super) fn classify(refusal: &Refusal) -> Classified {
    match refusal {
        Refusal::FoldersLeft { refusal, .. } => classify(refusal),
        Refusal::Drifted { observed, .. } => Classified::Drift(match observed {
            Some(post) => FileState::present(wire_hash(post.content_hash)),
            None => FileState::absent(),
        }),
        Refusal::Republished { .. }
        | Refusal::NotRegularFile { .. }
        | Refusal::SymlinkDestination { .. }
        | Refusal::LinkedAncestor { .. } => Classified::Drift(FileState::absent()),
        Refusal::DestinationExists { .. } | Refusal::FolderIsFile { .. } => Classified::NameTaken,
        Refusal::RootReplaced { .. } => Classified::RootReplaced,
        Refusal::InvalidRequest { .. } | Refusal::NotCaseFolding { .. } => {
            debug_assert!(
                false,
                "the planner resolved a request the kernel refuses: {refusal}"
            );
            Classified::Io(format!(
                "the plan asks what the filesystem cannot do: {refusal}"
            ))
        }
        Refusal::ExclusiveCreateUnsupported { .. }
        | Refusal::Environment { .. }
        | Refusal::LockFileReplaced { .. } => Classified::Io(refusal.to_string()),
    }
}

/// Check every target of `plan` and stage every written one.
///
/// Nothing is published here, and a refusal discards every shadow staged
/// before it, so a plan stopped in this phase leaves the vault as it found
/// it. What is returned holds no bytes: the observed and composed contents
/// are dropped when this returns.
pub(super) fn check_and_stage(
    anchor: &Path,
    root: norn_fs::Identity,
    shadows: &ShadowHome,
    plan: &ResolvedPlan,
    view: &TreeView,
    declared: &Declared,
) -> Result<StagedPlan, Stop> {
    let normalizer = view.normalizer();
    let units = units(plan, normalizer);
    let (states, _) =
        observe(plan, &units, view).map_err(|error| Stop::Failed(error.to_string()))?;
    let mut checks: Vec<RefusedCheck> = drifted_checks(plan, &states);
    checks.extend(
        failed_conditions(plan, view)
            .map_err(|error| Stop::Failed(error.to_string()))?
            .into_iter()
            .map(RefusedCheck::condition_failed),
    );
    if !checks.is_empty() {
        return Err(Stop::Refused(checks));
    }
    let composition =
        recompose(plan, &states, view).map_err(|error| Stop::Failed(error.to_string()))?;
    let mut contents: Vec<Option<Arc<[u8]>>> = Vec::with_capacity(units.len());
    for unit in &units {
        match content(plan, *unit, &states, &composition) {
            Ok(bytes) => contents.push(bytes),
            Err(check) => checks.push(check),
        }
    }
    if !checks.is_empty() {
        return Err(Stop::Refused(checks));
    }
    checks.extend(schema_checks(plan, &units, &states, &contents, declared));
    if !checks.is_empty() {
        return Err(Stop::Refused(checks));
    }
    drop(composition);
    let mut staged: Vec<StagedTarget> = Vec::with_capacity(units.len());
    for (unit, content) in units.iter().zip(&contents) {
        let held = match stage_one(anchor, root, shadows, plan, *unit, content.as_deref()) {
            Ok(held) => held,
            Err(stop) => {
                discard_all(anchor, shadows, staged);
                return Err(stop);
            }
        };
        staged.push(StagedTarget {
            unit: *unit,
            phase: phase(plan, *unit),
            held,
        });
    }
    Ok(StagedPlan {
        targets: in_publication_order(plan, staged, normalizer),
    })
}

/// Remove every shadow `staged` holds, publishing nothing.
pub(super) fn discard_all(
    anchor: &Path,
    shadows: &ShadowHome,
    staged: impl IntoIterator<Item = StagedTarget>,
) {
    for target in staged {
        if let Held::Staged(staged) = target.held {
            norn_fs::discard(anchor, staged, shadows);
        }
    }
}

/// A drifted check for every target holding neither of its states.
pub(super) fn drifted_checks(plan: &ResolvedPlan, states: &[TargetState]) -> Vec<RefusedCheck> {
    states
        .iter()
        .zip(&plan.transitions)
        .filter_map(|(state, transition)| match state {
            TargetState::Drifted(holds) => Some(RefusedCheck::drifted(
                transition.path.clone(),
                holds.clone(),
            )),
            _ => None,
        })
        .collect()
}

/// The bytes `unit` publishes, where it writes any.
///
/// A target already holding its after-state, and a respell halfway, hold the
/// after-state's bytes, and those are its content; every other written target
/// takes the recomposed bytes, which must hash to the after-state. A target
/// whose recomposition does not is refused as drift: the before-states it is
/// composed from are not what the plan was resolved from.
fn content(
    plan: &ResolvedPlan,
    unit: Unit,
    states: &[TargetState],
    composition: &Composition,
) -> Result<Option<Arc<[u8]>>, RefusedCheck> {
    let written = match unit {
        Unit::One(index) => index,
        Unit::Respell { new, .. } => new,
    };
    let transition = &plan.transitions[written];
    let FileState::Present { hash } = &transition.after else {
        return Ok(None);
    };
    let held = match unit {
        Unit::Respell { old, .. } => match (&states[old], &states[written]) {
            (TargetState::Halfway(bytes), _) | (_, TargetState::Landed(Some(bytes))) => {
                Some(bytes.clone())
            }
            _ => None,
        },
        Unit::One(_) => match &states[written] {
            TargetState::Landed(Some(bytes)) => Some(bytes.clone()),
            _ => None,
        },
    };
    if let Some(bytes) = held {
        return Ok(Some(bytes));
    }
    let composed = composition
        .targets
        .get(&transition.path)
        .and_then(|target| target.after.clone());
    match composed {
        Some(bytes) if wire_hash(norn_fs::ContentHash::of(&bytes)) == *hash => Ok(Some(bytes)),
        _ => Err(RefusedCheck::drifted(
            transition.path.clone(),
            transition.before.clone(),
        )),
    }
}

/// The schema checks refusing any composed result a target at its
/// before-state would publish.
fn schema_checks(
    plan: &ResolvedPlan,
    units: &[Unit],
    states: &[TargetState],
    contents: &[Option<Arc<[u8]>>],
    declared: &Declared,
) -> Vec<RefusedCheck> {
    // Where a written document's content came from when it stood nowhere
    // before: a document the plan takes away.
    let taken_away: Vec<(usize, &Arc<[u8]>)> = states
        .iter()
        .enumerate()
        .filter(|(index, _)| is_removal(&plan.transitions[*index]))
        .filter_map(|(index, state)| match state {
            TargetState::AtBefore(Some(bytes)) => Some((index, bytes)),
            _ => None,
        })
        .collect();
    let mut checks = Vec::new();
    for (unit, content) in units.iter().zip(contents) {
        let Some(after) = content else { continue };
        let (source, written) = match *unit {
            Unit::One(index) => (index, index),
            Unit::Respell { old, new } => (old, new),
        };
        let TargetState::AtBefore(own) = &states[source] else {
            continue;
        };
        let path = &plan.transitions[written].path;
        let before: Vec<schema::Judged> = match own {
            Some(bytes) => vec![schema::judge(path, bytes, declared)],
            None => taken_away
                .iter()
                .map(|(index, bytes)| {
                    schema::judge(&plan.transitions[*index].path, bytes, declared)
                })
                .collect(),
        };
        let after = schema::judge(path, after, declared);
        checks.extend(schema::refused(path, &after, &before));
    }
    checks
}

/// Stage `unit` with `content`, or say why the plan stops.
fn stage_one(
    anchor: &Path,
    root: norn_fs::Identity,
    shadows: &ShadowHome,
    plan: &ResolvedPlan,
    unit: Unit,
    content: Option<&[u8]>,
) -> Result<Held, Stop> {
    let (path, transition) = match unit {
        Unit::One(index) => {
            let transition = &plan.transitions[index];
            let kernel = match (&transition.before, &transition.after, content) {
                (FileState::Absent {}, FileState::Present { .. }, Some(content)) => {
                    norn_fs::Transition::Create { content }
                }
                (FileState::Present { hash }, FileState::Present { .. }, Some(content)) => {
                    norn_fs::Transition::Replace {
                        before: kernel_hash(hash),
                        content,
                    }
                }
                (FileState::Present { hash }, FileState::Absent {}, _) => {
                    norn_fs::Transition::Remove {
                        before: kernel_hash(hash),
                    }
                }
                _ => return Ok(Held::Nothing),
            };
            (&transition.path, kernel)
        }
        Unit::Respell { old, new } => {
            let (old, new) = (&plan.transitions[old], &plan.transitions[new]);
            let FileState::Present { hash } = &old.before else {
                unreachable!("a respell's old spelling holds a document before");
            };
            let content = if new.after == old.before {
                None
            } else {
                content
            };
            (
                &old.path,
                norn_fs::Transition::Respell {
                    to: Path::new(new.path.as_str()),
                    before: kernel_hash(hash),
                    content,
                },
            )
        }
    };
    match norn_fs::stage(anchor, root, Path::new(path.as_str()), transition, shadows) {
        Ok(Staging::Staged(staged)) => Ok(Held::Staged(staged)),
        Ok(Staging::Landed(landed)) => Ok(Held::Landed(landed)),
        Err(refusal) => Err(match classify(&refusal) {
            Classified::Drift(holds) => {
                Stop::Refused(vec![RefusedCheck::drifted(path.clone(), holds)])
            }
            Classified::NameTaken => Stop::Refused(vec![RefusedCheck::name_taken(path.clone())]),
            Classified::RootReplaced => Stop::RootReplaced,
            Classified::Io(detail) => Stop::Failed(detail),
        }),
    }
}

/// The filesystem layer's hash of a wire hash. Every wire hash a resolved plan
/// carries names a SHA-256 digest, so a hash that does not is no document's.
pub(super) fn kernel_hash(hash: &norn_wire::ContentHash) -> norn_fs::ContentHash {
    norn_fs::ContentHash::from_hex(hash.hex()).unwrap_or_else(|| norn_fs::ContentHash::of(b""))
}

/// Which phase `unit` publishes in.
fn phase(plan: &ResolvedPlan, unit: Unit) -> Phase {
    match unit {
        Unit::Respell { .. } => Phase::Replace,
        Unit::One(index) => {
            let transition: &Transition = &plan.transitions[index];
            if is_create(transition) {
                Phase::Create
            } else if is_removal(transition) {
                Phase::Remove
            } else if matches!(transition.after, FileState::Present { .. }) {
                Phase::Replace
            } else {
                Phase::Nothing
            }
        }
    }
}

/// `staged` in the order it publishes: creates, then replaces, then
/// removals; and among the replaces, every target a move draws its content
/// from after the target it moves to, so no source is replaced before what
/// draws on it landed. The planner refuses a cycle of content, so the order
/// exists; within one phase the plan's own order breaks ties.
fn in_publication_order(
    plan: &ResolvedPlan,
    mut staged: Vec<StagedTarget>,
    normalizer: &PathNormalizer,
) -> Vec<StagedTarget> {
    staged.sort_by_key(|target| target.phase);
    let index_of = transition_index(plan, normalizer);
    let unit_of: BTreeMap<usize, usize> = staged
        .iter()
        .enumerate()
        .flat_map(|(position, target)| {
            target
                .unit
                .transitions()
                .map(move |index| (index, position))
        })
        .collect();
    // Each staged position waits for the positions it must follow.
    let mut waits: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for operation in &plan.operations {
        let OperationKind::MoveDocument { from, to } = &operation.kind else {
            continue;
        };
        let (Some(from), Some(to)) = (
            identity(normalizer, from.as_str()).and_then(|id| index_of.get(&id)),
            identity(normalizer, to.as_str()).and_then(|id| index_of.get(&id)),
        ) else {
            continue;
        };
        let (source, destination) = (unit_of[from], unit_of[to]);
        if source != destination {
            waits.entry(source).or_default().insert(destination);
        }
    }
    let mut placed: BTreeSet<usize> = BTreeSet::new();
    let mut order: Vec<usize> = Vec::with_capacity(staged.len());
    while order.len() < staged.len() {
        let next = (0..staged.len())
            .find(|position| {
                !placed.contains(position)
                    && waits
                        .get(position)
                        .is_none_or(|before| before.iter().all(|wait| placed.contains(wait)))
            })
            // A cycle the planner let through: publish in phase order rather
            // than never.
            .unwrap_or_else(|| {
                (0..staged.len())
                    .find(|position| !placed.contains(position))
                    .expect("a position is left")
            });
        placed.insert(next);
        order.push(next);
    }
    let mut slots: Vec<Option<StagedTarget>> = staged.into_iter().map(Some).collect();
    order
        .into_iter()
        .map(|position| slots[position].take().expect("each position is taken once"))
        .collect()
}
