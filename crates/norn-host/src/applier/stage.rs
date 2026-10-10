//! The check-and-stage phase: every target's state, every condition, every
//! composed result's schema, and a shadow for every written target, before
//! anything is published.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::path::Path;
use std::sync::Arc;

use norn_fs::{PathNormalizer, Refusal, ShadowHome, Staging};
use norn_store::{HeldBlock, LinkFact};
use norn_wire::{
    DocumentPath, FileState, LinkAdvisory, OperationKind, PlanCondition, PlanFault, RefusedCheck,
    ResolvedPlan, SchemaViolation, Transition,
};

use super::observe::{
    TargetState, Unit, failed_conditions, identity, is_create, is_removal, misread, observe,
    recorded_lineage, transition_index, units,
};
use super::place::{Ground, Landing};
use super::recompose::{Recomposed, disagreement, recompose};
use super::schema::{self, Citations};
use super::shape::shape_disagrees;
use crate::derivation::Declared;
use crate::derivation::{document_links, frontmatter_block};
use crate::planner::compose::{After, Composition};
use crate::planner::control::role_at;
use crate::planner::lineage::Lineage;
use crate::planner::links::{
    Holding, LinkIndex, Target, change_set, entry_key, failed_address_resolutions, vouched,
    vouched_block,
};
use crate::planner::view::{Body, Entry, TreeView, VaultView, wire_hash};
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
    /// Each content destination and the source whose before-state it needs.
    pub(super) content_dependencies: Vec<(usize, usize)>,
}

/// One publication waiting for its turn.
///
/// **Plain data.** The unit names the plan's transitions by index, and the
/// kernel's record names the target, its root, its transition's hashes and
/// its shadow; neither holds a handle or a byte of content (ADR 0037).
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
}

/// When a publication runs: creates first, then replaces (a respell among
/// them), then removals (ADR 0037).
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
    ground: &Ground<'_>,
    shadows: &ShadowHome,
    plan: &ResolvedPlan,
    view: &TreeView,
    declared: &Declared,
    links: Links<'_>,
    citations: &mut Citations,
) -> Result<StagedPlan, Stop> {
    let checked = check(plan, view, declared, links, citations).map_err(Stop::from)?;
    stage(ground, shadows, plan, view.normalizer(), checked)
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
    contents: Vec<Option<Written>>,
    phases: Vec<Phase>,
    lineage: Lineage,
    stored: Vec<norn_store::DocumentPath>,
}

/// What a written target publishes.
///
/// **A carried document is published as a copy, never held.** A create or
/// a replace whose content the plan carries byte for byte from another file
/// is staged by the write kernel streaming that file into its shadow, held to
/// the hash the transition names (`norn_fs::Content::CopyOf`); the source
/// stands at that hash while every target is staged, since staging ends
/// before anything publishes and a source publishes after the targets
/// drawing on it.
#[derive(Clone, Debug)]
pub(super) enum Written {
    /// These bytes.
    Bytes(Arc<[u8]>),
    /// The bytes the file `source` holds at `state`, unread.
    Copy {
        /// The file the content stood at before the plan.
        source: DocumentPath,
        /// Its before-state, which is this target's after-state.
        state: FileState,
    },
}

/// Check every target of `plan`, reading the vault and writing nothing.
///
/// **The checks run in this order**: no operation carries a `where` target
/// or a folder move planning did not expand, which stops as
/// [`PlanFault::UnexpandedTarget`], nor a creation by rule it did not
/// expand, which stops as [`PlanFault::UnexpandedRule`], nor a cascade on a
/// kind that does not cascade, which stops as [`PlanFault::MisplacedCascade`],
/// nor does a control-file write stand beside a document operation, which
/// stops as [`PlanFault::ControlFileBesideDocuments`];
/// before any vault read, the transitions name exactly the files the operations
/// touch, each once ([`shape_disagrees`]); every target stands at the spelling
/// the vault gives it, at a place the vault reads documents at; the plan
/// records whether each hash's bytes decode as a document one way, and as
/// the bytes a target holds of its change decode ([`misread`]); no target
/// drifted and every content condition holds; the
/// operations, run again from the before-states, are exactly the plan's
/// transitions ([`recompose`]); the plan's resolution change set, computed
/// again from those results through `links` ([`link_checks`]), is exactly the
/// one it records, and each address resolution it carries resolves as
/// recorded at the after-state ([`failed_address_resolutions`]); and every
/// result introduces no schema violation, or, for a forced plan, has each
/// violation it introduces listed rather than refused, each citing its rules
/// through `citations`, the numbering of the response this judgment answers
/// in. A plan
/// whose shape, target places, recorded decoding or
/// recomposition fail is not what its operations do: its own
/// shape is wrong, and it stops as [`PlanFault::TransitionsDisagree`] naming
/// the files it disagrees at, never as drift. Drift, a failed condition, a
/// schema violation, a taken name, a replaced root and an I/O failure each
/// answer as themselves.
///
/// **With [`judge`], this is the one judgment of a resolved plan.** An apply
/// stages what it passes, where the kernel judges each target again as it
/// stages it; a preview of a resolved plan answers from it and from
/// [`judge`], which asks the kernel that same judgment and stages nothing. So
/// what a caller previewed is what an apply of the same plan over the same
/// files does.
pub(super) fn check<V: VaultView>(
    plan: &ResolvedPlan,
    view: &V,
    declared: &Declared,
    links: Links<'_>,
    citations: &mut Citations,
) -> Result<Checked, Unfit>
where
    V::Error: std::fmt::Display,
{
    // An operation whose target planning never expanded touches no file the
    // shape check or the recomposition could name, so it is refused first,
    // before it could pass unread; so is a creation by rule, which names no
    // path until planning expands it, a cascade on a kind that does not
    // cascade, which no planning wrote, and a control-file write beside a
    // document operation, which no planning resolves.
    if let Some(fault) = plan
        .unexpanded_targets()
        .or_else(|| plan.unexpanded_rules())
        .or_else(|| plan.misplaced_cascades())
        .or_else(|| plan.control_files_beside_documents())
    {
        return Err(Unfit::Invalid(fault));
    }
    let normalizer = view.normalizer();
    let stored = stored_paths(plan);
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
    let misread = misread(plan, &states);
    if !misread.is_empty() {
        return Err(Unfit::Invalid(disagreement(misread)));
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
    let mut composition = match recompose(plan, &states, &lineage, view)
        .map_err(|error| Unfit::Failed(error.to_string()))?
    {
        Recomposed::Sound(composition) => composition,
        Recomposed::Invalid(fault) => return Err(Unfit::Invalid(fault)),
    };
    let contents: Vec<Option<Written>> = units
        .iter()
        .map(|unit| content(plan, *unit, &states, &composition))
        .collect::<Result<_, _>>()
        .map_err(|path| Unfit::Invalid(disagreement([path])))?;
    // What the cascades left as written, matched or kept beside a link they
    // wrote, is read off the recomposition, as planning read it off its own
    // composition, so the two forecast alike for every holder not yet landed.
    // A holder an interrupted apply already landed recomposes from stand-in
    // bytes, so its skip advisory can be omitted or mislabelled; the limit is
    // stated in the planner's links module.
    let skipped = std::mem::take(&mut composition.skipped);
    let kept = std::mem::take(&mut composition.kept);
    drop(composition);
    let judges_place = declared.schema().reads_document_paths();
    let carried = carried_readings(plan, &units, &contents, &states, view, links, judges_place)?;
    let mut after: Vec<Option<Holding<'_>>> = vec![None; plan.transitions.len()];
    for ((unit, content), reading) in units.iter().zip(&contents).zip(&carried) {
        let written = match unit {
            Unit::One(index) => *index,
            Unit::Respell { new, .. } => *new,
        };
        after[written] = match (content, reading) {
            (Some(Written::Bytes(bytes)), _) => Some(Holding::Bytes(bytes)),
            (Some(Written::Copy { .. }), Some(reading)) => Some(Holding::Links(&reading.links)),
            _ => None,
        };
    }
    let targets: Vec<Target<'_>> = plan
        .transitions
        .iter()
        .zip(after)
        .map(|(transition, after)| {
            Target::new(
                &transition.path,
                &transition.before,
                &transition.after,
                after,
            )
        })
        .collect();
    let recomputed = change_set(
        &targets,
        &lineage,
        normalizer,
        &plan.operations,
        &skipped,
        &kept,
        links,
    )
    .map_err(Unfit::Unread)?;
    // An address resolution is judged here, not in `failed_conditions`: it is
    // a fact about the plan's after-state, which only the composed targets
    // draw.
    let unresolved_addresses =
        failed_address_resolutions(&targets, &plan.conditions, links).map_err(Unfit::Unread)?;
    drop(targets);
    let refused_entries = link_checks(&plan.conditions, &recomputed.entries);
    // A delete whose link choice the set it records contradicts — one
    // forbidding the links naming its document that a recorded link names,
    // or one rewriting them to no one document a link can be respelled
    // toward — is one planning leaves unresolved by the same rule
    // (`Removal::kept_by`), so the plan is not what its operations do. Where
    // the set moved since planning, the refusal's fresh plan answers for it
    // instead. The rule reads the change set alone, so an address resolution,
    // which is no input to it, neither causes nor hides the fault: where
    // every recorded entry matches, the plan stays invalid and has no fresh
    // plan. A move's, a rewriting delete's or a wikilink rewrite's cascade
    // omitting a rewrite planning would generate is not held here; the set
    // records the link it leaves, and the plan lands as recorded.
    if refused_entries.is_empty() && !recomputed.unkept.is_empty() {
        return Err(Unfit::Invalid(disagreement(
            recomputed.unkept.iter().filter_map(|&position| {
                match &plan.operations[position].kind {
                    OperationKind::DeleteDocument { path, .. } => Some(path.clone()),
                    _ => None,
                }
            }),
        )));
    }
    checks.extend(refused_entries);
    checks.extend(
        unresolved_addresses
            .into_iter()
            .map(RefusedCheck::condition_failed),
    );
    let schema = Judging {
        plan,
        states: &states,
        lineage: &lineage,
        normalizer,
        declared,
    };
    let violations = schema.violations(&units, &contents, &carried, citations);
    let forced = if plan.force {
        violations
    } else {
        checks.extend(violations.into_iter().map(RefusedCheck::violation));
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
    ground: &Ground<'_>,
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
        let content = contents[position].as_ref();
        let held = match stage_one(ground, shadows, plan, unit, content) {
            Ok(held) => held,
            Err(stop) => {
                discard_all(ground, shadows, plan, staged);
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
        content_dependencies: super::observe::content_dependencies(plan, &lineage, normalizer),
    })
}

/// Every transition's path as the store names it.
///
/// **A plan names each target at one spelling.** The planner writes every
/// transition at a normalized spelling, and the kernel keeps a path as it is
/// given, so a target spelled otherwise would be published, recorded and
/// derived at a second spelling of one file. Normalizing a path drops only
/// its `.` and empty components, and the document-path grammar a transition
/// is read through refuses both, so a transition's path is already the one
/// spelling of its file.
fn stored_paths(plan: &ResolvedPlan) -> Vec<norn_store::DocumentPath> {
    plan.transitions
        .iter()
        .map(|transition| norn_store::DocumentPath::from(&transition.path))
        .collect()
}

/// Remove every shadow `staged` holds, publishing nothing, each through the
/// anchor its target was staged under.
#[allow(clippy::disallowed_methods)] // The one applier: the vault write kernel's one caller.
pub(super) fn discard_all(
    ground: &Ground<'_>,
    shadows: &ShadowHome,
    plan: &ResolvedPlan,
    staged: impl IntoIterator<Item = StagedTarget>,
) {
    for target in staged {
        if let Held::Staged(staged) = target.held {
            // Only a target that landed somewhere was staged, so its landing
            // resolves; one that did not leaves its shadow to the home's sweep.
            if let Ok(landing) = ground.landing(staged_path(plan, target.unit)) {
                norn_fs::discard(landing.anchor, staged, shadows);
            }
        }
    }
}

/// The plan path the kernel stages `unit` at: its one transition's, or a
/// respell's old spelling.
pub(super) fn staged_path(plan: &ResolvedPlan, unit: Unit) -> &DocumentPath {
    match unit {
        Unit::One(index) => &plan.transitions[index].path,
        Unit::Respell { old, .. } => &plan.transitions[old].path,
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
/// not an entry, and is judged on its own; nor is an address resolution, which
/// the comparison ignores and [`failed_address_resolutions`] judges.
///
/// **The set is exact** (ADR 0037): a link whose resolution the vault outside
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
                // An address resolution is no entry of the change set: it is
                // never computed again from the operations, so the
                // comparison ignores it, and `failed_address_resolutions`
                // judges it at the after-state.
                PlanCondition::ContentHash { .. } | PlanCondition::AddressResolution { .. } => None,
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
            PlanCondition::ContentHash { .. } | PlanCondition::AddressResolution { .. } => false,
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

/// What the check reads of a document a move carries byte for byte: the
/// links it holds, and — where a schema rule reads where a document stands —
/// what its frontmatter block came to, which the schema check judges at the
/// place it leaves and the place it lands ([`Judging::violations`]).
struct CarriedReading {
    links: Vec<LinkFact>,
    /// `None` where no rule reads a document's place, or where the carried
    /// bytes decode as no document, which is judged alike wherever it
    /// stands.
    block: Option<HeldBlock>,
}

/// What each written target a move carries byte for byte holds, by unit: its
/// links, and its frontmatter block where `judges_place` says a rule reads
/// where a document stands; `None` for every other unit.
///
/// **The index first, the file once.** The links and the block are read from
/// the store's index where it vouches for them — derived from the very bytes
/// the plan carries, by hash ([`vouched`], [`vouched_block`]) — at the
/// source's spelling or, for a re-sent plan, the destination's. Where it
/// does not vouch for both, the file is read whole once at the plan's hash,
/// and both are read from those bytes: the one copy a move the index lags
/// holds, a declared limit. A carried move the index vouches for holds no
/// copy of its document, only the projection of its frontmatter.
fn carried_readings<V: VaultView>(
    plan: &ResolvedPlan,
    units: &[Unit],
    contents: &[Option<Written>],
    states: &[TargetState],
    view: &V,
    links: Links<'_>,
    judges_place: bool,
) -> Result<Vec<Option<CarriedReading>>, Unfit>
where
    V::Error: std::fmt::Display,
{
    let index_of = transition_index(plan, view.normalizer());
    let mut carried = Vec::with_capacity(units.len());
    for (unit, content) in units.iter().zip(contents) {
        let Some(Written::Copy { source, state }) = content else {
            carried.push(None);
            continue;
        };
        let written = &plan.transitions[match unit {
            Unit::One(index) => *index,
            Unit::Respell { new, .. } => *new,
        }]
        .path;
        let wants_block = judges_place && state.is_document();
        let mut held = None;
        let mut block = None;
        for at in [source, written] {
            if held.is_none() {
                held = vouched(at, state, links).map_err(Unfit::Unread)?;
                if held.is_some() && wants_block {
                    block = vouched_block(at, state, links).map_err(Unfit::Unread)?;
                }
            }
        }
        let reading = match held {
            Some(links) if block.is_some() || !wants_block => CarriedReading { links, block },
            _ => {
                // The source where this apply still sees it at that hash,
                // else the target the copy landed at.
                let seen = identity(view.normalizer(), source.as_str())
                    .and_then(|file| index_of.get(&file))
                    .is_some_and(|&index| matches!(states[index], TargetState::AtBefore(Some(_))));
                let at = if seen { source } else { written };
                let (links, read) = read_carried(at, state, view)?;
                CarriedReading {
                    links,
                    block: read.filter(|_| wants_block),
                }
            }
        };
        carried.push(Some(reading));
    }
    Ok(carried)
}

/// The links and the frontmatter block of the document at `at`, read from
/// its bytes whole where it stands at `state`; drift where it holds anything
/// else. The block is `None` where the bytes decode as no document.
fn read_carried<V: VaultView>(
    at: &DocumentPath,
    state: &FileState,
    view: &V,
) -> Result<(Vec<LinkFact>, Option<HeldBlock>), Unfit>
where
    V::Error: std::fmt::Display,
{
    let drifted = |holds: FileState| Unfit::Refused(vec![RefusedCheck::drifted(at.clone(), holds)]);
    let Some(file) = identity(view.normalizer(), at.as_str()) else {
        return Err(drifted(FileState::absent()));
    };
    match view
        .entry(&file)
        .map_err(|error| Unfit::Failed(error.to_string()))?
    {
        Entry::Document {
            at: spelled,
            hash,
            body: Body::Held(bytes),
        } if spelled == *at && state.hash() == Some(&hash) => {
            Ok((document_links(&bytes), frontmatter_block(&bytes)))
        }
        Entry::Document {
            at: spelled,
            hash,
            body,
        } if spelled == *at => Err(drifted(crate::planner::compose::standing(&body, hash))),
        _ => Err(drifted(FileState::absent())),
    }
}

/// What `unit` publishes, where it writes anything.
///
/// A target already holding its after-state, and a respell halfway, hold the
/// after-state's bytes, and those are its content where they were read; every
/// other written target takes the recomposed result, which [`recompose`] held
/// to its after-state — bytes, or a copy of the document the plan carries
/// there. A written target the operations leave nothing at, or a case-only
/// rename landing a carried document that is not its own — which the write
/// kernel's respell cannot copy, and the plan's one rule never carries — is
/// returned as the path its transition disagrees at.
fn content(
    plan: &ResolvedPlan,
    unit: Unit,
    states: &[TargetState],
    composition: &Composition,
) -> Result<Option<Written>, DocumentPath> {
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
            (TargetState::Halfway(Body::Held(bytes)), _)
            | (_, TargetState::Landed(Some(Body::Held(bytes)))) => Some(bytes.clone()),
            _ => None,
        },
        Unit::One(_) => match &states[written] {
            TargetState::Landed(Some(Body::Held(bytes))) => Some(bytes.clone()),
            _ => None,
        },
    };
    if let Some(bytes) = held {
        return Ok(Some(Written::Bytes(bytes)));
    }
    let copies = match unit {
        Unit::One(_) => true,
        // A respell carrying its own content unchanged publishes none.
        Unit::Respell { old, .. } => transition.after.same_content(&plan.transitions[old].before),
    };
    match composition
        .targets
        .get(&transition.path)
        .map(|target| &target.after)
    {
        Some(After::Bytes(bytes)) => Ok(Some(Written::Bytes(bytes.clone()))),
        Some(After::Carried { state, from }) if copies => Ok(Some(Written::Copy {
            source: from.clone(),
            state: state.clone(),
        })),
        _ => Err(transition.path.clone()),
    }
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
    /// **A control file is not judged under the schema** it may itself be:
    /// it is no document, and what its content must be — what its role's
    /// parser reads — is composition's to hold it to
    /// ([`crate::planner::control::unreadable_as_role`]), at planning and when
    /// the applier recomposes it.
    ///
    /// **Each result is judged against the document its content came from**:
    /// its own before-state where it is edited in place, the moved document's
    /// where a move carried it, and nothing where an operation wrote it. A
    /// result refuses each violation whose identity no such document carried
    /// ([`schema::introduced`]), and no other, whatever fields the plan
    /// writes into it.
    ///
    /// **A carried document is judged again at its destination, on its
    /// frontmatter alone** — against the schema rules and the field
    /// declarations, though a declaration's type or shape finding cannot
    /// change with the path. Its bytes are the moved document's own,
    /// unchanged, and every finding they conclude but a rule's is a function
    /// of the bytes, the same wherever they stand. A rule's `match.path`,
    /// `exclude.path` and `allowed_paths` read where a document stands, so the
    /// same bytes can breach a rule at one place and not another: where the
    /// schema states such a rule, the carried document's frontmatter block
    /// ([`CarriedReading`]) is judged at the place it leaves and the place it
    /// lands ([`crate::derivation::judge_block`]), and each violation whose
    /// identity the destination introduces refuses, a changed combined
    /// constraint included. Where no rule reads a place, nothing is judged
    /// again.
    fn violations(
        &self,
        units: &[Unit],
        contents: &[Option<Written>],
        carried: &[Option<CarriedReading>],
        citations: &mut Citations,
    ) -> Vec<SchemaViolation> {
        let index_of = transition_index(self.plan, self.normalizer);
        let case = crate::stored_path_order(self.normalizer.case_sensitivity()).glob_case();
        let mut checks = Vec::new();
        for ((unit, content), reading) in units.iter().zip(contents).zip(carried) {
            let after = match content {
                Some(Written::Bytes(after)) => after,
                Some(Written::Copy { source, .. }) => {
                    let written = match *unit {
                        Unit::One(index) | Unit::Respell { new: index, .. } => index,
                    };
                    if !matches!(self.states[written], TargetState::AtBefore(_)) {
                        continue;
                    }
                    if let Some(block) = reading.as_ref().and_then(|reading| reading.block.as_ref())
                    {
                        let path = &self.plan.transitions[written].path;
                        let before = schema::judge_block(source, block, self.declared, case);
                        let after = schema::judge_block(path, block, self.declared, case);
                        checks.extend(schema::introduced(path, after, &[before], citations));
                    }
                    continue;
                }
                None => continue,
            };
            let (source, written) = match *unit {
                Unit::One(index) => (index, index),
                Unit::Respell { old, new } => (old, new),
            };
            if !matches!(self.states[source], TargetState::AtBefore(_)) {
                continue;
            }
            let path = &self.plan.transitions[written].path;
            if role_at(path.as_str()).is_some() {
                continue;
            }
            let file = identity(self.normalizer, path.as_str());
            let source = file
                .as_ref()
                .and_then(|file| self.lineage.source(file))
                .map(|source| source.from.clone());
            let drawn_from = source
                .as_ref()
                .and_then(|source| index_of.get(source).copied());
            let before: Vec<schema::Judged> = drawn_from
                .and_then(|index| match &self.states[index] {
                    TargetState::AtBefore(Some(Body::Held(bytes))) => Some(schema::judge(
                        &self.plan.transitions[index].path,
                        bytes,
                        self.declared,
                        case,
                    )),
                    _ => None,
                })
                .into_iter()
                .collect();
            let after = schema::judge(path, after, self.declared, case);
            checks.extend(schema::introduced(path, after, &before, citations));
        }
        checks
    }
}

/// Stage `unit` with `content`, or say why the plan stops.
#[allow(clippy::disallowed_methods)] // The one applier: the vault write kernel's one caller.
fn stage_one(
    ground: &Ground<'_>,
    shadows: &ShadowHome,
    plan: &ResolvedPlan,
    unit: Unit,
    content: Option<&Written>,
) -> Result<Held, Stop> {
    let staged = through_kernel(
        ground,
        plan,
        unit,
        content,
        |anchor, root, relative, transition| {
            norn_fs::stage(anchor, root, relative, transition, shadows)
        },
    )?;
    Ok(match staged {
        Staging::Staged(staged) => Held::Staged(staged),
        Staging::Landed(landed) => Held::Landed(landed),
    })
}

/// Judge every written target of the plan `checked` passed as [`stage`]
/// would stage it, in the order they publish, and stage nothing: the first
/// target the kernel refuses stops the plan as it would stop staging it.
///
/// **This is the rest of an apply's judgment that [`check`] leaves to
/// staging**, which a preview answers from: the kernel's own descent to each
/// target through no link — so a target beneath a folder since swapped for a
/// link refuses as drift to absent — and what stands at the name, judged by
/// the kernel against the transition. It reads the vault and writes nothing.
pub(super) fn judge(
    ground: &Ground<'_>,
    plan: &ResolvedPlan,
    normalizer: &PathNormalizer,
    checked: &Checked,
) -> Result<(), Stop> {
    let order = publication_order(
        plan,
        &checked.units,
        &checked.phases,
        &checked.lineage,
        normalizer,
    );
    for position in order {
        let content = checked.contents[position].as_ref();
        through_kernel(
            ground,
            plan,
            checked.units[position],
            content,
            norn_fs::judge,
        )?;
    }
    Ok(())
}

/// Hand `unit`, with `content`, to the kernel through `kernel` — staging it,
/// or judging it as staging would — at the place it lands, and answer why
/// the plan stops where the kernel refuses. Every state pair goes through
/// the kernel, an absent-to-absent target included.
///
/// **A copy's source answers for itself.** The kernel refuses a copy whose
/// source is not at the hash it names — other bytes, nothing, a link, a
/// linked folder above it — naming the source, so the drift is the source's
/// and is reported at the source's path, the one its removal transition
/// names, never as the target drifting.
fn through_kernel<T, K>(
    ground: &Ground<'_>,
    plan: &ResolvedPlan,
    unit: Unit,
    content: Option<&Written>,
    kernel: K,
) -> Result<T, Stop>
where
    K: FnOnce(&Path, norn_fs::Identity, &Path, norn_fs::Transition<'_>) -> Result<T, Refusal>,
{
    let Some((path, transition)) = kernel_transition(plan, unit, content) else {
        return Err(Stop::Invalid(PlanFault::transitions_disagree(
            unit.transitions()
                .map(|index| plan.transitions[index].path.clone())
                .collect(),
        )));
    };
    let landing = ground.landing(path).map_err(Stop::Failed)?;
    // A target outside the vault is held to its folder as it stands now:
    // the kernel records that identity and publication checks it again.
    let Some(root) = landing.root(ground).map_err(Stop::Failed)? else {
        return Err(drifted_away(path, &landing, &transition));
    };
    match kernel(landing.anchor, root, landing.relative, transition) {
        Ok(answer) => Ok(answer),
        Err(refusal) => Err(match classify(&refusal) {
            Classified::Drift(holds) => {
                let source = match content {
                    Some(Written::Copy { source, .. })
                        if refused_at(&refusal) == Some(&landing.anchor.join(source.as_str())) =>
                    {
                        source
                    }
                    _ => path,
                };
                Stop::Refused(vec![RefusedCheck::drifted(source.clone(), holds)])
            }
            Classified::NameTaken => Stop::Refused(vec![RefusedCheck::name_taken(path.clone())]),
            Classified::RootReplaced => landing
                .replaced_outside()
                .map_or(Stop::RootReplaced, Stop::Failed),
            Classified::Io(detail) => Stop::Failed(detail),
        }),
    }
}

/// The path a kernel refusal names, where it names the file it refused.
fn refused_at(refusal: &Refusal) -> Option<&std::path::PathBuf> {
    match refusal {
        Refusal::FoldersLeft { refusal, .. } => refused_at(refusal),
        Refusal::Drifted { path, .. }
        | Refusal::Republished { path, .. }
        | Refusal::NotRegularFile { path }
        | Refusal::SymlinkDestination { path }
        | Refusal::LinkedAncestor { path, .. } => Some(path),
        _ => None,
    }
}

/// The plan path the kernel is asked about for `unit`, and the transition it
/// is asked for there; `None` if a write lacks its resolved content. A
/// copy is asked for as a create or a replace whose content is its source's
/// (`norn_fs::Content::CopyOf`), its source named below the vault root as the
/// plan names it.
fn kernel_transition<'p>(
    plan: &'p ResolvedPlan,
    unit: Unit,
    content: Option<&'p Written>,
) -> Option<(&'p DocumentPath, norn_fs::Transition<'p>)> {
    let bytes = match content {
        Some(Written::Bytes(bytes)) => Some(&bytes[..]),
        _ => None,
    };
    Some(match unit {
        Unit::One(index) => {
            let transition = &plan.transitions[index];
            // A copy publishes the write's after-state, which is its
            // source's before-state.
            let written = |after: &'p norn_wire::ContentHash| match content {
                Some(Written::Bytes(bytes)) => Some(norn_fs::Content::Held(bytes)),
                Some(Written::Copy { source, .. }) => Some(norn_fs::Content::CopyOf {
                    source: Path::new(source.as_str()),
                    hash: kernel_hash(after),
                }),
                None => None,
            };
            let kernel = match (&transition.before, &transition.after) {
                (FileState::Absent {}, FileState::Present { hash, .. }) => {
                    norn_fs::Transition::Create {
                        content: written(hash)?,
                    }
                }
                (FileState::Present { hash, .. }, FileState::Present { hash: after, .. }) => {
                    norn_fs::Transition::Replace {
                        before: kernel_hash(hash),
                        content: written(after)?,
                    }
                }
                (FileState::Present { hash, .. }, FileState::Absent {}) => {
                    norn_fs::Transition::Remove {
                        before: kernel_hash(hash),
                    }
                }
                (FileState::Absent {}, FileState::Absent {}) => norn_fs::Transition::Absent,
            };
            (&transition.path, kernel)
        }
        Unit::Respell { old, new } => {
            let (old, new) = (&plan.transitions[old], &plan.transitions[new]);
            let FileState::Present { hash, .. } = &old.before else {
                unreachable!("a respell's old spelling holds a document before");
            };
            let content = if new.after.same_content(&old.before) {
                None
            } else {
                bytes
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
    })
}

/// The stop for a target outside the vault whose folder is gone before it
/// was staged: a create's name is not there to take, and anything else no
/// longer holds what the plan was checked against.
fn drifted_away(
    path: &DocumentPath,
    landing: &Landing<'_>,
    transition: &norn_fs::Transition<'_>,
) -> Stop {
    match transition {
        norn_fs::Transition::Create { .. } => Stop::Failed(format!(
            "the folder `{}` the vault schema would be created in is gone",
            landing.anchor.display()
        )),
        _ => Stop::Refused(vec![RefusedCheck::drifted(
            path.clone(),
            FileState::absent(),
        )]),
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
    let unit_of: BTreeMap<usize, usize> = units
        .iter()
        .enumerate()
        .flat_map(|(position, unit)| unit.transitions().map(move |index| (index, position)))
        .collect();
    // Each source's unit waits for the units of the targets drawing on it.
    let mut waits: BTreeSet<(usize, usize)> = BTreeSet::new();
    for (target, source) in super::observe::content_dependencies(plan, lineage, normalizer) {
        let (target, source) = (unit_of[&target], unit_of[&source]);
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
