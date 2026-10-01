//! The seam the `apply` verb answers through: a preview planned on one
//! snapshot, or an apply admitted onto its entry's queue.
//!
//! **One entry point, one handle.** [`Host::apply`] answers both modes with a
//! [`PendingApply`]. A preview takes an ordinary read hold — the demand and the
//! refusals a read's hold carries, and one snapshot — plans any operations
//! through the one planner, judges the resolved plan through the one
//! applier's checks, and writes nothing, so its handle holds the answer
//! already and [`PendingApply::wait`] returns it at once. An apply is admitted
//! onto its entry's queue ([`Host::admit_apply`]), and its handle waits on the
//! job that runs it. A caller therefore never branches on the mode to learn how
//! to be answered, and the host call blocks no longer than admission or the
//! preview's own planning.
//!
//! **Every preview ends in the applier's judgment.** Operations are resolved
//! first, as an apply resolves them, and the plan they resolve to is then
//! judged as a resolved plan is, so a composed result the vault schema
//! refuses previews as `vault/plan-refused`, as its apply answers.
//!
//! **A plan is resolved against the files, on the ground the entry's coverage
//! stands on.** The planner reads before-states from the vault through
//! `norn-fs`, never from the store, so the snapshot a preview holds is what
//! its answer's reading names and what a `where` target is matched on
//! ([`PlanSnapshot`]), through the find builder in process, and where its
//! resolution change set reads the links the store holds.
//!
//! **The write verbs enter here.** [`Host::set`], [`Host::edit`] and
//! [`Host::new_document`] each compile their request to an authored plan and
//! answer through [`Host::apply`], so a verb previews and applies exactly as
//! the same operations sent as a plan do.
//!
//! **A resolved plan previews as the apply's own judgment of it.** The
//! applier's checks run over it, reading the vault and writing nothing, and
//! the preview answers the same plan where an apply would go on to stage it,
//! or what an apply of it would end in otherwise — a vault the checks could
//! not read included, which answers `vault/write-failed` with the plan, as
//! the apply does. So what a caller previewed is what applies (ADR 0031),
//! and a plan an interruption left part-landed previews as itself rather
//! than as its operations resolved afresh.
//!
//! **A refusal is never a report.** An operation that does not resolve
//! answers `vault/plan-refused` with the plan the rest resolved to, its
//! forecast and the unresolved operations; a plan whose own shape is wrong
//! answers `request/plan-invalid`; a root replaced since the coverage was
//! installed answers `vault/root-changed`. A vault the planner could not read
//! while resolving operations answers `host/apply-not-run`, nothing planned
//! and nothing written, with the refusal a read carries once the entry meets
//! the same failure as its cause: the root's coverage lost where the root no
//! longer stands, and trust withdrawn for the environment's refusal where it
//! stands and cannot be read.

use std::cell::RefCell;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::Arc;

use norn_fs::WatchError;
use norn_store::{ContentModel, LinkChange, PageRefusal, PathOverlay, ProbedLink, Snapshot};
use norn_wire::{
    ApplyMode, ApplyParams, ApplyReport, AuthoredPlan, EditParams, ErrorDetail, ErrorEnvelope,
    FindParams, NewParams, PlanDocument, Predicate, RootIdentity, SetParams, TrustState,
    UntrustedReason, VaultAddress, VaultAnswer, VaultName,
};

use crate::address::registered_name;
use crate::applier;
use crate::derivation::Declared;
use crate::lifecycle::{
    ApplyAnswer, Demand, EntryOps, Host, PendingApply, ReadRefusal, ReadSource, ReaderUnavailable,
    SnapshotSource, not_run, watcher_lost,
};
use crate::planner::expand::{
    ExpandingFailure, Matched, MatchedDocument, Matcher, listed, resolve_expanding,
};
use crate::planner::links::LinkIndex;
use crate::planner::resolve::{PlanningFailure, Resolution};
use crate::planner::view::TreeView;
use crate::read::every_page;
use crate::refusal::{PageRefused, page_refusal, reader_unavailable};

/// What a plan is resolved against: the vault root as the entry's coverage
/// spells it, the identity that root proved when the coverage was installed,
/// the roots the vault's walk does not enter, and the declaration the
/// coverage's store pins.
///
/// **Read off the coverage, never off the filesystem**: building one does no
/// I/O, so the entry records it under its gate. Whether the root still
/// stands where the coverage proved it is asked by `PlanGround::standing`,
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

/// Plan `authored` against the vault on `ground`, its `where` targets
/// matched and its resolution change set judged on `snapshot`.
///
/// The answer is the resolution, or the code planning ended in: a fault in
/// the plan's own shape, a vault the planner could not read, or the refusal
/// of the snapshot the `where` targets are matched on — answered as a read
/// meeting it is, damage included.
pub(crate) fn resolve_on(
    authored: AuthoredPlan,
    ground: &PlanGround,
    name: &VaultName,
    snapshot: &PlanSnapshot<'_>,
) -> Result<Resolution, PageRefused> {
    let view = TreeView::open(&ground.root, &ground.exclusions)
        .map_err(|error| PageRefused::Answered(unreadable(name, error)))?;
    resolve_expanding(authored, ground.root_identity(), &view, snapshot, snapshot).map_err(
        |failure| match failure {
            ExpandingFailure::Planning(PlanningFailure::Fault(fault)) => {
                PageRefused::Answered(ErrorEnvelope::new(
                    "the plan's operations are no plan: nothing was planned",
                    ErrorDetail::plan_invalid(fault),
                ))
            }
            ExpandingFailure::Planning(PlanningFailure::View(error)) => {
                PageRefused::Answered(unreadable(name, error))
            }
            ExpandingFailure::Match(refused) => refused,
        },
    )
}

/// The one snapshot a request plans against: where a plan's `where` targets
/// are matched, through the find builder, in process, and where its
/// resolution change set reads the links the store holds
/// ([`crate::planner::links`]).
///
/// **A preview reads the snapshot its read hold took**, under the content
/// model that snapshot pins. **An apply reads a snapshot its job establishes
/// inside the entry's claim**, on a read handle the store mints for the job
/// through the coverage's read seam ([`established_for_matching`]) the first
/// time a `where` target or a link resolution asks: the job holds the claim
/// and its store is the one writer, so that snapshot reads exactly the state
/// the apply's changeset builds on. One handle serves the job's matching, its
/// planning's change set and the applier's check of it
/// ([`PlanSnapshot::established`]), and the applier gives it back
/// ([`LinkIndex::release`]) before its changeset commits. A plan with no
/// `where` target that changes no document's presence and writes no link
/// mints none. Either way the match is the find a caller would have been
/// answered at the same instant, paged to its end.
pub(crate) struct PlanSnapshot<'a> {
    vault: VaultAddress,
    declared: &'a ContentModel,
    on: ReadOn<'a>,
}

/// Where a [`PlanSnapshot`] reads.
#[allow(clippy::large_enum_variant)] // One per request, on the stack and never moved: boxing the snapshot would allocate for a plan that reads none.
enum ReadOn<'a> {
    /// The snapshot a read hold took.
    Held(&'a Snapshot),
    /// A snapshot of the job's own, established by `mint` when first asked
    /// where there is one to ask, until it is released.
    Job {
        mint: Option<&'a dyn Fn() -> Result<Snapshot, ReaderUnavailable>>,
        established: RefCell<Option<Snapshot>>,
    },
}

impl<'a> PlanSnapshot<'a> {
    /// Reading `vault`'s `snapshot`, which pins `declared`.
    pub(crate) const fn held(
        vault: VaultAddress,
        snapshot: &'a Snapshot,
        declared: &'a ContentModel,
    ) -> Self {
        PlanSnapshot {
            vault,
            declared,
            on: ReadOn::Held(snapshot),
        }
    }

    /// Reading the snapshot `mint` establishes for `vault`, once, when first
    /// asked; `declared` is what that snapshot pins.
    pub(crate) const fn on_demand(
        vault: VaultAddress,
        mint: &'a dyn Fn() -> Result<Snapshot, ReaderUnavailable>,
        declared: &'a ContentModel,
    ) -> Self {
        PlanSnapshot {
            vault,
            declared,
            on: ReadOn::Job {
                mint: Some(mint),
                established: RefCell::new(None),
            },
        }
    }

    /// Reading `snapshot`, a job's own, for `vault`, which pins `declared`:
    /// the snapshot planning established, handed on to the applier, which
    /// mints nothing. Asked with none, it answers as a read seam that could
    /// not be minted.
    pub(crate) fn established(
        vault: VaultAddress,
        snapshot: Option<Snapshot>,
        declared: &'a ContentModel,
    ) -> Self {
        PlanSnapshot {
            vault,
            declared,
            on: ReadOn::Job {
                mint: None,
                established: RefCell::new(snapshot),
            },
        }
    }

    /// The job's snapshot, where one was established and not released.
    pub(crate) fn into_established(self) -> Option<Snapshot> {
        match self.on {
            ReadOn::Held(_) => None,
            ReadOn::Job { established, .. } => established.into_inner(),
        }
    }

    /// `read` over the snapshot. **A handle that cannot be minted or
    /// established refuses as a read over an unavailable read seam does**,
    /// `host/reader-unavailable`: a failed mint changes no trust label.
    fn reading<T>(&self, read: impl FnOnce(&Snapshot) -> T) -> Result<T, PageRefused> {
        match &self.on {
            ReadOn::Held(snapshot) => Ok(read(snapshot)),
            ReadOn::Job { mint, established } => {
                let mut established = established.borrow_mut();
                if established.is_none() {
                    let minted = match mint {
                        Some(mint) => mint(),
                        None => Err(ReaderUnavailable::new(
                            "the apply's snapshot was given back before this read asked for it",
                        )),
                    };
                    *established = Some(minted.map_err(|unavailable| {
                        PageRefused::Answered(reader_unavailable(unavailable.detail()))
                    })?);
                }
                Ok(read(
                    established
                        .as_ref()
                        .expect("the snapshot was just established"),
                ))
            }
        }
    }
}

/// A snapshot for one apply's `where` matching, established through
/// `source`'s own read seam — the mint every entry's read handle comes from
/// and the establishment every read runs — with the statements the mint ran
/// beside it, whichever way it ended.
///
/// The handle is the job's alone, so its connection is idle when taken; the
/// snapshot holds it, and both close when the snapshot drops.
pub(crate) fn established_for_matching<S>(
    source: &S,
) -> (
    Result<<S::Reader as ReadSource>::Snapshot, ReaderUnavailable>,
    u64,
)
where
    S: SnapshotSource,
{
    let minted = source.open_reader();
    let established = minted.reader.and_then(|reader| {
        let turn = Arc::new(reader)
            .try_take()
            .expect("a read handle minted for this apply alone is idle");
        S::Reader::establish(turn).map(|established| established.snapshot)
    });
    (established, minted.statements)
}

impl Matcher for PlanSnapshot<'_> {
    /// The refusal a read meeting it would answer with: the store's, a
    /// declaration the snapshot does not pin, or a read seam that could not
    /// be minted.
    type Error = PageRefused;

    /// Every document `predicates` match, in the find's own order, which is
    /// path order, every page of it read on the one snapshot.
    ///
    /// **A part the find reports unsatisfied matches nothing as asked**, so
    /// the answer is the report in words rather than the empty match; so is a
    /// request the builder refuses for what it asks, such as a bound that
    /// does not read as its key's declared type.
    fn matching(&self, predicates: &[Predicate]) -> Result<Matched, PageRefused> {
        let request = FindParams::new(self.vault.clone())
            .with_predicates(predicates.to_vec())
            .with_limit(norn_store::MAX_PAGE as u32);
        let mut paths = Vec::new();
        let paged = self.reading(|snapshot| {
            every_page(
                &request,
                |page| snapshot.find(page, self.declared),
                |page| {
                    if !page.unsatisfied.is_empty() {
                        return ControlFlow::Break(format!(
                            "the find reports parts it could not apply: {}",
                            listed(&page.unsatisfied)
                        ));
                    }
                    paths.extend(page.rows.into_iter().zip(page.content_hashes).map(
                        |(row, indexed)| MatchedDocument {
                            path: row.path,
                            indexed,
                        },
                    ));
                    ControlFlow::Continue(page.next)
                },
            )
        })?;
        match paged {
            Ok((_, None)) => Ok(Ok(paths)),
            Ok((_, Some(unsatisfied))) => Ok(Err(unsatisfied)),
            Err(refusal) => asked_amiss(refusal).map(Err).map_err(page_refusal),
        }
    }
}

impl LinkIndex for PlanSnapshot<'_> {
    /// The refusal a read meeting it would answer with, as the matcher's.
    type Error = PageRefused;

    fn changes(
        &self,
        overlay: &PathOverlay,
        probed: &[ProbedLink],
        each: &mut dyn FnMut(LinkChange),
    ) -> Result<(), PageRefused> {
        self.reading(|snapshot| snapshot.resolution_changes(overlay, probed, self.declared, each))?
            .map(|_| ())
            .map_err(page_refusal)
    }

    /// Give an apply's handle back, closing its snapshot; a held snapshot is
    /// its read hold's, and stays.
    fn release(&self) {
        if let ReadOn::Job { established, .. } = &self.on {
            established.borrow_mut().take();
        }
    }
}

/// A find refusal as a `where` target meets it: the request's own amiss
/// part, in words, which leaves the operation unresolved; or what a read
/// meeting it fails on, which fails planning.
///
/// Total by its match, as [`page_refusal`] is, so a refusal minted in the
/// store without an arm here does not compile.
fn asked_amiss(refusal: PageRefusal) -> Result<String, PageRefusal> {
    match refusal {
        PageRefusal::UnreadableBound { .. }
        | PageRefusal::EmptyMembership { .. }
        | PageRefusal::OutOfBound { .. }
        | PageRefusal::UnknownPart { .. }
        | PageRefusal::AmbiguousTarget(_)
        | PageRefusal::UnknownTarget { .. }
        | PageRefusal::PartNotTaken { .. } => Ok(refusal.to_string()),
        // A matcher sends no cursor and pins its own declaration, so these
        // are host defects or the store's own failure, answered as a read's.
        PageRefusal::DeclarationNotPinned { .. }
        | PageRefusal::OrderChanged(_)
        | PageRefusal::CursorNotTaken { .. }
        | PageRefusal::SummaryNotPaged
        | PageRefusal::Store(_) => Err(refusal),
    }
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
/// it, its links judged on `snapshot`: the same plan and its forecast, or the
/// answer an apply of it would end in — damage the snapshot met included,
/// which the caller publishes as a read's would be.
fn preview_resolved(
    plan: norn_wire::ResolvedPlan,
    ground: &PlanGround,
    snapshot: &PlanSnapshot<'_>,
) -> Result<ApplyReport, PageRefused> {
    match applier::preview(
        plan,
        &ground.root,
        ground.identity,
        &ground.exclusions,
        &ground.declared,
        snapshot,
    ) {
        Ok((plan, forecast)) => Ok(ApplyReport::previewed(plan, forecast)),
        Err(outcome) => match *outcome {
            applier::ApplyOutcome::Unread(refused) => Err(refused),
            outcome => Err(PageRefused::Answered(
                outcome
                    .into_wire()
                    .expect("a preview publishes nothing, so it never stands down")
                    .expect_err("a preview's refusal is no report"),
            )),
        },
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

    /// Answer a `set`: the frontmatter changes `params` names, compiled to
    /// operations and previewed or applied through [`Host::apply`].
    pub fn set(&self, params: SetParams) -> Result<PendingApply, ErrorEnvelope> {
        let mode = params.mode;
        self.apply_operations(mode, params.plan())
    }

    /// Answer an `edit`: the edits `params` names to one document, compiled
    /// to operations and previewed or applied through [`Host::apply`].
    pub fn edit(&self, params: EditParams) -> Result<PendingApply, ErrorEnvelope> {
        let mode = params.mode;
        self.apply_operations(mode, params.plan())
    }

    /// Answer a `new`: the document `params` creates at its path, compiled
    /// to an operation and previewed or applied through [`Host::apply`].
    pub fn new_document(&self, params: NewParams) -> Result<PendingApply, ErrorEnvelope> {
        let mode = params.mode;
        self.apply_operations(mode, params.plan())
    }

    /// A write verb's compiled `plan`, entering the one `apply` path.
    fn apply_operations(
        &self,
        mode: ApplyMode,
        plan: AuthoredPlan,
    ) -> Result<PendingApply, ErrorEnvelope> {
        self.apply(ApplyParams::new(mode, PlanDocument::operations(plan)))
    }

    /// Preview `plan` over the vault `name`: one read hold, one snapshot,
    /// and nothing written. Operations are resolved through the one planner,
    /// and the plan they resolve to is judged by the applier's own checks,
    /// as a resolved plan sent directly is: the preview answers that plan or
    /// exactly the refusal its apply would.
    ///
    /// The ground is the one the entry's coverage recorded, read under the
    /// gate; whether its root still stands there is asked of the filesystem
    /// after the gate is given back, as an apply's planning asks it.
    fn preview(&self, name: &VaultName, plan: PlanDocument) -> ApplyAnswer {
        let hold = self
            .begin_read(name)
            .map_err(|refusal| refusal.answer(name))?;
        let reading = hold.reading().answer_reading(name)?;
        // Production ops record a ground with every declaration, under the
        // gate hold that publishes the coverage this read holds, so only ops
        // that report none — test ops — reach the refusal: a host defect,
        // answered as the read seam it is, never as a cause the vault met.
        let ground = self.plan_ground(name).ok_or_else(|| {
            ErrorEnvelope::new(
                "the entry records no ground to plan against, so the preview was not planned",
                ErrorDetail::reader_unavailable(
                    "the entry's ops record no plan ground over its coverage",
                ),
            )
        })?;
        ground.standing(name)?;
        let snapshot =
            PlanSnapshot::held(plan.vault().clone(), hold.snapshot(), hold.content_model());
        let answered = |refused| match refused {
            PageRefused::Answered(refused) => refused,
            PageRefused::Damaged(detail) => {
                self.withdraw_for_read_damage(&hold, detail).answer(name)
            }
        };
        let report = match plan {
            // Planned as an apply plans it, then judged as an apply judges
            // what it planned: resolving checks no schema, so the plan it
            // answers goes through the same checks a resolved plan's does.
            PlanDocument::Operations(authored) => {
                let resolution =
                    resolve_on(authored, &ground, name, &snapshot).map_err(answered)?;
                preview_resolved(fully_resolved(resolution)?.plan, &ground, &snapshot)
                    .map_err(answered)?
            }
            PlanDocument::Resolved(resolved) => {
                preview_resolved(resolved, &ground, &snapshot).map_err(answered)?
            }
        };
        drop(snapshot);
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

    /// **An apply's matcher whose read handle cannot be minted refuses as a
    /// read over an unavailable read seam does**: `host/reader-unavailable`,
    /// never a failed statement nor damage, since a failed mint changes no
    /// trust label.
    #[test]
    fn a_matcher_whose_reader_cannot_be_minted_answers_reader_unavailable() {
        let name = VaultName::new("notes").unwrap();
        let declared = ContentModel::default();
        let refused = || {
            Err(crate::lifecycle::ReaderUnavailable::new(
                "the open was refused",
            ))
        };
        let matcher = PlanSnapshot::on_demand(VaultAddress::name(name), &refused, &declared);

        let answered = matcher.matching(&[Predicate::equal_to("wave", "flip")]);
        let Err(PageRefused::Answered(envelope)) = answered else {
            panic!("a refused mint answered {answered:?}");
        };
        assert_eq!(envelope.code(), &ReasonCode::HostReaderUnavailable);
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
