//! The seam the `apply` verb answers through: a preview planned on one
//! snapshot, or an apply admitted onto its entry's queue.
//!
//! **One entry point, one handle.** [`Host::apply`] answers both modes with a
//! [`PendingApply`]. A preview takes an ordinary read hold — the demand and the
//! refusals a read's hold carries, and one snapshot — plans operations through
//! the one planner or judges a resolved plan through the one applier's
//! checks, and writes nothing, so its handle holds the answer
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
//! **A resolved plan previews as the apply's own judgment of it.** The
//! applier's checks run over it, reading the vault and writing nothing, and
//! the preview answers the same plan where an apply would go on to stage it,
//! or the refusal an apply would answer. So what a caller previewed is what
//! applies (ADR 0031), and a plan an interruption left part-landed previews
//! as itself rather than as its operations resolved afresh.
//!
//! **A refusal is never a report.** An operation that does not resolve
//! answers `vault/plan-refused` with the plan the rest resolved to, its
//! forecast and the unresolved operations; a plan whose own shape is wrong
//! answers `request/plan-invalid`; a root replaced since the coverage was
//! installed answers `vault/root-changed`. A vault the planner could not read
//! answers `host/apply-not-run`, nothing planned and nothing written, with the
//! refusal a read carries once the entry meets the same failure as its cause:
//! the root's coverage lost where the root no longer stands, and trust
//! withdrawn for the environment's refusal where it stands and cannot be read.

use std::path::PathBuf;
use std::sync::Arc;

use norn_fs::WatchError;
use norn_store::Snapshot;
use norn_wire::{
    ApplyMode, ApplyParams, ApplyReport, AuthoredPlan, ErrorDetail, ErrorEnvelope, PlanDocument,
    RootIdentity, TrustState, UntrustedReason, VaultAnswer, VaultName,
};

use crate::address::registered_name;
use crate::applier::{self, PreviewStop};
use crate::derivation::Declared;
use crate::lifecycle::{
    ApplyAnswer, Demand, EntryOps, Host, PendingApply, ReadRefusal, ReadSource, SnapshotSource,
    not_run, watcher_lost,
};
use crate::planner::resolve::{PlanningFailure, Resolution, resolve};
use crate::planner::view::TreeView;

/// What a plan is resolved against: the vault root as the entry's coverage
/// spells it, the identity that root proved when the coverage was installed,
/// the roots the vault's walk does not enter, and the declaration the
/// coverage's store pins.
///
/// **Read off the coverage, never off the filesystem**: building one does no
/// I/O, so the entry records it under its gate. Whether the root still
/// stands where the coverage proved it is asked by [`PlanGround::standing`],
/// outside any gate hold.
#[derive(Clone)]
pub struct PlanGround {
    /// The vault root, as the coverage spells it.
    pub(crate) root: PathBuf,
    /// The `(device, inode)` the root proved when the coverage was installed.
    pub(crate) identity: norn_fs::Identity,
    /// The roots, relative to the vault root, its walk does not enter.
    pub(crate) exclusions: Vec<PathBuf>,
    /// The declaration the coverage's store pins, which a composed result is
    /// judged under.
    pub(crate) declared: Arc<Declared>,
}

impl std::fmt::Debug for PlanGround {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PlanGround")
            .field("root", &self.root)
            .field("identity", &self.identity)
            .field("exclusions", &self.exclusions)
            .finish_non_exhaustive()
    }
}

impl PlanGround {
    /// The root's identity, as a plan carries it.
    pub(crate) fn root_identity(&self) -> RootIdentity {
        RootIdentity::from_device_and_inode(self.identity.dev, self.identity.ino)
    }

    /// Whether the root still stands where the coverage proved it, asked of
    /// the filesystem: `vault/root-changed` where another directory stands
    /// at its path, and the refusal a read carries once the entry's watcher
    /// reports the root's coverage lost where nothing does.
    pub(crate) fn standing(&self, name: &VaultName) -> Result<(), ErrorEnvelope> {
        match norn_fs::path_identity(&self.root) {
            Ok(Some(now)) if now == self.identity => Ok(()),
            Ok(Some(now)) => Err(ErrorEnvelope::new(
                "the vault root was replaced since its coverage was installed",
                ErrorDetail::root_changed(
                    self.root_identity(),
                    RootIdentity::from_device_and_inode(now.dev, now.ino),
                ),
            )),
            Ok(None) => Err(not_run(
                ReadRefusal::NotServing(Demand::State(TrustState::untrusted(watcher_lost(
                    WatchError::CoverageLost(self.root.clone()),
                ))))
                .answer(name),
                None,
            )),
            Err(refusal) => Err(unreadable(name, refusal)),
        }
    }
}

/// Plan `authored` against the vault on `ground`.
///
/// The answer is the resolution, or the code planning ended in: a fault in
/// the plan's own shape, or a vault the planner could not read.
pub(crate) fn resolve_on(
    authored: AuthoredPlan,
    ground: &PlanGround,
    name: &VaultName,
) -> Result<Resolution, ErrorEnvelope> {
    let view = TreeView::open(&ground.root, &ground.exclusions)
        .map_err(|error| unreadable(name, error))?;
    resolve(
        authored,
        ground.root_identity(),
        &std::collections::BTreeSet::new(),
        &view,
    )
    .map_err(|failure| match failure {
        PlanningFailure::Fault(fault) => ErrorEnvelope::new(
            "the plan's operations are no plan: nothing was planned",
            ErrorDetail::plan_invalid(fault),
        ),
        PlanningFailure::View(error) => unreadable(name, error),
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

/// The answer to a vault standing where its coverage proved it that the
/// planner could not read: nothing was planned, and the cause is the refusal
/// a read carries once the entry's own walk meets the same failure — trust
/// withdrawn for the environment's refusal, in the environment's words.
pub(crate) fn unreadable(name: &VaultName, error: impl std::fmt::Display) -> ErrorEnvelope {
    not_run(
        ReadRefusal::NotServing(Demand::State(TrustState::untrusted(
            UntrustedReason::environmental_refusal(error.to_string()),
        )))
        .answer(name),
        None,
    )
}

/// Preview the resolved `plan` on `ground`, as [`applier::preview`] judges
/// it: the same plan and its forecast, or the refusal an apply would answer.
fn preview_resolved(
    plan: norn_wire::ResolvedPlan,
    ground: &PlanGround,
    name: &VaultName,
) -> Result<ApplyReport, ErrorEnvelope> {
    match applier::preview(
        plan,
        &ground.root,
        ground.identity,
        &ground.exclusions,
        &ground.declared,
    ) {
        Ok((plan, forecast)) => Ok(ApplyReport::previewed(plan, forecast)),
        Err(PreviewStop::Refused(outcome)) => Err((*outcome)
            .into_wire()
            .expect("a preview publishes nothing, so it never stands down")
            .expect_err("a preview's refusal is no report")),
        Err(PreviewStop::Unreadable(detail)) => Err(unreadable(name, detail)),
    }
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
    /// and nothing written. Operations are resolved through the one planner;
    /// a resolved plan is judged by the applier's own checks.
    ///
    /// The ground is the one the entry's coverage recorded, read under the
    /// gate; whether its root still stands there is asked of the filesystem
    /// after the gate is given back, as an apply's planning asks it.
    fn preview(&self, name: &VaultName, plan: PlanDocument) -> ApplyAnswer {
        let hold = self
            .begin_read(name)
            .map_err(|refusal| refusal.answer(name))?;
        let reading = hold.reading().answer_reading(name)?;
        let ground = self.plan_ground(name).ok_or_else(|| {
            unreadable(
                name,
                "the entry's coverage records no ground to plan against",
            )
        })?;
        ground.standing(name)?;
        let report = match plan {
            PlanDocument::Operations(authored) => {
                let resolution = fully_resolved(resolve_on(authored, &ground, name)?)?;
                ApplyReport::previewed(resolution.plan, resolution.forecast)
            }
            PlanDocument::Resolved(resolved) => preview_resolved(resolved, &ground, name)?,
        };
        drop(hold);
        Ok(VaultAnswer::new(reading, Vec::new(), report))
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: the roots the cases replace and remove.
mod tests {
    use super::*;
    use norn_testkit::scratch::Scratch;
    use norn_wire::ReasonCode;

    /// The ground a coverage installed over `root` records.
    fn ground_over(root: &std::path::Path) -> PlanGround {
        PlanGround {
            root: root.to_owned(),
            identity: norn_fs::path_identity(root)
                .unwrap()
                .expect("the root stands"),
            exclusions: Vec::new(),
            declared: Arc::new(Declared::unpinned()),
        }
    }

    /// **A root standing where its coverage proved it stands.**
    #[test]
    fn a_root_where_its_coverage_proved_it_stands() {
        let scratch = Scratch::new("norn-host-ground-stands");
        let name = VaultName::new("notes").unwrap();
        assert_eq!(ground_over(scratch.root()).standing(&name), Ok(()));
    }

    /// **A root replaced since its coverage was installed answers
    /// `vault/root-changed`**, naming the identity the coverage proved and
    /// the one the root's path names now.
    #[test]
    fn a_replaced_root_answers_root_changed() {
        let scratch = Scratch::new("norn-host-ground-replaced");
        let root = scratch.root().join("vault");
        std::fs::create_dir(&root).unwrap();
        let ground = ground_over(&root);
        std::fs::rename(&root, scratch.root().join("aside")).unwrap();
        std::fs::create_dir(&root).unwrap();
        let now = norn_fs::path_identity(&root).unwrap().unwrap();

        let refused = ground
            .standing(&VaultName::new("notes").unwrap())
            .expect_err("a replaced root stood");
        assert_eq!(refused.code(), &ReasonCode::VaultRootChanged);
        assert_eq!(
            refused.detail(),
            &ErrorDetail::root_changed(
                ground.root_identity(),
                RootIdentity::from_device_and_inode(now.dev, now.ino)
            )
        );
    }

    /// **A root that no longer stands answers not run, with the refusal a
    /// read carries once the entry's watcher reports the root's coverage
    /// lost** — the envelope, not one made up for the apply.
    #[test]
    fn a_vanished_root_answers_not_run_with_the_coverage_loss_a_read_carries() {
        let scratch = Scratch::new("norn-host-ground-vanished");
        let root = scratch.root().join("vault");
        std::fs::create_dir(&root).unwrap();
        let ground = ground_over(&root);
        std::fs::remove_dir(&root).unwrap();
        let name = VaultName::new("notes").unwrap();

        let lost = TrustState::untrusted(watcher_lost(WatchError::CoverageLost(root.clone())));
        assert_eq!(
            ground.standing(&name),
            Err(not_run(
                ReadRefusal::NotServing(Demand::State(lost)).answer(&name),
                None
            ))
        );
    }
}
