//! What an apply of a resolved plan came to, one variant per wire outcome.

use norn_wire::{
    AppliedTarget, ApplyReport, ChangesetOutcome, ErrorDetail, ErrorEnvelope, FolderPath, Forecast,
    InterruptionCause, RefusedCheck, ResolvedPlan, RootIdentity, UnresolvedOperation,
};

use norn_wire::DocumentPath;

/// What applying a resolved plan came to.
///
/// **One variant per outcome the wire names, and every variant given after
/// the plan was checked carries a resolved plan**, so a caller can always
/// finish or retry by sending it back: the plan applied or interrupted, or
/// the fresh plan a refusal resolved. Only a root-identity refusal carries
/// none, because no plan resolved against another root applies here.
#[derive(Debug)]
pub(crate) enum ApplyOutcome {
    /// Every target stands at its after-state.
    Applied(Applied),
    /// A check refused before anything was published.
    Refused(Box<Refused>),
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
    /// does not carry it; the apply job reads it to arm the heal.
    // A dormant carrier: its reader is NORN-295's `Host::apply` job, which
    // arms the heal an uncommitted changeset owes and has not landed.
    #[allow(dead_code)]
    pub(crate) changeset: ChangesetOutcome,
}

impl ApplyOutcome {
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
