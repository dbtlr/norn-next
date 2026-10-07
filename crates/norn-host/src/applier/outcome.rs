//! What an apply of a resolved plan came to, one variant per wire outcome.

use norn_wire::{
    AppliedTarget, ApplyReport, ChangesetOutcome, ErrorDetail, ErrorEnvelope, FolderPath, Forecast,
    InterruptionCause, PlanFault, RefusedCheck, ResolvedPlan, RootIdentity, SchemaViolation,
    UnresolvedOperation,
};

use norn_fs::Batch;
use norn_wire::{DocumentPath, FileState};

use super::schema::Citations;

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
        /// Original targets to reconcile if confirmed progress did not commit.
        healing: Vec<DocumentPath>,
    },
    /// Publication stopped after this attempt changed a target on disk.
    Interrupted(Box<Interrupted>),
    /// The apply's leg stopped standing before publication began — a
    /// teardown moved the entry past it — so every shadow was removed and no
    /// document was written. The apply job answers it with the teardown's
    /// cause and the plan the apply's progress recorded.
    StoodDown,
    /// The filesystem failed before this attempt published a target.
    WriteFailed {
        /// The resolved plan.
        plan: ResolvedPlan,
        /// The failure in words.
        detail: String,
        /// Targets already confirmed at their after-states.
        landed: Vec<DocumentPath>,
        /// Original targets to reconcile if confirmed progress did not commit.
        healing: Vec<DocumentPath>,
    },
    /// The snapshot the plan's resolution change set is judged on could not
    /// be read, so nothing was published. A caller answers it as a read
    /// meeting the same refusal is answered: a damaged store is published as
    /// damage, with the rebuild it owes, before anything turns this into an
    /// answer.
    Unread {
        refusal: crate::refusal::PageRefused,
        healing: Vec<DocumentPath>,
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
    /// Every schema violation its force let through: empty for a plan that
    /// is not forced, and for a forced plan that introduces no violation.
    pub(crate) forced: Vec<SchemaViolation>,
    /// The rule sets the response's violations cite.
    pub(crate) citations: Citations,
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
    /// The rule sets the schema violations among the checks, and those the
    /// forecast lists, cite.
    pub(crate) citations: Citations,
    /// Each operation the fresh plan leaves for the caller.
    pub(crate) unresolved: Vec<UnresolvedOperation>,
    /// Targets of the original plan observed at their after-states.
    pub(crate) landed: Vec<DocumentPath>,
    /// Original targets to reconcile if confirmed progress did not commit.
    pub(crate) healing: Vec<DocumentPath>,
}

/// An apply whose publication stopped after this attempt changed a target.
#[derive(Debug)]
pub(crate) struct Interrupted {
    /// The resolved plan. Sending it again finishes it.
    pub(crate) plan: ResolvedPlan,
    /// Every target that landed.
    pub(crate) landed: Vec<DocumentPath>,
    /// What stopped publication.
    pub(crate) cause: InterruptionCause,
    /// Every schema violation its force let through in a target that landed.
    pub(crate) forced: Vec<SchemaViolation>,
    /// The rule sets those violations cite.
    pub(crate) citations: Citations,
    /// Whether the landed subset committed, or the entry owes a heal. The wire
    /// does not carry it; the apply job reads it, through
    /// [`ApplyOutcome::heal`], to arm the heal.
    pub(crate) changeset: ChangesetOutcome,
}

impl ApplyOutcome {
    /// Keep confirmed progress and repair owed by a failed changeset even
    /// when refresh fails or drops completed operations from its fresh plan.
    pub(crate) fn with_confirmed_progress(
        mut self,
        known_landed: Vec<DocumentPath>,
        targets: Vec<DocumentPath>,
    ) -> Self {
        match &mut self {
            Self::Refused(refused) => refused.healing = targets,
            Self::RootChanged { healing, .. } => {
                *healing = targets;
            }
            Self::WriteFailed {
                landed, healing, ..
            } => {
                landed.extend(known_landed);
                landed.sort();
                landed.dedup();
                *healing = targets;
            }
            Self::Unread { healing, .. } => *healing = targets,
            _ => unreachable!("only a publication refusal or failure needs this carrier"),
        }
        self
    }

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
    /// `normalizer` spells each path as the entry's coverage does; a path it
    /// cannot spell widens the heal to the whole vault.
    pub(crate) fn heal(&self, normalizer: &norn_fs::PathNormalizer) -> Batch {
        let healing = match self {
            Self::Refused(refused) => Some(&refused.healing),
            Self::RootChanged { healing, .. } | Self::WriteFailed { healing, .. } => Some(healing),
            Self::Unread { healing, .. } => Some(healing),
            _ => None,
        };
        if let Some(targets) = healing {
            return heal_targets(targets.iter().map(|path| (path, false)), normalizer);
        }
        let (plan, landed): (&ResolvedPlan, &[DocumentPath]) = match self {
            ApplyOutcome::Applied(applied) if applied.changeset == ChangesetOutcome::Healing => {
                (&applied.plan, &[])
            }
            ApplyOutcome::Interrupted(interrupted)
                if interrupted.changeset == ChangesetOutcome::Healing =>
            {
                (&interrupted.plan, &interrupted.landed)
            }
            _ => return Batch::default(),
        };
        let every_target_landed = matches!(self, ApplyOutcome::Applied(_));
        heal_targets(
            plan.transitions.iter().map(|transition| {
                let landed = every_target_landed || landed.contains(&transition.path);
                (
                    &transition.path,
                    landed && matches!(transition.after, FileState::Absent {}),
                )
            }),
            normalizer,
        )
    }

    /// Whether confirmed or published progress failed to commit, so the
    /// entry owes the heal [`ApplyOutcome::heal`] names.
    pub(crate) fn owes_a_heal(&self) -> bool {
        match self {
            Self::Refused(refused) => !refused.healing.is_empty(),
            Self::RootChanged { healing, .. } | Self::WriteFailed { healing, .. } => {
                !healing.is_empty()
            }
            Self::Unread { healing, .. } => !healing.is_empty(),
            ApplyOutcome::Applied(applied) => applied.changeset == ChangesetOutcome::Healing,
            ApplyOutcome::Interrupted(interrupted) => {
                interrupted.changeset == ChangesetOutcome::Healing
            }
            _ => false,
        }
    }

    /// The outcome as the wire answers it: a report, or a refusal under its
    /// code.
    ///
    /// `None` for an apply that stood down: its answer is not the applier's
    /// to give, since the cause is the teardown that moved the entry past the
    /// apply's leg, which the apply job reads off the entry.
    pub(crate) fn into_wire(self) -> Option<Result<ApplyReport, ErrorEnvelope>> {
        Some(match self {
            ApplyOutcome::Applied(applied) => {
                let cited = applied.citations.cited_by(&applied.forced);
                Ok(ApplyReport::applied(
                    applied.plan,
                    applied.changeset,
                    applied.targets,
                    applied.folders_made,
                    applied.folders_removed,
                )
                .with_forced(applied.forced, cited))
            }
            ApplyOutcome::Refused(refused) => {
                let cited = refused.citations.cited_by_checks(&refused.checks);
                Err(ErrorEnvelope::new(
                    "a check refused the plan before anything was published",
                    ErrorDetail::plan_refused(
                        refused.plan,
                        refused.forecast,
                        refused.checks,
                        cited,
                        refused.unresolved,
                        refused.landed,
                    ),
                ))
            }
            ApplyOutcome::Invalid(fault) => Err(ErrorEnvelope::new(
                match fault {
                    PlanFault::TransitionsDisagree { .. } => {
                        "the plan's transitions are not what its operations do: nothing was published; preview its operations again"
                    }
                    _ => "the plan's operations are no plan: nothing was published",
                },
                ErrorDetail::plan_invalid(fault),
            )),
            ApplyOutcome::RootChanged {
                expected, found, ..
            } => Err(ErrorEnvelope::new(
                "the plan was resolved against another vault root",
                ErrorDetail::root_changed(expected, found),
            )),
            ApplyOutcome::Interrupted(interrupted) => {
                let cited = interrupted.citations.cited_by(&interrupted.forced);
                Err(ErrorEnvelope::new(
                    "publication stopped after part of the plan landed; send the plan again to finish it",
                    ErrorDetail::plan_interrupted(
                        interrupted.plan,
                        interrupted.landed,
                        interrupted.cause,
                        interrupted.forced,
                        cited,
                    ),
                ))
            }
            ApplyOutcome::WriteFailed {
                plan,
                detail,
                landed,
                ..
            } => Err(ErrorEnvelope::new(
                format!("the filesystem failed before this attempt published anything: {detail}"),
                ErrorDetail::write_failed(plan, detail, landed),
            )),
            ApplyOutcome::Unread {
                refusal: crate::refusal::PageRefused::Answered(refused),
                ..
            } => Err(refused),
            // A caller publishes damage before it answers; one that answers
            // it here answers as a read whose statement the store refused.
            ApplyOutcome::Unread {
                refusal: crate::refusal::PageRefused::Damaged(detail),
                ..
            } => Err(ErrorEnvelope::new(
                "the store refused a statement this apply ran",
                ErrorDetail::read_failed(norn_wire::ReadFailure::statement(), detail),
            )),
            ApplyOutcome::StoodDown => return None,
        })
    }
}

/// Re-read failed write-through targets through the ordinary reconcile path.
/// Missing paths become deaths there; no original bytes or plan are retained.
fn heal_targets<'a>(
    targets: impl IntoIterator<Item = (&'a DocumentPath, bool)>,
    normalizer: &norn_fs::PathNormalizer,
) -> Batch {
    let mut heal = Batch::default();
    for (target, absent) in targets {
        let Ok(path) = normalizer.normalize(std::path::Path::new(target.as_str())) else {
            return Batch::rescan(norn_fs::RescanScope::Vault);
        };
        heal.merge(if absent {
            Batch::vault_removal(path)
        } else {
            Batch::vault_change(path)
        });
    }
    heal
}
