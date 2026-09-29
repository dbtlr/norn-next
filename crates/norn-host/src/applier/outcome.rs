//! What an apply of a resolved plan came to, one variant per wire outcome.

use norn_wire::{
    AppliedTarget, ApplyReport, ChangesetOutcome, ErrorDetail, ErrorEnvelope, FolderPath, Forecast,
    InterruptionCause, PlanFault, RefusedCheck, ResolvedPlan, RootIdentity, UnresolvedOperation,
};

use norn_fs::Batch;
use norn_wire::{DocumentPath, FileState};

/// What applying a resolved plan came to.
///
/// **One variant per outcome the wire names, and every variant given after
/// the plan was checked carries a resolved plan**, so a caller can always
/// finish or retry by sending it back: the plan applied or interrupted, or
/// the fresh plan a refusal resolved. Only a root-identity refusal and a plan
/// whose own shape is wrong carry none: no plan resolved against another root
/// applies here, operations in a cycle are no plan at all, and a plan whose
/// transitions are not what its operations do is fixed by previewing its
/// operations again, not by a plan this apply resolves.
#[derive(Debug)]
pub(crate) enum ApplyOutcome {
    /// Every target stands at its after-state.
    Applied(Applied),
    /// A check refused before anything was published.
    Refused(Box<Refused>),
    /// The plan's own shape is wrong: its operations draw content from each
    /// other or require each other in a cycle, an identifier is carried twice
    /// or names no operation, or its transitions are not what its operations
    /// do from its before-states. Nothing was published.
    Invalid(PlanFault),
    /// The plan was resolved against another root.
    RootChanged {
        /// The root identity the plan carries.
        expected: RootIdentity,
        /// The vault's root identity now.
        found: RootIdentity,
    },
    /// Publication stopped after something landed.
    Interrupted(Box<Interrupted>),
    /// The filesystem refused before any target landed, so no document was
    /// written.
    WriteFailed {
        /// The resolved plan.
        plan: ResolvedPlan,
        /// The failure in words.
        detail: String,
    },
}

/// An applied plan.
#[derive(Debug)]
pub(crate) struct Applied {
    /// The resolved plan.
    pub(crate) plan: ResolvedPlan,
    /// Whether its changeset committed or the entry heals from the paths.
    pub(crate) changeset: ChangesetOutcome,
    /// Each target, in the plan's order, and whether this apply wrote it.
    pub(crate) targets: Vec<AppliedTarget>,
    /// The folders its creates made.
    pub(crate) folders_made: Vec<FolderPath>,
    /// The folders its removals left empty, which it removed.
    pub(crate) folders_removed: Vec<FolderPath>,
}

/// A refused plan, with the plan resolved afresh.
#[derive(Debug)]
pub(crate) struct Refused {
    /// The plan resolved afresh against what the vault holds now.
    pub(crate) plan: ResolvedPlan,
    /// Its forecast, marking every drifted target.
    pub(crate) forecast: Forecast,
    /// Each check that refused.
    pub(crate) checks: Vec<RefusedCheck>,
    /// Each operation the fresh plan leaves for the caller.
    pub(crate) unresolved: Vec<UnresolvedOperation>,
}

/// An apply whose publication stopped after something landed.
#[derive(Debug)]
pub(crate) struct Interrupted {
    /// The resolved plan. Sending it again finishes it.
    pub(crate) plan: ResolvedPlan,
    /// Every target that landed.
    pub(crate) landed: Vec<DocumentPath>,
    /// What stopped publication.
    pub(crate) cause: InterruptionCause,
    /// Whether the landed subset committed, or the entry owes a heal. The wire
    /// does not carry it; the apply job reads it, through
    /// [`ApplyOutcome::heal`], to arm the heal.
    pub(crate) changeset: ChangesetOutcome,
}

impl ApplyOutcome {
    /// What the entry derives because this outcome's changeset did not
    /// commit, and an empty batch where it owes none.
    ///
    /// **The heal is every path the plan touched**, as the watcher would
    /// report it had the ledger not expected the apply's own writes: a
    /// target that landed absent as a removal, since the apply knows the path
    /// is gone before any backend reports it, and every other as a change,
    /// which the reconcile reads as the path now stands. A target an
    /// interruption left unlanded is read again too: a respell cut short
    /// between its two steps left its content at the old spelling.
    /// `normalizer` spells each path as the entry's coverage does.
    pub(crate) fn heal(&self, normalizer: &norn_fs::PathNormalizer) -> Batch {
        let mut heal = Batch::default();
        let (plan, landed): (&ResolvedPlan, &[DocumentPath]) = match self {
            ApplyOutcome::Applied(applied) if applied.changeset == ChangesetOutcome::Healing => {
                (&applied.plan, &[])
            }
            ApplyOutcome::Interrupted(interrupted)
                if interrupted.changeset == ChangesetOutcome::Healing =>
            {
                (&interrupted.plan, &interrupted.landed)
            }
            _ => return heal,
        };
        let every_target_landed = matches!(self, ApplyOutcome::Applied(_));
        for transition in &plan.transitions {
            let Ok(path) = normalizer.normalize(std::path::Path::new(transition.path.as_str()))
            else {
                continue;
            };
            let landed = every_target_landed || landed.contains(&transition.path);
            heal.merge(match transition.after {
                FileState::Absent {} if landed => Batch::vault_removal(path),
                _ => Batch::vault_change(path),
            });
        }
        heal
    }

    /// Whether this outcome's changeset did not commit over something that
    /// landed, so the entry owes the heal [`ApplyOutcome::heal`] names.
    pub(crate) fn owes_a_heal(&self) -> bool {
        match self {
            ApplyOutcome::Applied(applied) => applied.changeset == ChangesetOutcome::Healing,
            ApplyOutcome::Interrupted(interrupted) => {
                interrupted.changeset == ChangesetOutcome::Healing
            }
            _ => false,
        }
    }

    /// The outcome as the wire answers it: a report, or a refusal under its
    /// code.
    pub(crate) fn into_wire(self) -> Result<ApplyReport, ErrorEnvelope> {
        match self {
            ApplyOutcome::Applied(applied) => Ok(ApplyReport::applied(
                applied.plan,
                applied.changeset,
                applied.targets,
                applied.folders_made,
                applied.folders_removed,
            )),
            ApplyOutcome::Refused(refused) => Err(ErrorEnvelope::new(
                "a check refused the plan before anything was published",
                ErrorDetail::plan_refused(
                    refused.plan,
                    refused.forecast,
                    refused.checks,
                    refused.unresolved,
                ),
            )),
            ApplyOutcome::Invalid(fault) => Err(ErrorEnvelope::new(
                match fault {
                    PlanFault::TransitionsDisagree { .. } => {
                        "the plan's transitions are not what its operations do: nothing was published; preview its operations again"
                    }
                    _ => "the plan's operations are no plan: nothing was published",
                },
                ErrorDetail::plan_invalid(fault),
            )),
            ApplyOutcome::RootChanged { expected, found } => Err(ErrorEnvelope::new(
                "the plan was resolved against another vault root",
                ErrorDetail::root_changed(expected, found),
            )),
            ApplyOutcome::Interrupted(interrupted) => Err(ErrorEnvelope::new(
                "publication stopped after part of the plan landed; send the plan again to finish it",
                ErrorDetail::plan_interrupted(
                    interrupted.plan,
                    interrupted.landed,
                    interrupted.cause,
                ),
            )),
            ApplyOutcome::WriteFailed { plan, detail } => Err(ErrorEnvelope::new(
                format!("the filesystem refused the plan before anything landed: {detail}"),
                ErrorDetail::write_failed(plan, detail),
            )),
        }
    }
}
