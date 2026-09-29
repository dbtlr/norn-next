//! The check-and-stage phase: every target's state, every condition, every
//! composed result's schema, and a shadow for every written target, before
//! anything is published.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use norn_fs::{PathNormalizer, Refusal, ShadowHome, Staging};
use norn_wire::{FileState, PlanFault, RefusedCheck, ResolvedPlan, Transition};

use super::observe::{
    TargetState, Unit, failed_conditions, identity, is_create, is_removal, observe,
    recorded_lineage, transition_index, units,
};
use super::recompose::{Recomposed, recompose};
use super::schema;
use crate::derivation::Declared;
use crate::planner::compose::Composition;
use crate::planner::lineage::Lineage;
use crate::planner::view::{TreeView, VaultView, wire_hash};

/// A plan every target of which is checked and staged: one fixed-size record
/// per publication, in the order they publish, and nothing that holds a file
/// open or a file's bytes.
#[derive(Debug)]
pub(super) struct StagedPlan {
    pub(super) targets: Vec<StagedTarget>,
    /// Each transition's path as the store names it, by index.
    pub(super) stored: Vec<norn_store::DocumentPath>,
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
    /// The plan is not what its operations do, in words.
    Unsound(String),
    /// The operations' own shape is wrong.
    Invalid(PlanFault),
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
/// **The checks run in this order**: the plan names every target once, at the
/// spelling the vault gives it, at a place the vault reads documents at and
/// the store can name; no target drifted and every condition holds; the
/// operations, run again from the before-states, are exactly the plan's
/// transitions ([`recompose`]); every result passes the vault schema; and the
/// publication order exists. A plan that fails the first or the third is not
/// what its operations do, and is refused as that rather than as drift.
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
    let stored = stored_paths(plan).map_err(Stop::Unsound)?;
    let units = units(plan, normalizer);
    let (states, _) =
        observe(plan, &units, view).map_err(|error| Stop::Failed(error.to_string()))?;
    if let Some(detail) = states.iter().find_map(|state| match state {
        TargetState::Unplaced(detail) => Some(detail.clone()),
        _ => None,
    }) {
        return Err(Stop::Unsound(detail));
    }
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
    let lineage = recorded_lineage(plan, normalizer);
    let composition = match recompose(plan, &states, &lineage, view)
        .map_err(|error| Stop::Failed(error.to_string()))?
    {
        Recomposed::Sound(composition) => composition,
        Recomposed::Unsound(detail) => return Err(Stop::Unsound(detail)),
        Recomposed::Invalid(fault) => return Err(Stop::Invalid(fault)),
    };
    let contents: Vec<Option<Arc<[u8]>>> = units
        .iter()
        .map(|unit| content(plan, *unit, &states, &composition))
        .collect::<Result<_, _>>()
        .map_err(Stop::Unsound)?;
    drop(composition);
    let schema = Judging {
        plan,
        states: &states,
        lineage: &lineage,
        normalizer,
        declared,
    };
    checks.extend(schema.checks(&units, &contents));
    if !checks.is_empty() {
        return Err(Stop::Refused(checks));
    }
    let phases: Vec<Phase> = units.iter().map(|unit| phase(plan, *unit)).collect();
    let order = publication_order(plan, &units, &phases, &lineage, normalizer);
    let mut staged: Vec<StagedTarget> = Vec::with_capacity(units.len());
    for position in order {
        let unit = units[position];
        let content = contents[position].as_deref();
        let held = match stage_one(anchor, root, shadows, plan, unit, content) {
            Ok(held) => held,
            Err(stop) => {
                discard_all(anchor, shadows, staged);
                return Err(stop);
            }
        };
        staged.push(StagedTarget {
            unit,
            phase: phases[position],
            held,
        });
    }
    Ok(StagedPlan {
        targets: staged,
        stored,
    })
}

/// Every transition's path as the store names it, or why the plan names a
/// target at a path the store cannot name, which would be published and never
/// recorded.
///
/// **A plan names each target at one spelling.** The planner writes every
/// transition at a normalized spelling, and the kernel keeps a path as it is
/// given — a `./` component included — so a target spelled otherwise, as a
/// plan edited by hand can be, would be published, recorded and derived at a
/// second spelling of one file. Normalizing a path drops only its `.` and
/// empty components, and the store's grammar refuses both, so a path the store
/// names is already the one spelling of its file.
fn stored_paths(plan: &ResolvedPlan) -> Result<Vec<norn_store::DocumentPath>, String> {
    plan.transitions
        .iter()
        .map(|transition| {
            let path = &transition.path;
            norn_store::DocumentPath::new(path.as_str())
                .map_err(|error| format!("`{path}` is no path the store can name: {error}"))
        })
        .collect()
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
/// takes the recomposed bytes, which [`recompose`] held to its after-state.
fn content(
    plan: &ResolvedPlan,
    unit: Unit,
    states: &[TargetState],
    composition: &Composition,
) -> Result<Option<Arc<[u8]>>, String> {
    let written = match unit {
        Unit::One(index) => index,
        Unit::Respell { new, .. } => new,
    };
    let transition = &plan.transitions[written];
    if !matches!(transition.after, FileState::Present { .. }) {
        return Ok(None);
    }
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
    composition
        .targets
        .get(&transition.path)
        .and_then(|target| target.after.clone())
        .map(Some)
        .ok_or_else(|| format!("its operations leave nothing at `{}`", transition.path))
}

/// What the schema check reads: the plan, what its targets hold, where each
/// target's content came from, and the declaration.
struct Judging<'a> {
    plan: &'a ResolvedPlan,
    states: &'a [TargetState],
    lineage: &'a Lineage,
    normalizer: &'a PathNormalizer,
    declared: &'a Declared,
}

impl Judging<'_> {
    /// The schema checks refusing any composed result a target at its
    /// before-state would publish.
    ///
    /// **Each result is judged against the document its content came from**:
    /// its own before-state where it is edited in place, the moved document's
    /// where a move carried it, and nothing where an operation wrote it.
    fn checks(&self, units: &[Unit], contents: &[Option<Arc<[u8]>>]) -> Vec<RefusedCheck> {
        let index_of = transition_index(self.plan, self.normalizer);
        let mut checks = Vec::new();
        for (unit, content) in units.iter().zip(contents) {
            let Some(after) = content else { continue };
            let (source, written) = match *unit {
                Unit::One(index) => (index, index),
                Unit::Respell { old, new } => (old, new),
            };
            if !matches!(self.states[source], TargetState::AtBefore(_)) {
                continue;
            }
            let path = &self.plan.transitions[written].path;
            let drawn_from = identity(self.normalizer, path.as_str())
                .and_then(|file| self.lineage.source(&file))
                .and_then(|source| index_of.get(&source.from).copied());
            let before: Vec<schema::Judged> = drawn_from
                .and_then(|index| match &self.states[index] {
                    TargetState::AtBefore(Some(bytes)) => Some(schema::judge(
                        &self.plan.transitions[index].path,
                        bytes,
                        self.declared,
                    )),
                    _ => None,
                })
                .into_iter()
                .collect();
            let after = schema::judge(path, after, self.declared);
            checks.extend(schema::refused(path, &after, &before));
        }
        checks
    }
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

/// The filesystem layer's hash of a wire hash.
///
/// **Total over the wire's type**: a wire hash is read through its grammar —
/// `sha256:` and 64 lowercase hexadecimal digits — whether it is built or
/// deserialized, and those digits are what the layer parses, so a hash of
/// another algorithm never reaches a plan to be mapped to anything.
pub(super) fn kernel_hash(hash: &norn_wire::ContentHash) -> norn_fs::ContentHash {
    norn_fs::ContentHash::from_hex(hash.hex()).expect("a wire hash spells a SHA-256 digest")
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

/// The order `units` publish in, by position: creates, then replaces, then
/// removals; and every target whose content is drawn from another target's
/// before-state before that source, so no source is replaced or removed
/// before what draws on it landed. Within one phase the plan's own order
/// breaks ties.
///
/// **Always an order.** Each file draws on at most one other, and
/// [`recompose`] refuses a plan whose drawing closes a cycle through the
/// planner's one content-cycle rule, so what is left to order has no cycle.
fn publication_order(
    plan: &ResolvedPlan,
    units: &[Unit],
    phases: &[Phase],
    lineage: &Lineage,
    normalizer: &PathNormalizer,
) -> Vec<usize> {
    let mut by_phase: Vec<usize> = (0..units.len()).collect();
    by_phase.sort_by_key(|&position| phases[position]);
    let index_of = transition_index(plan, normalizer);
    let unit_of: BTreeMap<usize, usize> = units
        .iter()
        .enumerate()
        .flat_map(|(position, unit)| unit.transitions().map(move |index| (index, position)))
        .collect();
    // Each source's unit waits for the units of the targets drawing on it.
    let mut waits: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for (file, drawn) in lineage.drawing() {
        let (Some(target), Some(source)) = (index_of.get(file), index_of.get(&drawn.from)) else {
            continue;
        };
        let (target, source) = (unit_of[target], unit_of[source]);
        if source != target {
            waits.entry(source).or_default().insert(target);
        }
    }
    let mut placed: BTreeSet<usize> = BTreeSet::new();
    let mut order: Vec<usize> = Vec::with_capacity(units.len());
    while order.len() < units.len() {
        let next = by_phase
            .iter()
            .copied()
            .find(|position| {
                !placed.contains(position)
                    && waits
                        .get(position)
                        .is_none_or(|before| before.iter().all(|wait| placed.contains(wait)))
            })
            .expect("recomposition refused every content cycle");
        placed.insert(next);
        order.push(next);
    }
    order
}
