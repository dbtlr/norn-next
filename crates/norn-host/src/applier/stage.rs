//! The check-and-stage phase: every target's state, every condition, every
//! composed result's schema, and a shadow for every written target, before
//! anything is published.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::path::Path;
use std::sync::Arc;

use norn_fs::{PathNormalizer, Refusal, ShadowHome, Staging};
use norn_wire::{
    DocumentPath, FileState, LinkAdvisory, PlanCondition, PlanFault, RefusedCheck, ResolvedPlan,
    SchemaViolation, Transition,
};

use super::observe::{
    TargetState, Unit, failed_conditions, identity, is_create, is_removal, observe,
    recorded_lineage, transition_index, units,
};
use super::recompose::{Recomposed, disagreement, recompose};
use super::schema;
use super::shape::shape_disagrees;
use crate::derivation::Declared;
use crate::planner::compose::Composition;
use crate::planner::lineage::Lineage;
use crate::planner::links::{LinkIndex, Target, change_set, entry_key};
use crate::planner::view::{TreeView, VaultView, wire_hash};
use crate::refusal::PageRefused;

/// Where a plan's resolution change set is computed again: the snapshot the
/// apply's intake left, or a preview's.
pub(crate) type Links<'a> = &'a dyn LinkIndex<Error = PageRefused>;

/// A plan every target of which is checked and staged: one fixed-size record
/// per publication, in the order they publish, and nothing that holds a file
/// open or a file's bytes.
#[derive(Debug)]
pub(super) struct StagedPlan {
    pub(super) targets: Vec<StagedTarget>,
    /// Every schema violation the plan's force let through.
    pub(super) forced: Vec<SchemaViolation>,
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
    /// The plan's own shape is wrong: its operations', or its transitions
    /// are not what its operations do.
    Invalid(PlanFault),
    /// The vault root is not the directory the plan's identity names.
    RootReplaced,
    /// The machine failed, in words.
    Failed(String),
    /// The snapshot the plan's links are judged on could not be read.
    Unread(PageRefused),
}

/// Why a plan failed its checks, before anything was staged.
#[derive(Debug)]
pub(super) enum Unfit {
    /// A check refused: drift, a condition, a schema violation.
    Refused(Vec<RefusedCheck>),
    /// The plan's own shape is wrong: its operations', or its transitions
    /// are not what its operations do.
    Invalid(PlanFault),
    /// The vault could not be read, in words.
    Failed(String),
    /// The snapshot the plan's links are judged on could not be read.
    Unread(PageRefused),
}

impl From<Unfit> for Stop {
    fn from(unfit: Unfit) -> Self {
        match unfit {
            Unfit::Refused(checks) => Stop::Refused(checks),
            Unfit::Invalid(fault) => Stop::Invalid(fault),
            Unfit::Failed(detail) => Stop::Failed(detail),
            Unfit::Unread(refused) => Stop::Unread(refused),
        }
    }
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

/// Check every target of `plan` and stage every written one: [`check`], then
/// [`stage`].
pub(super) fn check_and_stage(
    anchor: &Path,
    root: norn_fs::Identity,
    shadows: &ShadowHome,
    plan: &ResolvedPlan,
    view: &TreeView,
    declared: &Declared,
    links: Links<'_>,
) -> Result<StagedPlan, Stop> {
    let checked = check(plan, view, declared, links).map_err(Stop::from)?;
    stage(anchor, root, shadows, plan, view.normalizer(), checked)
}

/// A plan every check passed: what each target publishes, and in which
/// phase, with the lineage its publication order follows. Its contents are
/// held only until [`stage`] has staged them.
pub(super) struct Checked {
    /// Every schema violation the plan's force let through: none for a plan
    /// that is not forced, which refuses on each instead.
    pub(super) forced: Vec<SchemaViolation>,
    /// What the plan does to the links a caller should look at, from its
    /// resolution change set computed again.
    pub(super) links: Vec<LinkAdvisory>,
    units: Vec<Unit>,
    contents: Vec<Option<Arc<[u8]>>>,
    phases: Vec<Phase>,
    lineage: Lineage,
    stored: Vec<norn_store::DocumentPath>,
}

/// Check every target of `plan`, reading the vault and writing nothing.
///
/// **The checks run in this order**: no operation carries a `where` target
/// or a folder move planning did not expand, which stops as
/// [`PlanFault::UnexpandedTarget`], nor a cascade on a kind that does not
/// cascade, which stops as [`PlanFault::MisplacedCascade`];
/// the store can name every target; before
/// any vault read, the transitions name exactly the files the operations
/// touch, each once ([`shape_disagrees`]); every target stands at the spelling
/// the vault gives it, at a place the vault reads documents at; no target
/// drifted and every content condition holds; the operations, run again from
/// the before-states, are exactly the plan's transitions ([`recompose`]); the
/// plan's resolution change set, computed again from those results through
/// `links` ([`link_checks`]), is exactly the one it records; and every result
/// passes the vault schema, or, for a forced plan, has each violation it
/// introduces listed rather than refused. A plan whose store paths, shape,
/// target places or recomposition fail is not what its operations do: its own
/// shape is wrong, and it stops as [`PlanFault::TransitionsDisagree`] naming
/// the files it disagrees at, never as drift. Drift, a failed condition, a
/// schema violation, a taken name, a replaced root and an I/O failure each
/// answer as themselves.
///
/// **This is the one judgment of a resolved plan.** An apply stages what it
/// passes, and a preview of a resolved plan answers from it, so what a caller
/// previewed is what an apply of the same plan over the same files does.
pub(super) fn check(
    plan: &ResolvedPlan,
    view: &TreeView,
    declared: &Declared,
    links: Links<'_>,
) -> Result<Checked, Unfit> {
    // An operation whose target planning never expanded touches no file the
    // shape check or the recomposition could name, so it is refused first,
    // before it could pass unread; so is a cascade on a kind that does not
    // cascade, which no planning wrote.
    if let Some(fault) = plan
        .unexpanded_targets()
        .or_else(|| plan.misplaced_cascades())
    {
        return Err(Unfit::Invalid(fault));
    }
    let normalizer = view.normalizer();
    let stored = stored_paths(plan).map_err(|paths| Unfit::Invalid(disagreement(paths)))?;
    let misshapen = shape_disagrees(plan, normalizer);
    if !misshapen.is_empty() {
        return Err(Unfit::Invalid(disagreement(misshapen)));
    }
    let units = units(plan, normalizer);
    let (states, _) =
        observe(plan, &units, view).map_err(|error| Unfit::Failed(error.to_string()))?;
    let unplaced: Vec<DocumentPath> = states
        .iter()
        .zip(&plan.transitions)
        .filter(|(state, _)| matches!(state, TargetState::Unplaced))
        .map(|(_, transition)| transition.path.clone())
        .collect();
    if !unplaced.is_empty() {
        return Err(Unfit::Invalid(disagreement(unplaced)));
    }
    let mut checks: Vec<RefusedCheck> = drifted_checks(plan, &states);
    checks.extend(
        failed_conditions(plan, view)
            .map_err(|error| Unfit::Failed(error.to_string()))?
            .into_iter()
            .map(RefusedCheck::condition_failed),
    );
    if !checks.is_empty() {
        return Err(Unfit::Refused(checks));
    }
    let lineage = recorded_lineage(plan, normalizer);
    let composition = match recompose(plan, &states, &lineage, view)
        .map_err(|error| Unfit::Failed(error.to_string()))?
    {
        Recomposed::Sound(composition) => composition,
        Recomposed::Invalid(fault) => return Err(Unfit::Invalid(fault)),
    };
    let contents: Vec<Option<Arc<[u8]>>> = units
        .iter()
        .map(|unit| content(plan, *unit, &states, &composition))
        .collect::<Result<_, _>>()
        .map_err(|path| Unfit::Invalid(disagreement([path])))?;
    drop(composition);
    let mut after: Vec<Option<&[u8]>> = vec![None; plan.transitions.len()];
    for (unit, content) in units.iter().zip(&contents) {
        let written = match unit {
            Unit::One(index) => *index,
            Unit::Respell { new, .. } => *new,
        };
        after[written] = content.as_deref();
    }
    let targets: Vec<Target<'_>> = plan
        .transitions
        .iter()
        .zip(after)
        .map(|(transition, after)| Target {
            path: &transition.path,
            before: matches!(transition.before, FileState::Present { .. }),
            after,
        })
        .collect();
    let recomputed = change_set(&targets, &lineage, normalizer, &plan.operations, links)
        .map_err(Unfit::Unread)?;
    drop(targets);
    checks.extend(link_checks(&plan.conditions, &recomputed.entries));
    let schema = Judging {
        plan,
        states: &states,
        lineage: &lineage,
        normalizer,
        declared,
    };
    let violations = schema.violations(&units, &contents);
    let forced = if plan.force {
        violations
    } else {
        checks.extend(violations.into_iter().map(|violation| {
            RefusedCheck::schema_violation(
                violation.path,
                violation.kind,
                violation.target,
                violation.message,
            )
        }));
        Vec::new()
    };
    if !checks.is_empty() {
        return Err(Unfit::Refused(checks));
    }
    let phases: Vec<Phase> = units.iter().map(|unit| phase(plan, *unit)).collect();
    Ok(Checked {
        forced,
        links: recomputed.advisories,
        units,
        contents,
        phases,
        lineage,
        stored,
    })
}

/// Stage every written target of the plan `checked` passed, in the order they
/// publish: a shadow for every written target, a create included.
///
/// Nothing is published here, and a refusal discards every shadow staged
/// before it, so a plan stopped in this phase leaves the vault as it found
/// it. What is returned holds no bytes: the checked contents are dropped when
/// this returns.
pub(super) fn stage(
    anchor: &Path,
    root: norn_fs::Identity,
    shadows: &ShadowHome,
    plan: &ResolvedPlan,
    normalizer: &PathNormalizer,
    checked: Checked,
) -> Result<StagedPlan, Stop> {
    let Checked {
        forced,
        links: _,
        units,
        contents,
        phases,
        lineage,
        stored,
    } = checked;
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
        forced,
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
/// names is already the one spelling of its file. Where the store cannot
/// name a target, every such target's path is returned.
fn stored_paths(plan: &ResolvedPlan) -> Result<Vec<norn_store::DocumentPath>, Vec<DocumentPath>> {
    let mut stored = Vec::with_capacity(plan.transitions.len());
    let mut unnamed = Vec::new();
    for transition in &plan.transitions {
        match norn_store::DocumentPath::new(transition.path.as_str()) {
            Ok(path) => stored.push(path),
            Err(_) => unnamed.push(transition.path.clone()),
        }
    }
    if unnamed.is_empty() {
        Ok(stored)
    } else {
        Err(unnamed)
    }
}

/// Remove every shadow `staged` holds, publishing nothing.
#[allow(clippy::disallowed_methods)] // The one applier: the vault write kernel's one caller.
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

/// Every way the resolution change set a plan records differs from
/// `recomputed`, the set computed again from the plan: each entry it records
/// that the set does not hold with the same values fails, and each entry the
/// set holds that it does not record is unrecorded. A content condition is
/// not an entry, and is judged on its own.
///
/// **The set is exact** (ADR 0031): a link whose resolution the vault outside
/// the plan moved since planning — a document created or removed there that
/// a recorded link now names, or a new link to a document the plan moves —
/// refuses the plan, and the refusal's fresh plan records the set as it
/// stands now.
fn link_checks(recorded: &[PlanCondition], recomputed: &[PlanCondition]) -> Vec<RefusedCheck> {
    let entries = |conditions: &[PlanCondition]| -> BTreeMap<_, PlanCondition> {
        conditions
            .iter()
            .filter_map(|condition| match condition {
                PlanCondition::LinkResolution { link, .. } => {
                    Some((entry_key(link), condition.clone()))
                }
                PlanCondition::ContentHash { .. } => None,
            })
            .collect()
    };
    let computed = entries(recomputed);
    let recorded_keys = entries(recorded);
    let mut checks: Vec<RefusedCheck> = recorded
        .iter()
        .filter(|condition| match condition {
            PlanCondition::LinkResolution { link, .. } => {
                computed.get(&entry_key(link)) != Some(*condition)
            }
            PlanCondition::ContentHash { .. } => false,
        })
        .cloned()
        .map(RefusedCheck::condition_failed)
        .collect();
    checks.extend(
        computed
            .into_iter()
            .filter(|(key, _)| !recorded_keys.contains_key(key))
            .map(|(_, condition)| RefusedCheck::condition_unrecorded(condition)),
    );
    checks
}

/// The bytes `unit` publishes, where it writes any.
///
/// A target already holding its after-state, and a respell halfway, hold the
/// after-state's bytes, and those are its content; every other written target
/// takes the recomposed bytes, which [`recompose`] held to its after-state.
/// A written target the operations leave nothing at is returned as the path
/// its transition disagrees at.
fn content(
    plan: &ResolvedPlan,
    unit: Unit,
    states: &[TargetState],
    composition: &Composition,
) -> Result<Option<Arc<[u8]>>, DocumentPath> {
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
        .ok_or_else(|| transition.path.clone())
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
    /// The schema violations any composed result a target at its
    /// before-state would publish introduces.
    ///
    /// **Each result is judged against the document its content came from**:
    /// its own before-state where it is edited in place, the moved document's
    /// where a move carried it, and nothing where an operation wrote it.
    ///
    /// **The fields written into a result** are those the plan's frontmatter
    /// kinds write at its path, or at the path its content came from, since
    /// an operation composes on the document before or after a move carries
    /// it.
    fn violations(&self, units: &[Unit], contents: &[Option<Arc<[u8]>>]) -> Vec<SchemaViolation> {
        let index_of = transition_index(self.plan, self.normalizer);
        let written_fields = schema::written_fields(self.plan, self.normalizer);
        let no_field = BTreeSet::new();
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
            let file = identity(self.normalizer, path.as_str());
            let source = file
                .as_ref()
                .and_then(|file| self.lineage.source(file))
                .map(|source| source.from.clone());
            let drawn_from = source
                .as_ref()
                .and_then(|source| index_of.get(source).copied());
            let fields: BTreeSet<String> = [file.as_ref(), source.as_ref()]
                .into_iter()
                .flatten()
                .flat_map(|file| written_fields.get(file).unwrap_or(&no_field))
                .cloned()
                .collect();
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
            checks.extend(schema::introduced(path, &after, &before, &fields));
        }
        checks
    }
}

/// Stage `unit` with `content`, or say why the plan stops.
#[allow(clippy::disallowed_methods)] // The one applier: the vault write kernel's one caller.
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
                (FileState::Present { hash, .. }, FileState::Present { .. }, Some(content)) => {
                    norn_fs::Transition::Replace {
                        before: kernel_hash(hash),
                        content,
                    }
                }
                (FileState::Present { hash, .. }, FileState::Absent {}, _) => {
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
            let FileState::Present { hash, .. } = &old.before else {
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
///
/// **`O(n log n)` in units.** The units whose waits are all placed stand in
/// one queue ranked by phase, then position; each is placed once, and each
/// wait is released once, when the target it waits for is placed. So the
/// next unit placed is always the first ready one in phase order, the same
/// order a scan from the start would find.
fn publication_order(
    plan: &ResolvedPlan,
    units: &[Unit],
    phases: &[Phase],
    lineage: &Lineage,
    normalizer: &PathNormalizer,
) -> Vec<usize> {
    let mut by_phase: Vec<usize> = (0..units.len()).collect();
    by_phase.sort_by_key(|&position| phases[position]);
    let mut rank = vec![0; units.len()];
    for (ranked, &position) in by_phase.iter().enumerate() {
        rank[position] = ranked;
    }
    let index_of = transition_index(plan, normalizer);
    let unit_of: BTreeMap<usize, usize> = units
        .iter()
        .enumerate()
        .flat_map(|(position, unit)| unit.transitions().map(move |index| (index, position)))
        .collect();
    // Each source's unit waits for the units of the targets drawing on it.
    let mut waits: BTreeSet<(usize, usize)> = BTreeSet::new();
    for (file, drawn) in lineage.drawing() {
        let (Some(target), Some(source)) = (index_of.get(file), index_of.get(&drawn.from)) else {
            continue;
        };
        let (target, source) = (unit_of[target], unit_of[source]);
        if source != target {
            waits.insert((source, target));
        }
    }
    let mut waiting = vec![0usize; units.len()];
    let mut released: Vec<Vec<usize>> = vec![Vec::new(); units.len()];
    for &(source, target) in &waits {
        waiting[source] += 1;
        released[target].push(source);
    }
    let mut ready: BinaryHeap<Reverse<usize>> = (0..units.len())
        .filter(|&position| waiting[position] == 0)
        .map(|position| Reverse(rank[position]))
        .collect();
    let mut order: Vec<usize> = Vec::with_capacity(units.len());
    while let Some(Reverse(ranked)) = ready.pop() {
        count_order_step();
        let next = by_phase[ranked];
        order.push(next);
        for &source in &released[next] {
            count_order_step();
            waiting[source] -= 1;
            if waiting[source] == 0 {
                ready.push(Reverse(rank[source]));
            }
        }
    }
    assert_eq!(
        order.len(),
        units.len(),
        "recomposition refused every content cycle"
    );
    order
}

#[cfg(test)]
thread_local! {
    /// How many steps ordering publication has taken on this thread: each
    /// unit placed and each wait released. The count, not the clock, is what
    /// shows a plan's order costs a bounded amount of work per target.
    static ORDER_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn count_order_step() {
    ORDER_STEPS.with(|steps| steps.set(steps.get() + 1));
}

#[cfg(not(test))]
fn count_order_step() {}

#[cfg(test)]
mod tests {
    use norn_wire::{
        DocumentPath, FileState, Operation, OperationKind, ResolvedPlan, RootIdentity, Transition,
        VaultAddress, VaultName,
    };

    use super::super::observe::{recorded_lineage, units};
    use super::{ORDER_STEPS, phase, publication_order};
    use crate::planner::compose::content_hash;
    use crate::planner::view::VaultView;
    use crate::planner::view::memory::MemoryVault;

    fn path(text: &str) -> DocumentPath {
        DocumentPath::new(text).expect("a legal document path")
    }

    /// Ordering a plan's publication costs a bounded number of steps per
    /// target, however many targets it has: a vault-wide plan reaches its
    /// first publication without a pass over every target per target placed.
    /// Here every move's destination draws on its source, so every source
    /// waits.
    #[test]
    fn ordering_publication_takes_a_bounded_number_of_steps_per_target() {
        const MOVES: usize = 5_000;
        let vault = MemoryVault::with(&[]);
        let normalizer = VaultView::normalizer(&vault);
        let mut operations = Vec::with_capacity(MOVES);
        let mut transitions = Vec::with_capacity(2 * MOVES);
        for move_at in 0..MOVES {
            let (from, to) = (format!("a/{move_at}.md"), format!("b/{move_at}.md"));
            let hash = FileState::present(content_hash(from.as_bytes()));
            operations.push(Operation::new(OperationKind::move_document(
                path(&from),
                path(&to),
            )));
            transitions.push(Transition::new(
                path(&from),
                hash.clone(),
                FileState::absent(),
            ));
            transitions.push(Transition::new(path(&to), FileState::absent(), hash));
        }
        let plan = ResolvedPlan::new(
            VaultAddress::name(VaultName::new("notes").expect("a legal vault name")),
            RootIdentity::from_device_and_inode(1, 2),
            operations,
            transitions,
            Vec::new(),
        );
        let units = units(&plan, normalizer);
        let phases: Vec<_> = units.iter().map(|unit| phase(&plan, *unit)).collect();
        let lineage = recorded_lineage(&plan, normalizer);
        ORDER_STEPS.with(|steps| steps.set(0));
        let order = publication_order(&plan, &units, &phases, &lineage, normalizer);
        let steps = ORDER_STEPS.with(std::cell::Cell::get);
        assert_eq!(order.len(), 2 * MOVES);
        assert!(
            order.iter().take(MOVES).all(|&position| position % 2 == 1),
            "every destination publishes before any source is removed"
        );
        assert!(
            steps <= 4 * order.len(),
            "{steps} steps to order {} targets",
            order.len()
        );
    }
}
