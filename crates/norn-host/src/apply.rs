//! The seam the `apply` verb answers through: a preview planned on one
//! snapshot, or an apply admitted onto its entry's queue.
//!
//! **One entry point, one handle.** [`Host::apply`] answers both modes with a
//! [`PendingApply`]. A preview takes an ordinary read hold — the demand and the
//! refusals a read's hold carries, and one snapshot — plans the operations
//! through the one planner, and writes nothing, so its handle holds the answer
//! already and [`PendingApply::wait`] returns it at once. An apply is admitted
//! onto its entry's queue ([`Host::admit_apply`]), and its handle waits on the
//! job that runs it. A caller therefore never branches on the mode to learn how
//! to be answered, and the host call blocks no longer than admission or the
//! preview's own planning.
//!
//! **A plan is resolved against the files, on the ground the entry's coverage
//! stands on.** The planner reads before-states from the vault through
//! `norn-fs`, never from the store, so the snapshot a preview holds is what
//! its answer's reading names and what an operation that reads derived facts
//! would plan against; none of today's operation kinds reads one.
//!
//! **A refusal is never a report.** An operation that does not resolve
//! answers `vault/plan-refused` with the plan the rest resolved to, its
//! forecast and the unresolved operations; a plan whose own shape is wrong
//! answers `request/plan-invalid`; a vault the planner could not read answers
//! `host/apply-not-run` with the environment's refusal as its cause, since
//! nothing was planned and nothing written.

use std::collections::BTreeSet;
use std::path::PathBuf;

use norn_store::Snapshot;
use norn_wire::{
    ApplyMode, ApplyParams, ApplyReport, AuthoredPlan, ErrorDetail, ErrorEnvelope, OperationsTag,
    PlanDocument, RootIdentity, UntrustedReason, VaultAnswer, VaultName,
};

use crate::address::registered_name;
use crate::lifecycle::{
    ApplyAnswer, EntryOps, Host, PendingApply, ReadSource, SnapshotSource, not_run,
};
use crate::planner::resolve::{PlanningFailure, Resolution, resolve};
use crate::planner::view::TreeView;

/// What a plan is resolved against: the vault root as the entry's coverage
/// spells it, the identity that root proved, and the roots the vault's walk
/// does not enter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanGround {
    /// The vault root, as the coverage spells it.
    pub root: PathBuf,
    /// The `(device, inode)` of that root.
    pub identity: norn_fs::Identity,
    /// The roots, relative to the vault root, its walk does not enter.
    pub exclusions: Vec<PathBuf>,
}

impl PlanGround {
    /// The root's identity, as a plan carries it.
    pub(crate) fn root_identity(&self) -> RootIdentity {
        RootIdentity::from_device_and_inode(self.identity.dev, self.identity.ino)
    }
}

/// Plan `authored` against the vault on `ground`.
///
/// The answer is the resolution, or the code planning ended in: a fault in
/// the plan's own shape, or a vault the planner could not read.
pub(crate) fn resolve_on(
    authored: AuthoredPlan,
    ground: &PlanGround,
) -> Result<Resolution, ErrorEnvelope> {
    let view = TreeView::open(&ground.root, &ground.exclusions).map_err(unreadable)?;
    resolve(authored, ground.root_identity(), &BTreeSet::new(), &view).map_err(|failure| {
        match failure {
            PlanningFailure::Fault(fault) => ErrorEnvelope::new(
                "the plan's operations are no plan: nothing was planned",
                ErrorDetail::plan_invalid(fault),
            ),
            PlanningFailure::View(error) => unreadable(error),
        }
    })
}

/// A resolution, where every operation resolved; the refusal naming the ones
/// that did not otherwise, with the plan the rest resolved to.
pub(crate) fn fully_resolved(resolution: Resolution) -> Result<Resolution, ErrorEnvelope> {
    if resolution.unresolved.is_empty() {
        return Ok(resolution);
    }
    Err(ErrorEnvelope::new(
        "some of the plan's operations do not resolve against what the vault holds, so \
         nothing was written",
        ErrorDetail::plan_refused(
            resolution.plan,
            resolution.forecast,
            Vec::new(),
            resolution.unresolved,
        ),
    ))
}

/// The answer to a vault the planner could not read: nothing was planned,
/// and the environment refusing the read is the cause.
pub(crate) fn unreadable(error: impl std::fmt::Display) -> ErrorEnvelope {
    let detail = format!("the vault could not be read to plan against: {error}");
    not_run(
        ErrorEnvelope::new(
            detail.clone(),
            ErrorDetail::entry_untrusted(UntrustedReason::environmental_refusal(detail)),
        ),
        None,
    )
}

impl<O> Host<O>
where
    O: EntryOps,
    <O::Attachment as SnapshotSource>::Reader: ReadSource<Snapshot = Snapshot>,
{
    /// Answer an `apply`: preview the plan `params` carries, or admit it.
    ///
    /// The vault is the one the plan names. A preview is planned here and
    /// answered through the handle at once; an apply is admitted onto its
    /// entry's queue, refused at once with the code a read would carry where
    /// the entry stands on a cause, and answered through the handle when the
    /// job that runs it ends.
    pub fn apply(&self, params: ApplyParams) -> Result<PendingApply, ErrorEnvelope> {
        let name = registered_name(params.plan.vault())?.clone();
        let name = &name;
        match params.mode {
            ApplyMode::Preview => Ok(PendingApply::answered(self.preview(name, params.plan))),
            ApplyMode::Apply => self.admit_apply(name, params.plan),
        }
    }

    /// Preview `plan` over the vault `name`: one read hold, one snapshot,
    /// the plan resolved through the one planner, and nothing written.
    ///
    /// A resolved plan previews as its operations resolved afresh against
    /// what the vault holds now, provided it was resolved against this
    /// vault's root: a preview answers what sending it would plan, and a
    /// resolved plan's own transitions are checked only when it is applied.
    fn preview(&self, name: &VaultName, plan: PlanDocument) -> ApplyAnswer {
        let hold = self
            .begin_read(name)
            .map_err(|refusal| refusal.answer(name))?;
        let reading = hold.reading().answer_reading(name)?;
        let ground = self
            .plan_ground(name)
            .ok_or_else(|| unreadable("the entry's coverage records no ground to plan against"))?;
        let authored = match plan {
            PlanDocument::Operations(authored) => authored,
            PlanDocument::Resolved(resolved) => {
                let found = ground.root_identity();
                if resolved.root != found {
                    return Err(ErrorEnvelope::new(
                        "the plan was resolved against another vault root",
                        ErrorDetail::root_changed(resolved.root, found),
                    ));
                }
                AuthoredPlan {
                    plan: OperationsTag,
                    vault: resolved.vault,
                    operations: resolved.operations,
                    footnote: resolved.footnote,
                }
            }
        };
        let resolution = fully_resolved(resolve_on(authored, &ground)?)?;
        drop(hold);
        Ok(VaultAnswer::new(
            reading,
            Vec::new(),
            ApplyReport::previewed(resolution.plan, resolution.forecast),
        ))
    }
}
