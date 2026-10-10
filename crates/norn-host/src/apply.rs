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
//! **The write verbs enter here.** [`Host::set`], [`Host::edit`],
//! [`Host::new_document`], [`Host::move_path`] and [`Host::delete`] each
//! compile their request to an authored plan and answer through
//! [`Host::apply`], so a verb previews and applies exactly as the same
//! operations sent as a plan do.
//!
//! **A resolved plan previews as the apply's own judgment of it.** The
//! applier's checks run over it, then the write kernel's judgment of each
//! target as staging would meet it, reading the vault and writing nothing,
//! and the preview answers the same plan where an apply would go on to stage it,
//! or what an apply of it would end in otherwise — a vault the checks could
//! not read included, which answers `vault/write-failed` with the plan, as
//! the apply does. So what a caller previewed is what applies (ADR 0037),
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

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::Arc;

use norn_fs::WatchError;
use norn_store::{
    ContentModel, HeldLinks, LinkChange, PageRefusal, PathOverlay, PlanSide, ProbedLink,
    RepairSelection, Snapshot, TargetNaming,
};
use norn_wire::{
    AnswerReading, ApplyMode, ApplyParams, ApplyReport, AuthoredPlan, Citation, DeleteParams,
    DocumentPath, EditParams, ErrorDetail, ErrorEnvelope, FindParams, FindingRow, MoveParams,
    NewParams, PlanDocument, Predicate, Provenance, RepairParams, ResolvedPlan,
    RewriteWikilinkParams, RootIdentity, SchemaViolation, SetParams, SkippedFinding, TrustState,
    UntrustedReason, VaultAddress, VaultAnswer, VaultName,
};

use crate::address::registered_name;
use crate::applier;
use crate::clock::OneReading;
use crate::derivation::Declared;
use crate::evidence::{LinkJudgmentCost, PlanningHoldCost, RepairBatchCost, SnapshotWork};
use crate::lifecycle::{
    ApplyAnswer, Demand, EntryOps, Host, MintedReader, PendingApply, ReadHold, ReadRefusal,
    ReadSource, ReaderUnavailable, SnapshotSource, not_run, watcher_lost,
};
use crate::planner::control::SchemaPlace;
use crate::planner::expand::{
    ExpandingFailure, Matched, MatchedDocument, Matcher, listed, resolve_expanding,
};
use crate::planner::links::LinkIndex;
use crate::planner::repair;
use crate::planner::resolve::{PlanningFailure, Resolution};
use crate::planner::rule::Rules;
use crate::planner::view::{Barrier, Body, Entry, Remembered, TreeView, VaultView};
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
    /// Where the vault schema the registration reads lives, which a schema
    /// write lands at (ADR 0034).
    pub(crate) schema: SchemaPlace,
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
            .field("schema", &self.schema)
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
    let view = planning_view(ground, name).map_err(PageRefused::Answered)?;
    resolve_through(
        authored,
        ground,
        name,
        snapshot,
        &view,
        &OneReading::of(&crate::clock::local_now),
    )
}

/// The vault on `ground` as the planner reads it, or the refusal of a root
/// its walk cannot open.
fn planning_view(ground: &PlanGround, name: &VaultName) -> Result<TreeView, ErrorEnvelope> {
    TreeView::open(&ground.root, &ground.exclusions, &ground.schema)
        .map_err(|error| unreadable(name, error))
}

/// What a repair reads of the vault, through the view its plan resolves on.
struct RepairReading<'v, V>(&'v V);

impl<V: VaultView> repair::Reading for RepairReading<'_, V> {
    type Error = V::Error;

    /// The bytes of the document at `path` as the view reads it whole, or
    /// that no document whose bytes can be read stands there: what a repair
    /// composes a document's fixes onto.
    fn before(&self, path: &DocumentPath) -> Result<repair::Before, V::Error> {
        let Ok(at) = self
            .0
            .normalizer()
            .normalize(std::path::Path::new(path.as_str()))
        else {
            return Ok(repair::Before::Unread);
        };
        Ok(match self.0.entry(&at)? {
            Entry::Document {
                body: Body::Held(bytes),
                ..
            } => repair::Before::Held(bytes),
            _ => repair::Before::Unread,
        })
    }

    /// What stands at `path` as the view reads it streamed, under the root's
    /// identity rule: the read the move's own planning makes of its
    /// destination, which the shared view answers again from memory.
    fn destination(&self, path: &DocumentPath) -> Result<repair::Destination, V::Error> {
        let at = match self
            .0
            .normalizer()
            .normalize(std::path::Path::new(path.as_str()))
        {
            Ok(at) => at,
            Err(error) => return Ok(repair::Destination::Closed(error.to_string())),
        };
        Ok(match self.0.streamed_entry(&at)? {
            Entry::Absent { .. } => {
                // Every folder above the place, by the same identity rule, so
                // the batch's routes are judged against one another's places
                // above and beneath theirs.
                let spelled = path.as_str();
                let folders = spelled
                    .match_indices('/')
                    .filter_map(|(end, _)| {
                        self.0
                            .normalizer()
                            .normalize(std::path::Path::new(&spelled[..end]))
                            .ok()
                    })
                    .collect();
                repair::Destination::Free { at, folders }
            }
            Entry::Blocked {
                detail,
                barrier: Barrier::Closed,
            } => repair::Destination::Closed(detail),
            Entry::Document { .. } | Entry::Folder | Entry::Blocked { .. } => {
                repair::Destination::Taken
            }
        })
    }
}

/// The resolved plan of the repair of `rows`, a batch's findings, under
/// `ground`'s declaration, with the findings it skips and the findings each
/// operation fixes: each fix composed onto the bytes `view` reads, its clock
/// read once through `clock`, and the plan it makes resolved on `snapshot`.
///
/// **A repair whose clock gives no reading is refused as a creation is**: the
/// operation of each default that reads the clock is left unresolved, naming
/// the clock, and `vault/plan-refused` answers it in both modes.
///
/// **Routes are admitted one at a time, each judged on the plan the routes
/// before it left.** A route's move carries the link cascade the one planner
/// generates, which rewrites links in other documents and in the moved
/// document itself, and no judgment of one document's own fixes sees those
/// rewrites. So the batch is first planned with every route withheld: each
/// document's fixes judged where it stands, with no cascade, which its own
/// composition judges exactly. Then each withheld route, in batch order, is
/// tried on the plan the routes accepted so far make: the batch planned
/// again with that route admitted too — its document composed with the route
/// first, its fixes judged where it lands — resolved, and judged whole by
/// the applier's own check ([`applier::judged_whole`]), on the same view and
/// the same clock reading, nothing written. The trial is accepted where it
/// introduces no violation and every finding it fixes or drops no longer
/// holds where its document lands ([`repair::unkept`]). Otherwise the route
/// is skipped as one the judge would refuse, noting the document and the
/// violation, or the finding it would make hold again, which only the route
/// can explain, since the plan it was tried on was accepted; and the next
/// route is tried on the accepted plan without it. A skipped finding the
/// final plan's result no longer holds, one a cascade eliminated, is then
/// dropped ([`repair::settle`]).
///
/// **The cost is one resolution of the plan with every route withheld, and
/// one planning, resolution and judgment of the plan per route tried**, at
/// most one more resolution than the batch has routes: a route its trial
/// cannot make, its destination taken by an accepted route, changes no
/// operation and is neither resolved nor judged. A batch holding no route is
/// resolved once and not judged here. **A trial that does not resolve** — an
/// operation left unresolved, a plan of the wrong shape, a vault that does
/// not read — is answered as that refusal, as any plan's is; one the check
/// stops for a cause other than the schema, such as drift, is judged clean
/// here, and its preview or apply answers that cause.
fn repaired<V: VaultView>(
    rows: &[FindingRow],
    ground: &PlanGround,
    name: &VaultName,
    snapshot: &PlanSnapshot<'_>,
    view: &V,
    clock: &OneReading<'_>,
    refused: &dyn Fn(PageRefused) -> ErrorEnvelope,
) -> Result<(ResolvedPlan, Vec<SkippedFinding>, Vec<Citation>), ErrorEnvelope>
where
    V::Error: std::fmt::Display,
{
    let case = crate::stored_path_order(view.normalizer().case_sensitivity()).glob_case();
    let planning = |admitted: &BTreeSet<u64>| {
        let repairing = repair::Repairing {
            declared: &ground.declared,
            case,
            clock,
            admitted,
        };
        repair::plan(rows, &repairing, &RepairReading(view))
            .map_err(|error| unreadable(name, error))
    };
    let resolving = |planned: &repair::Planned| {
        let authored = AuthoredPlan::new(snapshot.vault.clone(), planned.operations.clone());
        // The resolution reads the clock through the plan's one reading: no
        // repair plan reaches a clock read in expansion today, and the shared
        // reading keeps the plan at one should a repair ever create by rule.
        let mut resolution =
            resolve_through(authored, ground, name, snapshot, view, clock).map_err(refused)?;
        resolution
            .unresolved
            .extend(planned.unresolved.iter().cloned());
        fully_resolved(resolution).map(|resolution| resolution.plan)
    };
    let mut admitted = BTreeSet::new();
    let mut accepted = planning(&admitted)?;
    let mut plan = resolving(&accepted)?;
    let mut holdings = applier::Holdings::new();
    let mut notes = BTreeMap::new();
    for route in accepted.withheld.clone() {
        admitted.insert(route.finding);
        let trial = planning(&admitted)?;
        if trial.operations == accepted.operations {
            accepted = trial;
            continue;
        }
        let tried = resolving(&trial)?;
        let whole = applier::judged_whole(&tried, view, &ground.declared, snapshot);
        let refusal = match whole.introduced.first() {
            Some(violation) => Some(breaking(&route.to, violation)),
            None => repair::unkept(&trial, &whole.holdings)
                .map(|(found, fixed)| holding_again(&route.to, found, fixed)),
        };
        match refusal {
            Some(note) => {
                admitted.remove(&route.finding);
                notes.insert(route.finding, note);
            }
            None => {
                (accepted, plan, holdings) = (trial, tried, whole.holdings);
            }
        }
    }
    repair::settle(&mut accepted, &holdings, &notes);
    Ok((plan, accepted.skipped, accepted.citations))
}

/// The note the skip of a route to `to` carries where the plan it was tried
/// on would introduce `violation`.
fn breaking(to: &DocumentPath, violation: &SchemaViolation) -> String {
    let on = violation
        .target
        .as_deref()
        .map(|target| format!(" on `{target}`"))
        .unwrap_or_default();
    let value = violation
        .value
        .as_ref()
        .map(|value| format!(" for `{}`", value.text()))
        .unwrap_or_default();
    format!(
        "moving the document to `{to}`, its link cascade with it, would leave `{}` breaking \
         its schema: `{}`{on}{value}",
        violation.path,
        violation.kind.as_str(),
    )
}

/// The note the skip of a route to `to` carries where the plan it was tried
/// on would make `found` hold again, a finding the plan cites as `fixed` or
/// drops.
fn holding_again(to: &DocumentPath, found: &repair::Found, fixed: bool) -> String {
    let held = &found.held;
    let on = held
        .field
        .as_deref()
        .map(|field| format!(" on `{field}`"))
        .unwrap_or_default();
    let value = held
        .value
        .as_deref()
        .map(|value| format!(" for `{}`", norn_store::value_head(value).text()))
        .unwrap_or_default();
    let claim = if fixed {
        "which the plan fixes"
    } else {
        "which the plan no longer finds"
    };
    format!(
        "moving the document to `{to}`, its link cascade with it, would leave `{}` holding \
         `{}`{on}{value} again, {claim}",
        found.at,
        held.kind.as_str(),
    )
}

/// Plan `authored` as [`resolve_on`] does, reading the vault through `view`
/// and the clock through `clock`: what a repair resolves its plan through, on
/// the view it read its documents' bytes from and the one reading its rule
/// defaults filled from.
fn resolve_through<V: VaultView>(
    authored: AuthoredPlan,
    ground: &PlanGround,
    name: &VaultName,
    snapshot: &PlanSnapshot<'_>,
    view: &V,
    clock: &OneReading<'_>,
) -> Result<Resolution, PageRefused>
where
    V::Error: std::fmt::Display,
{
    // The creation rules are the pinned schema's, the declaration the
    // applier's schema check judges the plan's results under, and the clock
    // is read at most once for the plan.
    let rules = Rules {
        schema: ground.declared.schema(),
        clock,
    };
    resolve_expanding(
        authored,
        ground.root_identity(),
        view,
        snapshot,
        snapshot,
        &rules,
    )
    .map_err(|failure| match failure {
        ExpandingFailure::Planning(PlanningFailure::Fault(fault)) => {
            PageRefused::Answered(ErrorEnvelope::new(
                "the plan's operations are no plan: nothing was planned",
                ErrorDetail::plan_invalid(fault),
            ))
        }
        ExpandingFailure::Planning(PlanningFailure::View(error)) => {
            PageRefused::Answered(unreadable(name, error))
        }
        ExpandingFailure::Snapshot(refused) => refused,
    })
}

/// The one snapshot a request plans against: where a plan's `where` targets
/// are matched, through the find builder, in process, and where its
/// resolution change set reads the links the store holds
/// ([`crate::planner::links`]).
///
/// **A preview reads the snapshot its read hold took**, under the content
/// model that snapshot pins. **An apply reads a snapshot its job establishes
/// inside the entry's claim**, on a read handle the store mints for the job
/// through the coverage's read seam ([`established_for_the_job`]) the first
/// time a `where` target or a link resolution asks: the job holds the claim
/// and its store is the one writer, so that snapshot reads exactly the state
/// the apply's changeset builds on. One handle serves the job's matching, its
/// planning's change set, the applier's check of it and the fresh plan a
/// refusal resolves, whichever of them asks first, and the applier gives it
/// back ([`LinkIndex::release`]) before its changeset commits. A plan with no
/// `where` target that changes no document's presence, deletes none, writes
/// no link and carries no address resolution mints none unless the applier
/// refuses it and the plan resolved afresh does one of those. Either way the
/// match is the find a caller would have been answered at the same instant,
/// paged to its end.
///
/// **What its link judgments cost is kept** ([`PlanSnapshot::link_judgment_cost`]):
/// every judgment of the resolution door it runs adds what the door reported
/// and what its snapshot's counters moved by, so a preview's account holds
/// what its planning and its check really ran.
pub(crate) struct PlanSnapshot<'a> {
    vault: VaultAddress,
    declared: &'a ContentModel,
    on: ReadOn<'a>,
    judged: Cell<LinkJudgmentCost>,
}

/// Where a [`PlanSnapshot`] reads.
#[allow(clippy::large_enum_variant)] // One per request, on the stack and never moved: boxing the snapshot would allocate for a plan that reads none.
enum ReadOn<'a> {
    /// The snapshot a read hold took.
    Held(&'a Snapshot),
    /// A snapshot of the job's own, established by `mint` when first asked,
    /// until it is released.
    Job {
        mint: &'a dyn Fn() -> Result<Snapshot, ReaderUnavailable>,
        established: RefCell<Option<Snapshot>>,
        /// What the snapshots released so far ran, kept past their release
        /// so the job's account reads what every snapshot it established
        /// ran ([`PlanSnapshot::job_snapshot_work`]).
        released: Cell<SnapshotWork>,
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
            judged: Cell::new(LinkJudgmentCost::NONE),
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
                mint,
                established: RefCell::new(None),
                released: Cell::new(SnapshotWork::NONE),
            },
            judged: Cell::new(LinkJudgmentCost::NONE),
        }
    }

    /// What every judgment of a plan's links run through this snapshot has
    /// cost so far.
    pub(crate) fn link_judgment_cost(&self) -> LinkJudgmentCost {
        self.judged.get()
    }

    /// What every snapshot this job established has run so far — released
    /// or still standing — read off each snapshot's own counters: none for a
    /// held snapshot, which is its read hold's, nor for a job that read
    /// nothing of the store.
    ///
    /// It never panics, so the job's account can be read while the job
    /// unwinds: a snapshot still borrowed for a read is left out, which only
    /// a read cut short by that unwind leaves it.
    pub(crate) fn job_snapshot_work(&self) -> SnapshotWork {
        match &self.on {
            ReadOn::Held(_) => SnapshotWork::NONE,
            ReadOn::Job {
                established,
                released,
                ..
            } => {
                let standing = established.try_borrow().ok().and_then(|standing| {
                    standing
                        .as_ref()
                        .map(|snapshot| SnapshotWork::of(snapshot.counters()))
                });
                standing.map_or(released.get(), |standing| released.get().plus(standing))
            }
        }
    }

    /// `read` over the snapshot. **A handle that cannot be minted refuses as
    /// a read over an unavailable read seam does**, `host/reader-unavailable`:
    /// a failed mint changes no trust label.
    fn reading<T>(&self, read: impl FnOnce(&Snapshot) -> T) -> Result<T, PageRefused> {
        match &self.on {
            ReadOn::Held(snapshot) => Ok(read(snapshot)),
            ReadOn::Job {
                mint, established, ..
            } => {
                let mut established = established.borrow_mut();
                if established.is_none() {
                    *established = Some(mint().map_err(|unavailable| {
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

/// A snapshot for one apply job's reads of the store — its `where` matching,
/// its planning's resolution change set, the applier's check of it and the
/// fresh plan a refusal resolves — established on `minted`, a handle the
/// coverage's own read seam minted, the mint every entry's read handle comes
/// from, by the establishment every read runs, with the statements the mint
/// ran beside it, whichever way it ended.
///
/// The handle is the job's alone, so its connection is idle when taken; the
/// snapshot holds it, and both close when the snapshot drops.
pub(crate) fn established_for_the_job<R>(
    minted: MintedReader<R>,
) -> (Result<R::Snapshot, ReaderUnavailable>, u64)
where
    R: ReadSource,
{
    let established = minted.reader.and_then(|reader| {
        let turn = Arc::new(reader)
            .try_take()
            .expect("a read handle minted for this apply alone is idle");
        R::establish(turn).map(|established| established.snapshot)
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
        let work = self
            .reading(|snapshot| {
                let before = snapshot.counters();
                snapshot
                    .resolution_changes(overlay, probed, self.declared, each)
                    .map(|work| LinkJudgmentCost::of(&work, before, snapshot.counters()))
            })?
            .map_err(page_refusal)?;
        self.judged.set(self.judged.get().plus(work));
        Ok(())
    }

    fn target(
        &self,
        overlay: &PathOverlay,
        address: &str,
        headed: PlanSide,
    ) -> Result<TargetNaming, PageRefused> {
        let (naming, work) = self
            .reading(|snapshot| {
                let before = snapshot.counters();
                snapshot
                    .target_naming(overlay, address, headed, self.declared)
                    .map(|(naming, work)| {
                        (
                            naming,
                            LinkJudgmentCost::of(&work, before, snapshot.counters()),
                        )
                    })
            })?
            .map_err(page_refusal)?;
        self.judged.set(self.judged.get().plus(work));
        Ok(naming)
    }

    /// The links the snapshot holds for `holder`, read by its path without
    /// its body. What the read costs is the snapshot's own counters', as
    /// every read on it is; it is no link judgment, so it adds nothing to
    /// [`PlanSnapshot::link_judgment_cost`].
    fn held_links(&self, holder: &DocumentPath) -> Result<Option<HeldLinks>, PageRefused> {
        self.reading(|snapshot| snapshot.held_links(&holder.into()))?
            .map_err(|problem| page_refusal(PageRefusal::Store(problem)))
    }

    /// The frontmatter the snapshot holds for `holder`, read by its path
    /// without its body. What the read costs is the snapshot's own
    /// counters', as every read on it is.
    fn held_frontmatter(
        &self,
        holder: &DocumentPath,
    ) -> Result<Option<norn_store::HeldFrontmatter>, PageRefused> {
        self.reading(|snapshot| snapshot.held_frontmatter(&holder.into()))?
            .map_err(|problem| page_refusal(PageRefusal::Store(problem)))
    }

    /// Give an apply's handle back, closing its snapshot; a held snapshot is
    /// its read hold's, and stays.
    fn release(&self) {
        if let ReadOn::Job {
            established,
            released,
            ..
        } = &self.on
            && let Some(snapshot) = established.borrow_mut().take()
        {
            released.set(released.get().plus(SnapshotWork::of(snapshot.counters())));
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
        // A matcher sends no cursor, pins its own declaration and is a find,
        // which names no rule, so these are host defects or the store's own
        // failure, answered as a read's.
        PageRefusal::DeclarationNotPinned { .. }
        | PageRefusal::OrderChanged(_)
        | PageRefusal::CursorNotTaken { .. }
        | PageRefusal::SummaryNotPaged
        | PageRefusal::UnknownRule { .. }
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
            Vec::new(),
            resolution.unresolved,
            Vec::new(),
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
///
/// **What a folder move leaves behind is planning's to say.** A resolved
/// plan carries only the document moves a folder move expanded into, so the
/// applier's judgment cannot see the files the folder kept; the operations'
/// planning read them, and hands them here as `left_behind` for the forecast
/// to name. A resolved plan sent directly names none.
fn preview_resolved(
    plan: norn_wire::ResolvedPlan,
    left_behind: Vec<norn_wire::FilePath>,
    ground: &PlanGround,
    snapshot: &PlanSnapshot<'_>,
) -> Result<ApplyReport, PageRefused> {
    match applier::preview(
        plan,
        &ground.root,
        ground.identity,
        &ground.exclusions,
        &ground.schema,
        &ground.declared,
        snapshot,
    ) {
        Ok((plan, forecast)) => Ok(ApplyReport::previewed(
            plan,
            forecast.with_left_behind(left_behind),
        )),
        Err(outcome) => match *outcome {
            applier::ApplyOutcome::Unread { refusal, .. } => Err(refusal),
            outcome => Err(PageRefused::Answered(
                outcome
                    .into_wire()
                    .expect("a preview publishes nothing, so it never stands down")
                    .expect_err("a preview's refusal is no report"),
            )),
        },
    }
}

/// The ground a read hold plans against, with its root asked of the
/// filesystem.
///
/// Production ops record a ground with every declaration, under the gate hold
/// that publishes the coverage this read holds, so only ops that report none
/// — test ops — reach the refusal: a host defect, answered as the read seam
/// it is, never as a cause the vault met.
fn planning_ground<'h, O>(
    hold: &'h ReadHold<O>,
    name: &VaultName,
) -> Result<&'h PlanGround, ErrorEnvelope>
where
    O: EntryOps,
{
    let ground = hold.plan_ground().ok_or_else(|| {
        ErrorEnvelope::new(
            "the entry records no ground to plan against, so the plan was not made",
            ErrorDetail::reader_unavailable(
                "the entry's ops record no plan ground over its coverage",
            ),
        )
    })?;
    ground.standing(name)?;
    Ok(ground)
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

    /// Answer a `move`: the document or folder `params` names, compiled to
    /// one operation and previewed or applied through [`Host::apply`].
    ///
    /// **The move carries its link cascade.** Planning respells every link
    /// that would stop naming a moved document, or leaves it as written with
    /// the forecast saying why, and a folder move expands into one document
    /// move per document the folder holds, its forecast naming every file it
    /// leaves behind. `move` is a Rust keyword, so the verb's method is
    /// named for what it moves.
    pub fn move_path(&self, params: MoveParams) -> Result<PendingApply, ErrorEnvelope> {
        let mode = params.mode;
        self.apply_operations(mode, params.plan())
    }

    /// Answer a `delete`: the document `params` removes, compiled to one
    /// operation and previewed or applied through [`Host::apply`].
    ///
    /// **The delete says what becomes of the links naming its document.**
    /// Saying neither flag, it is refused while any link names the document,
    /// its operation unresolved naming every holder and how many links; with
    /// `rewrite_to` it carries the cascade respelling each of them to name
    /// that document; with `allow_broken_links` it lands, the forecast
    /// advising on each link it leaves broken.
    pub fn delete(&self, params: DeleteParams) -> Result<PendingApply, ErrorEnvelope> {
        let mode = params.mode;
        self.apply_operations(mode, params.plan())
    }

    /// Answer a `rewrite_wikilink`: every wikilink naming what `params`
    /// names `old`, compiled to one operation retargeting them to `new` and
    /// previewed or applied through [`Host::apply`].
    ///
    /// **The rewrite carries its link cascade.** Where `old` names one
    /// document before the plan, every wikilink resolving to it, whatever
    /// its spelling, is respelled in its own form to name `new`'s document;
    /// where it names none, every broken wikilink that would name a
    /// document standing at the place `old` spells, its case read as the
    /// root reads it, is. An ambiguous wikilink is left as written, the forecast saying
    /// so, and a Markdown link is no wikilink. An `old` naming several
    /// documents, a `new` naming none or several, and a rewrite retargeting
    /// nothing are refused, the operation unresolved saying why.
    pub fn rewrite_wikilink(
        &self,
        params: RewriteWikilinkParams,
    ) -> Result<PendingApply, ErrorEnvelope> {
        let mode = params.mode;
        self.apply_operations(mode, params.plan())
    }

    /// Answer a `repair`: the next batch of the findings `params` selects,
    /// planned as one resolved plan and previewed or applied through
    /// [`Host::apply`].
    ///
    /// The batch is whole documents in path order ([`Snapshot::repair_batch`]),
    /// read under one hold with the plan resolved on that hold's snapshot.
    /// The plan carries its [`Provenance`]: the findings it left alone, the
    /// findings each operation fixes, the generation it read, the cursor that
    /// continues the batch, and, on a first batch, how many selected findings
    /// remain after it. The hold is given back before the plan enters
    /// [`Host::apply`], so the applier judges and writes the resolved plan as
    /// it judges any, and the provenance it carries decides nothing there.
    ///
    /// A request a read would refuse is refused as the read refuses it,
    /// including a cursor no repair minted. A selection holding parts the
    /// builder could not apply as asked, such as a predicate on a key the
    /// vault does not hold, is refused as `request/unsatisfied` naming every
    /// part, in both modes: a read answers such a part in-band and matches
    /// nothing, and a write goes no further than that nothing.
    pub fn repair(&self, params: RepairParams) -> Result<PendingApply, ErrorEnvelope> {
        let mode = params.mode;
        let plan = self.repair_plan(&params)?;
        self.apply(ApplyParams::new(mode, PlanDocument::resolved(plan)))
    }

    /// The resolved plan of the batch `params` selects, planned on one read
    /// hold that is given back when this returns.
    fn repair_plan(&self, params: &RepairParams) -> Result<ResolvedPlan, ErrorEnvelope> {
        let name = registered_name(&params.vault)?;
        let (_, plan) =
            self.planning_on_hold(name, params.vault.clone(), |ground, snapshot, refused| {
                let selection = RepairSelection::from(params);
                let batch = snapshot
                    .reading(|held| held.repair_batch(&selection, snapshot.declared))
                    .map_err(refused)?;
                self.count_repair_batch(RepairBatchCost::of(
                    batch.as_ref().ok().map(|batch| &batch.work),
                ));
                let batch = batch.map_err(|refusal| refused(page_refusal(refusal)))?;
                // A read answers an unsatisfied part in-band and matches
                // nothing; a write goes no further than that nothing
                // (`planner::expand`), so the repair is refused, naming every
                // part, before anything is planned.
                if !batch.unsatisfied.is_empty() {
                    return Err(ErrorEnvelope::new(
                        "the repair's selection holds parts that could not be applied as \
                         asked, so nothing was planned",
                        ErrorDetail::unsatisfied(batch.unsatisfied),
                    ));
                }
                // The batch's `moved` is not carried: a repair cursor names a
                // path and the batch reads the state that stands now, so
                // nothing in it is the caller's to act on. Its advisories are
                // dropped as a `where` target's are (`PlanSnapshot::matching`
                // reads only the find's rows).
                // One view of the vault serves the fixes and the planning
                // they resolve through, so a document a fix is made to is read
                // once and composed from the bytes its fix was judged on; and
                // one clock reading serves every default the plan fills.
                let view = planning_view(ground, name)?;
                let view = Remembered::over(&view);
                let reading = OneReading::of(&crate::clock::local_now);
                let (plan, skipped, citations) = repaired(
                    &batch.rows,
                    ground,
                    name,
                    snapshot,
                    &view,
                    &reading,
                    refused,
                )?;
                let mut provenance =
                    Provenance::new(batch.snapshot.generation, skipped).with_citations(citations);
                if let Some(remaining) = batch.remaining {
                    provenance = provenance.with_remaining(remaining);
                }
                if let Some(next) = batch.next {
                    provenance = provenance.continued_by(next);
                }
                Ok(plan.with_provenance(provenance))
            })?;
        Ok(plan)
    }

    /// Plan on one read hold over the vault `name`: the hold, the ground the
    /// entry recorded for it, and a [`PlanSnapshot`] of `vault` on the hold's
    /// snapshot, handed to `plan` with the mapping of a refusal the snapshot
    /// met to the envelope that answers it, damage withdrawn from service as
    /// a read withdraws it. This is the one way a request plans on a read
    /// hold, so a preview and a repair count one account.
    ///
    /// What the plan's link judgments cost, what the hold's snapshot ran
    /// whoever ran it, and the readings of the clock its plan took, are the
    /// read account's, however `plan` ended. The hold is given back when this
    /// returns.
    fn planning_on_hold<T>(
        &self,
        name: &VaultName,
        vault: VaultAddress,
        plan: impl FnOnce(
            &PlanGround,
            &PlanSnapshot<'_>,
            &dyn Fn(PageRefused) -> ErrorEnvelope,
        ) -> Result<T, ErrorEnvelope>,
    ) -> Result<(AnswerReading, T), ErrorEnvelope> {
        let hold = self
            .begin_read(name)
            .map_err(|refusal| refusal.answer(name))?;
        let reading = hold.reading().answer_reading(name)?;
        let ground = planning_ground(&hold, name)?;
        let snapshot = PlanSnapshot::held(vault, hold.snapshot(), hold.content_model());
        let refused = |refused| match refused {
            PageRefused::Answered(refused) => refused,
            PageRefused::Damaged(detail) => {
                self.withdraw_for_read_damage(&hold, detail).answer(name)
            }
        };
        let (planned, clock_reads) =
            crate::evidence::clock_reads_of(|| plan(ground, &snapshot, &refused));
        self.count_preview_link_judgments(snapshot.link_judgment_cost());
        self.count_planning_hold(PlanningHoldCost::of(hold.snapshot().counters()));
        self.count_clock_reads(clock_reads);
        Ok((reading, planned?))
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
    /// gate hold that established the read's snapshot and carried on the
    /// hold, so the preview takes the gate no more once its snapshot stands;
    /// whether its root still stands there is asked of the filesystem after
    /// the gate is given back, as an apply's planning asks it.
    pub(crate) fn preview(&self, name: &VaultName, plan: PlanDocument) -> ApplyAnswer {
        let (reading, report) =
            self.planning_on_hold(name, plan.vault().clone(), |ground, snapshot, answered| {
                match plan {
                    // Planned as an apply plans it, then judged as an apply
                    // judges what it planned: resolving checks no schema, so
                    // the plan it answers goes through the same checks a
                    // resolved plan's does.
                    PlanDocument::Operations(authored) => {
                        let resolution =
                            resolve_on(authored, ground, name, snapshot).map_err(answered)?;
                        let resolution = fully_resolved(resolution)?;
                        preview_resolved(
                            resolution.plan,
                            resolution.forecast.left_behind,
                            ground,
                            snapshot,
                        )
                        .map_err(answered)
                    }
                    PlanDocument::Resolved(resolved) => {
                        preview_resolved(resolved, Vec::new(), ground, snapshot).map_err(answered)
                    }
                }
            })?;
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
            schema: SchemaPlace::default(),
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

    /// **A repair whose clock gives no reading is refused as a `new` is**:
    /// `vault/plan-refused` with the operation of the default that reads the
    /// clock left unresolved, naming the clock, in the words a creation
    /// refuses in; and nothing is skipped. The mode decides nothing here: the
    /// plan is refused before a preview or an apply takes it.
    #[test]
    fn a_repair_whose_clock_gives_no_reading_is_refused_as_a_new_is() {
        use crate::planner::view::memory::MemoryVault;
        use norn_config::schema::{LocalTimestamp, NotALocalTimestamp, VaultSchema};
        use norn_wire::{FindingKind, FindingRow, Severity, UnresolvedReason};

        let scratch = Scratch::new("norn-host-repair-clock-unread");
        let mut ground = ground_over(scratch.root());
        let schema = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    required:\n      created: {default: '{{now}}'}\n";
        ground.declared = Arc::new(Declared::pinned(
            VaultSchema::parse(schema.as_bytes()).expect("a schema"),
            "a fingerprint",
        ));
        let name = VaultName::new("notes").unwrap();
        let declared = ContentModel::default();
        let mint = || Err(crate::lifecycle::ReaderUnavailable::new("never minted"));
        let snapshot = PlanSnapshot::on_demand(VaultAddress::name(name.clone()), &mint, &declared);
        let vault = MemoryVault::with(&[("a.md", "---\ntype: task\n---\n")]);
        let clock = || -> Result<LocalTimestamp, NotALocalTimestamp> { Err(NotALocalTimestamp) };
        let reading = OneReading::of(&clock);
        let missing = FindingRow::new(
            7,
            FindingKind::RequiredMissing,
            Severity::Warning,
            DocumentPath::new("a.md").unwrap(),
            Some("created".to_string()),
            None,
            norn_wire::CandidateHead::new([], 0).unwrap(),
            None,
            "created is missing",
            1,
        );
        let refused = |_: PageRefused| -> ErrorEnvelope { panic!("the snapshot was asked") };

        let answered = repaired(
            &[missing],
            &ground,
            &name,
            &snapshot,
            &vault,
            &reading,
            &refused,
        );

        let envelope = answered.expect_err("a repair with no clock was refused");
        assert_eq!(envelope.code(), &ReasonCode::VaultPlanRefused);
        let ErrorDetail::PlanRefused {
            plan, unresolved, ..
        } = envelope.detail()
        else {
            panic!("refused with {:?}", envelope.detail());
        };
        assert!(plan.operations.is_empty());
        let [left] = unresolved.as_slice() else {
            panic!("one unresolved: {unresolved:?}");
        };
        let UnresolvedReason::NoLongerResolves { detail, .. } = &left.reason else {
            panic!("left out for {:?}", left.reason);
        };
        assert_eq!(
            detail,
            &crate::clock::cannot_fill("the rule default for `created`")
        );
    }
}
