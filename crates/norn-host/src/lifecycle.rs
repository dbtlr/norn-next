use std::collections::BTreeMap;
use std::fmt;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use norn_config::VaultName;
use norn_fs::{Batch, Identity, RescanScope, WatchError};
use norn_wire::{MaintainerIdentity, TrustState, UntrustedReason, WarmingPhase, WatcherLossCause};

use crate::registry::{AliasConflict, ServingRegistry};

/// Lifecycle timing chosen by the composition root. There is intentionally no
/// ambient or library default.
#[derive(Clone, Copy, Debug)]
pub struct LifecyclePolicy {
    pub idle_after: Duration,
    pub worker_slots: usize,
    /// Cadence of the one host-wide nonblocking watcher scan.
    pub watch_poll_interval: Duration,
}

/// Work coalesced behind an entry's capacity-one runnable marker.
#[derive(Debug, Default)]
pub struct ReconcileWork {
    pub batch: Batch,
}

/// The effectful half of an entry lifecycle.
pub trait EntryOps: Send + Sync + 'static {
    type Attachment: Send + 'static;

    /// Acquire maintainership, establish watcher coverage, and only then run
    /// one full hash-authoritative heal. After this returns, the lifecycle
    /// performs one nonblocking [`EntryOps::poll`] before it may publish Ready;
    /// raced and continuing facts then use ordinary coalesced reconciliation.
    fn attach(
        &self,
        name: &VaultName,
        progress: &ProgressReporter<Self::Attachment>,
    ) -> Result<Self::Attachment, JobFailure>;
    /// Apply one coalesced envelope. Schema dirtiness dominates document roots;
    /// a rescan widens rather than discarding uncertainty.
    fn reconcile(
        &self,
        name: &VaultName,
        attachment: &mut Self::Attachment,
        work: ReconcileWork,
        progress: &ProgressReporter<Self::Attachment>,
    ) -> Result<(), JobFailure>;
    /// Re-establish trust using resources retained after an environmental or
    /// watcher refusal. Implementations restart watcher coverage before the
    /// full heal when coverage was terminally lost.
    fn recover(
        &self,
        name: &VaultName,
        attachment: &mut Self::Attachment,
        progress: &ProgressReporter<Self::Attachment>,
    ) -> Result<(), JobFailure>;
    /// Nonblocking read of at most one settled watcher batch.
    fn poll(
        &self,
        name: &VaultName,
        attachment: &mut Self::Attachment,
    ) -> Result<Option<Batch>, JobFailure>;
    /// Cheap, nonblocking check for whether worker-scheduled maintenance is due.
    fn maintenance_due(&self, _: &VaultName, _: &Self::Attachment) -> bool {
        false
    }
    /// Run blocking maintenance on the bounded lifecycle worker pool.
    fn maintain(&self, _: &VaultName, _: &mut Self::Attachment) -> Result<(), JobFailure> {
        Ok(())
    }
    fn detach(&self, name: &VaultName, attachment: Self::Attachment);
}

/// Epoch-bound, monotonic publication of one lifecycle job's warming progress.
///
/// A job says what kind of work it is doing by entering a phase, and it enters
/// the phase **before** the work rather than after: the phase is what a caller
/// reads while there is nothing yet to count.
///
/// The two phases a job can be in are the two this type offers, and only one
/// of them counts anything — an asymmetry that is the shape of the type rather
/// than a rule beside it. [`Self::installing_coverage`] returns nothing,
/// because the prologue reads no document and has nothing to report.
/// [`Self::healing`] returns the [`Healing`] handle, and that handle owns the
/// only `report`. So counted progress cannot be published under the coverage
/// phase: reporting requires a handle, and the handle exists only where the
/// heal does. The teardown phase is not offered here at all, because no job
/// runs it: the lifecycle publishes it directly on the leg that releases an
/// entry's resources.
pub struct ProgressReporter<A> {
    entry: std::sync::Weak<Entry<A>>,
    epoch: u64,
}

impl<A> ProgressReporter<A> {
    #[cfg(test)]
    pub(crate) fn disconnected() -> Self {
        Self {
            entry: std::sync::Weak::new(),
            epoch: 0,
        }
    }

    /// Enter the prologue that precedes the first document read, keeping the
    /// counters beside it. Nothing is counted here, so nothing is handed back
    /// to count with.
    pub fn installing_coverage(&self) {
        self.enter(WarmingPhase::InstallingCoverage);
    }

    /// Enter the heal, keeping the counters beside it, and take the handle its
    /// counted progress is reported through.
    pub fn healing(&self) -> Healing<'_, A> {
        self.enter(WarmingPhase::Healing);
        Healing(self)
    }

    fn enter(&self, phase: WarmingPhase) {
        self.publish(|_, healed, total| TrustState::warming(phase, healed, total));
    }

    /// Replace this entry's warming state, under this job's own epoch and only
    /// while the entry is still warming. Anything else has moved past what
    /// this job has to say.
    fn publish(&self, next: impl FnOnce(WarmingPhase, u64, Option<u64>) -> TrustState) {
        let Some(entry) = self.entry.upgrade() else {
            return;
        };
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        if state.epoch != self.epoch {
            return;
        }
        let TrustState::Warming {
            phase,
            healed,
            total_estimate,
            ..
        } = state.trust
        else {
            return;
        };
        state.trust = next(phase, healed, total_estimate);
    }
}

/// The counting half of a warming entry, reached only by entering the heal.
///
/// It borrows the reporter that made it, so it cannot outlive the job whose
/// progress it publishes, and holding one is the standing evidence that the
/// counts it publishes belong to the phase it was entered under.
pub struct Healing<'a, A>(&'a ProgressReporter<A>);

impl<A> Healing<'_, A> {
    /// Publish how far the heal has come, keeping the phase it is running
    /// under.
    pub fn report(&self, healed: u64, total_estimate: Option<u64>) {
        self.0.publish(|phase, prior_healed, prior_total| {
            let healed = healed.max(prior_healed);
            let total = match (prior_total, total_estimate) {
                (Some(left), Some(right)) => Some(left.max(right).max(healed)),
                (Some(left), None) => Some(left.max(healed)),
                (None, Some(right)) => Some(right.max(healed)),
                (None, None) => None,
            };
            TrustState::warming(phase, healed, total)
        });
    }
}

fn reporter<A>(entry: &Arc<Entry<A>>, epoch: u64) -> ProgressReporter<A> {
    ProgressReporter {
        entry: Arc::downgrade(entry),
        epoch,
    }
}

/// A lifecycle job's semantic failure class.
///
/// A terminal watcher failure carries the watch error itself, because the
/// trust state it publishes distinguishes a failed backend from lost coverage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JobFailure {
    Environmental(String),
    WatcherTerminal(WatchError),
    LostMaintainership,
    MaintainerContended(MaintainerIdentity),
}

/// The immediate answer to client demand. Warming never blocks the caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Demand {
    State(TrustState),
    MaintainerContended(MaintainerIdentity),
    DuplicateRoot(AliasConflict),
    UnknownVault,
}

#[derive(Debug)]
pub enum HostError {
    NoWorkerSlots,
    ZeroWatchPollInterval,
    WorkerStopped,
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoWorkerSlots => f.write_str("the host requires at least one worker slot"),
            Self::ZeroWatchPollInterval => {
                f.write_str("the host requires a nonzero watcher poll interval")
            }
            Self::WorkerStopped => f.write_str("the host worker pool stopped"),
        }
    }
}

impl std::error::Error for HostError {}

struct Entry<A> {
    gate: Mutex<EntryState<A>>,
}

struct EntryState<A> {
    trust: TrustState,
    attachment: Option<A>,
    pending: Batch,
    recovery_required: bool,
    /// The live demand leases asking for the recovery the entry currently owes.
    recovery_demands: usize,
    /// Which recovery requirement the demands above were raised against. A
    /// requirement that replaces another retires its demands by moving on from
    /// their generation, so a lease that asked for an earlier recovery neither
    /// satisfies this one nor discounts a lease that did ask for it.
    recovery_generation: u64,
    identity_refused: bool,
    runnable: bool,
    queued: bool,
    active_epoch: Option<u64>,
    pending_dispatch: Option<Job>,
    /// The incumbent another process reported while holding this vault's
    /// maintainer lock. While it is set the entry is parked: `demand` answers
    /// contention instead of a trust state, and no path re-attaches until
    /// [`Host::retry`] clears it.
    maintainer_contended: Option<MaintainerIdentity>,
    duplicate_root: Option<AliasConflict>,
    last_demand: Instant,
    demand_leases: usize,
    safety_pins: usize,
    detach_due: bool,
    detach_scheduled: bool,
    detach_in_flight: bool,
    epoch: u64,
}

impl<A> EntryState<A> {
    /// Owe a recovery no lease has asked for yet.
    ///
    /// The demands standing behind the requirement this one replaces are
    /// retired with it: what they asked for is the run that just ended, and a
    /// lease that watched it fail is not asking for the same run again.
    fn require_recovery(&mut self) {
        self.recovery_required = true;
        self.retire_recovery_demands();
    }

    /// Owe a recovery, leaving the demands already standing behind one
    /// standing.
    ///
    /// A lease that asked for lost coverage to come back is still asking for
    /// exactly that, and a trust snapshot cannot tell one lost-coverage state
    /// from the next — so a demand retired here is one no caller knows to raise
    /// again.
    fn require_recovery_keeping_demands(&mut self) {
        self.recovery_required = true;
    }

    /// Owe no recovery. The demands that were waiting on one retire with it.
    fn clear_recovery(&mut self) {
        self.recovery_required = false;
        self.retire_recovery_demands();
    }

    fn retire_recovery_demands(&mut self) {
        self.recovery_demands = 0;
        self.recovery_generation += 1;
    }

    /// Ask for the recovery the entry owes, answering the token that withdraws
    /// the demand. An entry owing no recovery has nothing to ask of it.
    fn demand_recovery(&mut self) -> Option<u64> {
        if !self.recovery_required {
            return None;
        }
        self.recovery_demands += 1;
        Some(self.recovery_generation)
    }

    /// Withdraw a demand, where the requirement it was raised against is still
    /// the one the entry owes.
    fn withdraw_recovery_demand(&mut self, generation: u64) {
        if generation == self.recovery_generation {
            self.recovery_demands = self.recovery_demands.saturating_sub(1);
        }
    }

    /// Whether a live lease is waiting on the recovery the entry owes.
    fn recovery_demanded(&self) -> bool {
        self.recovery_demands > 0
    }
}

#[derive(Clone)]
enum Job {
    Attach(VaultName, u64),
    Recover(VaultName, u64),
    Reconcile(VaultName, u64),
    Maintenance(VaultName, u64),
    Detach(VaultName, u64),
}

impl Job {
    fn epoch(&self) -> u64 {
        match self {
            Self::Attach(_, epoch)
            | Self::Recover(_, epoch)
            | Self::Reconcile(_, epoch)
            | Self::Maintenance(_, epoch)
            | Self::Detach(_, epoch) => *epoch,
        }
    }
}

/// The wire reason a terminal watch failure publishes.
///
/// Terminal loss and a rescan-worthy overflow are different reasons because
/// they resume differently: coverage that ended waits for client demand, while
/// an overflow re-heals on the lifecycle's own dispatch.
///
/// The detail is the watch error's own rendering, so the prose a person reads
/// is a sentence about the failure rather than a bare value inviting a parse.
fn watcher_lost(error: WatchError) -> UntrustedReason {
    let detail = error.to_string();
    let cause = match error {
        WatchError::Backend(_) => WatcherLossCause::Backend,
        WatchError::CoverageLost(_) => WatcherLossCause::CoverageLost,
    };
    UntrustedReason::watcher_lost(cause, detail)
}

fn trust_for_pending_reconcile(pending: &Batch) -> TrustState {
    if pending.rescans().is_empty() {
        // Coverage is installed and the facts it delivered are what is left to
        // derive, so the entry is warming on the document side of the ladder.
        TrustState::warming(WarmingPhase::Healing, 0, None)
    } else {
        TrustState::untrusted(UntrustedReason::WatcherOverflow)
    }
}

fn schedule_due_detach<A>(state: &mut EntryState<A>, name: &VaultName) -> Option<Job> {
    if state.detach_due
        && state.attachment.is_some()
        && state.demand_leases == 0
        && state.safety_pins == 0
        && !state.runnable
    {
        state.runnable = true;
        state.detach_scheduled = true;
        state.epoch += 1;
        let job = Job::Detach(name.clone(), state.epoch);
        state.pending_dispatch = Some(job.clone());
        Some(job)
    } else {
        None
    }
}

/// Schedule the work an outstanding demand lease is waiting on, where the entry
/// is free to run it.
///
/// A claim holds `runnable` for as long as it lasts, so a demand raised against
/// a claimed entry records its lease and schedules nothing. Every path that ends
/// a claim answers those leases here, which is what makes the claim's own
/// completion the moment the demand is honored rather than some later one. The
/// attachment in hand picks the job exactly as [`Host::demand`] does: coverage
/// still held is recovered, coverage that is gone is attached again.
///
/// An entry that is serving, already working, giving its resources back, parked
/// on a conflict or on a contended maintainer, refused on identity, or owed a
/// recovery no live lease has demanded owes an outstanding lease nothing. A
/// recovery runs only where a lease has demanded it, because a terminal failure
/// does not autonomously restart coverage. A release in flight owes the lease
/// the re-attach [`finish_release`] ends with, which is why nothing is
/// scheduled here against one.
fn schedule_demanded_work<A>(state: &mut EntryState<A>, name: &VaultName) -> Option<Job> {
    if state.demand_leases == 0
        || !matches!(
            state.trust,
            TrustState::Unattached | TrustState::Untrusted { .. }
        )
        || state.runnable
        || state.detach_in_flight
        || (state.recovery_required && !state.recovery_demanded())
        || state.duplicate_root.is_some()
        || state.maintainer_contended.is_some()
        || state.identity_refused
    {
        return None;
    }
    state.runnable = true;
    // The job below is an attach or a recover, and both establish coverage
    // before a document is read.
    state.trust = TrustState::warming(WarmingPhase::InstallingCoverage, 0, None);
    state.epoch += 1;
    let job = if state.attachment.is_some() {
        Job::Recover(name.clone(), state.epoch)
    } else {
        Job::Attach(name.clone(), state.epoch)
    };
    state.pending_dispatch = Some(job.clone());
    Some(job)
}

/// Enter the window in which an entry's resources are going back.
///
/// Taking the attachment is the instant the entry stops being readable, and the
/// state says so here rather than at the end of the leg. It cannot say
/// Unattached yet: Unattached is what an entry publishes once
/// [`EntryOps::detach`] has given the watcher, the store and the maintainer lock
/// back, and a caller that waits for it is waiting for exactly that. Warming is
/// the state meaning attached and not readable, and the phase beside it names
/// the leg — resources going back, nothing counted.
///
/// The flag, not the label, is what makes the window safe to leave the gate
/// open across. [`Host::demand`] and [`schedule_demanded_work`] both refuse to
/// schedule while a release is in flight, so a lease raised inside the window
/// records itself and is answered by [`finish_release`] rather than racing the
/// publication that ends it — and it stays answered there even where another
/// writer overwrites the phase mid-window.
fn begin_release<A>(state: &mut EntryState<A>) {
    state.runnable = false;
    state.detach_in_flight = true;
    state.trust = TrustState::warming(WarmingPhase::ReleasingCoverage, 0, None);
}

/// Give an entry's resources back, publish the state that says they are back,
/// and honor the demand raised while they were going back.
///
/// Every teardown leg ends here, and the order is the whole of the contract:
/// [`EntryOps::detach`] returns before Unattached is published, so Unattached
/// means released on every leg that reaches it rather than on one of them.
///
/// The lease standing at the end of the release is answered by the release
/// itself, because the release is what the lease was waiting behind. An entry
/// parked on a duplicate root, a contended maintainer or an identity refusal
/// owes it nothing, and neither does one owing a recovery no live lease asked
/// for. The requirement is read before it is cleared: a lease that asked for a
/// recovery is asking for the coverage the re-attach below installs.
///
/// The leg's claim on the entry ends where its release does, so the epilogue
/// that would otherwise end it later finds nothing left to end: a re-attach
/// dispatched below is the entry's own work, running against a claim it holds
/// rather than one a finished leg is still entitled to take away.
///
/// The follow-up is sent from here and left nowhere else. A `pending_dispatch`
/// marker beside a job this leg has already sent is one a dispatcher tick would
/// send a second time, under the same epoch.
fn finish_release<O: EntryOps>(
    shared: &Arc<Shared<O>>,
    entry: &Arc<Entry<O::Attachment>>,
    name: &VaultName,
    epoch: u64,
    attachment: Option<O::Attachment>,
) {
    if let Some(attachment) = attachment {
        shared.ops.detach(name, attachment);
    }
    let mut state = entry.gate.lock().expect("entry gate poisoned");
    let reattach_requested = !state.recovery_required || state.recovery_demanded();
    if state.active_epoch == Some(epoch) {
        state.active_epoch = None;
    }
    state.detach_in_flight = false;
    state.pending = Batch::default();
    state.clear_recovery();
    state.runnable = false;
    state.detach_due = false;
    state.detach_scheduled = false;
    state.trust = TrustState::Unattached;
    if state.demand_leases > 0
        && reattach_requested
        && state.duplicate_root.is_none()
        && state.maintainer_contended.is_none()
        && !state.identity_refused
    {
        state.runnable = true;
        state.trust = TrustState::warming(WarmingPhase::InstallingCoverage, 0, None);
        state.epoch += 1;
        let next = Job::Attach(name.clone(), state.epoch);
        drop(state);
        dispatch_followup(shared, next);
    }
}

fn dispatch_pending<O: EntryOps>(
    shared: &Arc<Shared<O>>,
    entry: &Arc<Entry<O::Attachment>>,
) -> Result<(), HostError> {
    let job = {
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        if state.queued || state.active_epoch.is_some() {
            return Ok(());
        }
        let Some(job) = state.pending_dispatch.clone() else {
            return Ok(());
        };
        state.queued = true;
        job
    };
    if shared.shutting_down.load(Ordering::SeqCst) {
        return Err(HostError::WorkerStopped);
    }
    let jobs = shared.jobs.lock().expect("job sender poisoned");
    let Some(jobs) = jobs.as_ref() else {
        return Err(HostError::WorkerStopped);
    };
    match jobs.try_send(job.clone()) {
        Ok(()) => Ok(()),
        Err(mpsc::TrySendError::Full(_)) => {
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            state.queued = false;
            Ok(())
        }
        Err(mpsc::TrySendError::Disconnected(_)) => Err(HostError::WorkerStopped),
    }
}

fn retry_pending_dispatches<O: EntryOps>(shared: &Arc<Shared<O>>) {
    for entry in shared.entries.values() {
        let _ = dispatch_pending(shared, entry);
    }
}

fn refuse_conflict<O: EntryOps>(shared: &Arc<Shared<O>>, conflict: &AliasConflict) {
    let entries = conflict
        .aliases
        .iter()
        .filter_map(|name| shared.entries.get(name).map(|entry| (name, entry)))
        .collect::<Vec<_>>();
    let mut states = entries
        .iter()
        .map(|(_, entry)| entry.gate.lock().expect("entry gate poisoned"))
        .collect::<Vec<_>>();
    let mut releasing = Vec::new();
    for ((name, entry), state) in entries.iter().zip(&mut states) {
        state.epoch += 1;
        state.queued = false;
        state.pending_dispatch = None;
        state.pending = Batch::default();
        state.clear_recovery();
        state.identity_refused = false;
        state.duplicate_root = Some(conflict.clone());
        if state.active_epoch.is_none() && !state.detach_in_flight {
            state.runnable = false;
            match state.attachment.take() {
                // The refusal reaches an idle entry holding the coverage the
                // conflict invalidates, so this is a teardown like any other:
                // the resources go back below, and Unattached is published
                // after they have.
                Some(attachment) => {
                    let epoch = state.epoch;
                    begin_release(state);
                    releasing.push(((*name).clone(), *entry, epoch, attachment));
                }
                // Nothing is held, so the entry is already released and can
                // say so.
                None => state.trust = TrustState::Unattached,
            }
        } else if !state.detach_in_flight {
            // The entry has a job in flight, which is holding the attachment
            // and will give it back when it ends. Unattached is published here
            // rather than there, so for the length of that job this entry says
            // released while its resources are still out — the one publication
            // this contract does not yet cover. A release already under way
            // takes neither route: it has published the phase that names it,
            // and publishes Unattached itself once the resources are back.
            state.trust = TrustState::Unattached;
        }
    }
    drop(states);
    for (name, entry, epoch, attachment) in releasing {
        finish_release(shared, entry, &name, epoch, Some(attachment));
    }
}

fn refuse_identity_error<O: EntryOps>(shared: &Arc<Shared<O>>, name: &VaultName, detail: String) {
    let Some(entry) = shared.entries.get(name) else {
        return;
    };
    let attachment = {
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        state.epoch += 1;
        state.queued = false;
        state.pending_dispatch = None;
        state.pending.merge(Batch::rescan(RescanScope::Vault));
        state.require_recovery();
        state.identity_refused = true;
        state.trust = TrustState::untrusted(UntrustedReason::environmental_refusal(detail));
        if state.active_epoch.is_none() && !state.detach_in_flight {
            state.runnable = false;
            state.attachment.take()
        } else {
            None
        }
    };
    if let Some(attachment) = attachment {
        shared.ops.detach(name, attachment);
    }
}

fn recheck_and_refuse<O: EntryOps>(
    shared: &Arc<Shared<O>>,
    name: &VaultName,
) -> Result<Option<AliasConflict>, HostError> {
    if let Ok(None) = shared.registry.recheck(name) {
        if let Some(entry) = shared.entries.get(name) {
            entry
                .gate
                .lock()
                .expect("entry gate poisoned")
                .identity_refused = false;
        }
        return Ok(None);
    }
    let _attach_guard = shared.attach_gate.lock().expect("attach gate poisoned");
    let conflict = match shared.registry.recheck(name) {
        Ok(conflict) => conflict,
        Err(refusal) => {
            refuse_identity_error(shared, name, refusal.to_string());
            return Ok(None);
        }
    };
    if let Some(conflict) = &conflict {
        refuse_conflict(shared, conflict);
    } else if let Some(entry) = shared.entries.get(name) {
        entry
            .gate
            .lock()
            .expect("entry gate poisoned")
            .identity_refused = false;
    }
    Ok(conflict)
}

struct Shared<O: EntryOps> {
    registry: ServingRegistry,
    entries: BTreeMap<VaultName, Arc<Entry<O::Attachment>>>,
    ops: Arc<O>,
    jobs: Mutex<Option<mpsc::SyncSender<Job>>>,
    shutting_down: AtomicBool,
    idle_after: Duration,
    attach_gate: Mutex<BTreeMap<Identity, VaultName>>,
}

pub struct Host<O: EntryOps> {
    shared: Arc<Shared<O>>,
    workers: Vec<thread::JoinHandle<()>>,
    dispatcher_stop: mpsc::Sender<()>,
    dispatcher: Option<thread::JoinHandle<()>>,
}

/// One client operation's lifecycle guard and immediate trust answer.
/// Dropping it ends the demand lease and starts a fresh idle interval.
pub struct DemandLease<O: EntryOps> {
    outcome: Demand,
    held: Option<(Arc<Shared<O>>, VaultName)>,
    /// The recovery this lease asked the entry for, where it found one owed.
    /// The demand is the lease's own, so dropping withdraws it: a requirement
    /// outliving every lease that asked for it is one nobody is waiting on.
    recovery_demand: Option<u64>,
}

impl<O: EntryOps> Deref for DemandLease<O> {
    type Target = Demand;
    fn deref(&self) -> &Self::Target {
        &self.outcome
    }
}

impl<O: EntryOps> DemandLease<O> {
    pub fn outcome(&self) -> &Demand {
        &self.outcome
    }

    /// The current completion of the asynchronous demand that created this
    /// lease. In particular, maintainer contention becomes observable here on
    /// the same lease whose first answer was `Warming`.
    pub fn completion(&self) -> Demand {
        let Some((shared, name)) = &self.held else {
            return self.outcome.clone();
        };
        let Some(entry) = shared.entries.get(name) else {
            return Demand::UnknownVault;
        };
        let state = entry.gate.lock().expect("entry gate poisoned");
        state
            .maintainer_contended
            .clone()
            .map(Demand::MaintainerContended)
            .or_else(|| state.duplicate_root.clone().map(Demand::DuplicateRoot))
            .unwrap_or_else(|| Demand::State(state.trust.clone()))
    }
}

impl<O: EntryOps> Drop for DemandLease<O> {
    fn drop(&mut self) {
        let Some((shared, name)) = self.held.take() else {
            return;
        };
        let Some(entry) = shared.entries.get(&name) else {
            return;
        };
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        state.demand_leases = state.demand_leases.saturating_sub(1);
        if let Some(generation) = self.recovery_demand {
            state.withdraw_recovery_demand(generation);
        }
        if state.demand_leases == 0 {
            state.last_demand = Instant::now();
            state.detach_due = false;
        }
    }
}

impl<O: EntryOps> Drop for Host<O> {
    fn drop(&mut self) {
        self.shared.shutting_down.store(true, Ordering::SeqCst);
        let _ = self.dispatcher_stop.send(());
        let dispatcher = self.dispatcher.take();
        // Destruction is a teardown per entry, and it names its leg like every
        // other one: an entry with resources still out publishes the releasing
        // phase here and Unattached below, once they are back. The window has a
        // reader — a demand lease holds the shared state itself, so it outlives
        // the host and reads the entry through its own handle.
        let mut releasing = Vec::new();
        for (name, entry) in &self.shared.entries {
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            state.epoch += 1;
            state.runnable = false;
            state.queued = false;
            state.pending_dispatch = None;
            state.pending = Batch::default();
            if state.active_epoch.is_none() && !state.detach_in_flight {
                match state.attachment.take() {
                    Some(attachment) => {
                        begin_release(&mut state);
                        releasing.push((name.clone(), Some(attachment)));
                    }
                    // Nothing is held, so the entry is already released and can
                    // say so.
                    None => state.trust = TrustState::Unattached,
                }
            } else {
                // A job or a release in flight is holding this entry's
                // resources. The joins below wait for it, and whatever it
                // leaves in the entry is given back after them.
                begin_release(&mut state);
                releasing.push((name.clone(), None));
            }
        }
        self.shared.jobs.lock().expect("job sender poisoned").take();
        if let Some(dispatcher) = dispatcher
            && dispatcher.thread().id() != thread::current().id()
        {
            let _ = dispatcher.join();
        }
        for worker in self.workers.drain(..) {
            // Re-entrant destruction from an EntryOps callback is exotic but
            // must not attempt to join the thread currently running Drop.
            if worker.thread().id() != thread::current().id() {
                let _ = worker.join();
            }
        }
        for (name, attachment) in releasing {
            let Some(entry) = self.shared.entries.get(&name) else {
                continue;
            };
            // A leg that ended between the loop above and its join gave its
            // attachment back to the entry rather than to the ops, so the
            // entry is asked again for what it holds.
            let attachment = attachment.or_else(|| {
                entry
                    .gate
                    .lock()
                    .expect("entry gate poisoned")
                    .attachment
                    .take()
            });
            if let Some(attachment) = attachment {
                self.shared.ops.detach(&name, attachment);
            }
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            state.detach_in_flight = false;
            state.trust = TrustState::Unattached;
        }
    }
}

impl<O: EntryOps> Host<O> {
    pub fn new(
        registry: ServingRegistry,
        ops: O,
        policy: LifecyclePolicy,
    ) -> Result<Self, HostError> {
        if policy.worker_slots == 0 {
            return Err(HostError::NoWorkerSlots);
        }
        if policy.watch_poll_interval.is_zero() {
            return Err(HostError::ZeroWatchPollInterval);
        }
        let now = Instant::now();
        let entries = registry
            .entries()
            .map(|entry| {
                (
                    entry.name.clone(),
                    Arc::new(Entry {
                        gate: Mutex::new(EntryState {
                            trust: TrustState::Unattached,
                            attachment: None,
                            pending: Batch::default(),
                            recovery_required: false,
                            recovery_demands: 0,
                            recovery_generation: 0,
                            identity_refused: false,
                            runnable: false,
                            queued: false,
                            active_epoch: None,
                            pending_dispatch: None,
                            maintainer_contended: None,
                            duplicate_root: None,
                            last_demand: now,
                            demand_leases: 0,
                            safety_pins: 0,
                            detach_due: false,
                            detach_scheduled: false,
                            detach_in_flight: false,
                            epoch: 0,
                        }),
                    }),
                )
            })
            .collect();
        let (jobs, receiver) = mpsc::sync_channel(policy.worker_slots);
        let receiver = Arc::new(Mutex::new(receiver));
        let shared = Arc::new(Shared {
            registry,
            entries,
            ops: Arc::new(ops),
            jobs: Mutex::new(Some(jobs)),
            shutting_down: AtomicBool::new(false),
            idle_after: policy.idle_after,
            attach_gate: Mutex::new(BTreeMap::new()),
        });
        let mut workers = Vec::with_capacity(policy.worker_slots);
        for _ in 0..policy.worker_slots {
            let shared = Arc::downgrade(&shared);
            let receiver = Arc::clone(&receiver);
            workers.push(thread::spawn(move || {
                loop {
                    let job = receiver.lock().expect("worker receiver poisoned").recv();
                    match job {
                        Ok(job) => {
                            let Some(shared) = shared.upgrade() else {
                                break;
                            };
                            run_job(&shared, job);
                        }
                        Err(_) => break,
                    }
                }
            }));
        }
        let (dispatcher_stop, stop) = mpsc::channel();
        let dispatcher_shared = Arc::downgrade(&shared);
        let dispatcher = thread::spawn(move || {
            loop {
                match stop.recv_timeout(policy.watch_poll_interval) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                let Some(shared) = dispatcher_shared.upgrade() else {
                    break;
                };
                let _ = reap_idle_shared(&shared, Instant::now());
                poll_watchers(&shared);
                retry_pending_dispatches(&shared);
            }
        });
        Ok(Self {
            shared,
            workers,
            dispatcher_stop,
            dispatcher: Some(dispatcher),
        })
    }

    pub fn state(&self, name: &VaultName) -> Option<TrustState> {
        self.shared.entries.get(name).map(|entry| {
            entry
                .gate
                .lock()
                .expect("entry gate poisoned")
                .trust
                .clone()
        })
    }

    /// Record client demand and, where necessary, start one asynchronous
    /// attach/retry. Concurrent callers only observe Warming.
    pub fn demand(&self, name: &VaultName) -> Result<DemandLease<O>, HostError> {
        let Some(entry) = self.shared.entries.get(name) else {
            return Ok(DemandLease {
                outcome: Demand::UnknownVault,
                held: None,
                recovery_demand: None,
            });
        };
        if let Some(conflict) = recheck_and_refuse(&self.shared, name)? {
            return Ok(DemandLease {
                outcome: Demand::DuplicateRoot(conflict),
                held: None,
                recovery_demand: None,
            });
        }
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        state.demand_leases += 1;
        let recovery_demand = state.demand_recovery();
        state.detach_due = false;
        if state.detach_scheduled && !state.detach_in_flight {
            state.epoch += 1;
            state.runnable = false;
            state.queued = false;
            state.pending_dispatch = None;
            state.detach_scheduled = false;
        }
        if let Some(incumbent) = state.maintainer_contended.clone() {
            drop(state);
            return Ok(DemandLease {
                outcome: Demand::MaintainerContended(incumbent),
                held: Some((Arc::clone(&self.shared), name.clone())),
                recovery_demand,
            });
        }
        // A release in flight is the entry's resources on their way back, and
        // the flag says so whatever label stands beside it: the lease is
        // recorded here and honored by the release, so nothing is scheduled
        // against coverage that is going away.
        let schedule = matches!(
            state.trust,
            TrustState::Unattached | TrustState::Untrusted { .. }
        ) && !state.runnable
            && !state.detach_in_flight
            && !state.identity_refused;
        if schedule {
            state.runnable = true;
            // The job below is an attach or a recover, and both establish
            // coverage before a document is read.
            state.trust = TrustState::warming(WarmingPhase::InstallingCoverage, 0, None);
            state.epoch += 1;
            let epoch = state.epoch;
            let job = if state.attachment.is_some() {
                Job::Recover(name.clone(), epoch)
            } else {
                Job::Attach(name.clone(), epoch)
            };
            state.pending_dispatch = Some(job);
            let answer = Demand::State(state.trust.clone());
            drop(state);
            dispatch_pending(&self.shared, entry)?;
            return Ok(DemandLease {
                outcome: answer,
                held: Some((Arc::clone(&self.shared), name.clone())),
                recovery_demand,
            });
        }
        let answer = Demand::State(state.trust.clone());
        drop(state);
        Ok(DemandLease {
            outcome: answer,
            held: Some((Arc::clone(&self.shared), name.clone())),
            recovery_demand,
        })
    }

    /// Explicitly retry a demand whose prior completion reported contention.
    pub fn retry(&self, name: &VaultName) -> Result<DemandLease<O>, HostError> {
        if let Some(entry) = self.shared.entries.get(name) {
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            state.maintainer_contended = None;
            state.duplicate_root = None;
        }
        self.demand(name)
    }

    /// Merge watcher facts and schedule at most one runnable job for the entry.
    pub fn accept_batch(&self, name: &VaultName, batch: Batch) -> Result<(), HostError> {
        let Some(entry) = self.shared.entries.get(name) else {
            return Ok(());
        };
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        let rescan = !batch.rescans().is_empty();
        state.pending.merge(batch);
        if state.attachment.is_none() && !state.runnable {
            return Ok(());
        }
        if rescan {
            state.trust = TrustState::untrusted(UntrustedReason::WatcherOverflow);
        } else if matches!(state.trust, TrustState::Ready) {
            state.trust = TrustState::warming(WarmingPhase::Healing, 0, None);
        }
        if !state.runnable && state.attachment.is_some() && !state.recovery_required {
            state.runnable = true;
            state.epoch += 1;
            let epoch = state.epoch;
            state.pending_dispatch = Some(Job::Reconcile(name.clone(), epoch));
            drop(state);
            dispatch_pending(&self.shared, entry)?;
        }
        Ok(())
    }

    /// A terminal watcher failure does not autonomously restart coverage.
    pub fn watcher_failed(&self, name: &VaultName, error: WatchError) {
        if let Some(entry) = self.shared.entries.get(name) {
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            state.require_recovery();
            state.pending.merge(Batch::rescan(RescanScope::Vault));
            state.epoch += 1;
            if state.active_epoch.is_none() {
                state.runnable = false;
                state.queued = false;
                state.pending_dispatch = None;
            }
            state.trust = TrustState::untrusted(watcher_lost(error));
        }
    }

    /// Schedule expired entries for teardown. Safety-pinned work is allowed to
    /// finish; its release performs the expired detach immediately.
    pub fn reap_idle(&self, now: Instant) -> Result<(), HostError> {
        reap_idle_shared(&self.shared, now)
    }
}

fn reap_idle_shared<O: EntryOps>(shared: &Arc<Shared<O>>, now: Instant) -> Result<(), HostError> {
    let mut entries = Vec::new();
    for (name, entry) in &shared.entries {
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        if state.demand_leases == 0
            && (state.attachment.is_some() || state.safety_pins > 0)
            && now.saturating_duration_since(state.last_demand) >= shared.idle_after
        {
            state.detach_due = true;
            if schedule_due_detach(&mut state, name).is_some() {
                entries.push(Arc::clone(entry));
            }
        }
    }
    for entry in entries {
        dispatch_pending(shared, &entry)?;
    }
    Ok(())
}

/// Scan every attached entry once. The dispatcher is the only caller, and the
/// effect is nonblocking, so one slow vault cannot consume a worker slot or
/// manufacture a thread per attachment.
fn poll_watchers<O: EntryOps>(shared: &Arc<Shared<O>>) {
    for (name, entry) in &shared.entries {
        let (mut attachment, epoch) = {
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            if state.runnable {
                continue;
            }
            if !state.pending.is_empty() && state.attachment.is_some() && !state.recovery_required {
                state.runnable = true;
                state.epoch += 1;
                state.pending_dispatch = Some(Job::Reconcile(name.clone(), state.epoch));
                drop(state);
                let _ = dispatch_pending(shared, entry);
                continue;
            }
            let Some(attachment) = state.attachment.take() else {
                continue;
            };
            state.safety_pins += 1;
            state.runnable = true;
            state.active_epoch = Some(state.epoch);
            (attachment, state.epoch)
        };
        let result = shared.ops.poll(name, &mut attachment);
        let maintenance_due = result.is_ok() && shared.ops.maintenance_due(name, &attachment);
        let mut schedule = None;
        let mut stale = None;
        let mut release = None;
        {
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            state.safety_pins = state.safety_pins.saturating_sub(1);
            if state.epoch != epoch {
                stale = Some(attachment);
            } else {
                match result {
                    Ok(None) => {
                        state.active_epoch = None;
                        state.runnable = false;
                        state.attachment = Some(attachment);
                        if maintenance_due && !state.recovery_required {
                            state.runnable = true;
                            state.epoch += 1;
                            let job = Job::Maintenance(name.clone(), state.epoch);
                            state.pending_dispatch = Some(job.clone());
                            schedule = Some(job);
                        }
                    }
                    Ok(Some(batch)) => {
                        state.active_epoch = None;
                        state.runnable = false;
                        let rescan = !batch.rescans().is_empty();
                        state.pending.merge(batch);
                        if !state.recovery_required {
                            state.trust = if rescan {
                                TrustState::untrusted(UntrustedReason::WatcherOverflow)
                            } else {
                                TrustState::warming(WarmingPhase::Healing, 0, None)
                            };
                        }
                        state.attachment = Some(attachment);
                        if !state.recovery_required {
                            state.runnable = true;
                            state.epoch += 1;
                            let job = Job::Reconcile(name.clone(), state.epoch);
                            state.pending_dispatch = Some(job.clone());
                            schedule = Some(job);
                        }
                    }
                    Err(JobFailure::LostMaintainership) => {
                        state.epoch += 1;
                        begin_release(&mut state);
                        release = Some(attachment);
                    }
                    Err(JobFailure::MaintainerContended(incumbent)) => {
                        state.maintainer_contended = Some(incumbent);
                        state.epoch += 1;
                        begin_release(&mut state);
                        release = Some(attachment);
                    }
                    // A recovery demand raised inside this claim outlives the
                    // failure the claim reports, so both legs below keep it.
                    Err(JobFailure::WatcherTerminal(error)) => {
                        state.active_epoch = None;
                        state.runnable = false;
                        state.require_recovery_keeping_demands();
                        state.pending.merge(Batch::rescan(RescanScope::Vault));
                        state.trust = TrustState::untrusted(watcher_lost(error));
                        state.attachment = Some(attachment);
                    }
                    Err(JobFailure::Environmental(detail)) => {
                        state.active_epoch = None;
                        state.runnable = false;
                        state.require_recovery_keeping_demands();
                        state.pending.merge(Batch::rescan(RescanScope::Vault));
                        state.trust =
                            TrustState::untrusted(UntrustedReason::environmental_refusal(detail));
                        state.attachment = Some(attachment);
                    }
                }
                // A leg that is releasing the entry schedules nothing against
                // it: the work an outstanding lease is owed is the re-attach
                // the release itself ends with, once the resources are back.
                if release.is_none() {
                    if schedule.is_none() {
                        schedule = schedule_demanded_work(&mut state, name);
                    }
                    if schedule.is_none() {
                        schedule = schedule_due_detach(&mut state, name);
                    }
                }
            }
        }
        if let Some(attachment) = release {
            finish_release(shared, entry, name, epoch, Some(attachment));
            continue;
        }
        if let Some(attachment) = stale {
            // The entry moved on while this poll held it, so the poll owns
            // nothing but the attachment it took: it gives that back and
            // publishes nothing, because whatever moved the entry on has
            // already said where the entry stands.
            shared.ops.detach(name, attachment);
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            if state.active_epoch == Some(epoch) {
                state.active_epoch = None;
                state.runnable = false;
            }
            if let Some(job) = schedule_demanded_work(&mut state, name) {
                schedule = Some(job);
            }
        }
        if let Some(job) = schedule {
            let _ = job;
            let _ = dispatch_pending(shared, entry);
        }
    }
}

fn run_job<O: EntryOps>(shared: &Arc<Shared<O>>, job: Job) {
    let name = match &job {
        Job::Attach(name, _)
        | Job::Recover(name, _)
        | Job::Reconcile(name, _)
        | Job::Maintenance(name, _)
        | Job::Detach(name, _) => name,
    };
    let Some(entry) = shared.entries.get(name) else {
        return;
    };
    let epoch = job.epoch();
    {
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        if state.epoch != epoch {
            if state.pending_dispatch.as_ref().map(Job::epoch) == Some(epoch) {
                state.queued = false;
                state.pending_dispatch = None;
            }
            return;
        }
        state.queued = false;
        state.pending_dispatch = None;
        state.active_epoch = Some(epoch);
    }
    run_job_inner(shared, job);
    let mut state = entry.gate.lock().expect("entry gate poisoned");
    if state.active_epoch == Some(epoch) {
        state.active_epoch = None;
        if state.epoch != epoch && state.pending_dispatch.is_none() {
            state.runnable = false;
        }
    }
}

fn run_job_inner<O: EntryOps>(shared: &Arc<Shared<O>>, job: Job) {
    let name = match &job {
        Job::Attach(name, _)
        | Job::Recover(name, _)
        | Job::Reconcile(name, _)
        | Job::Maintenance(name, _)
        | Job::Detach(name, _) => name,
    };
    let Some(entry) = shared.entries.get(name) else {
        return;
    };
    match job {
        Job::Attach(name, epoch) => {
            // Classification and the identity claim are atomic, but the heal is
            // deliberately outside this gate so unrelated vaults can attach in
            // parallel. Publication revalidates under the same gate.
            let mut attach_claims = shared.attach_gate.lock().expect("attach gate poisoned");
            match shared.registry.recheck(&name) {
                Ok(Some(conflict)) => {
                    refuse_conflict(shared, &conflict);
                    let mut state = entry.gate.lock().expect("entry gate poisoned");
                    if state.epoch == epoch {
                        state.runnable = false;
                        state.duplicate_root = Some(conflict);
                        // The refusal precedes the attach, so this entry holds
                        // nothing to give back and Unattached is true as it is
                        // published.
                        state.trust = TrustState::Unattached;
                    }
                    return;
                }
                Err(refusal) => {
                    let mut state = entry.gate.lock().expect("entry gate poisoned");
                    if state.epoch == epoch {
                        state.runnable = false;
                        state.trust = TrustState::untrusted(
                            UntrustedReason::environmental_refusal(refusal.to_string()),
                        );
                    }
                    return;
                }
                Ok(None) => {}
            }
            let claim_identity = match shared.registry.identity(&name) {
                Ok(identity) => identity,
                Err(refusal) => {
                    let mut state = entry.gate.lock().expect("entry gate poisoned");
                    if state.epoch == epoch {
                        state.runnable = false;
                        state.trust = TrustState::untrusted(
                            UntrustedReason::environmental_refusal(refusal.to_string()),
                        );
                    }
                    return;
                }
            };
            if let Some(identity) = claim_identity {
                if let Some(owner) = attach_claims.get(&identity).filter(|owner| *owner != &name) {
                    let mut aliases = vec![owner.clone(), name.clone()];
                    aliases.sort();
                    let conflict = AliasConflict { aliases };
                    drop(attach_claims);
                    refuse_conflict(shared, &conflict);
                    let mut state = entry.gate.lock().expect("entry gate poisoned");
                    if state.epoch == epoch {
                        state.runnable = false;
                        state.duplicate_root = Some(conflict);
                        // Another alias holds the claim on this identity, so
                        // this entry never acquired anything: it holds nothing
                        // to give back before it says Unattached.
                        state.trust = TrustState::Unattached;
                    }
                    return;
                }
                attach_claims.insert(identity, name.clone());
            }
            drop(attach_claims);
            let result =
                shared
                    .ops
                    .attach(&name, &reporter(entry, epoch))
                    .and_then(|mut attachment| {
                        match drain_observed(&shared.ops, &name, &mut attachment) {
                            Ok((pending, saturated)) => Ok((attachment, pending, saturated)),
                            Err(error) => {
                                shared.ops.detach(&name, attachment);
                                Err(error)
                            }
                        }
                    });
            let mut attach_claims = shared.attach_gate.lock().expect("attach gate poisoned");
            if let Some(identity) = claim_identity
                && attach_claims.get(&identity) == Some(&name)
            {
                attach_claims.remove(&identity);
            }
            let mut post_conflict = match shared.registry.recheck(&name) {
                Ok(conflict) => conflict,
                Err(refusal) => {
                    drop(attach_claims);
                    if let Ok((attachment, _, _)) = result {
                        shared.ops.detach(&name, attachment);
                    }
                    let mut state = entry.gate.lock().expect("entry gate poisoned");
                    if state.epoch == epoch {
                        state.runnable = false;
                        state.trust = TrustState::untrusted(
                            UntrustedReason::environmental_refusal(refusal.to_string()),
                        );
                    }
                    return;
                }
            };
            if post_conflict.is_none()
                && let Ok(Some(identity)) = shared.registry.identity(&name)
                && let Some(owner) = attach_claims.get(&identity).filter(|owner| *owner != &name)
            {
                let mut aliases = vec![owner.clone(), name.clone()];
                aliases.sort();
                post_conflict = Some(AliasConflict { aliases });
            }
            if let Some(conflict) = post_conflict {
                drop(attach_claims);
                if let Ok((attachment, _, _)) = result {
                    shared.ops.detach(&name, attachment);
                }
                refuse_conflict(shared, &conflict);
                let mut state = entry.gate.lock().expect("entry gate poisoned");
                if state.epoch == epoch {
                    state.runnable = false;
                    state.duplicate_root = Some(conflict);
                    state.trust = TrustState::Unattached;
                }
                return;
            }
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            if state.epoch != epoch {
                if let Ok((attachment, _, _)) = result {
                    drop(state);
                    shared.ops.detach(&name, attachment);
                }
                return;
            }
            state.runnable = false;
            match result {
                Ok((attachment, observed, handoff_saturated)) => {
                    state.pending.merge(observed);
                    state.attachment = Some(attachment);
                    state.clear_recovery();
                    state.identity_refused = false;
                    state.maintainer_contended = None;
                    state.duplicate_root = None;
                    if state.pending.is_empty() && !handoff_saturated {
                        state.trust = TrustState::Ready;
                    } else {
                        state.trust = trust_for_pending_reconcile(&state.pending);
                        state.epoch += 1;
                        let epoch = state.epoch;
                        state.runnable = true;
                        drop(state);
                        drop(attach_claims);
                        dispatch_followup(shared, Job::Reconcile(name, epoch));
                    }
                }
                // An attach that failed acquired nothing, and gave back
                // whatever it had reached before it failed. Both branches below
                // therefore publish Unattached holding nothing, which is what
                // Unattached says.
                Err(JobFailure::MaintainerContended(incumbent)) => {
                    state.maintainer_contended = Some(incumbent);
                    state.trust = TrustState::Unattached;
                }
                Err(JobFailure::LostMaintainership) => {
                    state.attachment = None;
                    state.pending = Batch::default();
                    state.trust = TrustState::Unattached;
                }
                Err(JobFailure::WatcherTerminal(error)) => {
                    state.trust = TrustState::untrusted(watcher_lost(error));
                }
                Err(JobFailure::Environmental(detail)) => {
                    state.trust =
                        TrustState::untrusted(UntrustedReason::environmental_refusal(detail));
                }
            }
        }
        Job::Recover(name, epoch) => {
            let mut attachment = {
                let mut state = entry.gate.lock().expect("entry gate poisoned");
                if state.epoch != epoch {
                    return;
                }
                state.safety_pins += 1;
                let Some(attachment) = state.attachment.take() else {
                    state.safety_pins -= 1;
                    state.runnable = false;
                    return;
                };
                attachment
            };
            let mut observed = Batch::default();
            let mut handoff_saturated = false;
            let result = shared
                .ops
                .recover(&name, &mut attachment, &reporter(entry, epoch))
                .and_then(|()| {
                    let (batch, saturated) = drain_observed(&shared.ops, &name, &mut attachment)?;
                    observed = batch;
                    handoff_saturated = saturated;
                    Ok(())
                });
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            state.safety_pins -= 1;
            if state.epoch != epoch {
                drop(state);
                shared.ops.detach(&name, attachment);
                return;
            }
            state.runnable = false;
            let mut next = None;
            match result {
                Ok(()) => {
                    state.pending.merge(observed);
                    state.attachment = Some(attachment);
                    state.clear_recovery();
                    if state.detach_due {
                        next = schedule_due_detach(&mut state, &name);
                    } else if state.pending.is_empty() && !handoff_saturated {
                        state.trust = TrustState::Ready;
                    } else {
                        state.runnable = true;
                        state.epoch += 1;
                        next = Some(Job::Reconcile(name.clone(), state.epoch));
                    }
                }
                Err(JobFailure::LostMaintainership) => {
                    begin_release(&mut state);
                    drop(state);
                    finish_release(shared, entry, &name, epoch, Some(attachment));
                    return;
                }
                Err(JobFailure::MaintainerContended(incumbent)) => {
                    state.maintainer_contended = Some(incumbent);
                    begin_release(&mut state);
                    drop(state);
                    finish_release(shared, entry, &name, epoch, Some(attachment));
                    return;
                }
                Err(JobFailure::WatcherTerminal(error)) => {
                    state.require_recovery();
                    state.pending.merge(Batch::rescan(RescanScope::Vault));
                    state.attachment = Some(attachment);
                    state.trust = TrustState::untrusted(watcher_lost(error));
                    next = schedule_due_detach(&mut state, &name);
                }
                Err(JobFailure::Environmental(detail)) => {
                    state.require_recovery();
                    state.pending.merge(Batch::rescan(RescanScope::Vault));
                    state.attachment = Some(attachment);
                    state.trust =
                        TrustState::untrusted(UntrustedReason::environmental_refusal(detail));
                    next = schedule_due_detach(&mut state, &name);
                }
            }
            drop(state);
            if let Some(job) = next {
                dispatch_followup(shared, job);
            }
        }
        Job::Reconcile(name, epoch) => loop {
            let (mut attachment, work) = {
                let mut state = entry.gate.lock().expect("entry gate poisoned");
                if state.epoch != epoch {
                    return;
                }
                state.safety_pins += 1;
                let Some(attachment) = state.attachment.take() else {
                    state.safety_pins -= 1;
                    state.runnable = false;
                    return;
                };
                let work = ReconcileWork {
                    batch: std::mem::take(&mut state.pending),
                };
                if !matches!(
                    state.trust,
                    TrustState::Warming { .. } | TrustState::Untrusted { .. }
                ) {
                    state.trust = TrustState::warming(WarmingPhase::Healing, 0, None);
                }
                (attachment, work)
            };
            let mut result =
                shared
                    .ops
                    .reconcile(&name, &mut attachment, work, &reporter(entry, epoch));
            let mut handoff_saturated = false;
            let mut observed = Batch::default();
            if result.is_ok() {
                match drain_observed(&shared.ops, &name, &mut attachment) {
                    Ok((batch, saturated)) => {
                        observed = batch;
                        handoff_saturated = saturated;
                    }
                    Err(error) => result = Err(error),
                }
            }
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            state.safety_pins -= 1;
            if state.epoch != epoch {
                drop(state);
                shared.ops.detach(&name, attachment);
                return;
            }
            match result {
                Ok(()) => {
                    state.pending.merge(observed);
                    state.attachment = Some(attachment);
                    if handoff_saturated || !state.pending.is_empty() {
                        state.trust = trust_for_pending_reconcile(&state.pending);
                    }
                    if state.detach_due {
                        state.runnable = false;
                        let next = schedule_due_detach(&mut state, &name);
                        drop(state);
                        if let Some(job) = next {
                            dispatch_followup(shared, job);
                        }
                        break;
                    } else if handoff_saturated {
                        state.epoch += 1;
                        let next = Job::Reconcile(name.clone(), state.epoch);
                        drop(state);
                        dispatch_followup(shared, next);
                        break;
                    } else if state.pending.is_empty() {
                        state.runnable = false;
                        if !state.recovery_required {
                            state.trust = TrustState::Ready;
                        }
                        break;
                    }
                }
                Err(JobFailure::LostMaintainership) => {
                    begin_release(&mut state);
                    drop(state);
                    finish_release(shared, entry, &name, epoch, Some(attachment));
                    break;
                }
                Err(JobFailure::MaintainerContended(incumbent)) => {
                    state.maintainer_contended = Some(incumbent);
                    begin_release(&mut state);
                    drop(state);
                    finish_release(shared, entry, &name, epoch, Some(attachment));
                    break;
                }
                Err(JobFailure::WatcherTerminal(error)) => {
                    state.attachment = Some(attachment);
                    state.runnable = false;
                    state.require_recovery();
                    state.pending.merge(Batch::rescan(RescanScope::Vault));
                    state.trust = TrustState::untrusted(watcher_lost(error));
                    let next = schedule_due_detach(&mut state, &name);
                    drop(state);
                    if let Some(job) = next {
                        dispatch_followup(shared, job);
                    }
                    break;
                }
                Err(JobFailure::Environmental(detail)) => {
                    state.attachment = Some(attachment);
                    state.runnable = false;
                    state.require_recovery();
                    state.pending.merge(Batch::rescan(RescanScope::Vault));
                    state.trust =
                        TrustState::untrusted(UntrustedReason::environmental_refusal(detail));
                    let next = schedule_due_detach(&mut state, &name);
                    drop(state);
                    if let Some(job) = next {
                        dispatch_followup(shared, job);
                    }
                    break;
                }
            }
        },
        Job::Maintenance(name, epoch) => {
            let mut attachment = {
                let mut state = entry.gate.lock().expect("entry gate poisoned");
                if state.epoch != epoch {
                    return;
                }
                state.safety_pins += 1;
                let Some(attachment) = state.attachment.take() else {
                    state.safety_pins -= 1;
                    state.runnable = false;
                    return;
                };
                attachment
            };
            let mut result = shared.ops.maintain(&name, &mut attachment);
            let mut observed = Batch::default();
            let mut handoff_saturated = false;
            if result.is_ok() {
                match drain_observed(&shared.ops, &name, &mut attachment) {
                    Ok((batch, saturated)) => {
                        observed = batch;
                        handoff_saturated = saturated;
                    }
                    Err(error) => result = Err(error),
                }
            }
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            state.safety_pins -= 1;
            if state.epoch != epoch {
                drop(state);
                shared.ops.detach(&name, attachment);
                return;
            }
            let mut next = None;
            match result {
                Ok(()) => {
                    state.pending.merge(observed);
                    state.attachment = Some(attachment);
                    state.runnable = false;
                    if state.detach_due {
                        next = schedule_due_detach(&mut state, &name);
                    } else if handoff_saturated || !state.pending.is_empty() {
                        state.trust = trust_for_pending_reconcile(&state.pending);
                        state.runnable = true;
                        state.epoch += 1;
                        next = Some(Job::Reconcile(name.clone(), state.epoch));
                    }
                }
                Err(JobFailure::LostMaintainership) => {
                    begin_release(&mut state);
                    drop(state);
                    finish_release(shared, entry, &name, epoch, Some(attachment));
                    return;
                }
                Err(JobFailure::MaintainerContended(incumbent)) => {
                    state.maintainer_contended = Some(incumbent);
                    begin_release(&mut state);
                    drop(state);
                    finish_release(shared, entry, &name, epoch, Some(attachment));
                    return;
                }
                Err(JobFailure::WatcherTerminal(error)) => {
                    state.attachment = Some(attachment);
                    state.runnable = false;
                    state.require_recovery();
                    state.pending.merge(Batch::rescan(RescanScope::Vault));
                    state.trust = TrustState::untrusted(watcher_lost(error));
                    next = schedule_due_detach(&mut state, &name);
                }
                Err(JobFailure::Environmental(detail)) => {
                    state.attachment = Some(attachment);
                    state.runnable = false;
                    state.require_recovery();
                    state.pending.merge(Batch::rescan(RescanScope::Vault));
                    state.trust =
                        TrustState::untrusted(UntrustedReason::environmental_refusal(detail));
                    next = schedule_due_detach(&mut state, &name);
                }
            }
            drop(state);
            if let Some(job) = next {
                dispatch_followup(shared, job);
            }
        }
        Job::Detach(name, epoch) => {
            let attachment = {
                let mut state = entry.gate.lock().expect("entry gate poisoned");
                if state.epoch != epoch {
                    return;
                }
                let attachment = state.attachment.take();
                begin_release(&mut state);
                attachment
            };
            finish_release(shared, entry, &name, epoch, attachment);
        }
    }
}

const HANDOFF_BATCH_LIMIT: usize = 8;

fn drain_observed<O: EntryOps>(
    ops: &Arc<O>,
    name: &VaultName,
    attachment: &mut O::Attachment,
) -> Result<(Batch, bool), JobFailure> {
    let mut pending = Batch::default();
    for _ in 0..HANDOFF_BATCH_LIMIT {
        match ops.poll(name, attachment)? {
            Some(batch) => pending.merge(batch),
            None => return Ok((pending, false)),
        }
    }
    Ok((pending, true))
}

/// A worker never waits for room in its own bounded queue. If every queue slot
/// is occupied, its capacity-one follow-up returns to `pending_dispatch` for a
/// later dispatcher tick, after the queued sibling has had a chance to run.
fn dispatch_followup<O: EntryOps>(shared: &Arc<Shared<O>>, job: Job) {
    if shared.shutting_down.load(Ordering::SeqCst) {
        return;
    }
    let result = {
        let jobs = shared.jobs.lock().expect("job sender poisoned");
        let Some(jobs) = jobs.as_ref() else {
            return;
        };
        jobs.try_send(job)
    };
    match result {
        Ok(()) => {}
        Err(mpsc::TrySendError::Full(job)) => {
            let name = match &job {
                Job::Attach(name, _)
                | Job::Recover(name, _)
                | Job::Reconcile(name, _)
                | Job::Maintenance(name, _)
                | Job::Detach(name, _) => name,
            };
            if let Some(entry) = shared.entries.get(name) {
                let mut state = entry.gate.lock().expect("entry gate poisoned");
                if state.epoch == job.epoch() {
                    state.queued = false;
                    state.pending_dispatch = Some(job);
                }
            }
        }
        Err(mpsc::TrySendError::Disconnected(_)) => {}
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // fixtures impersonate external filesystem retargets.
mod tests {
    use super::*;
    use norn_config::registry::{Entry as RegistryEntry, VaultRoot};
    use norn_testkit::wait::{Budget, Observed, wait_until};
    use std::cell::Cell;
    use std::sync::atomic::{AtomicUsize, Ordering};

    thread_local! {
        /// Whether `poll` on this thread runs inside a job's own handoff drain
        /// — the loop `drain_observed` runs right after `attach`, `reconcile`,
        /// `recover` or `maintain` return — rather than the dispatcher's
        /// unprompted per-tick check. A job thread marks itself on entry to one
        /// of those calls and stays marked for the rest of its life, because
        /// every job this suite runs lands on the same small, dedicated pool of
        /// worker threads, and the dispatcher's own poll never runs on one of
        /// them.
        ///
        /// This is what gives the dispatcher's ambient poll and a job's own
        /// handoff drain independent batch sources below: setting one without
        /// the other no longer depends on which of the two ticks first.
        static ON_JOB_THREAD: Cell<bool> = const { Cell::new(false) };
    }

    #[derive(Default)]
    struct FakeOps {
        attaches: AtomicUsize,
        detaches: AtomicUsize,
        recovers: AtomicUsize,
        reconciles: AtomicUsize,
        terminal_recover: std::sync::atomic::AtomicBool,
        terminal_reconcile: std::sync::atomic::AtomicBool,
        environmental_recover: std::sync::atomic::AtomicBool,
        environmental_reconcile: std::sync::atomic::AtomicBool,
        /// The maintainership a leg reports it has lost, one leg per flag.
        /// Every one of them is a teardown: the leg gives the entry back and
        /// the entry stops being attached.
        lost_recover: std::sync::atomic::AtomicBool,
        lost_reconcile: std::sync::atomic::AtomicBool,
        lost_maintenance: std::sync::atomic::AtomicBool,
        lost_poll: std::sync::atomic::AtomicBool,
        /// The incumbent a leg reports while giving the entry back — the same
        /// teardown as a lost maintainership, plus the park that keeps the
        /// entry from re-attaching against another process's lock.
        contend_recover: std::sync::atomic::AtomicBool,
        contend_reconcile: std::sync::atomic::AtomicBool,
        contend_maintenance: std::sync::atomic::AtomicBool,
        contend_attach: std::sync::atomic::AtomicBool,
        block_attach: std::sync::atomic::AtomicBool,
        attach_started: std::sync::atomic::AtomicBool,
        attach_release: std::sync::atomic::AtomicBool,
        heal_in_attach: std::sync::atomic::AtomicBool,
        block_detach: std::sync::atomic::AtomicBool,
        detach_started: std::sync::atomic::AtomicBool,
        detach_release: std::sync::atomic::AtomicBool,
        block_reconcile: std::sync::atomic::AtomicBool,
        block_reconcile_at: AtomicUsize,
        reconcile_started: std::sync::atomic::AtomicBool,
        reconcile_release: std::sync::atomic::AtomicBool,
        block_poll: std::sync::atomic::AtomicBool,
        poll_started: std::sync::atomic::AtomicBool,
        poll_release: std::sync::atomic::AtomicBool,
        maintenance_due: std::sync::atomic::AtomicBool,
        maintenances: AtomicUsize,
        block_maintenance: std::sync::atomic::AtomicBool,
        maintenance_started: std::sync::atomic::AtomicBool,
        maintenance_release: std::sync::atomic::AtomicBool,
        polls: Mutex<BTreeMap<VaultName, usize>>,
        /// The batch source for the dispatcher's own unprompted poll ticks.
        /// It yields empty batches only; a rescan is something only the
        /// handoff source below hands out.
        ambient_poll_batches: AtomicUsize,
        /// The batch source for a job's own handoff drain — the poll loop
        /// `drain_observed` runs after `attach`, `reconcile`, `recover` or
        /// `maintain` returns, on the same thread. Separate from
        /// `ambient_poll_batches` so a caller that wants a handoff to saturate
        /// does not race the dispatcher's own ambient tick for the same
        /// batches; see `ON_JOB_THREAD`.
        handoff_poll_batches: AtomicUsize,
        handoff_rescan_poll_batch: std::sync::atomic::AtomicBool,
        terminal_poll: Mutex<Option<WatchError>>,
        environmental_poll: std::sync::atomic::AtomicBool,
        contend_poll: std::sync::atomic::AtomicBool,
    }

    impl EntryOps for Arc<FakeOps> {
        type Attachment = ();

        fn attach(&self, _: &VaultName, progress: &ProgressReporter<()>) -> Result<(), JobFailure> {
            ON_JOB_THREAD.with(|flag| flag.set(true));
            self.attaches.fetch_add(1, Ordering::SeqCst);
            if self.heal_in_attach.load(Ordering::SeqCst) {
                progress.healing().report(1, Some(2));
            }
            if self.block_attach.load(Ordering::SeqCst) {
                self.attach_started.store(true, Ordering::SeqCst);
                wait_for_flag("attach_release", &self.attach_release);
            }
            if self.contend_attach.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::MaintainerContended(
                    MaintainerIdentity::unknown(),
                ));
            }
            Ok(())
        }

        fn reconcile(
            &self,
            _: &VaultName,
            _: &mut (),
            _: ReconcileWork,
            _: &ProgressReporter<()>,
        ) -> Result<(), JobFailure> {
            ON_JOB_THREAD.with(|flag| flag.set(true));
            let reconcile = self.reconciles.fetch_add(1, Ordering::SeqCst) + 1;
            if self.block_reconcile.load(Ordering::SeqCst)
                || self.block_reconcile_at.load(Ordering::SeqCst) == reconcile
            {
                self.reconcile_started.store(true, Ordering::SeqCst);
                wait_for_flag("reconcile_release", &self.reconcile_release);
            }
            if self.terminal_reconcile.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::WatcherTerminal(WatchError::Backend(
                    "lost".into(),
                )));
            }
            if self.environmental_reconcile.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::Environmental("refused".into()));
            }
            if self.lost_reconcile.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::LostMaintainership);
            }
            if self.contend_reconcile.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::MaintainerContended(
                    MaintainerIdentity::unknown(),
                ));
            }
            Ok(())
        }

        fn recover(
            &self,
            _: &VaultName,
            _: &mut (),
            _: &ProgressReporter<()>,
        ) -> Result<(), JobFailure> {
            ON_JOB_THREAD.with(|flag| flag.set(true));
            self.recovers.fetch_add(1, Ordering::SeqCst);
            // Recovery is the one leg this fake gives a duration to: it is
            // what a wait that spans a recovery has to outlast.
            thread::sleep(Duration::from_millis(20));
            if self.terminal_recover.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::WatcherTerminal(WatchError::Backend(
                    "lost".into(),
                )));
            }
            if self.environmental_recover.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::Environmental("refused".into()));
            }
            if self.lost_recover.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::LostMaintainership);
            }
            if self.contend_recover.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::MaintainerContended(
                    MaintainerIdentity::unknown(),
                ));
            }
            Ok(())
        }

        fn poll(&self, name: &VaultName, _: &mut ()) -> Result<Option<Batch>, JobFailure> {
            *self
                .polls
                .lock()
                .expect("poll counts poisoned")
                .entry(name.clone())
                .or_default() += 1;
            if self.block_poll.load(Ordering::SeqCst) {
                self.poll_started.store(true, Ordering::SeqCst);
                wait_for_flag("poll_release", &self.poll_release);
            }
            if let Some(error) = self
                .terminal_poll
                .lock()
                .expect("terminal poll poisoned")
                .take()
            {
                return Err(JobFailure::WatcherTerminal(error));
            }
            if self.environmental_poll.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::Environmental("refused".into()));
            }
            if self.contend_poll.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::MaintainerContended(
                    MaintainerIdentity::unknown(),
                ));
            }
            if self.lost_poll.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::LostMaintainership);
            }
            let on_job_thread = ON_JOB_THREAD.with(Cell::get);
            if on_job_thread && self.handoff_rescan_poll_batch.swap(false, Ordering::SeqCst) {
                return Ok(Some(Batch::rescan(RescanScope::Vault)));
            }
            let batch_count = if on_job_thread {
                &self.handoff_poll_batches
            } else {
                &self.ambient_poll_batches
            };
            if batch_count
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                    value.checked_sub(1)
                })
                .is_ok()
            {
                return Ok(Some(Batch::default()));
            }
            Ok(None)
        }

        fn maintenance_due(&self, _: &VaultName, _: &()) -> bool {
            self.maintenance_due.swap(false, Ordering::SeqCst)
        }

        fn maintain(&self, _: &VaultName, _: &mut ()) -> Result<(), JobFailure> {
            ON_JOB_THREAD.with(|flag| flag.set(true));
            self.maintenances.fetch_add(1, Ordering::SeqCst);
            if self.block_maintenance.load(Ordering::SeqCst) {
                self.maintenance_started.store(true, Ordering::SeqCst);
                wait_for_flag("maintenance_release", &self.maintenance_release);
            }
            if self.lost_maintenance.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::LostMaintainership);
            }
            if self.contend_maintenance.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::MaintainerContended(
                    MaintainerIdentity::unknown(),
                ));
            }
            Ok(())
        }

        fn detach(&self, _: &VaultName, _: ()) {
            if self.block_detach.load(Ordering::SeqCst) {
                self.detach_started.store(true, Ordering::SeqCst);
                wait_for_flag("detach_release", &self.detach_release);
            }
            self.detaches.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn fixture(ops: Arc<FakeOps>, idle_after: Duration) -> (Host<Arc<FakeOps>>, VaultName) {
        let name = VaultName::new("notes").unwrap();
        let entry = RegistryEntry::new(
            name.clone(),
            VaultRoot::new("/tmp/norn-host-lifecycle-fixture").unwrap(),
        );
        let registry = ServingRegistry::from_entries([entry]).unwrap();
        let host = Host::new(
            registry,
            ops,
            LifecyclePolicy {
                idle_after,
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();
        (host, name)
    }

    /// A fixture whose dispatcher never ticks inside a test's own run: no
    /// ambient watcher poll, no idle reap, and no retry of a dispatch a full
    /// queue refused.
    ///
    /// Some callers count the reconciles one `accept_batch` handoff produces.
    /// An ambient poll claim coincident with that `accept_batch` takes the
    /// rescan into a claimed entry, which leaves the entry watcher-overflowed
    /// and makes the work scheduled at claim completion a recovery rather
    /// than the reconcile the count is about — a real route, and not the one
    /// these tests pin.
    ///
    /// Standing down the dispatcher costs its other duties too, so a test
    /// here also assumes nothing it dispatches is refused for a full queue:
    /// the tick that would retry such a dispatch is a minute away, past any
    /// wait budget in this suite.
    fn fixture_without_ambient_polling(ops: Arc<FakeOps>) -> (Host<Arc<FakeOps>>, VaultName) {
        let name = VaultName::new("notes").unwrap();
        let entry = RegistryEntry::new(
            name.clone(),
            VaultRoot::new("/tmp/norn-host-lifecycle-fixture").unwrap(),
        );
        let registry = ServingRegistry::from_entries([entry]).unwrap();
        let host = Host::new(
            registry,
            ops,
            LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots: 1,
                watch_poll_interval: Duration::from_secs(60),
            },
        )
        .unwrap();
        (host, name)
    }

    /// The budget every wait in this suite obeys: long enough that a loaded
    /// machine is not the thing under test, and short enough that a state
    /// that never arrives is reported rather than waited on. A 20ms
    /// `FakeOps::recover` alone can eat a tenth of this on a quiet machine,
    /// so the bound is a wall clock, not an iteration count that races it.
    ///
    /// It is tighter than the bound the production suite in `production`
    /// declares, and deliberately: the subjects here converge over a channel
    /// and a fake, while those cross a real filesystem and a real watcher.
    fn lifecycle_wait_budget() -> Budget {
        Budget::new(Duration::from_secs(10), Duration::from_millis(250))
    }

    /// Wait for one exact trust state, reporting the last state observed.
    ///
    /// The failure is the testkit's own: which bound it passed, how long it
    /// ran, how many times it asked, and the state it last saw. A wait that
    /// expires because one probe was slow and a wait that expires because the
    /// state never came are different diagnoses, and only that report tells
    /// them apart.
    fn wait_for_state<O: EntryOps>(host: &Host<O>, name: &VaultName, expected: TrustState) {
        wait_until(
            &format!("the trust state to become {expected:?}"),
            lifecycle_wait_budget(),
            || match host.state(name) {
                Some(state) if state == expected => Observed::Met(()),
                state => Observed::pending(format!("the state is {state:?}")),
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
    }

    /// Wait for the fake to have given back `expected` attachments, on the one
    /// budget, reporting how many it had given back when the wait gave up.
    fn wait_for_detaches(ops: &FakeOps, expected: usize, what: &str) {
        wait_until(what, lifecycle_wait_budget(), || {
            let detaches = ops.detaches.load(Ordering::SeqCst);
            if detaches == expected {
                Observed::Met(())
            } else {
                Observed::pending(format!("{detaches} detaches so far"))
            }
        })
        .unwrap_or_else(|failure| panic!("{failure}"));
    }

    /// The state a lost watcher publishes, with the cause and the prose the
    /// watch error carried.
    fn lost(cause: WatcherLossCause, detail: &str) -> TrustState {
        TrustState::untrusted(UntrustedReason::watcher_lost(cause, detail))
    }

    /// Whether a state is an environmental refusal, judged by the reason alone.
    /// The detail beside it is the platform's own account of the refusal, which
    /// is prose rather than a value to match — but a refusal carries an account
    /// of itself, so the prose is there.
    fn refuses_environmentally(state: Option<&TrustState>) -> bool {
        let Some(TrustState::Untrusted {
            reason: UntrustedReason::EnvironmentalRefusal { detail, .. },
            ..
        }) = state
        else {
            return false;
        };
        assert!(
            !detail.is_empty(),
            "an environmental refusal published no account of itself"
        );
        true
    }

    /// The state the fake's terminal watch error publishes: a failed backend,
    /// described by the error's own rendering of its message.
    fn backend_lost() -> TrustState {
        lost(WatcherLossCause::Backend, "filesystem watcher failed: lost")
    }

    /// The margin a negative check gets. Proving that nothing happened has no
    /// event to wait for, so the check waits a fixed span and then reads the
    /// counter or state it expects to find unmoved.
    ///
    /// The span is sized against the work being ruled out, not against the
    /// machine: the dispatcher ticks every 2ms under `fixture` and the fake's
    /// whole recover leg is 20ms, so this covers several chances for the ruled
    /// out thing to happen. A positive condition is never waited for this way
    /// — that is `wait_until` and the wrappers here that build on it.
    fn settle() {
        thread::sleep(Duration::from_millis(20));
    }

    /// Wait for one of a fake's own markers, on the budget every other wait
    /// here obeys. The observation is a boolean, so the failure names the
    /// marker rather than a state.
    ///
    /// Waiting sleeps between questions rather than yielding: several of these
    /// markers are set by the very job threads the wait is waiting on, and a
    /// spin competes with them for the core that would set the marker.
    fn wait_for_flag(label: &str, flag: &std::sync::atomic::AtomicBool) {
        wait_until(label, lifecycle_wait_budget(), || {
            if flag.load(Ordering::SeqCst) {
                Observed::Met(())
            } else {
                Observed::pending("not set")
            }
        })
        .unwrap_or_else(|failure| panic!("{failure}"));
    }

    /// A demanded entry names the work it is doing before it can count any of
    /// it. Coverage installation is the whole of the attach prologue — a lock,
    /// a sweep, a watcher — and it counts no document, so counters alone
    /// cannot tell it from a heal that is not progressing.
    #[test]
    fn a_demanded_entry_warms_under_the_coverage_phase_before_a_heal_counts() {
        let ops = Arc::new(FakeOps::default());
        ops.block_attach.store(true, Ordering::SeqCst);
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let lease = host.demand(&name).unwrap();
        assert_eq!(
            *lease.outcome(),
            Demand::State(TrustState::warming(
                WarmingPhase::InstallingCoverage,
                0,
                None
            ))
        );
        wait_for_flag("attach_started", &ops.attach_started);
        assert_eq!(
            host.state(&name),
            Some(TrustState::warming(
                WarmingPhase::InstallingCoverage,
                0,
                None
            ))
        );
        ops.attach_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        drop(lease);
    }

    /// A phase and the counters beside it are one publication: entering the
    /// heal moves the phase, and the progress reported under it keeps that
    /// phase rather than replacing it.
    #[test]
    fn counted_progress_is_published_under_the_phase_it_runs_in() {
        let ops = Arc::new(FakeOps::default());
        ops.heal_in_attach.store(true, Ordering::SeqCst);
        ops.block_attach.store(true, Ordering::SeqCst);
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let lease = host.demand(&name).unwrap();
        wait_for_flag("attach_started", &ops.attach_started);
        assert_eq!(
            host.state(&name),
            Some(TrustState::warming(WarmingPhase::Healing, 1, Some(2)))
        );
        ops.attach_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        drop(lease);
    }

    #[test]
    fn concurrent_demand_is_single_flight() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        for _ in 0..20 {
            let _ = host.demand(&name).unwrap();
        }
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn stored_contention_is_reported_without_scheduling_a_hidden_retry() {
        let ops = Arc::new(FakeOps::default());
        ops.contend_attach.store(true, Ordering::SeqCst);
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let initial = host.demand(&name).unwrap();
        wait_until(
            "the contended attach to park the entry",
            lifecycle_wait_budget(),
            || {
                let attaches = ops.attaches.load(Ordering::SeqCst);
                let state = host.state(&name);
                if attaches == 1 && state == Some(TrustState::Unattached) {
                    Observed::Met(())
                } else {
                    Observed::pending(format!("{attaches} attaches, state is {state:?}"))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        assert!(matches!(
            initial.completion(),
            Demand::MaintainerContended(_)
        ));
        let lease = host.demand(&name).unwrap();
        assert!(matches!(lease.completion(), Demand::MaintainerContended(_)));
        settle();
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        drop(lease);
        drop(initial);
        drop(host.retry(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);
    }

    /// Teardown stops saying Ready at the instant the entry stops being
    /// readable, and names the leg it is on rather than borrowing the phase of
    /// the leg that installs coverage. It cannot say Unattached yet: that is
    /// the state a caller waits on to know the watcher, the store and the lock
    /// have been given back, and this leg has not given them back yet.
    #[test]
    fn demand_during_in_flight_detach_reports_the_release_rather_than_ready() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::ZERO);
        let initial = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        drop(initial);
        ops.block_detach.store(true, Ordering::SeqCst);
        host.reap_idle(Instant::now()).unwrap();
        wait_for_flag("detach_started", &ops.detach_started);
        let releasing = TrustState::warming(WarmingPhase::ReleasingCoverage, 0, None);
        assert_eq!(host.state(&name), Some(releasing.clone()));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);
        let lease = host.demand(&name).unwrap();
        assert_eq!(
            lease.completion(),
            Demand::State(releasing),
            "teardown must not advertise coverage installation, or a readable entry"
        );
        ops.detach_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);
        drop(lease);
    }

    /// The state a leg publishes while the resources it is giving back are
    /// still out.
    fn releasing() -> TrustState {
        TrustState::warming(WarmingPhase::ReleasingCoverage, 0, None)
    }

    /// Provoke one teardown leg, answering the lease it took to provoke it.
    type Provoke =
        fn(&Arc<FakeOps>, &Host<Arc<FakeOps>>, &VaultName) -> Option<DemandLease<Arc<FakeOps>>>;

    /// Build the host one teardown family runs on.
    type Fixture = fn(Arc<FakeOps>) -> (Host<Arc<FakeOps>>, VaultName);

    /// Drive an entry into one teardown leg and prove the leg releases before
    /// it publishes the state that says it has released.
    ///
    /// The fake holds `detach` open, so the entry is read from inside the
    /// release rather than after it. A leg that published Unattached before
    /// calling `detach` is caught here by the entry saying released while the
    /// fake has given nothing back — the ordering this pins is the whole
    /// difference between the two observations.
    ///
    /// The held `detach` is why the fixture matters: the wait below takes the
    /// first release it sees for the leg's own, so a family whose provocation
    /// can invalidate a watcher poll in flight runs without ambient polling.
    /// The stale poll's release is a real one and reaches the same fake first,
    /// and the entry it reads is then the one the poll left rather than the one
    /// the leg under test is tearing down.
    ///
    /// The lease a leg needed to run at all is dropped inside the window,
    /// because a lease outstanding when a release finishes is honored by
    /// re-attaching, and the re-attach is the subject of its own test.
    fn teardown_releases_before_it_publishes(
        fixture: Fixture,
        arm: fn(&FakeOps),
        provoke: Provoke,
    ) {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_detach.store(true, Ordering::SeqCst);
        arm(&ops);
        let lease = provoke(&ops, &host, &name);
        wait_for_flag("detach_started", &ops.detach_started);
        assert_eq!(
            host.state(&name),
            Some(releasing()),
            "the entry reported its resources released while they were still out"
        );
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);
        drop(lease);

        ops.detach_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Unattached);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
    }

    /// The host a poll-leg teardown runs on: the leg is the dispatcher's own
    /// watcher poll, so the dispatcher has to be ticking.
    fn polling_fixture(ops: Arc<FakeOps>) -> (Host<Arc<FakeOps>>, VaultName) {
        fixture(ops, Duration::from_secs(60))
    }

    /// The dispatcher's own watcher poll reaches the entry unprompted, so the
    /// leg needs nothing to provoke it.
    fn by_a_watcher_poll(
        _: &Arc<FakeOps>,
        _: &Host<Arc<FakeOps>>,
        _: &VaultName,
    ) -> Option<DemandLease<Arc<FakeOps>>> {
        None
    }

    /// A recovery runs where a lease asks for one, so the lease that asks is
    /// held until the leg it schedules is inside its release.
    ///
    /// The watcher failure that owes the recovery also invalidates any watcher
    /// poll in flight, which is why this family runs without ambient polling.
    fn by_a_demanded_recovery(
        _: &Arc<FakeOps>,
        host: &Host<Arc<FakeOps>>,
        name: &VaultName,
    ) -> Option<DemandLease<Arc<FakeOps>>> {
        host.watcher_failed(name, WatchError::Backend("gone".into()));
        Some(host.demand(name).unwrap())
    }

    /// Watcher facts schedule the reconcile that reports the failure, and the
    /// rescan is what keeps them scheduling it: a batch merged into an entry a
    /// poll claim already holds is picked up by the next tick only where the
    /// entry has something pending.
    fn by_a_reconciled_batch(
        _: &Arc<FakeOps>,
        host: &Host<Arc<FakeOps>>,
        name: &VaultName,
    ) -> Option<DemandLease<Arc<FakeOps>>> {
        host.accept_batch(name, Batch::rescan(RescanScope::Vault))
            .unwrap();
        None
    }

    /// Maintenance is scheduled by the watcher poll that finds it due, and
    /// this family runs without ambient polling, so the poll it needs is the
    /// one driven here.
    fn by_due_maintenance(
        ops: &Arc<FakeOps>,
        host: &Host<Arc<FakeOps>>,
        _: &VaultName,
    ) -> Option<DemandLease<Arc<FakeOps>>> {
        ops.maintenance_due.store(true, Ordering::SeqCst);
        poll_watchers(&host.shared);
        None
    }

    #[test]
    fn a_poll_that_lost_maintainership_releases_before_it_publishes_unattached() {
        teardown_releases_before_it_publishes(
            polling_fixture,
            |ops| ops.lost_poll.store(true, Ordering::SeqCst),
            by_a_watcher_poll,
        );
    }

    #[test]
    fn a_poll_that_found_contention_releases_before_it_publishes_unattached() {
        teardown_releases_before_it_publishes(
            polling_fixture,
            |ops| ops.contend_poll.store(true, Ordering::SeqCst),
            by_a_watcher_poll,
        );
    }

    #[test]
    fn a_recover_that_lost_maintainership_releases_before_it_publishes_unattached() {
        teardown_releases_before_it_publishes(
            fixture_without_ambient_polling,
            |ops| ops.lost_recover.store(true, Ordering::SeqCst),
            by_a_demanded_recovery,
        );
    }

    #[test]
    fn a_recover_that_found_contention_releases_before_it_publishes_unattached() {
        teardown_releases_before_it_publishes(
            fixture_without_ambient_polling,
            |ops| ops.contend_recover.store(true, Ordering::SeqCst),
            by_a_demanded_recovery,
        );
    }

    #[test]
    fn a_reconcile_that_lost_maintainership_releases_before_it_publishes_unattached() {
        teardown_releases_before_it_publishes(
            fixture_without_ambient_polling,
            |ops| ops.lost_reconcile.store(true, Ordering::SeqCst),
            by_a_reconciled_batch,
        );
    }

    #[test]
    fn a_reconcile_that_found_contention_releases_before_it_publishes_unattached() {
        teardown_releases_before_it_publishes(
            fixture_without_ambient_polling,
            |ops| ops.contend_reconcile.store(true, Ordering::SeqCst),
            by_a_reconciled_batch,
        );
    }

    #[test]
    fn a_maintenance_that_lost_maintainership_releases_before_it_publishes_unattached() {
        teardown_releases_before_it_publishes(
            fixture_without_ambient_polling,
            |ops| ops.lost_maintenance.store(true, Ordering::SeqCst),
            by_due_maintenance,
        );
    }

    #[test]
    fn a_maintenance_that_found_contention_releases_before_it_publishes_unattached() {
        teardown_releases_before_it_publishes(
            fixture_without_ambient_polling,
            |ops| ops.contend_maintenance.store(true, Ordering::SeqCst),
            by_due_maintenance,
        );
    }

    /// Two attached aliases of one vault, ready to be refused as a duplicate
    /// root.
    ///
    /// Without ambient polling: a dispatcher tick holding either alias in a
    /// watcher poll makes a refusal take the in-flight route, where the release
    /// belongs to the job that holds it rather than to the refusal these tests
    /// are about.
    fn two_alias_host(ops: Arc<FakeOps>) -> (Host<Arc<FakeOps>>, VaultName, VaultName) {
        let a = VaultName::new("a").unwrap();
        let b = VaultName::new("b").unwrap();
        let registry = ServingRegistry::from_entries([
            RegistryEntry::new(
                a.clone(),
                VaultRoot::new("/tmp/norn-host-refused-a").unwrap(),
            ),
            RegistryEntry::new(
                b.clone(),
                VaultRoot::new("/tmp/norn-host-refused-b").unwrap(),
            ),
        ])
        .unwrap();
        let host = Host::new(
            registry,
            ops,
            LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots: 2,
                watch_poll_interval: Duration::from_secs(60),
            },
        )
        .unwrap();
        drop(host.demand(&a).unwrap());
        drop(host.demand(&b).unwrap());
        wait_for_state(&host, &a, TrustState::Ready);
        wait_for_state(&host, &b, TrustState::Ready);
        (host, a, b)
    }

    /// A duplicate-root refusal reaching idle entries is a teardown per alias:
    /// each one names the leg it is on, and none of them reports its resources
    /// released until they are.
    #[test]
    fn a_refused_alias_releases_before_it_publishes_unattached() {
        let ops = Arc::new(FakeOps::default());
        let (host, a, b) = two_alias_host(Arc::clone(&ops));

        ops.block_detach.store(true, Ordering::SeqCst);
        let shared = Arc::clone(&host.shared);
        let conflict = AliasConflict {
            aliases: vec![a.clone(), b.clone()],
        };
        // The refusal runs off the test thread because it releases both
        // aliases inline, and this test reads the entries from inside that
        // release.
        let refusal = thread::spawn(move || refuse_conflict(&shared, &conflict));
        wait_for_flag("detach_started", &ops.detach_started);
        assert_eq!(host.state(&a), Some(releasing()));
        assert_eq!(host.state(&b), Some(releasing()));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);

        ops.detach_release.store(true, Ordering::SeqCst);
        refusal.join().unwrap();
        wait_for_state(&host, &a, TrustState::Unattached);
        wait_for_state(&host, &b, TrustState::Unattached);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 2);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);
    }

    /// A refusal reaching an entry whose release is already under way leaves
    /// the publication to that release: a second refusal passing over the
    /// window is not evidence that anything came back.
    #[test]
    fn a_refusal_over_a_release_in_flight_leaves_the_release_to_publish() {
        let ops = Arc::new(FakeOps::default());
        let (host, a, b) = two_alias_host(Arc::clone(&ops));

        ops.block_detach.store(true, Ordering::SeqCst);
        let shared = Arc::clone(&host.shared);
        let conflict = AliasConflict {
            aliases: vec![a.clone(), b.clone()],
        };
        let second = conflict.clone();
        // The first refusal runs off the test thread because it releases both
        // aliases inline, and the second one below runs from inside that
        // release.
        let refusal = thread::spawn(move || refuse_conflict(&shared, &conflict));
        wait_for_flag("detach_started", &ops.detach_started);

        refuse_conflict(&host.shared, &second);
        assert_eq!(
            (host.state(&a), host.state(&b)),
            (Some(releasing()), Some(releasing())),
            "a refusal published released over a release that had given nothing back"
        );
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);

        ops.detach_release.store(true, Ordering::SeqCst);
        refusal.join().unwrap();
        wait_for_state(&host, &a, TrustState::Unattached);
        wait_for_state(&host, &b, TrustState::Unattached);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 2);
    }

    /// A demand raised while an entry is giving its resources back defers to
    /// the release and is honored by it: the entry is warming, so nothing is
    /// scheduled against resources still on their way out, and the re-attach
    /// the lease is owed runs when they are back.
    #[test]
    fn a_demand_raised_during_a_teardown_is_honored_when_the_release_finishes() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_detach.store(true, Ordering::SeqCst);
        ops.lost_poll.store(true, Ordering::SeqCst);
        wait_for_flag("detach_started", &ops.detach_started);

        let lease = host.demand(&name).unwrap();
        assert_eq!(*lease.outcome(), Demand::State(releasing()));
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            1,
            "the demand scheduled an attach against an entry still holding its resources"
        );

        ops.detach_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        drop(lease);
    }

    /// A demand raised inside a job leg's release is honored the same way the
    /// dispatcher's own leg honors one, and the re-attach it dispatches runs as
    /// the entry's own claimed work: the leg's claim ended with its release, so
    /// nothing left over from the finished leg unclaims the job in flight.
    #[test]
    fn a_job_leg_release_honors_a_demand_with_a_claimed_re_attach() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_detach.store(true, Ordering::SeqCst);
        ops.lost_recover.store(true, Ordering::SeqCst);
        host.watcher_failed(&name, WatchError::Backend("gone".into()));
        let recovering = host.demand(&name).unwrap();
        wait_for_flag("detach_started", &ops.detach_started);

        let lease = host.demand(&name).unwrap();
        assert_eq!(*lease.outcome(), Demand::State(releasing()));

        ops.block_attach.store(true, Ordering::SeqCst);
        ops.detach_release.store(true, Ordering::SeqCst);
        wait_for_flag("attach_started", &ops.attach_started);
        {
            let entry = host.shared.entries.get(&name).unwrap();
            let state = entry.gate.lock().unwrap();
            assert!(
                state.runnable,
                "the release's own re-attach is in flight against an unclaimed entry"
            );
            assert_eq!(state.active_epoch, Some(state.epoch));
        }

        ops.attach_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 1);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);
        drop(lease);
        drop(recovering);
    }

    /// A teardown that parks the entry on a contended maintainer honors the
    /// demand raised inside it by reporting the contention: the release owes
    /// the lease an answer, and re-attaching against another process's lock is
    /// not one.
    #[test]
    fn a_demand_raised_during_a_contended_teardown_stays_parked() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_detach.store(true, Ordering::SeqCst);
        ops.contend_poll.store(true, Ordering::SeqCst);
        wait_for_flag("detach_started", &ops.detach_started);

        let lease = host.demand(&name).unwrap();
        assert!(matches!(lease.outcome(), Demand::MaintainerContended(_)));

        ops.detach_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Unattached);
        settle();
        assert_eq!(host.state(&name), Some(TrustState::Unattached));
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        assert!(matches!(lease.completion(), Demand::MaintainerContended(_)));
        drop(lease);
    }

    /// Destruction is a teardown like any other: the entry names the leg it is
    /// on while its resources are going back, and says released only once they
    /// are. The window has a reader — a lease holds the shared state itself and
    /// reads the entry through it, without the host the destruction consumed.
    #[test]
    fn destruction_releases_before_it_publishes_unattached() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_detach.store(true, Ordering::SeqCst);
        let destruction = thread::spawn(move || drop(host));
        wait_for_flag("detach_started", &ops.detach_started);
        assert_eq!(
            lease.completion(),
            Demand::State(releasing()),
            "destruction reported the entry readable, or released, while its resources were out"
        );
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);

        ops.detach_release.store(true, Ordering::SeqCst);
        destruction.join().unwrap();
        assert_eq!(lease.completion(), Demand::State(TrustState::Unattached));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
    }

    /// A job the joins wait for may have given its attachment back to the entry
    /// before its claim ended, so destruction gives back what it finds in the
    /// entry as well as what it took itself.
    #[test]
    fn destruction_gives_back_an_attachment_a_finished_job_left_behind() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        {
            let entry = host.shared.entries.get(&name).unwrap();
            let mut state = entry.gate.lock().unwrap();
            // The instant a leg has re-stored the attachment and has not yet
            // cleared its claim: destruction reads the entry as busy and takes
            // nothing from it.
            state.active_epoch = Some(state.epoch);
        }

        drop(host);
        assert_eq!(
            ops.detaches.load(Ordering::SeqCst),
            1,
            "destruction dropped an attachment instead of giving it back"
        );
    }

    /// The release window is closed by the release itself, not by the label
    /// standing in the entry: a writer that overwrites the phase mid-window
    /// still schedules nothing against resources on their way out, and the
    /// lease raised there is answered by the release.
    #[test]
    fn a_relabelled_release_window_still_schedules_nothing() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_detach.store(true, Ordering::SeqCst);
        ops.lost_poll.store(true, Ordering::SeqCst);
        wait_for_flag("detach_started", &ops.detach_started);
        host.watcher_failed(&name, WatchError::Backend("gone".into()));

        let lease = host.demand(&name).unwrap();
        assert!(
            !matches!(
                lease.outcome(),
                Demand::State(TrustState::Warming {
                    phase: WarmingPhase::InstallingCoverage,
                    ..
                })
            ),
            "the demand claimed coverage installation against an entry still releasing: {:?}",
            lease.outcome()
        );
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);

        ops.detach_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);
        drop(lease);
    }

    #[test]
    fn idle_deadline_begins_when_the_final_demand_lease_ends() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_millis(20));
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        // Elapsed time is the subject here, so these two sleeps are the test's
        // own clock rather than a wait: the first spends more than the idle
        // interval while the lease is held, and the second spends it again
        // after the lease ends, when it is allowed to count.
        thread::sleep(Duration::from_millis(30));
        drop(lease);
        host.reap_idle(Instant::now()).unwrap();
        assert_eq!(host.state(&name), Some(TrustState::Ready));
        thread::sleep(Duration::from_millis(25));
        host.reap_idle(Instant::now()).unwrap();
        wait_for_state(&host, &name, TrustState::Unattached);
    }

    #[test]
    fn long_held_lease_gets_a_fresh_idle_interval_after_release() {
        let ops = Arc::new(FakeOps::default());
        let idle_after = Duration::from_millis(200);
        let (host, name) = fixture(Arc::clone(&ops), idle_after);
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        // The test's own clock, not a wait: it spends the whole idle interval
        // and more while the lease is held, so the reap that follows is the
        // one a fresh interval has to survive.
        thread::sleep(idle_after + Duration::from_millis(50));
        assert_eq!(host.state(&name), Some(TrustState::Ready));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);

        let released = Instant::now();
        drop(lease);
        host.reap_idle(released + idle_after / 2).unwrap();
        assert_eq!(host.state(&name), Some(TrustState::Ready));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);
        wait_for_state(&host, &name, TrustState::Unattached);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn idle_reap_releases_the_attachment_and_returns_to_unattached() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::ZERO);
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        drop(lease);
        host.reap_idle(Instant::now()).unwrap();
        wait_for_state(&host, &name, TrustState::Unattached);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn dispatcher_reaps_idle_attachment_despite_watcher_churn() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_millis(20));
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        drop(lease);
        for _ in 0..3 {
            host.accept_batch(&name, Batch::rescan(RescanScope::Vault))
                .unwrap();
            // Churn is what this test feeds the dispatcher, so the gap is the
            // input rather than a wait: it spaces the batches across several
            // dispatcher ticks instead of collapsing them into one.
            thread::sleep(Duration::from_millis(4));
        }
        wait_for_state(&host, &name, TrustState::Unattached);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        assert!(ops.reconciles.load(Ordering::SeqCst) > 0);
    }

    #[test]
    fn demand_lease_cancels_queued_detach_until_client_work_ends() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::ZERO);
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        host.reap_idle(Instant::now()).unwrap();
        settle();
        assert_eq!(host.state(&name), Some(TrustState::Ready));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);
        drop(lease);
        wait_for_state(&host, &name, TrustState::Unattached);
    }

    #[test]
    fn terminal_watcher_failure_recovers_only_on_demand() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let _ = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        host.watcher_failed(&name, WatchError::Backend("gone".into()));
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 0);
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 1);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        drop(lease);
    }

    /// The watch error a terminal failure carried reaches the trust state: a
    /// backend that stopped and a vault root that left coverage are two causes
    /// with two states, and the error's own account of itself is the state's
    /// detail — a sentence a person reads, not a value shaped to be parsed.
    #[test]
    fn a_terminal_watcher_failure_publishes_the_cause_it_carried() {
        for (error, expected) in [
            (
                WatchError::Backend("the backend stopped".into()),
                lost(
                    WatcherLossCause::Backend,
                    "filesystem watcher failed: the backend stopped",
                ),
            ),
            (
                WatchError::CoverageLost(std::path::PathBuf::from("/tmp/norn-host-vault-root")),
                lost(
                    WatcherLossCause::CoverageLost,
                    "watch coverage was lost for /tmp/norn-host-vault-root",
                ),
            ),
        ] {
            let ops = Arc::new(FakeOps::default());
            let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
            let held = host.demand(&name).unwrap();
            wait_for_state(&host, &name, TrustState::Ready);
            host.watcher_failed(&name, error);
            assert_eq!(host.state(&name), Some(expected));
            drop(held);
        }
    }

    /// A terminal poll failure publishes its cause once, and the dispatcher
    /// keeps polling the attachment it kept. Those later ticks report no facts,
    /// so the state a client reads long after the loss is still the cause that
    /// ended coverage rather than something minted on top of it.
    #[test]
    fn a_published_watcher_cause_outlives_the_ticks_that_follow_it() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let held = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        *ops.terminal_poll.lock().unwrap() = Some(WatchError::CoverageLost(
            std::path::PathBuf::from("/tmp/norn-host-lifecycle-fixture"),
        ));
        let expected = lost(
            WatcherLossCause::CoverageLost,
            "watch coverage was lost for /tmp/norn-host-lifecycle-fixture",
        );
        wait_for_state(&host, &name, expected.clone());
        let polls_at_loss = *ops
            .polls
            .lock()
            .unwrap()
            .get(&name)
            .expect("the entry was polled");
        wait_until(
            "four more watcher polls of the entry",
            lifecycle_wait_budget(),
            || {
                let polls = ops.polls.lock().unwrap().get(&name).copied().unwrap_or(0);
                if polls > polls_at_loss + 4 {
                    Observed::Met(())
                } else {
                    Observed::pending(format!("{polls} polls so far"))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        assert!(
            ops.polls.lock().unwrap().get(&name).copied().unwrap_or(0) > polls_at_loss + 4,
            "the dispatcher stopped polling the entry"
        );
        assert_eq!(host.state(&name), Some(expected));
        drop(held);
    }

    /// Contention reported by a poll parks the entry the way the attach path
    /// does: the entry publishes Unattached, nothing re-attaches against the
    /// lock another process holds, and demand keeps answering contention until
    /// a retry clears it.
    #[test]
    fn contention_reported_by_a_poll_parks_the_entry_until_retry() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let held = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        ops.contend_poll.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Unattached);
        settle();
        assert_eq!(host.state(&name), Some(TrustState::Unattached));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        assert!(matches!(
            host.demand(&name).unwrap().outcome(),
            Demand::MaintainerContended(_)
        ));
        settle();
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);

        let retried = host.retry(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);
        drop((retried, held));
    }

    #[test]
    fn recover_drains_pending_invalidations_before_publishing_ready() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        host.watcher_failed(&name, WatchError::Backend("gone".into()));
        // Held across the wait: the recovery this test counts is the one this
        // lease asks for, and a lease dropped before it runs withdraws it.
        let lease = host.demand(&name).unwrap();
        host.accept_batch(&name, Batch::rescan(RescanScope::Vault))
            .unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.reconciles.load(Ordering::SeqCst), 1);
        drop(lease);
    }

    #[test]
    fn attach_handoff_saturation_stays_warming_until_a_followup_drain() {
        let ops = Arc::new(FakeOps::default());
        ops.handoff_poll_batches
            .store(HANDOFF_BATCH_LIMIT, Ordering::SeqCst);
        ops.block_reconcile.store(true, Ordering::SeqCst);
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));

        let lease = host.demand(&name).unwrap();
        wait_for_flag("reconcile_started", &ops.reconcile_started);
        assert!(matches!(
            host.state(&name),
            Some(TrustState::Warming { .. })
        ));

        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        drop(lease);
    }

    #[test]
    fn attach_handoff_preserves_observed_rescan_as_untrusted() {
        let ops = Arc::new(FakeOps::default());
        ops.handoff_rescan_poll_batch.store(true, Ordering::SeqCst);
        ops.block_reconcile_at.store(1, Ordering::SeqCst);
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));

        let lease = host.demand(&name).unwrap();
        wait_for_flag("reconcile_started", &ops.reconcile_started);
        assert_eq!(
            host.state(&name),
            Some(TrustState::untrusted(UntrustedReason::WatcherOverflow))
        );

        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        drop(lease);
    }

    #[test]
    fn reconcile_handoff_saturation_requires_an_additional_reconcile_before_ready() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);

        // The handoff drain's own batch source, not the dispatcher's ambient
        // one: a shared counter would let an ambient tick spend a batch this
        // saturation depends on before `accept_batch` below ever schedules
        // the reconcile that is supposed to drain it.
        ops.handoff_poll_batches
            .store(HANDOFF_BATCH_LIMIT, Ordering::SeqCst);
        host.accept_batch(&name, Batch::rescan(RescanScope::Vault))
            .unwrap();
        wait_for_state(&host, &name, TrustState::Ready);

        assert_eq!(ops.reconciles.load(Ordering::SeqCst), 2);
        drop(lease);
    }

    #[test]
    fn reconcile_handoff_saturation_preserves_observed_rescan_as_untrusted() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);

        // See the sibling saturation test above: the handoff drain's own
        // batch source, isolated from the dispatcher's ambient ticks.
        ops.handoff_rescan_poll_batch.store(true, Ordering::SeqCst);
        ops.handoff_poll_batches
            .store(HANDOFF_BATCH_LIMIT - 1, Ordering::SeqCst);
        ops.block_reconcile_at.store(2, Ordering::SeqCst);
        host.accept_batch(&name, Batch::default()).unwrap();

        wait_until(
            "a second reconcile, saturating the handoff",
            lifecycle_wait_budget(),
            || {
                let reconciles = ops.reconciles.load(Ordering::SeqCst);
                if reconciles == 2 {
                    Observed::Met(())
                } else {
                    Observed::pending(format!("{reconciles} reconciles so far"))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        assert_eq!(ops.reconciles.load(Ordering::SeqCst), 2);
        assert_eq!(
            host.state(&name),
            Some(TrustState::untrusted(UntrustedReason::WatcherOverflow))
        );

        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        drop(lease);
    }

    #[test]
    fn terminal_failure_during_recover_stays_watcher_untrusted() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        host.watcher_failed(&name, WatchError::Backend("gone".into()));
        ops.terminal_recover.store(true, Ordering::SeqCst);
        // Held across the wait: the recovery that fails below is the one this
        // lease asks for, and a lease dropped before it runs withdraws it.
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, backend_lost());
        drop(lease);
    }

    #[test]
    fn terminal_failure_during_reconcile_stays_watcher_untrusted() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        ops.terminal_reconcile.store(true, Ordering::SeqCst);
        host.accept_batch(&name, Batch::rescan(RescanScope::Vault))
            .unwrap();
        wait_for_state(&host, &name, backend_lost());
    }

    #[test]
    fn failed_reconcile_requires_demand_recovery_before_later_facts_can_be_ready() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        ops.environmental_reconcile.store(true, Ordering::SeqCst);
        host.accept_batch(&name, Batch::rescan(RescanScope::Vault))
            .unwrap();
        wait_for_state(
            &host,
            &name,
            TrustState::untrusted(UntrustedReason::environmental_refusal("refused")),
        );
        let failed_count = ops.reconciles.load(Ordering::SeqCst);
        host.accept_batch(&name, Batch::default()).unwrap();
        settle();
        assert_eq!(ops.reconciles.load(Ordering::SeqCst), failed_count);
        assert_eq!(
            host.state(&name),
            Some(TrustState::untrusted(
                UntrustedReason::environmental_refusal("refused")
            ))
        );
        // Held across the wait; see the sibling recover-side test below for
        // why an immediately dropped lease here can wedge the entry.
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 1);
        assert!(ops.reconciles.load(Ordering::SeqCst) > failed_count);
        drop(lease);
    }

    #[test]
    fn failed_recover_cannot_be_bypassed_by_a_later_watcher_fact() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        host.watcher_failed(&name, WatchError::Backend("gone".into()));
        ops.environmental_recover.store(true, Ordering::SeqCst);
        // Held across both waits below: a lease dropped before either state
        // is reached would withdraw the recovery request it raised.
        let lease = host.demand(&name).unwrap();
        wait_for_state(
            &host,
            &name,
            TrustState::untrusted(UntrustedReason::environmental_refusal("refused")),
        );
        host.accept_batch(&name, Batch::default()).unwrap();
        settle();
        assert_eq!(ops.reconciles.load(Ordering::SeqCst), 0);
        assert_eq!(
            host.state(&name),
            Some(TrustState::untrusted(
                UntrustedReason::environmental_refusal("refused")
            ))
        );
        drop(lease);
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 2);
        assert_eq!(ops.reconciles.load(Ordering::SeqCst), 1);
        drop(lease);
    }

    #[test]
    fn watcher_failure_invalidates_an_in_flight_reconcile() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        ops.block_reconcile.store(true, Ordering::SeqCst);
        host.accept_batch(&name, Batch::rescan(RescanScope::Vault))
            .unwrap();
        wait_for_flag("reconcile_started", &ops.reconcile_started);
        host.watcher_failed(&name, WatchError::Backend("lost".into()));
        let raced_demand = host.demand(&name).unwrap();
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, backend_lost());
        settle();
        assert_eq!(host.state(&name), Some(backend_lost()));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        drop(raced_demand);
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn host_drop_waits_for_in_flight_work_and_its_attachment_teardown() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        ops.block_reconcile.store(true, Ordering::SeqCst);
        host.accept_batch(&name, Batch::rescan(RescanScope::Vault))
            .unwrap();
        wait_for_flag("reconcile_started", &ops.reconcile_started);
        let returned = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let drop_returned = Arc::clone(&returned);
        let dropper = thread::spawn(move || {
            drop(host);
            drop_returned.store(true, Ordering::SeqCst);
        });
        settle();
        assert!(!returned.load(Ordering::SeqCst));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);
        ops.reconcile_release.store(true, Ordering::SeqCst);
        dropper.join().unwrap();
        assert!(returned.load(Ordering::SeqCst));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn host_drop_detaches_an_idle_attachment_before_returning() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        drop(host);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
    }

    /// A demand raised while a job is claimed schedules nothing, and the lease
    /// it returns is the caller's only handle on the completion — so a caller
    /// that drops it has asked for nothing and gets nothing until it asks
    /// again.
    ///
    /// The entry is not stuck: it is waiting, which is what lazy attachment
    /// means. A caller that wants the work either keeps its lease or demands
    /// again, and both reach `Ready`.
    #[test]
    fn a_demand_dropped_during_an_invalidated_poll_leaves_the_entry_waiting() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        ops.block_poll.store(true, Ordering::SeqCst);
        wait_for_flag("poll_started", &ops.poll_started);

        host.watcher_failed(&name, WatchError::Backend("lost".into()));
        drop(host.demand(&name).unwrap());
        ops.poll_release.store(true, Ordering::SeqCst);
        wait_for_detaches(&ops, 1, "the stale poll to give up its attachment");
        settle();
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            1,
            "a demand nobody was holding restarted the entry on its own"
        );
        assert_eq!(host.state(&name), Some(backend_lost()));

        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn demand_during_invalidated_poll_waits_for_attachment_ownership() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        ops.block_poll.store(true, Ordering::SeqCst);
        wait_for_flag("poll_started", &ops.poll_started);
        host.watcher_failed(&name, WatchError::Backend("lost".into()));
        let demand = host.demand(&name).unwrap();
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        assert_eq!(demand.completion(), Demand::State(backend_lost()));
        ops.poll_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        drop(demand);
    }

    #[test]
    fn held_lease_does_not_autonomously_recover_a_terminal_poll_failure() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let held = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        ops.block_poll.store(true, Ordering::SeqCst);
        wait_for_flag("poll_started", &ops.poll_started);

        host.watcher_failed(&name, WatchError::Backend("lost".into()));
        ops.poll_release.store(true, Ordering::SeqCst);
        wait_for_detaches(&ops, 1, "the stale poll to give up its attachment");
        settle();
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        assert_eq!(host.state(&name), Some(backend_lost()));

        let retry = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);
        drop((held, retry));
    }

    /// Leave the entry where a terminal watcher failure leaves it: attached,
    /// untrusted, and recovering only on demand. The dispatcher goes on claiming
    /// it for watcher polls from here, and that claim is the window a demand
    /// raised against the entry has to survive.
    fn attached_awaiting_recovery(ops: &Arc<FakeOps>) -> (Host<Arc<FakeOps>>, VaultName) {
        let (host, name) = fixture(Arc::clone(ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        *ops.terminal_poll.lock().unwrap() = Some(WatchError::Backend("lost".into()));
        wait_for_state(&host, &name, backend_lost());
        (host, name)
    }

    /// Hold the entry in a watcher poll's claim, and answer once it is held.
    fn claim_for_a_poll(ops: &Arc<FakeOps>) {
        ops.block_poll.store(true, Ordering::SeqCst);
        wait_for_flag("poll_started", &ops.poll_started);
    }

    /// The demand a claimed entry answers with its own state, having scheduled
    /// nothing: what the entry publishes is the proof the claim was held, since
    /// a demand that scheduled the work would have published `Warming` instead.
    fn demand_inside_a_claim(
        host: &Host<Arc<FakeOps>>,
        name: &VaultName,
    ) -> DemandLease<Arc<FakeOps>> {
        let lease = host.demand(name).unwrap();
        assert_eq!(
            host.state(name),
            Some(backend_lost()),
            "the demand reached an unclaimed entry and scheduled the work itself"
        );
        lease
    }

    /// A poll that finds nothing gives the entry back, and the demand raised
    /// while it held the claim is run against the coverage it hands back.
    #[test]
    fn a_demand_raised_during_a_poll_runs_when_the_poll_finds_nothing() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = attached_awaiting_recovery(&ops);
        claim_for_a_poll(&ops);
        let lease = demand_inside_a_claim(&host, &name);

        ops.poll_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(lease.completion(), Demand::State(TrustState::Ready));
        assert_eq!(
            ops.recovers.load(Ordering::SeqCst),
            1,
            "the demand was answered by recovering the coverage still in hand"
        );
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);
        drop(lease);
    }

    /// Facts reported by the poll are merged first and the demand is run after,
    /// so an entry that owes a recovery reconciles nothing until coverage is
    /// installed again.
    #[test]
    fn a_demand_raised_during_a_poll_runs_when_the_poll_reports_facts() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = attached_awaiting_recovery(&ops);
        claim_for_a_poll(&ops);
        ops.ambient_poll_batches.store(1, Ordering::SeqCst);
        let lease = demand_inside_a_claim(&host, &name);

        ops.poll_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 1);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);
        drop(lease);
    }

    /// A recovery demanded during the claim outlives the terminal failure the
    /// same poll reports: the lease that raised it is still waiting, and the
    /// failure it landed on is the one it asked to be recovered from.
    #[test]
    fn a_standing_recovery_demand_survives_a_terminal_poll_failure() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = attached_awaiting_recovery(&ops);
        claim_for_a_poll(&ops);
        *ops.terminal_poll.lock().unwrap() = Some(WatchError::Backend("lost".into()));
        let lease = demand_inside_a_claim(&host, &name);

        ops.poll_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 1);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        drop(lease);
    }

    /// An environmental refusal reported by the poll is answered the same way,
    /// because the demand it landed on is a demand for the entry to be usable
    /// and the refusal is what stands between them.
    #[test]
    fn a_demand_raised_during_a_poll_survives_an_environmental_poll_refusal() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = attached_awaiting_recovery(&ops);
        claim_for_a_poll(&ops);
        ops.environmental_poll.store(true, Ordering::SeqCst);
        let lease = demand_inside_a_claim(&host, &name);

        ops.poll_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 1);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        drop(lease);
    }

    /// A lease taken before the failure demanded nothing of it, so the terminal
    /// poll leg leaves the entry lost and waiting. The recovery is the next
    /// demand's to ask for, and asking runs it.
    #[test]
    fn a_lease_predating_a_terminal_poll_failure_does_not_recover_it() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let held = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        claim_for_a_poll(&ops);
        *ops.terminal_poll.lock().unwrap() = Some(WatchError::Backend("lost".into()));

        ops.poll_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, backend_lost());
        settle();
        assert_eq!(host.state(&name), Some(backend_lost()));
        assert_eq!(
            ops.recovers.load(Ordering::SeqCst),
            0,
            "a lease that demanded no recovery got one anyway"
        );
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);

        let retry = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 1);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        drop((held, retry));
    }

    /// Wait for the dispatcher to run `count` further watcher polls of the
    /// entry. A leg that publishes nothing is observed through the polls that
    /// follow it, because the entry is only claimable again once it has run.
    fn wait_for_polls(ops: &Arc<FakeOps>, name: &VaultName, count: usize) {
        let polls = || ops.polls.lock().unwrap().get(name).copied().unwrap_or(0);
        let target = polls() + count;
        wait_until(
            &format!("{count} further watcher polls of the entry"),
            lifecycle_wait_budget(),
            || {
                let seen = polls();
                if seen >= target {
                    Observed::Met(())
                } else {
                    Observed::pending(format!("{seen} of {target} polls"))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
    }

    /// A recovery demand belongs to the lease that raised it. A lease that
    /// withdraws inside the claim has asked for nothing, so the poll's
    /// completion leaves the entry waiting even though other leases are
    /// outstanding — they demanded the entry, not its recovery.
    #[test]
    fn a_recovery_demand_withdrawn_inside_a_claim_is_not_served_to_another_lease() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let held = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        *ops.terminal_poll.lock().unwrap() = Some(WatchError::Backend("lost".into()));
        wait_for_state(&host, &name, backend_lost());
        claim_for_a_poll(&ops);
        drop(demand_inside_a_claim(&host, &name));

        ops.poll_release.store(true, Ordering::SeqCst);
        wait_for_polls(&ops, &name, 3);
        assert_eq!(
            ops.recovers.load(Ordering::SeqCst),
            0,
            "a demand nobody was holding restarted the entry on its own"
        );
        assert_eq!(host.state(&name), Some(backend_lost()));

        let retry = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 1);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        drop((held, retry));
    }

    /// Withdrawing one recovery demand withdraws only that one. A lease still
    /// holding its own is still waiting on the recovery, and the poll's
    /// completion runs it.
    #[test]
    fn a_withdrawn_recovery_demand_leaves_a_concurrent_one_standing() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = attached_awaiting_recovery(&ops);
        claim_for_a_poll(&ops);
        let standing = demand_inside_a_claim(&host, &name);
        drop(demand_inside_a_claim(&host, &name));

        ops.poll_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 1);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        drop(standing);
    }

    /// A demand withdrawn after the recovery it asked for has already run is
    /// spent, and a spent demand answers for nobody: the lease waiting on the
    /// recovery the entry owes now is still waiting, and the claim's completion
    /// runs it.
    #[test]
    fn a_spent_recovery_demand_does_not_withdraw_the_one_a_later_lease_raised() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = attached_awaiting_recovery(&ops);
        let spent = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 1);

        // A second terminal failure owes a second recovery, and the lease still
        // holding the first one's demand never asked for this one.
        *ops.terminal_poll.lock().unwrap() = Some(WatchError::Backend("lost".into()));
        wait_for_state(&host, &name, backend_lost());
        claim_for_a_poll(&ops);
        let waiting = demand_inside_a_claim(&host, &name);
        drop(spent);

        ops.poll_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(
            ops.recovers.load(Ordering::SeqCst),
            2,
            "a spent demand cancelled the recovery a live lease was waiting on"
        );
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        drop(waiting);
    }

    #[derive(Default)]
    struct PollingOps {
        emit: std::sync::atomic::AtomicBool,
        queued: AtomicUsize,
        reconciles: AtomicUsize,
        /// Hold the reconcile half-healed, so the warming leg it opens lasts
        /// as long as the test watching it needs rather than a fixed span a
        /// wait's own cadence can step over.
        hold_reconcile: std::sync::atomic::AtomicBool,
        reconcile_started: std::sync::atomic::AtomicBool,
        reconcile_release: std::sync::atomic::AtomicBool,
    }

    impl EntryOps for Arc<PollingOps> {
        type Attachment = ();
        fn attach(&self, _: &VaultName, _: &ProgressReporter<()>) -> Result<(), JobFailure> {
            Ok(())
        }
        fn reconcile(
            &self,
            _: &VaultName,
            _: &mut (),
            _: ReconcileWork,
            progress: &ProgressReporter<()>,
        ) -> Result<(), JobFailure> {
            let healing = progress.healing();
            healing.report(1, Some(2));
            self.reconciles.fetch_add(1, Ordering::SeqCst);
            if self.hold_reconcile.load(Ordering::SeqCst) {
                self.reconcile_started.store(true, Ordering::SeqCst);
                wait_for_flag("reconcile_release", &self.reconcile_release);
            }
            healing.report(2, Some(2));
            Ok(())
        }
        fn recover(
            &self,
            _: &VaultName,
            _: &mut (),
            _: &ProgressReporter<()>,
        ) -> Result<(), JobFailure> {
            Ok(())
        }
        fn poll(&self, _: &VaultName, _: &mut ()) -> Result<Option<Batch>, JobFailure> {
            if self
                .queued
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                    value.checked_sub(1)
                })
                .is_ok()
            {
                return Ok(Some(Batch::rescan(RescanScope::Vault)));
            }
            Ok(self.emit.swap(false, Ordering::SeqCst).then(Batch::default))
        }
        fn detach(&self, _: &VaultName, _: ()) {}
    }

    #[test]
    fn attach_handoff_drains_two_already_queued_batches_before_ready() {
        let ops = Arc::new(PollingOps::default());
        ops.queued.store(2, Ordering::SeqCst);
        let name = VaultName::new("notes").unwrap();
        let registry = ServingRegistry::from_entries([RegistryEntry::new(
            name.clone(),
            VaultRoot::new("/tmp/norn-host-two-batches").unwrap(),
        )])
        .unwrap();
        let host = Host::new(
            registry,
            Arc::clone(&ops),
            LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.queued.load(Ordering::SeqCst), 0);
        assert_eq!(ops.reconciles.load(Ordering::SeqCst), 1);
        drop(lease);
    }

    #[derive(Default)]
    struct QueueFullOps {
        a_started: std::sync::atomic::AtomicBool,
        b_started: std::sync::atomic::AtomicBool,
        release_a: std::sync::atomic::AtomicBool,
        a_polled: std::sync::atomic::AtomicBool,
    }
    impl EntryOps for Arc<QueueFullOps> {
        type Attachment = VaultName;
        fn attach(
            &self,
            name: &VaultName,
            _: &ProgressReporter<VaultName>,
        ) -> Result<VaultName, JobFailure> {
            if name.as_str() == "a" {
                self.a_started.store(true, Ordering::SeqCst);
                wait_for_flag("release_a", &self.release_a);
            } else if name.as_str() == "b" {
                self.b_started.store(true, Ordering::SeqCst);
            }
            Ok(name.clone())
        }
        fn reconcile(
            &self,
            _: &VaultName,
            _: &mut VaultName,
            _: ReconcileWork,
            _: &ProgressReporter<VaultName>,
        ) -> Result<(), JobFailure> {
            Ok(())
        }
        fn recover(
            &self,
            _: &VaultName,
            _: &mut VaultName,
            _: &ProgressReporter<VaultName>,
        ) -> Result<(), JobFailure> {
            Ok(())
        }
        fn poll(&self, name: &VaultName, _: &mut VaultName) -> Result<Option<Batch>, JobFailure> {
            Ok(
                (name.as_str() == "a" && !self.a_polled.swap(true, Ordering::SeqCst))
                    .then(|| Batch::rescan(RescanScope::Vault)),
            )
        }
        fn detach(&self, _: &VaultName, _: VaultName) {}
    }

    #[test]
    fn unrelated_attaches_use_distinct_worker_slots() {
        let ops = Arc::new(QueueFullOps::default());
        let a = VaultName::new("a").unwrap();
        let b = VaultName::new("b").unwrap();
        let registry = ServingRegistry::from_entries([
            RegistryEntry::new(
                a.clone(),
                VaultRoot::new("/tmp/norn-host-parallel-a").unwrap(),
            ),
            RegistryEntry::new(
                b.clone(),
                VaultRoot::new("/tmp/norn-host-parallel-b").unwrap(),
            ),
        ])
        .unwrap();
        let host = Host::new(
            registry,
            Arc::clone(&ops),
            LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots: 2,
                watch_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();

        let a_lease = host.demand(&a).unwrap();
        wait_for_flag("a_started", &ops.a_started);
        let b_lease = host.demand(&b).unwrap();
        wait_for_flag("b_started", &ops.b_started);
        wait_for_state(&host, &b, TrustState::Ready);

        ops.release_a.store(true, Ordering::SeqCst);
        wait_for_state(&host, &a, TrustState::Ready);
        drop((a_lease, b_lease));
    }

    #[test]
    fn worker_defers_followup_when_a_sibling_fills_its_only_queue_slot() {
        let ops = Arc::new(QueueFullOps::default());
        let a = VaultName::new("a").unwrap();
        let b = VaultName::new("b").unwrap();
        let registry = ServingRegistry::from_entries([
            RegistryEntry::new(a.clone(), VaultRoot::new("/tmp/norn-host-queue-a").unwrap()),
            RegistryEntry::new(b.clone(), VaultRoot::new("/tmp/norn-host-queue-b").unwrap()),
        ])
        .unwrap();
        let host = Host::new(
            registry,
            Arc::clone(&ops),
            LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();
        let a_lease = host.demand(&a).unwrap();
        wait_for_flag("a_started", &ops.a_started);
        let b_lease = host.demand(&b).unwrap();
        ops.release_a.store(true, Ordering::SeqCst);
        wait_for_state(&host, &a, TrustState::Ready);
        wait_for_state(&host, &b, TrustState::Ready);
        drop((a_lease, b_lease));
    }

    #[test]
    fn public_demand_returns_warming_when_the_bounded_queue_is_full() {
        let ops = Arc::new(QueueFullOps::default());
        let names = ["a", "b", "c"].map(|name| VaultName::new(name).unwrap());
        let registry = ServingRegistry::from_entries(names.iter().map(|name| {
            RegistryEntry::new(
                name.clone(),
                VaultRoot::new(format!("/tmp/norn-host-full-queue-{name}")).unwrap(),
            )
        }))
        .unwrap();
        let host = Host::new(
            registry,
            Arc::clone(&ops),
            LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();
        let a = host.demand(&names[0]).unwrap();
        wait_for_flag("a_started", &ops.a_started);
        let b = host.demand(&names[1]).unwrap();
        let started = Instant::now();
        let c = host.demand(&names[2]).unwrap();
        assert!(started.elapsed() < Duration::from_millis(50));
        assert!(matches!(
            c.outcome(),
            Demand::State(TrustState::Warming { .. })
        ));
        ops.release_a.store(true, Ordering::SeqCst);
        wait_for_state(&host, &names[2], TrustState::Ready);
        drop((a, b, c));
    }

    #[test]
    fn failed_send_releases_the_marker_after_a_newer_epoch_is_installed() {
        let ops = Arc::new(QueueFullOps::default());
        let names = ["a", "b", "c"].map(|name| VaultName::new(name).unwrap());
        let registry = ServingRegistry::from_entries(names.iter().map(|name| {
            RegistryEntry::new(
                name.clone(),
                VaultRoot::new(format!("/tmp/norn-host-send-race-{name}")).unwrap(),
            )
        }))
        .unwrap();
        let mut host = Host::new(
            registry,
            Arc::clone(&ops),
            LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();
        host.dispatcher_stop.send(()).unwrap();
        host.dispatcher.take().unwrap().join().unwrap();
        let a = host.demand(&names[0]).unwrap();
        wait_for_flag("a_started", &ops.a_started);
        let b = host.demand(&names[1]).unwrap();

        let entry = Arc::clone(host.shared.entries.get(&names[2]).unwrap());
        let jobs_guard = host.shared.jobs.lock().unwrap();
        {
            let mut state = entry.gate.lock().unwrap();
            state.epoch = 1;
            state.runnable = true;
            state.pending_dispatch = Some(Job::Attach(names[2].clone(), 1));
        }
        let shared = Arc::clone(&host.shared);
        let dispatched_entry = Arc::clone(&entry);
        let dispatch = thread::spawn(move || dispatch_pending(&shared, &dispatched_entry));
        wait_until(
            "the dispatch to take the queued marker",
            lifecycle_wait_budget(),
            || {
                if entry.gate.lock().unwrap().queued {
                    Observed::Met(())
                } else {
                    Observed::pending("the marker is not taken")
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        {
            let mut state = entry.gate.lock().unwrap();
            state.epoch = 2;
            state.pending_dispatch = Some(Job::Attach(names[2].clone(), 2));
        }
        drop(jobs_guard);
        dispatch.join().unwrap().unwrap();

        let state = entry.gate.lock().unwrap();
        assert!(!state.queued);
        assert_eq!(state.pending_dispatch.as_ref().map(Job::epoch), Some(2));
        drop(state);
        ops.release_a.store(true, Ordering::SeqCst);
        drop((a, b));
    }

    #[cfg(unix)]
    #[test]
    fn async_attach_rechecks_aliases_after_a_registry_root_is_retargeted() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!(
            "norn-host-alias-race-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let a_root = base.join("a");
        let b_root = base.join("b");
        std::fs::create_dir_all(&a_root).unwrap();
        std::fs::create_dir_all(&b_root).unwrap();
        let ops = Arc::new(QueueFullOps::default());
        let a = VaultName::new("a").unwrap();
        let b = VaultName::new("b").unwrap();
        let registry = ServingRegistry::from_entries([
            RegistryEntry::new(a.clone(), VaultRoot::new(&a_root).unwrap()),
            RegistryEntry::new(b.clone(), VaultRoot::new(&b_root).unwrap()),
        ])
        .unwrap();
        let host = Host::new(
            registry,
            Arc::clone(&ops),
            LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots: 2,
                watch_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();
        let a_lease = host.demand(&a).unwrap();
        wait_for_flag("a_started", &ops.a_started);
        let b_lease = host.demand(&b).unwrap();
        std::fs::remove_dir(&b_root).unwrap();
        symlink(&a_root, &b_root).unwrap();
        ops.release_a.store(true, Ordering::SeqCst);
        wait_until(
            "b's demand to report the alias conflict",
            lifecycle_wait_budget(),
            || {
                let completion = b_lease.completion();
                if matches!(completion, Demand::DuplicateRoot(_)) {
                    Observed::Met(())
                } else {
                    Observed::pending(format!("{completion:?}"))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        assert!(matches!(b_lease.completion(), Demand::DuplicateRoot(_)));
        assert!(matches!(a_lease.completion(), Demand::DuplicateRoot(_)));
        drop((a_lease, b_lease, host));
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn live_retarget_refuses_and_detaches_every_attached_alias_during_poll() {
        use std::os::unix::fs::symlink;

        let base = std::env::temp_dir().join(format!(
            "norn-host-live-retarget-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let a_root = base.join("a");
        let b_root = base.join("b");
        std::fs::create_dir_all(&a_root).unwrap();
        std::fs::create_dir_all(&b_root).unwrap();
        let ops = Arc::new(FakeOps::default());
        let a = VaultName::new("a").unwrap();
        let b = VaultName::new("b").unwrap();
        let registry = ServingRegistry::from_entries([
            RegistryEntry::new(a.clone(), VaultRoot::new(&a_root).unwrap()),
            RegistryEntry::new(b.clone(), VaultRoot::new(&b_root).unwrap()),
        ])
        .unwrap();
        let host = Host::new(
            registry,
            Arc::clone(&ops),
            LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots: 2,
                watch_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();
        let a_lease = host.demand(&a).unwrap();
        let b_lease = host.demand(&b).unwrap();
        wait_for_state(&host, &a, TrustState::Ready);
        wait_for_state(&host, &b, TrustState::Ready);

        ops.block_poll.store(true, Ordering::SeqCst);
        wait_for_flag("poll_started", &ops.poll_started);
        std::fs::remove_dir(&b_root).unwrap();
        symlink(&a_root, &b_root).unwrap();
        let refused = host.demand(&b).unwrap();
        assert!(matches!(refused.outcome(), Demand::DuplicateRoot(_)));
        assert!(matches!(a_lease.completion(), Demand::DuplicateRoot(_)));
        assert!(matches!(b_lease.completion(), Demand::DuplicateRoot(_)));
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);

        ops.poll_release.store(true, Ordering::SeqCst);
        wait_for_detaches(&ops, 2, "both aliases to detach");
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 2);
        assert!(matches!(a_lease.completion(), Demand::DuplicateRoot(_)));
        assert!(matches!(b_lease.completion(), Demand::DuplicateRoot(_)));
        drop((refused, a_lease, b_lease, host));
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn identity_recheck_refusal_invalidates_and_detaches_a_live_entry() {
        use std::os::unix::fs::symlink;

        let base = std::env::temp_dir().join(format!(
            "norn-host-identity-refusal-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = base.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let ops = Arc::new(FakeOps::default());
        let name = VaultName::new("notes").unwrap();
        let registry = ServingRegistry::from_entries([RegistryEntry::new(
            name.clone(),
            VaultRoot::new(&root).unwrap(),
        )])
        .unwrap();
        let host = Host::new(
            registry,
            Arc::clone(&ops),
            LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        ops.block_poll.store(true, Ordering::SeqCst);
        wait_for_flag("poll_started", &ops.poll_started);

        std::fs::remove_dir(&root).unwrap();
        symlink("root", &root).unwrap();
        let demand = host.demand(&name).unwrap();
        assert!(matches!(
            demand.completion(),
            Demand::State(ref state) if refuses_environmentally(Some(state))
        ));
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        ops.poll_release.store(true, Ordering::SeqCst);
        wait_for_detaches(&ops, 1, "the refused alias to detach");
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        settle();
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        assert!(refuses_environmentally(host.state(&name).as_ref()));
        drop((demand, host));
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn identity_refusal_does_not_poison_demand_for_an_unrelated_live_entry() {
        use std::os::unix::fs::symlink;

        let base = std::env::temp_dir().join(format!(
            "norn-host-identity-refusal-isolation-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let healthy_root = base.join("healthy");
        let refused_root = base.join("refused");
        std::fs::create_dir_all(&healthy_root).unwrap();
        std::fs::create_dir_all(&refused_root).unwrap();
        let ops = Arc::new(FakeOps::default());
        let healthy = VaultName::new("healthy").unwrap();
        let refused = VaultName::new("refused").unwrap();
        let registry = ServingRegistry::from_entries([
            RegistryEntry::new(healthy.clone(), VaultRoot::new(&healthy_root).unwrap()),
            RegistryEntry::new(refused.clone(), VaultRoot::new(&refused_root).unwrap()),
        ])
        .unwrap();
        let host = Host::new(
            registry,
            Arc::clone(&ops),
            LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots: 2,
                watch_poll_interval: Duration::from_secs(60),
            },
        )
        .unwrap();
        let healthy_lease = host.demand(&healthy).unwrap();
        let refused_lease = host.demand(&refused).unwrap();
        wait_for_state(&host, &healthy, TrustState::Ready);
        wait_for_state(&host, &refused, TrustState::Ready);
        std::fs::remove_dir(&refused_root).unwrap();
        symlink("refused", &refused_root).unwrap();

        let renewed_healthy = host.demand(&healthy).unwrap();
        assert_eq!(
            renewed_healthy.completion(),
            Demand::State(TrustState::Ready)
        );
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);

        let refused_again = host.demand(&refused).unwrap();
        assert!(matches!(
            refused_again.completion(),
            Demand::State(ref state) if refuses_environmentally(Some(state))
        ));
        assert_eq!(host.state(&healthy), Some(TrustState::Ready));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);

        drop((
            refused_again,
            renewed_healthy,
            refused_lease,
            healthy_lease,
            host,
        ));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn dispatcher_closes_ready_before_reconciling_a_polled_batch() {
        let ops = Arc::new(PollingOps::default());
        let name = VaultName::new("notes").unwrap();
        let registry = ServingRegistry::from_entries([RegistryEntry::new(
            name.clone(),
            VaultRoot::new("/tmp/norn-host-poll-fixture").unwrap(),
        )])
        .unwrap();
        let host = Host::new(
            registry,
            Arc::clone(&ops),
            LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();
        let _ = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        ops.hold_reconcile.store(true, Ordering::SeqCst);
        ops.emit.store(true, Ordering::SeqCst);
        wait_for_flag("reconcile_started", &ops.reconcile_started);
        wait_until(
            "warming progress to advance past zero",
            lifecycle_wait_budget(),
            || match host.state(&name) {
                Some(TrustState::Warming { healed, .. }) if healed > 0 => Observed::Met(()),
                state => Observed::pending(format!("the state is {state:?}")),
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
    }

    #[test]
    fn idle_expiry_during_safety_pin_detaches_when_work_releases_it() {
        let ops = Arc::new(PollingOps::default());
        let name = VaultName::new("notes").unwrap();
        let registry = ServingRegistry::from_entries([RegistryEntry::new(
            name.clone(),
            VaultRoot::new("/tmp/norn-host-pinned-idle").unwrap(),
        )])
        .unwrap();
        let host = Host::new(
            registry,
            Arc::clone(&ops),
            LifecyclePolicy {
                idle_after: Duration::ZERO,
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        // The reconcile is held open across the reap below, which is the whole
        // point of the pin: idle expiry lands on an entry with work in flight.
        ops.hold_reconcile.store(true, Ordering::SeqCst);
        ops.emit.store(true, Ordering::SeqCst);
        wait_for_flag("reconcile_started", &ops.reconcile_started);
        wait_until(
            "the polled batch to open a warming leg",
            lifecycle_wait_budget(),
            || match host.state(&name) {
                Some(TrustState::Warming { .. }) => Observed::Met(()),
                state => Observed::pending(format!("the state is {state:?}")),
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        drop(lease);
        host.reap_idle(Instant::now()).unwrap();
        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Unattached);
    }

    #[test]
    fn quiet_attached_entry_eventually_runs_scheduled_maintenance() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);

        ops.maintenance_due.store(true, Ordering::SeqCst);
        wait_until(
            "scheduled maintenance to run once",
            lifecycle_wait_budget(),
            || {
                let maintenances = ops.maintenances.load(Ordering::SeqCst);
                if maintenances == 1 {
                    Observed::Met(())
                } else {
                    Observed::pending(format!("{maintenances} maintenances so far"))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        assert_eq!(ops.maintenances.load(Ordering::SeqCst), 1);
        assert_eq!(host.state(&name), Some(TrustState::Ready));
        drop(lease);
    }

    #[test]
    fn maintenance_handoff_saturation_stays_warming_until_a_followup_drain() {
        let ops = Arc::new(FakeOps::default());
        // Runs without ambient polling: a dispatcher tick coincident with the
        // follow-up reconcile's dispatch claims the attachment out from under
        // it and the reconcile drops itself. This test pins handoff
        // saturation, not that race, so it drives the single poll it needs —
        // the one that finds maintenance due — itself.
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_maintenance.store(true, Ordering::SeqCst);
        ops.maintenance_due.store(true, Ordering::SeqCst);
        poll_watchers(&host.shared);
        wait_for_flag("maintenance_started", &ops.maintenance_started);
        // The maintenance job's own handoff drain, not the batch source a
        // watcher poll spends: see the reconcile-side saturation test above
        // for why a shared counter would leave this ambiguous between the
        // handoff saturating and a poll scheduling a reconcile of its own.
        ops.handoff_poll_batches
            .store(HANDOFF_BATCH_LIMIT, Ordering::SeqCst);
        ops.block_reconcile.store(true, Ordering::SeqCst);
        ops.maintenance_release.store(true, Ordering::SeqCst);

        wait_for_flag("reconcile_started", &ops.reconcile_started);
        assert!(matches!(
            host.state(&name),
            Some(TrustState::Warming { .. })
        ));

        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        drop(lease);
    }

    #[test]
    fn maintenance_handoff_saturation_preserves_untrusted_rescan_state() {
        let ops = Arc::new(FakeOps::default());
        // Runs without ambient polling, and drives the poll that finds
        // maintenance due itself: see the sibling saturation test above for
        // the tick that would otherwise claim the attachment the follow-up
        // reconcile is dispatched against.
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_maintenance.store(true, Ordering::SeqCst);
        ops.maintenance_due.store(true, Ordering::SeqCst);
        poll_watchers(&host.shared);
        wait_for_flag("maintenance_started", &ops.maintenance_started);
        ops.handoff_rescan_poll_batch.store(true, Ordering::SeqCst);
        ops.handoff_poll_batches
            .store(HANDOFF_BATCH_LIMIT - 1, Ordering::SeqCst);
        ops.block_reconcile.store(true, Ordering::SeqCst);
        ops.maintenance_release.store(true, Ordering::SeqCst);

        wait_for_flag("reconcile_started", &ops.reconcile_started);
        assert_eq!(
            host.state(&name),
            Some(TrustState::untrusted(UntrustedReason::WatcherOverflow))
        );

        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        drop(lease);
    }

    #[test]
    fn blocked_maintenance_does_not_stall_other_vault_polling_or_reaping() {
        let ops = Arc::new(FakeOps::default());
        let a = VaultName::new("a").unwrap();
        let b = VaultName::new("b").unwrap();
        let registry = ServingRegistry::from_entries([
            RegistryEntry::new(a.clone(), VaultRoot::new("/tmp/norn-host-maint-a").unwrap()),
            RegistryEntry::new(b.clone(), VaultRoot::new("/tmp/norn-host-maint-b").unwrap()),
        ])
        .unwrap();
        let host = Host::new(
            registry,
            Arc::clone(&ops),
            LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots: 2,
                watch_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();
        let lease_a = host.demand(&a).unwrap();
        let lease_b = host.demand(&b).unwrap();
        wait_for_state(&host, &a, TrustState::Ready);
        wait_for_state(&host, &b, TrustState::Ready);

        ops.block_maintenance.store(true, Ordering::SeqCst);
        ops.maintenance_due.store(true, Ordering::SeqCst);
        wait_for_flag("maintenance_started", &ops.maintenance_started);
        let polls_before = *ops
            .polls
            .lock()
            .unwrap()
            .get(&b)
            .expect("vault b was polled");
        wait_until(
            "vault b to be polled again",
            lifecycle_wait_budget(),
            || {
                let polls = ops.polls.lock().unwrap().get(&b).copied().unwrap_or(0);
                if polls > polls_before {
                    Observed::Met(())
                } else {
                    Observed::pending(format!("{polls} polls so far"))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        assert!(ops.polls.lock().unwrap().get(&b).copied().unwrap_or(0) > polls_before);

        drop(lease_b);
        host.reap_idle(Instant::now() + Duration::from_secs(61))
            .unwrap();
        wait_for_state(&host, &b, TrustState::Unattached);

        ops.maintenance_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &a, TrustState::Ready);
        drop(lease_a);
    }
}
