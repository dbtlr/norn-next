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

mod claim;

use claim::{Claim, Coverage, Leg};

/// Lifecycle timing chosen by the composition root. There is intentionally no
/// ambient or library default.
#[derive(Clone, Copy, Debug)]
pub struct LifecyclePolicy {
    pub idle_after: Duration,
    pub worker_slots: usize,
    /// Cadence of the one host-wide nonblocking watcher scan.
    pub watch_poll_interval: Duration,
}

/// Work coalesced behind an entry's capacity-one scheduling marker.
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
        if !state.claim.stands_at(self.epoch) {
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
///
/// The three park variants are what an entry nothing re-attaches answers, and
/// they answer it whatever trust state stands beside them: a park outlives the
/// release that publishes Unattached over it, so the answer names what keeps
/// the entry from re-arming rather than what its resources are doing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Demand {
    State(TrustState),
    MaintainerContended(MaintainerIdentity),
    DuplicateRoot(AliasConflict),
    /// The registry's own account of a root it cannot read.
    IdentityRefused(String),
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
    /// The entry's coverage, and who holds it.
    coverage: Coverage<A>,
    pending: Batch,
    recovery_required: bool,
    /// The live demand leases asking for the recovery the entry currently owes.
    recovery_demands: usize,
    /// Which recovery requirement the demands above were raised against. A
    /// requirement that replaces another retires its demands by moving on from
    /// their generation, so a lease that asked for an earlier recovery neither
    /// satisfies this one nor discounts a lease that did ask for it.
    recovery_generation: u64,
    /// The registry's account of a root it cannot read, from the recheck that
    /// refused it. While it is set the entry is parked, and the detail is what
    /// the park answers with.
    identity_refused: Option<String>,
    /// The entry's hold on itself: the epoch its work stands at, what holds its
    /// scheduling gate, the leg running against it, and the job holding its one
    /// queue slot.
    claim: Claim,
    /// The incumbent another process reported while holding this vault's
    /// maintainer lock. While it is set the entry is parked, and only
    /// [`Host::retry`] clears it.
    maintainer_contended: Option<MaintainerIdentity>,
    /// The conflict a recheck found over this entry's root. While it is set the
    /// entry is parked, and a recheck that passes clears it.
    duplicate_root: Option<AliasConflict>,
    last_demand: Instant,
    demand_leases: usize,
    /// The legs running against the entry that are between the lock which gave
    /// them its coverage and the lock which ends them.
    ///
    /// This is what says the entry's coverage is coming back, which is a
    /// narrower fact than [`EntryState::coverage`] answering who holds it: a
    /// leg past its own end holds coverage no lock of its own will give back —
    /// the poll giving a stale attachment to the ops, a release on its way to
    /// [`finish_release`] — and a pin stands for neither. What reads a pin is
    /// therefore work deciding whether to wait, and what reads the coverage is
    /// work deciding whether it may be taken.
    safety_pins: usize,
    detach_due: bool,
    detach_scheduled: bool,
    detach_in_flight: bool,
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

    /// Pin the entry for a leg that is about to run outside its lock. The leg
    /// comes back to a lock of its own, so what it holds is coming back with
    /// it.
    fn pin(&mut self) {
        self.safety_pins += 1;
    }

    /// Give back the pin a leg took. A leg answers for what it holds from here.
    fn unpin(&mut self) {
        self.safety_pins = self.safety_pins.saturating_sub(1);
    }

    /// Whether a leg is running against the entry that comes back to a lock of
    /// its own.
    fn pinned(&self) -> bool {
        self.safety_pins > 0
    }

    /// What the entry is parked on, where it is parked.
    ///
    /// A park is the entry standing still: nothing schedules against it,
    /// nothing re-attaches, and no release re-arms while one stands. Every path
    /// that asks whether the entry may be worked, and every answer a lease
    /// reports, reads it here, so the demand that admits a lease and the
    /// release that honors it cannot disagree about whether the entry is
    /// parked.
    ///
    /// The order below is the precedence: a maintainer another process holds
    /// outranks a root reached under more than one name, which outranks a root
    /// the registry cannot read. Each is a fact about a wider thing than the
    /// one after it, and a wider fact is one the narrower read cannot answer:
    /// a root the registry cannot read is a root it cannot classify either, so
    /// a conflict raised over that root is unanswered rather than retired and
    /// keeps its place above the refusal.
    fn parked(&self) -> Option<Demand> {
        self.maintainer_contended
            .clone()
            .map(Demand::MaintainerContended)
            .or_else(|| self.duplicate_root.clone().map(Demand::DuplicateRoot))
            .or_else(|| self.identity_refused.clone().map(Demand::IdentityRefused))
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

    fn name(&self) -> &VaultName {
        match self {
            Self::Attach(name, _)
            | Self::Recover(name, _)
            | Self::Reconcile(name, _)
            | Self::Maintenance(name, _)
            | Self::Detach(name, _) => name,
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
        && state.coverage.in_hand()
        && state.demand_leases == 0
        && !state.pinned()
        && !state.claim.is_held()
    {
        state.detach_scheduled = true;
        Some(
            state
                .claim
                .schedule(|epoch| Job::Detach(name.clone(), epoch)),
        )
    } else {
        None
    }
}

/// Schedule the work an outstanding demand lease is waiting on, where the entry
/// is free to run it.
///
/// A claim holds the entry's gate for as long as it lasts, so a demand raised
/// against a claimed entry records its lease and schedules nothing. Every path
/// that ends a claim answers those leases here, which is what makes the claim's
/// own completion the moment the demand is honored rather than some later one.
/// The attachment in hand picks the job exactly as [`Host::demand`] does:
/// coverage still held is recovered, coverage that is gone is attached again.
///
/// An entry that is serving, already working, giving its resources back,
/// parked, or owed a recovery no live lease has demanded owes an outstanding
/// lease nothing. A recovery runs only where a lease has demanded it, because a
/// terminal failure does not autonomously restart coverage. A release in flight
/// owes the lease the re-attach [`finish_release`] ends with, which is why
/// nothing is scheduled here against one.
fn schedule_demanded_work<A>(state: &mut EntryState<A>, name: &VaultName) -> Option<Job> {
    if state.demand_leases == 0
        || !matches!(
            state.trust,
            TrustState::Unattached | TrustState::Untrusted { .. }
        )
        || state.claim.is_held()
        || state.detach_in_flight
        || (state.recovery_required && !state.recovery_demanded())
        || state.parked().is_some()
    {
        return None;
    }
    // The job below is an attach or a recover, and both establish coverage
    // before a document is read.
    state.trust = TrustState::warming(WarmingPhase::InstallingCoverage, 0, None);
    let attached = state.coverage.in_hand();
    Some(state.claim.schedule(|epoch| {
        if attached {
            Job::Recover(name.clone(), epoch)
        } else {
            Job::Attach(name.clone(), epoch)
        }
    }))
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
    // A job that lost the attachment to this leg left its marker behind for a
    // later tick, and the resources it was scheduled against are going back:
    // the marker ends with the gate, because a job nothing holds the entry for
    // is one no dispatch reaches.
    state.claim.open();
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
/// itself, because the release is what the lease was waiting behind. A parked
/// entry owes it nothing, and neither does one owing a recovery no live lease
/// asked for. The park outlives the publication below: Unattached says the
/// resources are back, and the lease still reports the park that is why nothing
/// follows it. The requirement is read before it is cleared: a lease that asked
/// for a recovery is asking for the coverage the re-attach below installs.
///
/// The leg's claim on the entry ends where its release does, so the epilogue
/// that would otherwise end it later finds nothing left to end: a re-attach
/// dispatched below is the entry's own work, running against a claim it holds
/// rather than one a finished leg is still entitled to take away.
///
/// The follow-up is sent from here and left nowhere else. A marker beside a job
/// this leg has already sent is one a dispatcher tick would send a second time,
/// under the same epoch.
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
    state.claim.end_leg(epoch);
    // The coverage this leg was handed is with the ops now, so the entry holds
    // none: what a leg parked back before it reached here is the entry's own
    // and stands.
    state.coverage.released_by(epoch);
    state.detach_in_flight = false;
    state.pending = Batch::default();
    state.clear_recovery();
    // A job that lost the attachment to this leg left its marker behind for a
    // later tick, and the resources it was scheduled against have gone back:
    // the marker ends with the coverage, because a job nothing holds the entry
    // for is one no dispatch reaches.
    state.claim.open();
    state.detach_due = false;
    state.detach_scheduled = false;
    state.trust = TrustState::Unattached;
    if state.demand_leases > 0 && reattach_requested && state.parked().is_none() {
        state.trust = TrustState::warming(WarmingPhase::InstallingCoverage, 0, None);
        let next = state
            .claim
            .hand_on(|epoch| Job::Attach(name.clone(), epoch));
        // The whole re-arm lands in the critical section that publishes the
        // state it warms into: the trust write, the entry's move on to this
        // job, and the slot naming it are one step, so nothing observes a
        // warming entry with no work coming, and no producer takes the slot
        // for a newer job in between. The send below gives it back where it
        // does not happen.
        state.claim.take_slot(next.epoch());
        drop(state);
        dispatch_followup(shared, next);
    }
}

/// Answer for a scheduled job that found the attachment taken at its own
/// epoch.
///
/// The entry still stands at this job's epoch, so nothing has moved on from the
/// work the job carries, and a pin standing beside a taken attachment says a
/// claim is holding it at that same epoch. Such a claim gives the attachment
/// back where it ends, so the job returns to its marker for a later dispatcher
/// tick rather than being dropped, and the marker keeps the gate held because
/// the work is still scheduled against the entry.
///
/// Where no pin stands, no claim is holding the attachment and none will give
/// one back, so the job ends here rather than waiting for coverage that is not
/// coming.
///
/// The marker planted here is the one a claim did not plant itself, which is
/// why [`Claim::end_poll`] leaves it standing: a claim that opened the gate
/// over it would leave a job no dispatch can reach.
fn restore_lost_claim<A>(state: &mut EntryState<A>, job: Job) {
    if !state.pinned() {
        state.claim.open();
        return;
    }
    state.claim.restore(job);
}

/// Give back the queue slot a send took, where the entry still holds it for
/// that send.
fn release_queue_slot<A>(entry: &Arc<Entry<A>>, epoch: u64) {
    let mut state = entry.gate.lock().expect("entry gate poisoned");
    state.claim.free_slot(epoch);
}

/// Send the job an entry has scheduled, where the entry has no job in the
/// channel and none in flight.
///
/// The slot is taken under the same lock that reads the marker, so the job in
/// the channel and the slot naming it are installed together, and given back
/// together where a full queue refuses the send.
fn dispatch_pending<O: EntryOps>(
    shared: &Arc<Shared<O>>,
    entry: &Arc<Entry<O::Attachment>>,
) -> Result<(), HostError> {
    let job = {
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        let Some(job) = state.claim.take_slot_for_marked() else {
            return Ok(());
        };
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
            release_queue_slot(entry, job.epoch());
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
        state.claim.invalidate();
        state.pending = Batch::default();
        state.clear_recovery();
        state.identity_refused = None;
        state.duplicate_root = Some(conflict.clone());
        if state.claim.leg().is_none() && !state.detach_in_flight {
            state.claim.open();
            let epoch = state.claim.epoch();
            match state.coverage.take(epoch) {
                // The refusal reaches an idle entry holding the coverage the
                // conflict invalidates, so this is a teardown like any other:
                // the release below is what holds it from here, the resources
                // go back, and Unattached is published after they have.
                Some(attachment) => {
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

/// Park an entry on the registry's own account of a root it cannot read, and
/// give back whatever coverage the entry is still holding.
///
/// A conflict park standing over the entry stands through this one. The read
/// that reaches here classified nothing — it failed on this root before it
/// could say what any root resolves to — so it neither confirms nor contradicts
/// a conflict, and [`EntryState::parked`] ranks the conflict first for exactly
/// that reason. A read that can reach the root again is what retires it.
fn refuse_identity_error<O: EntryOps>(shared: &Arc<Shared<O>>, name: &VaultName, detail: String) {
    let Some(entry) = shared.entries.get(name) else {
        return;
    };
    let attachment = {
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        state.claim.invalidate();
        state.pending.merge(Batch::rescan(RescanScope::Vault));
        state.require_recovery();
        state.identity_refused = Some(detail.clone());
        state.trust = TrustState::untrusted(UntrustedReason::environmental_refusal(detail));
        if state.claim.leg().is_none() && !state.detach_in_flight {
            state.claim.open();
            state.coverage.give_up()
        } else {
            None
        }
    };
    if let Some(attachment) = attachment {
        shared.ops.detach(name, attachment);
    }
}

/// Re-read the registry over one entry and park it on whatever the read
/// refuses.
///
/// A recheck that passes is the answer to both of the parks a recheck can
/// raise: it read this root and classified it against every other registered
/// root, so neither the refusal nor the conflict it once raised still stands.
/// Clearing both here is what makes the demand this recheck admits a demand the
/// paths behind it honor — a park left standing over a passing recheck is an
/// entry admitted at one predicate and refused at the next.
///
/// Maintainer contention is untouched: the registry says nothing about another
/// process's lock, so nothing read here can retire it.
///
/// The park this leaves behind is the whole of the answer, so nothing is
/// reported back: the caller reads [`EntryState::parked`] like every other
/// reader does, and one park is answered by one predicate.
fn recheck_and_refuse<O: EntryOps>(shared: &Arc<Shared<O>>, name: &VaultName) {
    if let Ok(None) = shared.registry.recheck(name) {
        clear_registry_parks(shared, name);
        return;
    }
    let _attach_guard = shared.attach_gate.lock().expect("attach gate poisoned");
    let conflict = match shared.registry.recheck(name) {
        Ok(conflict) => conflict,
        Err(refusal) => {
            refuse_identity_error(shared, name, refusal.to_string());
            return;
        }
    };
    if let Some(conflict) = &conflict {
        refuse_conflict(shared, conflict);
    } else {
        clear_registry_parks(shared, name);
    }
}

/// Retire the parks a passing registry recheck has answered for.
fn clear_registry_parks<O: EntryOps>(shared: &Arc<Shared<O>>, name: &VaultName) {
    let Some(entry) = shared.entries.get(name) else {
        return;
    };
    let mut state = entry.gate.lock().expect("entry gate poisoned");
    state.identity_refused = None;
    state.duplicate_root = None;
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
    ///
    /// A parked entry answers the park, and only an entry nothing parks answers
    /// its trust state: the park is what says whether anything more is coming,
    /// and a trust state published over one says nothing about that.
    pub fn completion(&self) -> Demand {
        let Some((shared, name)) = &self.held else {
            return self.outcome.clone();
        };
        let Some(entry) = shared.entries.get(name) else {
            return Demand::UnknownVault;
        };
        let state = entry.gate.lock().expect("entry gate poisoned");
        state
            .parked()
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
            state.claim.invalidate();
            state.claim.open();
            state.pending = Batch::default();
            if state.claim.leg().is_none() && !state.detach_in_flight {
                match state.coverage.give_up() {
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
                    .coverage
                    .give_up()
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
                            coverage: Coverage::none(),
                            pending: Batch::default(),
                            recovery_required: false,
                            recovery_demands: 0,
                            recovery_generation: 0,
                            identity_refused: None,
                            claim: Claim::default(),
                            maintainer_contended: None,
                            duplicate_root: None,
                            last_demand: now,
                            demand_leases: 0,
                            safety_pins: 0,
                            detach_due: false,
                            detach_scheduled: false,
                            detach_in_flight: false,
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
    ///
    /// The recheck below runs first and parks the entry on whatever it refuses,
    /// so the park read under the entry lock is the one this call's own read
    /// established. Every demand takes the same route to its answer: the lease
    /// is recorded, and the park — this call's or an older one's — is what it
    /// reports.
    pub fn demand(&self, name: &VaultName) -> Result<DemandLease<O>, HostError> {
        let Some(entry) = self.shared.entries.get(name) else {
            return Ok(DemandLease {
                outcome: Demand::UnknownVault,
                held: None,
                recovery_demand: None,
            });
        };
        recheck_and_refuse(&self.shared, name);
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        state.demand_leases += 1;
        let recovery_demand = state.demand_recovery();
        state.detach_due = false;
        if state.detach_scheduled && !state.detach_in_flight {
            state.claim.invalidate();
            state.claim.open();
            state.detach_scheduled = false;
        }
        // A parked entry is one nothing re-attaches, so the lease is recorded
        // and answered with the park itself rather than with a trust state that
        // says nothing about why no work follows it.
        if let Some(park) = state.parked() {
            drop(state);
            return Ok(DemandLease {
                outcome: park,
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
        ) && !state.claim.is_held()
            && !state.detach_in_flight;
        if schedule {
            // The job below is an attach or a recover, and both establish
            // coverage before a document is read.
            state.trust = TrustState::warming(WarmingPhase::InstallingCoverage, 0, None);
            let attached = state.coverage.in_hand();
            state.claim.schedule(|epoch| {
                if attached {
                    Job::Recover(name.clone(), epoch)
                } else {
                    Job::Attach(name.clone(), epoch)
                }
            });
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

    /// Explicitly retry a demand whose prior completion reported a park.
    ///
    /// Contention is the park nothing else retires: no read this host performs
    /// says whether another process still holds the vault's maintainer lock, so
    /// a caller asking again is the whole of the evidence that it may be tried.
    /// The parks the registry raises are left to the recheck the demand below
    /// runs, which is the read that adjudicates them.
    pub fn retry(&self, name: &VaultName) -> Result<DemandLease<O>, HostError> {
        if let Some(entry) = self.shared.entries.get(name) {
            entry
                .gate
                .lock()
                .expect("entry gate poisoned")
                .maintainer_contended = None;
        }
        self.demand(name)
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
            && (state.coverage.in_hand() || state.pinned())
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
            if state.claim.is_held() {
                continue;
            }
            // The same shape the post-poll arm below stands for, read before
            // the entry is claimed, and unreached for the same reason: nothing
            // leaves facts standing in an unclaimed entry with no recovery owed
            // and nothing scheduled. The guard is live — an entry owing a
            // recovery is passed through to the poll rather than reconciled.
            if !state.pending.is_empty() && state.coverage.in_hand() && !state.recovery_required {
                state
                    .claim
                    .schedule(|epoch| Job::Reconcile(name.clone(), epoch));
                drop(state);
                let _ = dispatch_pending(shared, entry);
                continue;
            }
            let epoch = state.claim.epoch();
            let Some(attachment) = state.coverage.take(epoch) else {
                continue;
            };
            state.pin();
            state.claim.begin_poll(epoch);
            (attachment, epoch)
        };
        let result = shared.ops.poll(name, &mut attachment);
        let maintenance_due = result.is_ok() && shared.ops.maintenance_due(name, &attachment);
        let mut schedule = None;
        let mut stale = None;
        let mut release = None;
        {
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            state.unpin();
            if !state.claim.stands_at(epoch) {
                stale = Some(attachment);
            } else {
                match result {
                    Ok(None) => {
                        state.claim.end_poll(epoch);
                        state.coverage.park_by(epoch, attachment);
                        if maintenance_due && !state.recovery_required {
                            schedule = Some(
                                state
                                    .claim
                                    .schedule(|epoch| Job::Maintenance(name.clone(), epoch)),
                            );
                        } else if !state.pending.is_empty() && !state.recovery_required {
                            // Facts standing in the entry with nothing
                            // scheduled against them. No writer of
                            // `state.pending` leaves the entry in that shape:
                            // each one either schedules the follow-up under the
                            // same lock, sets `recovery_required` beside the
                            // facts, or clears them outright, so this arm
                            // reaches nothing today. It stands for the entry
                            // this poll holds — the poll would be what
                            // schedules the reconcile such facts are owed, with
                            // maintenance above carrying them instead where
                            // both are due, because its own handoff ends in the
                            // same reconcile. The guard is what is live: an
                            // entry owing a recovery schedules neither arm,
                            // because what reconciles facts is coverage, and
                            // the recovery a demand asks for is what installs
                            // it again.
                            schedule = Some(
                                state
                                    .claim
                                    .schedule(|epoch| Job::Reconcile(name.clone(), epoch)),
                            );
                        }
                    }
                    Ok(Some(batch)) => {
                        state.claim.end_poll(epoch);
                        let rescan = !batch.rescans().is_empty();
                        state.pending.merge(batch);
                        if !state.recovery_required {
                            state.trust = if rescan {
                                TrustState::untrusted(UntrustedReason::WatcherOverflow)
                            } else {
                                TrustState::warming(WarmingPhase::Healing, 0, None)
                            };
                        }
                        state.coverage.park_by(epoch, attachment);
                        if !state.recovery_required {
                            schedule = Some(
                                state
                                    .claim
                                    .schedule(|epoch| Job::Reconcile(name.clone(), epoch)),
                            );
                        }
                    }
                    Err(JobFailure::LostMaintainership) => {
                        state.claim.supersede();
                        begin_release(&mut state);
                        release = Some(attachment);
                    }
                    Err(JobFailure::MaintainerContended(incumbent)) => {
                        state.maintainer_contended = Some(incumbent);
                        state.claim.supersede();
                        begin_release(&mut state);
                        release = Some(attachment);
                    }
                    // A recovery demand raised inside this claim outlives the
                    // failure the claim reports, so both legs below keep it.
                    // A job that was waiting on this claim does not: the
                    // coverage it would have worked against is gone, and what
                    // installs coverage again is the recovery a demand asks
                    // for.
                    Err(JobFailure::WatcherTerminal(error)) => {
                        state.claim.drop_marker();
                        state.claim.end_poll(epoch);
                        state.require_recovery_keeping_demands();
                        state.pending.merge(Batch::rescan(RescanScope::Vault));
                        state.trust = TrustState::untrusted(watcher_lost(error));
                        state.coverage.park_by(epoch, attachment);
                    }
                    Err(JobFailure::Environmental(detail)) => {
                        state.claim.drop_marker();
                        state.claim.end_poll(epoch);
                        state.require_recovery_keeping_demands();
                        state.pending.merge(Batch::rescan(RescanScope::Vault));
                        state.trust =
                            TrustState::untrusted(UntrustedReason::environmental_refusal(detail));
                        state.coverage.park_by(epoch, attachment);
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
            state.coverage.released_by(epoch);
            if state.claim.leg() == Some(Leg::Poll(epoch)) {
                // A job that lost the attachment to this poll left its marker
                // at the poll's own epoch, and the entry has moved past that
                // epoch: the marker goes back with the claim rather than
                // standing for work whatever moved the entry on superseded.
                if state.claim.marker().map(Job::epoch) == Some(epoch) {
                    state.claim.drop_marker();
                }
                state.claim.end_poll(epoch);
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
    let Some(entry) = shared.entries.get(job.name()) else {
        return;
    };
    let epoch = job.epoch();
    {
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        // The job reached a worker, so the queue slot it took is given back
        // whatever epoch the entry stands at: a slot standing for a job no
        // channel holds is an entry no later dispatch can reach. The slot goes
        // back only where it is this job's own, and so does the marker — both
        // at another epoch belong to the work that superseded this job.
        state.claim.free_slot(epoch);
        if !state.claim.stands_at(epoch) {
            if state.claim.marker().map(Job::epoch) == Some(epoch) {
                state.claim.drop_marker();
            }
            return;
        }
        state.claim.begin_job_leg(epoch);
    }
    run_job_inner(shared, job);
    // Whatever the leg still holds ends here. A claim it gave up itself —
    // handed to the job it dispatched, or ended by the release it ran — is
    // already over, and the epoch it moved on to is that job's, not a
    // supersession to open the entry's gate over. What this epilogue ends is a
    // claim held to the last: an entry whose epoch moved on under a leg was
    // invalidated by something that left the scheduling to the leg's end, so
    // the gate goes back with the claim.
    //
    // Coverage the leg still holds ends with it: a leg gives what it took back
    // to the entry, to a release, or to the ops, so coverage still out with a
    // leg that has ended went to the ops and the entry holds none.
    let mut state = entry.gate.lock().expect("entry gate poisoned");
    if state.claim.end_job_leg(epoch) {
        state.coverage.released_by(epoch);
        if !state.claim.stands_at(epoch) {
            state.claim.release();
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
                    if state.claim.stands_at(epoch) {
                        state.claim.release();
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
                    if state.claim.stands_at(epoch) {
                        state.claim.release();
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
                    if state.claim.stands_at(epoch) {
                        state.claim.release();
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
                    if state.claim.stands_at(epoch) {
                        state.claim.release();
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
                    if state.claim.stands_at(epoch) {
                        state.claim.release();
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
                if state.claim.stands_at(epoch) {
                    state.claim.release();
                    state.duplicate_root = Some(conflict);
                    state.trust = TrustState::Unattached;
                }
                return;
            }
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            if !state.claim.stands_at(epoch) {
                if let Ok((attachment, _, _)) = result {
                    drop(state);
                    shared.ops.detach(&name, attachment);
                }
                return;
            }
            state.claim.release();
            match result {
                Ok((attachment, observed, handoff_saturated)) => {
                    state.pending.merge(observed);
                    state.coverage.install(attachment);
                    state.clear_recovery();
                    state.identity_refused = None;
                    state.maintainer_contended = None;
                    state.duplicate_root = None;
                    if state.pending.is_empty() && !handoff_saturated {
                        state.trust = TrustState::Ready;
                    } else {
                        state.trust = trust_for_pending_reconcile(&state.pending);
                        let next = state
                            .claim
                            .hand_on(|epoch| Job::Reconcile(name.clone(), epoch));
                        drop(state);
                        drop(attach_claims);
                        dispatch_handoff(shared, entry, epoch, next);
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
                if !state.claim.stands_at(epoch) {
                    return;
                }
                state.pin();
                let Some(attachment) = state.coverage.take(epoch) else {
                    state.unpin();
                    restore_lost_claim(&mut state, Job::Recover(name.clone(), epoch));
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
            state.unpin();
            if !state.claim.stands_at(epoch) {
                drop(state);
                shared.ops.detach(&name, attachment);
                return;
            }
            state.claim.release();
            let mut next = None;
            match result {
                Ok(()) => {
                    state.pending.merge(observed);
                    state.coverage.park_by(epoch, attachment);
                    state.clear_recovery();
                    if state.detach_due {
                        next = schedule_due_detach(&mut state, &name);
                    } else if state.pending.is_empty() && !handoff_saturated {
                        state.trust = TrustState::Ready;
                    } else {
                        next = Some(
                            state
                                .claim
                                .hand_on(|epoch| Job::Reconcile(name.clone(), epoch)),
                        );
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
                    state.coverage.park_by(epoch, attachment);
                    state.trust = TrustState::untrusted(watcher_lost(error));
                    next = schedule_due_detach(&mut state, &name);
                }
                Err(JobFailure::Environmental(detail)) => {
                    state.require_recovery();
                    state.pending.merge(Batch::rescan(RescanScope::Vault));
                    state.coverage.park_by(epoch, attachment);
                    state.trust =
                        TrustState::untrusted(UntrustedReason::environmental_refusal(detail));
                    next = schedule_due_detach(&mut state, &name);
                }
            }
            drop(state);
            if let Some(job) = next {
                dispatch_handoff(shared, entry, epoch, job);
            }
        }
        Job::Reconcile(name, epoch) => loop {
            let (mut attachment, work) = {
                let mut state = entry.gate.lock().expect("entry gate poisoned");
                if !state.claim.stands_at(epoch) {
                    return;
                }
                state.pin();
                let Some(attachment) = state.coverage.take(epoch) else {
                    state.unpin();
                    restore_lost_claim(&mut state, Job::Reconcile(name.clone(), epoch));
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
            state.unpin();
            if !state.claim.stands_at(epoch) {
                drop(state);
                shared.ops.detach(&name, attachment);
                return;
            }
            match result {
                Ok(()) => {
                    state.pending.merge(observed);
                    state.coverage.park_by(epoch, attachment);
                    if handoff_saturated || !state.pending.is_empty() {
                        state.trust = trust_for_pending_reconcile(&state.pending);
                    }
                    if state.detach_due {
                        state.claim.release();
                        let next = schedule_due_detach(&mut state, &name);
                        drop(state);
                        if let Some(job) = next {
                            dispatch_handoff(shared, entry, epoch, job);
                        }
                        break;
                    } else if handoff_saturated {
                        let next = state
                            .claim
                            .hand_on(|epoch| Job::Reconcile(name.clone(), epoch));
                        drop(state);
                        dispatch_handoff(shared, entry, epoch, next);
                        break;
                    } else if state.pending.is_empty() {
                        state.claim.release();
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
                    state.coverage.park_by(epoch, attachment);
                    state.claim.release();
                    state.require_recovery();
                    state.pending.merge(Batch::rescan(RescanScope::Vault));
                    state.trust = TrustState::untrusted(watcher_lost(error));
                    let next = schedule_due_detach(&mut state, &name);
                    drop(state);
                    if let Some(job) = next {
                        dispatch_handoff(shared, entry, epoch, job);
                    }
                    break;
                }
                Err(JobFailure::Environmental(detail)) => {
                    state.coverage.park_by(epoch, attachment);
                    state.claim.release();
                    state.require_recovery();
                    state.pending.merge(Batch::rescan(RescanScope::Vault));
                    state.trust =
                        TrustState::untrusted(UntrustedReason::environmental_refusal(detail));
                    let next = schedule_due_detach(&mut state, &name);
                    drop(state);
                    if let Some(job) = next {
                        dispatch_handoff(shared, entry, epoch, job);
                    }
                    break;
                }
            }
        },
        Job::Maintenance(name, epoch) => {
            let mut attachment = {
                let mut state = entry.gate.lock().expect("entry gate poisoned");
                if !state.claim.stands_at(epoch) {
                    return;
                }
                state.pin();
                let Some(attachment) = state.coverage.take(epoch) else {
                    state.unpin();
                    restore_lost_claim(&mut state, Job::Maintenance(name.clone(), epoch));
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
            state.unpin();
            if !state.claim.stands_at(epoch) {
                drop(state);
                shared.ops.detach(&name, attachment);
                return;
            }
            let mut next = None;
            match result {
                Ok(()) => {
                    state.pending.merge(observed);
                    state.coverage.park_by(epoch, attachment);
                    state.claim.release();
                    if state.detach_due {
                        next = schedule_due_detach(&mut state, &name);
                    } else if handoff_saturated || !state.pending.is_empty() {
                        state.trust = trust_for_pending_reconcile(&state.pending);
                        next = Some(
                            state
                                .claim
                                .hand_on(|epoch| Job::Reconcile(name.clone(), epoch)),
                        );
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
                    state.coverage.park_by(epoch, attachment);
                    state.claim.release();
                    state.require_recovery();
                    state.pending.merge(Batch::rescan(RescanScope::Vault));
                    state.trust = TrustState::untrusted(watcher_lost(error));
                    next = schedule_due_detach(&mut state, &name);
                }
                Err(JobFailure::Environmental(detail)) => {
                    state.coverage.park_by(epoch, attachment);
                    state.claim.release();
                    state.require_recovery();
                    state.pending.merge(Batch::rescan(RescanScope::Vault));
                    state.trust =
                        TrustState::untrusted(UntrustedReason::environmental_refusal(detail));
                    next = schedule_due_detach(&mut state, &name);
                }
            }
            drop(state);
            if let Some(job) = next {
                dispatch_handoff(shared, entry, epoch, job);
            }
        }
        Job::Detach(name, epoch) => {
            let attachment = {
                let mut state = entry.gate.lock().expect("entry gate poisoned");
                if !state.claim.stands_at(epoch) {
                    return;
                }
                let attachment = state.coverage.take(epoch);
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

/// Hand the entry the next job it owes, ending this leg's claim on the entry.
///
/// A leg owns the entry until it schedules the entry's next job: what it sends
/// runs against the entry's own claim, so the epilogue in [`run_job`] finds
/// nothing left to end — in particular no gate to open, because the gate now
/// stands for the job in flight rather than for the leg that sent it. A gate
/// opened there is an entry another claim may take, and a job dispatched
/// against an entry taken from under it is work nothing is left holding.
///
/// The job leaves its marker as it goes into the queue. A marker beside
/// a job already sent is one a dispatcher tick sends a second time, under the
/// same epoch; [`dispatch_followup`] puts the marker back where a full queue
/// refuses the send.
///
/// The queue slot is taken under the same lock that ends the claim, so no
/// instant separates the two: a slot taken after that lock is one another
/// producer can take for a newer job first, and the follow-up would then be
/// standing in a slot naming work the channel really holds.
fn dispatch_handoff<O: EntryOps>(
    shared: &Arc<Shared<O>>,
    entry: &Arc<Entry<O::Attachment>>,
    leg: u64,
    job: Job,
) {
    {
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        state.claim.hand_off(leg, &job);
    }
    dispatch_followup(shared, job);
}

/// A worker never waits for room in its own bounded queue. If every queue slot
/// is occupied, its capacity-one follow-up returns to its marker for a later
/// dispatcher tick, after the sibling in the queue has had a chance to run.
///
/// The caller takes the entry's queue slot for this job, under the lock that
/// ends its claim on the entry. What this send does with the slot is give it
/// back — where the channel refuses the job, where the channel is gone, and
/// where the host is shutting down — and it gives back only a slot still
/// naming this job, because a slot at another epoch is that job's own
/// occupancy and an entry whose slot is free is one a tick sends the job in it
/// a second time.
fn dispatch_followup<O: EntryOps>(shared: &Arc<Shared<O>>, job: Job) {
    let epoch = job.epoch();
    let entry = shared.entries.get(job.name());
    if shared.shutting_down.load(Ordering::SeqCst) {
        if let Some(entry) = entry {
            release_queue_slot(entry, epoch);
        }
        return;
    }
    let result = {
        let jobs = shared.jobs.lock().expect("job sender poisoned");
        match jobs.as_ref() {
            Some(jobs) => jobs.try_send(job),
            None => Err(mpsc::TrySendError::Disconnected(job)),
        }
    };
    match result {
        Ok(()) => {}
        Err(mpsc::TrySendError::Full(job)) => {
            if let Some(entry) = entry {
                let mut state = entry.gate.lock().expect("entry gate poisoned");
                state.claim.free_slot(epoch);
                if state.claim.stands_at(epoch) {
                    // The entry still stands at this job, so this job is the
                    // work it owes: it takes the marker back whatever else is
                    // standing there, because anything else was raised under an
                    // epoch the entry has already left.
                    state.claim.mark(job);
                }
            }
        }
        Err(mpsc::TrySendError::Disconnected(_)) => {
            if let Some(entry) = entry {
                release_queue_slot(entry, epoch);
            }
        }
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
        /// This is what gives a watcher poll and a job's own handoff drain
        /// independent batch sources below: setting one without the other no
        /// longer depends on which of the two ticks first.
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
        /// The batch sources for a poll that runs off a job thread, named for
        /// the seam rather than for who drives it: the dispatcher's own
        /// unprompted tick and a tick a case drives itself by calling
        /// [`poll_watchers`] both arrive here. Each batch is spent by one poll
        /// — empty from the first counter, a vault-wide rescan from the second.
        off_thread_poll_batches: AtomicUsize,
        off_thread_rescan_poll_batches: AtomicUsize,
        /// The batch sources for a job's own handoff drain — the poll loop
        /// `drain_observed` runs after `attach`, `reconcile`, `recover` or
        /// `maintain` returns, on the same thread. Separate from the off-thread
        /// pair above so a caller that wants a handoff to saturate does not
        /// race a watcher poll for the same batches; see `ON_JOB_THREAD`.
        handoff_poll_batches: AtomicUsize,
        handoff_rescan_poll_batches: AtomicUsize,
        terminal_poll: Mutex<Option<WatchError>>,
        environmental_poll: std::sync::atomic::AtomicBool,
        contend_poll: std::sync::atomic::AtomicBool,
    }

    /// Take one batch off a source, reporting whether there was one to take.
    fn spend_one(source: &AtomicUsize) -> bool {
        source
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_sub(1)
            })
            .is_ok()
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
            let (empty, rescans) = if on_job_thread {
                (
                    &self.handoff_poll_batches,
                    &self.handoff_rescan_poll_batches,
                )
            } else {
                (
                    &self.off_thread_poll_batches,
                    &self.off_thread_rescan_poll_batches,
                )
            };
            if spend_one(rescans) {
                return Ok(Some(Batch::rescan(RescanScope::Vault)));
            }
            if spend_one(empty) {
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
    /// A caller here drives those duties itself, which is what makes the
    /// interleaving under test the one the test set up rather than the one a
    /// tick arrived at first.
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

    /// The fixture above with vaults beside the one under test, whose only
    /// role is to occupy worker slots: a job blocked on one entry is what
    /// holds another entry's job in the channel long enough for a test to
    /// drive the window around it.
    fn host_without_ambient_polling(
        ops: Arc<FakeOps>,
        names: &[&VaultName],
        worker_slots: usize,
    ) -> Host<Arc<FakeOps>> {
        let registry = ServingRegistry::from_entries(names.iter().map(|name| {
            RegistryEntry::new(
                (*name).clone(),
                VaultRoot::new(format!("/tmp/norn-host-lifecycle-{name}")).unwrap(),
            )
        }))
        .unwrap();
        Host::new(
            registry,
            ops,
            LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots,
                watch_poll_interval: Duration::from_secs(60),
            },
        )
        .unwrap()
    }

    /// A directory under the system temp directory, named uniquely to this
    /// process and instant so cases running beside each other never share one.
    ///
    /// The roots the fixtures above name resolve to nothing, which the registry
    /// reads as a root that is registrable and not yet present. A case whose
    /// subject is what the registry reads off a root needs a root the
    /// filesystem answers for, and this is where those live.
    fn temp_base(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "norn-host-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    /// A host over roots the filesystem answers for, with the dispatcher
    /// ticking. Every root is created before the host reads the registry.
    fn host_over_roots(
        ops: Arc<FakeOps>,
        roots: &[(&VaultName, &std::path::Path)],
        worker_slots: usize,
    ) -> Host<Arc<FakeOps>> {
        for (_, root) in roots {
            std::fs::create_dir_all(root).unwrap();
        }
        let registry = ServingRegistry::from_entries(roots.iter().map(|(name, root)| {
            RegistryEntry::new((*name).clone(), VaultRoot::new(root).unwrap())
        }))
        .unwrap();
        Host::new(
            registry,
            ops,
            LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots,
                watch_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap()
    }

    /// Retarget a vault root to a symlink that resolves to itself. The identity
    /// read the registry performs over a root refuses on the cycle, so this is
    /// the environmental refusal every registry read of that root reports from
    /// here on.
    #[cfg(unix)]
    fn refuse_root_identity(root: &std::path::Path) {
        use std::os::unix::fs::symlink;

        let target = root
            .file_name()
            .expect("the root has a final component")
            .to_owned();
        std::fs::remove_dir(root).unwrap();
        symlink(target, root).unwrap();
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

    /// Wait for the fake to have entered `expected` reconciles, on the one
    /// budget, reporting how many it had entered when the wait gave up. The
    /// count rises before a reconcile that blocks starts waiting, so a case
    /// that wants its workers occupied waits here.
    fn wait_for_reconciles(ops: &FakeOps, expected: usize, what: &str) {
        wait_until(what, lifecycle_wait_budget(), || {
            let reconciles = ops.reconciles.load(Ordering::SeqCst);
            if reconciles >= expected {
                Observed::Met(())
            } else {
                Observed::pending(format!("{reconciles} reconciles so far"))
            }
        })
        .unwrap_or_else(|failure| panic!("{failure}"));
    }

    fn wait_for_attaches(ops: &FakeOps, expected: usize, what: &str) {
        wait_until(what, lifecycle_wait_budget(), || {
            let attaches = ops.attaches.load(Ordering::SeqCst);
            if attaches == expected {
                Observed::Met(())
            } else {
                Observed::pending(format!("{attaches} attaches so far"))
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

    /// Whether a demand answers the identity park, judged by the variant alone.
    /// The detail is the registry's own account of the root it cannot read, and
    /// the park carries one.
    fn refuses_identity(demand: &Demand) -> bool {
        let Demand::IdentityRefused(detail) = demand else {
            return false;
        };
        assert!(
            !detail.is_empty(),
            "an identity park reported no account of itself"
        );
        true
    }

    /// Wait for the entry to publish an environmental refusal, on the one
    /// budget, reporting the state it last saw. The refusal's detail is the
    /// platform's own prose, so what is waited for is the reason — and
    /// [`refuses_environmentally`] is what holds the account beside it to being
    /// there at all.
    fn wait_for_environmental_refusal<O: EntryOps>(host: &Host<O>, name: &VaultName) {
        wait_until(
            "the entry to publish an environmental refusal",
            lifecycle_wait_budget(),
            || {
                let state = host.state(name);
                if refuses_environmentally(state.as_ref()) {
                    Observed::Met(())
                } else {
                    Observed::pending(format!("the state is {state:?}"))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
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

    /// How many watcher polls have reached the vault.
    fn polls_of(ops: &Arc<FakeOps>, name: &VaultName) -> usize {
        ops.polls
            .lock()
            .expect("poll counts poisoned")
            .get(name)
            .copied()
            .unwrap_or(0)
    }

    /// Drive one watcher poll that reports terminal loss over the named vault,
    /// leaving the entry attached, untrusted, and owing a recovery no lease has
    /// asked for yet.
    ///
    /// The poll is driven rather than ambient, so the loss lands where the
    /// caller put it rather than wherever a dispatcher tick arrived first. A
    /// round skips an entry whose claim is held or whose attachment is out, and
    /// the error stays armed for whichever poll runs next, so two facts
    /// together say the caller got the poll it named: the error is gone, and
    /// the round polled the vault the caller named.
    fn lose_coverage_through_a_driven_poll(
        ops: &Arc<FakeOps>,
        host: &Host<Arc<FakeOps>>,
        name: &VaultName,
        detail: &str,
    ) {
        let polled = polls_of(ops, name);
        *ops.terminal_poll.lock().expect("terminal poll poisoned") =
            Some(WatchError::Backend(detail.into()));
        poll_watchers(&host.shared);
        assert!(
            ops.terminal_poll
                .lock()
                .expect("terminal poll poisoned")
                .is_none(),
            "the driven poll reported no loss"
        );
        assert!(
            polls_of(ops, name) > polled,
            "the driven poll reported the loss somewhere other than {name}"
        );
    }

    /// Arm one batch on the given source and drive one round of watcher polls,
    /// naming the vault the batch is for.
    ///
    /// The poll is driven rather than ambient, so the facts land in the entry
    /// the caller aimed them at rather than wherever a dispatcher tick arrived
    /// first. A round skips an entry whose claim is held or whose attachment is
    /// out, and an unspent batch stays armed for whichever poll runs next, so
    /// two facts together say the caller got the poll it named: the source is
    /// empty, and the round polled the vault the caller named. Either one alone
    /// holds while the batch lands on some other entry, which is a premise
    /// breaking under a case rather than the thing the case is about.
    ///
    /// What the entry then published is the caller's own to assert: this says
    /// the batch reached the poll, not what the poll did with it.
    fn report_through_a_driven_poll(
        ops: &Arc<FakeOps>,
        host: &Host<Arc<FakeOps>>,
        name: &VaultName,
        source: &AtomicUsize,
    ) {
        let polled = polls_of(ops, name);
        source.store(1, Ordering::SeqCst);
        poll_watchers(&host.shared);
        assert_eq!(
            source.load(Ordering::SeqCst),
            0,
            "the driven poll spent no batch"
        );
        assert!(
            polls_of(ops, name) > polled,
            "the driven poll reported the batch somewhere other than {name}"
        );
    }

    /// Arm one batch on the given source and wait for the dispatcher's own tick
    /// to spend it. The wait is what makes the arrival something the case can
    /// order against, rather than something it assumes a tick reached.
    fn report_through_an_ambient_poll(source: &AtomicUsize) {
        source.store(1, Ordering::SeqCst);
        wait_until(
            "a dispatcher tick to report the armed batch",
            lifecycle_wait_budget(),
            || {
                if source.load(Ordering::SeqCst) == 0 {
                    Observed::Met(())
                } else {
                    Observed::pending("the batch is still armed".to_string())
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
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
    /// The recovery is owed by a watcher poll reporting terminal loss, and this
    /// family runs without ambient polling, so the poll that reports it is the
    /// one driven here.
    fn by_a_demanded_recovery(
        ops: &Arc<FakeOps>,
        host: &Host<Arc<FakeOps>>,
        name: &VaultName,
    ) -> Option<DemandLease<Arc<FakeOps>>> {
        lose_coverage_through_a_driven_poll(ops, host, name, "gone");
        Some(host.demand(name).unwrap())
    }

    /// Watcher facts schedule the reconcile that reports the failure, and the
    /// poll that observes them is the one driven here: this family runs without
    /// ambient polling, so the tick that carries the facts in is the case's
    /// own.
    fn by_a_reconciled_batch(
        ops: &Arc<FakeOps>,
        host: &Host<Arc<FakeOps>>,
        name: &VaultName,
    ) -> Option<DemandLease<Arc<FakeOps>>> {
        report_through_a_driven_poll(ops, host, name, &ops.off_thread_rescan_poll_batches);
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

    /// The recheck that admits a demand is what clears the conflict park, so
    /// the release the lease is recorded behind honors it. One predicate reads
    /// the park on both sides of the window: a lease the demand path admitted
    /// is never a lease the release path refuses.
    #[test]
    fn a_lease_admitted_by_a_passing_recheck_is_honored_by_the_release() {
        let ops = Arc::new(FakeOps::default());
        let (host, a, b) = two_alias_host(Arc::clone(&ops));

        ops.block_detach.store(true, Ordering::SeqCst);
        let shared = Arc::clone(&host.shared);
        let conflict = AliasConflict {
            aliases: vec![a.clone(), b.clone()],
        };
        // The refusal runs off the test thread because it releases both
        // aliases inline, and this test demands from inside that release.
        let refusal = thread::spawn(move || refuse_conflict(&shared, &conflict));
        wait_for_flag("detach_started", &ops.detach_started);

        // The registry reports no conflict over these roots, so the recheck
        // this demand runs passes and the lease is recorded against an entry
        // whose release is still in flight.
        let lease = host.demand(&a).unwrap();
        assert_eq!(*lease.outcome(), Demand::State(releasing()));

        ops.detach_release.store(true, Ordering::SeqCst);
        refusal.join().unwrap();
        wait_for_state(&host, &a, TrustState::Ready);
        assert_eq!(lease.completion(), Demand::State(TrustState::Ready));
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            3,
            "the release refused the re-arm for a conflict the recheck had cleared"
        );
        drop((lease, host));
    }

    /// The answer a lease reports is the answer the demand that raised it acted
    /// on: the recheck clears the conflict park before the demand schedules
    /// against it, so no lease reports a conflict for the length of the work
    /// that was scheduled over one.
    #[test]
    fn a_demand_scheduled_over_a_cleared_conflict_answers_the_work_it_scheduled() {
        let ops = Arc::new(FakeOps::default());
        let (host, a, b) = two_alias_host(Arc::clone(&ops));
        let conflict = AliasConflict {
            aliases: vec![a.clone(), b.clone()],
        };
        refuse_conflict(&host.shared, &conflict);
        wait_for_state(&host, &a, TrustState::Unattached);

        ops.block_attach.store(true, Ordering::SeqCst);
        let lease = host.demand(&a).unwrap();
        let installing = Demand::State(TrustState::warming(
            WarmingPhase::InstallingCoverage,
            0,
            None,
        ));
        assert_eq!(*lease.outcome(), installing);
        wait_for_flag("attach_started", &ops.attach_started);
        assert_eq!(
            lease.completion(),
            installing,
            "the lease reported a conflict against the work the same demand scheduled"
        );

        ops.attach_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &a, TrustState::Ready);
        drop((lease, host));
    }

    /// A park outlives the release that publishes over it. The release says
    /// Unattached because the resources are back, and the lease still reports
    /// the identity refusal that keeps the entry from re-arming rather than a
    /// released state with no account of why nothing follows it.
    #[cfg(unix)]
    #[test]
    fn a_release_over_an_identity_refusal_reports_the_refusal() {
        let base = temp_base("released-identity-park");
        let root = base.join("root");
        let ops = Arc::new(FakeOps::default());
        let name = VaultName::new("notes").unwrap();
        let host = host_over_roots(Arc::clone(&ops), &[(&name, &root)], 1);
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_detach.store(true, Ordering::SeqCst);
        ops.lost_poll.store(true, Ordering::SeqCst);
        wait_for_flag("detach_started", &ops.detach_started);

        refuse_root_identity(&root);
        let lease = host.demand(&name).unwrap();
        assert!(
            refuses_environmentally(host.state(&name).as_ref()),
            "the demand's recheck did not refuse the root the registry cannot read"
        );

        ops.detach_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Unattached);
        let Demand::IdentityRefused(detail) = lease.completion() else {
            panic!(
                "the release published released over a park that keeps the entry from re-arming: {:?}",
                lease.completion()
            );
        };
        assert!(
            !detail.is_empty(),
            "the identity park reported no account of itself"
        );
        settle();
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            1,
            "the release re-armed against a root the registry cannot read"
        );

        drop((lease, host));
        let _ = std::fs::remove_dir_all(base);
    }

    /// One park is answered in one order however many stand at once: a
    /// maintainer another process holds outranks a root reached under more than
    /// one name, which outranks a root the registry cannot read.
    #[test]
    fn a_lease_answers_the_widest_park_standing_over_the_entry() {
        let ops = Arc::new(FakeOps::default());
        let (host, a, b) = two_alias_host(Arc::clone(&ops));
        let lease = host.demand(&a).unwrap();
        let entry = Arc::clone(host.shared.entries.get(&a).unwrap());
        let conflict = AliasConflict {
            aliases: vec![a.clone(), b.clone()],
        };

        {
            let mut state = entry.gate.lock().unwrap();
            state.maintainer_contended = Some(MaintainerIdentity::unknown());
            state.duplicate_root = Some(conflict.clone());
            state.identity_refused = Some("the root cannot be read".into());
        }
        assert_eq!(
            lease.completion(),
            Demand::MaintainerContended(MaintainerIdentity::unknown())
        );

        entry.gate.lock().unwrap().maintainer_contended = None;
        assert_eq!(lease.completion(), Demand::DuplicateRoot(conflict));

        entry.gate.lock().unwrap().duplicate_root = None;
        assert_eq!(
            lease.completion(),
            Demand::IdentityRefused("the root cannot be read".into())
        );

        drop((lease, host));
    }

    /// A watcher poll reads the same park the demand path does: an entry
    /// holding coverage a conflict has refused is one no ambient tick schedules
    /// against, however long that coverage stays in the entry.
    #[test]
    fn a_poll_schedules_no_demanded_work_against_a_parked_entry() {
        let ops = Arc::new(FakeOps::default());
        let (host, a, b) = two_alias_host(Arc::clone(&ops));
        let lease = host.demand(&a).unwrap();

        // The entry a refusal leaves behind where a leg is registered: the
        // coverage stays parked in the entry, the conflict park stands, and
        // Unattached is published over both.
        {
            let entry = host.shared.entries.get(&a).unwrap();
            let mut state = entry.gate.lock().unwrap();
            state.duplicate_root = Some(AliasConflict {
                aliases: vec![a.clone(), b.clone()],
            });
            state.trust = TrustState::Unattached;
        }

        poll_watchers(&host.shared);
        settle();

        {
            let entry = host.shared.entries.get(&a).unwrap();
            let state = entry.gate.lock().unwrap();
            assert!(
                !state.claim.is_held(),
                "a poll scheduled work against an entry a conflict has parked"
            );
        }
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 0);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);
        assert!(matches!(lease.completion(), Demand::DuplicateRoot(_)));
        drop((lease, host));
    }

    /// A retry recovers an entry from a conflict the registry no longer
    /// reports, and it recovers it through the recheck the demand runs: the
    /// registry's parks are retired by the read that adjudicates them, not by
    /// the caller asking again.
    #[test]
    fn a_retry_over_a_resolved_conflict_reaches_ready() {
        let ops = Arc::new(FakeOps::default());
        let (host, a, b) = two_alias_host(Arc::clone(&ops));
        refuse_conflict(
            &host.shared,
            &AliasConflict {
                aliases: vec![a.clone(), b.clone()],
            },
        );
        wait_for_state(&host, &a, TrustState::Unattached);

        let retried = host.retry(&a).unwrap();
        wait_for_state(&host, &a, TrustState::Ready);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 3);
        drop((retried, host));
    }

    /// A conflict is answered by the park it sets, and a lease over a parked
    /// entry is a lease like any other: it is held against the entry and it
    /// re-reads the park. So the conflict a demand was refused for is only the
    /// lease's answer for as long as it stands — once a later recheck has
    /// retired it, that same lease reports the state the entry is in rather
    /// than the refusal it was born from.
    #[cfg(unix)]
    #[test]
    fn a_lease_stops_answering_a_conflict_a_later_recheck_retired() {
        use std::os::unix::fs::symlink;

        let base = temp_base("retired-conflict-lease");
        let a_root = base.join("a");
        let b_root = base.join("b");
        let ops = Arc::new(FakeOps::default());
        let a = VaultName::new("a").unwrap();
        let b = VaultName::new("b").unwrap();
        let host = host_over_roots(Arc::clone(&ops), &[(&a, &a_root), (&b, &b_root)], 2);

        // b's root becomes a second name for a's, so b's own demand is the
        // recheck that refuses it.
        std::fs::remove_dir(&b_root).unwrap();
        symlink(&a_root, &b_root).unwrap();
        let lease = host.demand(&b).unwrap();
        assert!(
            matches!(lease.completion(), Demand::DuplicateRoot(_)),
            "a lease over an entry a conflict has parked did not answer the park"
        );

        // The roots are two again, so the recheck the retry runs is the read
        // that retires the park.
        std::fs::remove_file(&b_root).unwrap();
        std::fs::create_dir(&b_root).unwrap();
        let retried = host.retry(&b).unwrap();
        wait_for_state(&host, &b, TrustState::Ready);
        assert_eq!(
            lease.completion(),
            Demand::State(TrustState::Ready),
            "the lease kept answering a conflict a later recheck retired"
        );

        drop((lease, retried, host));
        let _ = std::fs::remove_dir_all(base);
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
        lose_coverage_through_a_driven_poll(&ops, &host, &name, "gone");
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
                state.claim.is_held(),
                "the release's own re-attach is in flight against an unclaimed entry"
            );
            assert!(
                state.claim.leg() == Some(Leg::Job(state.claim.epoch())),
                "the re-attach runs against a leg the entry does not name"
            );
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

    /// Watcher facts standing in an entry whose resources are going back leave
    /// the window alone: the entry holds nothing to reconcile them against, so
    /// the phase that names the release stands until the release ends it, and
    /// the release takes the facts with the resources.
    ///
    /// The facts are written under the entry gate. The detach leg holds the
    /// claim for the length of the window, so every poll passes the entry over,
    /// and the gate is where facts reaching a release really stand.
    #[test]
    fn watcher_facts_standing_in_a_release_window_leave_it_standing() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::ZERO);
        // The lease is held until the release is armed: the idle interval here
        // is zero, so an entry with no lease on it is reapable the instant it
        // attaches, and the reap this test is about is the armed one.
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_detach.store(true, Ordering::SeqCst);
        drop(lease);
        host.reap_idle(Instant::now()).unwrap();
        wait_for_flag("detach_started", &ops.detach_started);
        {
            let entry = host.shared.entries.get(&name).unwrap();
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            assert!(
                state.detach_in_flight,
                "the release these facts stand in is not in flight"
            );
            state.pending.merge(Batch::rescan(RescanScope::Vault));
        }

        ops.detach_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Unattached);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);

        // The facts went back with the resources they described, so the
        // coverage installed next owes nothing to a vault it never watched.
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);
        assert_eq!(
            ops.reconciles.load(Ordering::SeqCst),
            0,
            "the release left facts standing for the next attach to reconcile"
        );
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
            // The instant a leg has re-stored the attachment and given the gate
            // back, and has not yet cleared its claim: destruction reads the
            // entry as busy and takes nothing from it.
            let epoch = state.claim.epoch();
            state.claim.begin_job_leg(epoch);
            state.claim.release();
        }

        drop(host);
        assert_eq!(
            ops.detaches.load(Ordering::SeqCst),
            1,
            "destruction dropped an attachment instead of giving it back"
        );
    }

    /// The release window is closed by the release itself, not by the label
    /// standing in the entry: a phase overwritten mid-window still schedules
    /// nothing against resources on their way out, and the lease raised there
    /// is answered by the release.
    ///
    /// `detach_in_flight` is the whole of that guard, so the label written
    /// below is the sharpest one there is: Unattached claims the resources are
    /// already back, and it is one of the two states `demand` schedules
    /// against. What refuses the scheduling is the flag beside it.
    #[test]
    fn a_relabelled_release_window_still_schedules_nothing() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_detach.store(true, Ordering::SeqCst);
        ops.lost_poll.store(true, Ordering::SeqCst);
        wait_for_flag("detach_started", &ops.detach_started);
        {
            let entry = host.shared.entries.get(&name).unwrap();
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            assert!(
                state.detach_in_flight,
                "the release this test relabels is not in flight"
            );
            state.trust = TrustState::Unattached;
        }

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

    /// An entry whose lease has gone is reaped on a dispatcher tick, and
    /// watcher facts still arriving on those same ticks do not keep it alive:
    /// the reap happens anyway, and the resources go back once.
    #[test]
    fn dispatcher_reaps_idle_attachment_despite_watcher_churn() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_millis(20));
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);

        // One batch reported and waited for while the lease still holds the
        // entry against the reap. It is what says a dispatcher tick carries
        // this source into this entry at all, so the reconcile below is an
        // arrival rather than a race with the reap that follows.
        report_through_an_ambient_poll(&ops.off_thread_rescan_poll_batches);
        wait_for_reconciles(&ops, 1, "the reconcile the reported rescan schedules");

        drop(lease);
        for _ in 0..3 {
            // Churn across the reap window, fed in unwaited: whether a tick
            // spends each batch before the reap takes the attachment is the
            // input this test varies, not a fact it asserts. The gap spaces
            // the batches across several ticks instead of collapsing them
            // into one.
            ops.off_thread_rescan_poll_batches
                .fetch_add(1, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(4));
        }
        wait_for_state(&host, &name, TrustState::Unattached);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
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
        let (host, name) = attached_awaiting_recovery(&ops);
        settle();
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
            *ops.terminal_poll.lock().expect("terminal poll poisoned") = Some(error);
            wait_for_state(&host, &name, expected);
            drop(held);
        }
    }

    /// A terminal poll failure publishes its cause once, and the dispatcher
    /// keeps polling the attachment it kept. Neither a tick that reports
    /// nothing nor one that reports a vault-wide rescan replaces that cause:
    /// the entry owes a recovery, and what a rescan says about an entry whose
    /// coverage is gone is nothing the loss did not already say. The state a
    /// client reads long after the loss is the cause that ended coverage rather
    /// than something minted on top of it.
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
        assert_eq!(host.state(&name), Some(expected.clone()));

        report_through_an_ambient_poll(&ops.off_thread_rescan_poll_batches);
        settle();
        assert_eq!(
            host.state(&name),
            Some(expected),
            "a rescan replaced the cause that ended coverage"
        );
        assert_eq!(
            ops.reconciles.load(Ordering::SeqCst),
            0,
            "a rescan scheduled work against coverage the entry no longer has"
        );
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

    /// The invalidation the recovery drains is the vault-wide rescan the
    /// terminal poll left standing when it lost coverage: nothing that was
    /// watched while coverage was gone is known, so the entry serves again only
    /// after the reconcile that rereads it.
    #[test]
    fn recover_drains_pending_invalidations_before_publishing_ready() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = attached_awaiting_recovery(&ops);
        // Held across the wait: the recovery this test counts is the one this
        // lease asks for, and a lease dropped before it runs withdraws it.
        let lease = host.demand(&name).unwrap();
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
        ops.handoff_rescan_poll_batches.store(1, Ordering::SeqCst);
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

        // The handoff drain's own batch source, not the source the driven poll
        // below spends: a shared counter would let that poll take a batch this
        // saturation depends on before it ever schedules the reconcile that is
        // supposed to drain it.
        ops.handoff_poll_batches
            .store(HANDOFF_BATCH_LIMIT, Ordering::SeqCst);
        report_through_a_driven_poll(&ops, &host, &name, &ops.off_thread_rescan_poll_batches);
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
        // batch source, isolated from the poll the case drives. The rescan
        // this test is about is the one the handoff observes, so the poll
        // below reports a batch carrying no fact — the reconcile it schedules
        // is what runs the handoff, and the entry stays trusted until the
        // handoff itself says otherwise.
        ops.handoff_rescan_poll_batches.store(1, Ordering::SeqCst);
        ops.handoff_poll_batches
            .store(HANDOFF_BATCH_LIMIT - 1, Ordering::SeqCst);
        ops.block_reconcile_at.store(2, Ordering::SeqCst);
        report_through_a_driven_poll(&ops, &host, &name, &ops.off_thread_poll_batches);

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

    /// The recover leg's own terminal failure republishes watcher-untrusted.
    /// The poll that owed the recovery carried a different account of itself,
    /// so the state waited for below can only have come from the recover.
    #[test]
    fn terminal_failure_during_recover_stays_watcher_untrusted() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        *ops.terminal_poll.lock().expect("terminal poll poisoned") =
            Some(WatchError::Backend("gone".into()));
        wait_for_state(
            &host,
            &name,
            lost(WatcherLossCause::Backend, "filesystem watcher failed: gone"),
        );
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
        // The reconcile is scheduled by a dispatcher tick that finds facts:
        // the subject is the leg's own failure, so the tick that starts it is
        // the ambient one.
        report_through_an_ambient_poll(&ops.off_thread_rescan_poll_batches);
        wait_for_state(&host, &name, backend_lost());
    }

    #[test]
    fn failed_reconcile_requires_demand_recovery_before_later_facts_can_be_ready() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        ops.environmental_reconcile.store(true, Ordering::SeqCst);
        report_through_an_ambient_poll(&ops.off_thread_rescan_poll_batches);
        wait_for_state(
            &host,
            &name,
            TrustState::untrusted(UntrustedReason::environmental_refusal("refused")),
        );
        let failed_count = ops.reconciles.load(Ordering::SeqCst);
        // The later fact reaches the entry through a tick of its own, and the
        // wait for the batch to be spent is what says the tick got there: the
        // entry owes a recovery, so the poll reporting it schedules nothing.
        report_through_an_ambient_poll(&ops.off_thread_poll_batches);
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
        let (host, name) = attached_awaiting_recovery(&ops);
        ops.environmental_recover.store(true, Ordering::SeqCst);
        // Held across both waits below: a lease dropped before either state
        // is reached would withdraw the recovery request it raised.
        let lease = host.demand(&name).unwrap();
        wait_for_state(
            &host,
            &name,
            TrustState::untrusted(UntrustedReason::environmental_refusal("refused")),
        );
        // The later fact reaches the entry through a tick of its own, and the
        // wait for the batch to be spent is what says the tick got there: the
        // entry owes a recovery, so the poll reporting it schedules nothing.
        report_through_an_ambient_poll(&ops.off_thread_poll_batches);
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

    /// An identity refusal found by a demand-time recheck supersedes the
    /// reconcile in flight against the root it refuses: the entry publishes the
    /// refusal at once, and the reconcile gives its attachment back rather than
    /// restoring it into an entry that has moved past it. Nothing re-attaches
    /// against a root the registry is refusing.
    #[cfg(unix)]
    #[test]
    fn an_identity_refusal_invalidates_an_in_flight_reconcile() {
        let base = temp_base("reconcile-identity-refusal");
        let root = base.join("root");
        let ops = Arc::new(FakeOps::default());
        let name = VaultName::new("notes").unwrap();
        let host = host_over_roots(Arc::clone(&ops), &[(&name, &root)], 1);
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_reconcile.store(true, Ordering::SeqCst);
        // A dispatcher tick reporting facts is what puts a reconcile in
        // flight; which tick it was is not this case's subject.
        report_through_an_ambient_poll(&ops.off_thread_rescan_poll_batches);
        wait_for_flag("reconcile_started", &ops.reconcile_started);

        refuse_root_identity(&root);
        let refused = host.demand(&name).unwrap();
        assert!(
            refuses_identity(&refused.completion()),
            "the demand that refused the root did not report the park it raised"
        );
        assert!(refuses_environmentally(host.state(&name).as_ref()));
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);

        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for_detaches(
            &ops,
            1,
            "the invalidated reconcile to give its attachment back",
        );
        settle();
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            1,
            "the entry re-attached against a root the registry refuses"
        );
        assert!(refuses_environmentally(host.state(&name).as_ref()));
        drop((refused, host));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn host_drop_waits_for_in_flight_work_and_its_attachment_teardown() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        ops.block_reconcile.store(true, Ordering::SeqCst);
        // A dispatcher tick reporting facts is what puts a reconcile in
        // flight; which tick it was is not this case's subject.
        report_through_an_ambient_poll(&ops.off_thread_rescan_poll_batches);
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
        ops.off_thread_poll_batches.store(1, Ordering::SeqCst);
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
            state.claim.stand_at(1);
            state.claim.mark(Job::Attach(names[2].clone(), 1));
        }
        let shared = Arc::clone(&host.shared);
        let dispatched_entry = Arc::clone(&entry);
        let dispatch = thread::spawn(move || dispatch_pending(&shared, &dispatched_entry));
        wait_until(
            "the dispatch to take the entry's queue slot",
            lifecycle_wait_budget(),
            || {
                if entry.gate.lock().unwrap().claim.slot() == Some(1) {
                    Observed::Met(())
                } else {
                    Observed::pending("the slot is not taken")
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        {
            let mut state = entry.gate.lock().unwrap();
            state.claim.stand_at(2);
            state.claim.mark(Job::Attach(names[2].clone(), 2));
        }
        drop(jobs_guard);
        dispatch.join().unwrap().unwrap();

        let state = entry.gate.lock().unwrap();
        assert!(state.claim.slot().is_none());
        assert_eq!(state.claim.marker().map(Job::epoch), Some(2));
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
        assert!(
            refuses_identity(&demand.completion()),
            "the demand that refused the root did not report the park it raised"
        );
        assert!(refuses_environmentally(host.state(&name).as_ref()));
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

    /// A poll the entry has moved past gives back everything it took: the
    /// attachment goes to [`EntryOps::detach`], and the claim and the marker go
    /// back to the entry. The lease standing against the entry by then is
    /// scheduled there, because the poll is the last thing holding the entry
    /// and the dispatcher reaches an entry holding no attachment for nothing.
    ///
    /// The invalidator is an identity refusal, which parks the entry against
    /// its own root. The root answering again is what unparks it, and the
    /// recheck a demand runs is what reads that — so the lease below is raised
    /// against an entry that owes work and has a poll in flight over it.
    #[cfg(unix)]
    #[test]
    fn a_superseded_poll_gives_the_entry_back_and_schedules_the_lease_on_it() {
        let base = temp_base("superseded-poll-give-back");
        let root = base.join("root");
        let ops = Arc::new(FakeOps::default());
        let name = VaultName::new("notes").unwrap();
        let host = host_over_roots(Arc::clone(&ops), &[(&name, &root)], 1);
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_poll.store(true, Ordering::SeqCst);
        wait_for_flag("poll_started", &ops.poll_started);
        refuse_root_identity(&root);
        drop(host.demand(&name).unwrap());
        assert!(refuses_environmentally(host.state(&name).as_ref()));

        // The root answers again. The poll still holds the entry, so the lease
        // the demand below returns is recorded against an entry nothing can
        // schedule against yet.
        std::fs::remove_file(&root).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let lease = host.demand(&name).unwrap();
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            1,
            "a lease raised under a live poll claim scheduled work against it"
        );

        ops.poll_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(
            ops.detaches.load(Ordering::SeqCst),
            1,
            "the superseded poll kept the attachment it took"
        );
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);

        drop((lease, host));
        let _ = std::fs::remove_dir_all(base);
    }

    /// The recheck a scheduled attach runs before it acquires anything is a
    /// second read of the registry, and a root that stopped answering between
    /// the demand and the job is what it is there to catch: the attach refuses
    /// environmentally with the platform's account of the refusal, having
    /// acquired nothing.
    ///
    /// The window is the queue. The one worker is inside another vault's
    /// attach, so the job scheduled for the entry under test waits in the
    /// channel while its root is retargeted.
    ///
    /// The recheck is the arm every refusing root reaches: it and the identity
    /// read beside it ask the filesystem the same question about the same root,
    /// and the recheck asks first. The identity arm answers a root whose
    /// identity stops resolving between those two reads.
    #[cfg(unix)]
    #[test]
    fn an_attach_time_registry_refusal_acquires_nothing_and_says_why() {
        let base = temp_base("attach-recheck-refusal");
        let subject_root = base.join("subject");
        let holding_root = base.join("holding");
        let ops = Arc::new(FakeOps::default());
        let subject = VaultName::new("subject").unwrap();
        let holding = VaultName::new("holding").unwrap();
        let host = host_over_roots(
            Arc::clone(&ops),
            &[(&subject, &subject_root), (&holding, &holding_root)],
            1,
        );

        ops.block_attach.store(true, Ordering::SeqCst);
        let holding_lease = host.demand(&holding).unwrap();
        wait_for_flag("attach_started", &ops.attach_started);
        let lease = host.demand(&subject).unwrap();

        refuse_root_identity(&subject_root);
        ops.attach_release.store(true, Ordering::SeqCst);
        wait_for_environmental_refusal(&host, &subject);
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            1,
            "the refused attach acquired the entry before it read the registry"
        );
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);

        drop((lease, holding_lease, host));
        let _ = std::fs::remove_dir_all(base);
    }

    /// The recheck an attach runs after its heal catches a root that stopped
    /// answering while the heal ran: the attach gives back everything it
    /// acquired and then refuses environmentally, with the platform's account
    /// of the refusal.
    #[cfg(unix)]
    #[test]
    fn a_post_heal_registry_refusal_gives_the_attachment_back_and_says_why() {
        let base = temp_base("post-heal-recheck-refusal");
        let root = base.join("root");
        let ops = Arc::new(FakeOps::default());
        let name = VaultName::new("notes").unwrap();
        let host = host_over_roots(Arc::clone(&ops), &[(&name, &root)], 1);

        ops.block_attach.store(true, Ordering::SeqCst);
        let lease = host.demand(&name).unwrap();
        wait_for_flag("attach_started", &ops.attach_started);
        refuse_root_identity(&root);
        ops.attach_release.store(true, Ordering::SeqCst);

        wait_for_environmental_refusal(&host, &name);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        assert_eq!(
            ops.detaches.load(Ordering::SeqCst),
            1,
            "the refused attach kept the resources its heal acquired"
        );
        settle();
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            1,
            "the entry re-attached against a root the registry refuses"
        );
        assert!(refuses_environmentally(host.state(&name).as_ref()));

        drop((lease, host));
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
        assert!(
            refuses_identity(&refused_again.completion()),
            "the demand that refused the root did not report the park it raised"
        );
        assert!(refuses_environmentally(host.state(&refused).as_ref()));
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

    /// A job dispatched against an entry whose attachment a watcher poll then
    /// claims is the entry's work either way. The poll gives back what it took,
    /// so the job waits for the tick that follows rather than disappearing with
    /// the trust state it was dispatched to publish.
    #[test]
    fn a_job_that_loses_the_attachment_to_a_poll_runs_when_the_poll_gives_it_back() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);

        // The claim the follow-up below loses: a poll holding the attachment
        // at the very epoch the follow-up was dispatched against.
        ops.block_poll.store(true, Ordering::SeqCst);
        let shared = Arc::clone(&host.shared);
        let poll = thread::spawn(move || poll_watchers(&shared));
        wait_for_flag("poll_started", &ops.poll_started);

        let entry = host.shared.entries.get(&name).unwrap();
        let epoch = entry.gate.lock().unwrap().claim.epoch();
        // The send a job leg makes when it hands the entry its next job: no
        // marker beside it, because the leg sent it itself, and the queue slot
        // already taken, because the leg takes it under its own claim.
        entry.gate.lock().unwrap().claim.take_slot(epoch);
        dispatch_followup(&host.shared, Job::Reconcile(name.clone(), epoch));
        wait_until(
            "the job that lost the attachment to record itself for a later tick",
            lifecycle_wait_budget(),
            || {
                let state = entry.gate.lock().expect("entry gate poisoned");
                if state.claim.marker().is_some() {
                    Observed::Met(())
                } else {
                    Observed::pending("the entry has no job scheduled".to_string())
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));

        ops.poll_release.store(true, Ordering::SeqCst);
        poll.join().unwrap();
        retry_pending_dispatches(&host.shared);
        wait_until(
            "the restored reconcile to run",
            lifecycle_wait_budget(),
            || {
                let reconciles = ops.reconciles.load(Ordering::SeqCst);
                if reconciles == 1 {
                    Observed::Met(())
                } else {
                    Observed::pending(format!("{reconciles} reconciles so far"))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        wait_for_state(&host, &name, TrustState::Ready);
        drop(lease);
    }

    /// A claim gives back an entry that has a job scheduled against it with its
    /// gate still held. The gate stands for the job the claim did not take
    /// away, so nothing may claim the entry out from under it — a later tick
    /// passes the entry over, and the job that was waiting runs at the epoch it
    /// was dispatched against.
    #[test]
    fn a_poll_giving_an_entry_back_leaves_the_job_waiting_on_it_dispatchable() {
        let ops = Arc::new(FakeOps::default());
        let a = VaultName::new("a").unwrap();
        let b = VaultName::new("b").unwrap();
        let host = host_without_ambient_polling(Arc::clone(&ops), &[&a, &b], 1);
        // One vault attaches at a time: the single queue slot this fixture
        // gives is the one the attaches would otherwise contend for, and a
        // send this fixture refuses waits for a tick a minute away.
        let lease_a = host.demand(&a).unwrap();
        wait_for_state(&host, &a, TrustState::Ready);
        let lease_b = host.demand(&b).unwrap();
        wait_for_state(&host, &b, TrustState::Ready);

        ops.block_poll.store(true, Ordering::SeqCst);
        let shared = Arc::clone(&host.shared);
        let poll = thread::spawn(move || poll_watchers(&shared));
        wait_for_flag("poll_started", &ops.poll_started);

        let entry = host.shared.entries.get(&a).unwrap();
        let epoch = entry.gate.lock().unwrap().claim.epoch();
        // The job that loses the attachment to the poll above and records
        // itself for a later tick, sent as a leg sends one: the queue slot
        // taken under the claim the leg is ending.
        entry.gate.lock().unwrap().claim.take_slot(epoch);
        dispatch_followup(&host.shared, Job::Reconcile(a.clone(), epoch));
        wait_until(
            "the job that lost the attachment to record itself for a later tick",
            lifecycle_wait_budget(),
            || {
                let state = entry.gate.lock().expect("entry gate poisoned");
                if state.claim.marker().is_some() {
                    Observed::Met(())
                } else {
                    Observed::pending("the entry has no job scheduled".to_string())
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));

        ops.poll_release.store(true, Ordering::SeqCst);
        poll.join().unwrap();

        // The recorded job goes into the queue behind a worker occupied by
        // the other vault, which is the window a claim taken here would leave
        // it stranded in. The entry under test is holding its own gate for the
        // job recorded above, so the tick below passes it over and the facts
        // land on the other vault.
        ops.block_reconcile.store(true, Ordering::SeqCst);
        report_through_a_driven_poll(&ops, &host, &b, &ops.off_thread_rescan_poll_batches);
        wait_for_flag("reconcile_started", &ops.reconcile_started);
        retry_pending_dispatches(&host.shared);

        // A tick reaching the entry now finds the gate standing for the job in
        // the channel, so it takes nothing: the entry is passed over, and the
        // job runs at the epoch it was dispatched against.
        let polled_before = polls_of(&ops, &a);
        poll_watchers(&host.shared);
        assert_eq!(
            polls_of(&ops, &a),
            polled_before,
            "a tick claimed an entry whose gate stands for a job in the channel"
        );

        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &a, TrustState::Ready);
        wait_for_state(&host, &b, TrustState::Ready);
        drop(lease_a);
        drop(lease_b);
    }

    /// A claim that ends by reporting lost coverage leaves nothing scheduled.
    /// The job that was waiting on the claim was dispatched against coverage
    /// that is now gone, and what installs coverage again is the recovery a
    /// demand asks for.
    #[test]
    fn a_claim_that_loses_coverage_drops_the_job_that_was_waiting_on_it() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_poll.store(true, Ordering::SeqCst);
        let shared = Arc::clone(&host.shared);
        let poll = thread::spawn(move || poll_watchers(&shared));
        wait_for_flag("poll_started", &ops.poll_started);

        let entry = host.shared.entries.get(&name).unwrap();
        let epoch = entry.gate.lock().unwrap().claim.epoch();
        // A leg's send, taking the entry's queue slot under the claim it ends.
        entry.gate.lock().unwrap().claim.take_slot(epoch);
        dispatch_followup(&host.shared, Job::Reconcile(name.clone(), epoch));
        wait_until(
            "the job that lost the attachment to record itself for a later tick",
            lifecycle_wait_budget(),
            || {
                let state = entry.gate.lock().expect("entry gate poisoned");
                if state.claim.marker().is_some() {
                    Observed::Met(())
                } else {
                    Observed::pending("the entry has no job scheduled".to_string())
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));

        *ops.terminal_poll.lock().unwrap() = Some(WatchError::Backend("lost".into()));
        ops.poll_release.store(true, Ordering::SeqCst);
        poll.join().unwrap();

        let (runnable, scheduled) = {
            let state = entry.gate.lock().expect("entry gate poisoned");
            (state.claim.is_held(), state.claim.marker().is_some())
        };
        assert!(
            !scheduled && !runnable,
            "an entry owing a recovery kept a job scheduled against coverage it no longer has"
        );
        assert_eq!(ops.reconciles.load(Ordering::SeqCst), 0);

        drop(lease);
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 1);
        drop(lease);
    }

    /// A claim that ends by refusing environmentally leaves nothing scheduled,
    /// for the reason its lost-coverage twin above does: the job that was
    /// waiting on the claim was dispatched against coverage the refusal has
    /// taken out of service, and what puts it back is the recovery a demand
    /// asks for.
    #[test]
    fn a_claim_that_refuses_environmentally_drops_the_job_that_was_waiting_on_it() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_poll.store(true, Ordering::SeqCst);
        let shared = Arc::clone(&host.shared);
        let poll = thread::spawn(move || poll_watchers(&shared));
        wait_for_flag("poll_started", &ops.poll_started);

        let entry = host.shared.entries.get(&name).unwrap();
        let epoch = entry.gate.lock().unwrap().claim.epoch();
        // A leg's send, taking the entry's queue slot under the claim it ends.
        entry.gate.lock().unwrap().claim.take_slot(epoch);
        dispatch_followup(&host.shared, Job::Reconcile(name.clone(), epoch));
        wait_until(
            "the job that lost the attachment to record itself for a later tick",
            lifecycle_wait_budget(),
            || {
                let state = entry.gate.lock().expect("entry gate poisoned");
                if state.claim.marker().is_some() {
                    Observed::Met(())
                } else {
                    Observed::pending("the entry has no job scheduled".to_string())
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));

        ops.environmental_poll.store(true, Ordering::SeqCst);
        ops.poll_release.store(true, Ordering::SeqCst);
        poll.join().unwrap();

        let (runnable, scheduled) = {
            let state = entry.gate.lock().expect("entry gate poisoned");
            (state.claim.is_held(), state.claim.marker().is_some())
        };
        assert!(
            !scheduled && !runnable,
            "an entry owing a recovery kept a job scheduled against coverage it no longer has"
        );
        assert!(refuses_environmentally(host.state(&name).as_ref()));
        assert_eq!(ops.reconciles.load(Ordering::SeqCst), 0);

        drop(lease);
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 1);
        drop(lease);
    }

    /// A release takes the marker of the job that was waiting on the claim it
    /// ends. The job was scheduled against resources this leg has given back,
    /// and a marker standing over an entry whose gate the release opened is a
    /// job no dispatcher tick reaches — work the entry carries and never does.
    #[test]
    fn a_release_takes_the_marker_of_the_job_that_was_waiting_on_the_claim() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        // No lease stands over the release below, so the release publishes
        // Unattached and schedules nothing: what the entry is left holding is
        // the whole of what this test reads.
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_poll.store(true, Ordering::SeqCst);
        let shared = Arc::clone(&host.shared);
        let poll = thread::spawn(move || poll_watchers(&shared));
        wait_for_flag("poll_started", &ops.poll_started);

        let entry = host.shared.entries.get(&name).unwrap();
        let epoch = entry.gate.lock().unwrap().claim.epoch();
        // A leg's send, taking the entry's queue slot under the claim it ends.
        entry.gate.lock().unwrap().claim.take_slot(epoch);
        dispatch_followup(&host.shared, Job::Reconcile(name.clone(), epoch));
        wait_until(
            "the job that lost the attachment to record itself for a later tick",
            lifecycle_wait_budget(),
            || {
                let state = entry.gate.lock().expect("entry gate poisoned");
                if state.claim.marker().is_some() {
                    Observed::Met(())
                } else {
                    Observed::pending("the entry has no job scheduled".to_string())
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));

        // The claim ends by giving the entry's resources back, which is the
        // teardown the marker above outlives.
        ops.lost_poll.store(true, Ordering::SeqCst);
        ops.poll_release.store(true, Ordering::SeqCst);
        poll.join().unwrap();

        wait_for_state(&host, &name, TrustState::Unattached);
        let (runnable, scheduled) = {
            let state = entry.gate.lock().expect("entry gate poisoned");
            (state.claim.is_held(), state.claim.marker().is_some())
        };
        assert!(
            !scheduled && !runnable,
            "a released entry kept a job scheduled against resources it has given back"
        );
        wait_for_detaches(&ops, 1, "the release to give the attachment back");
        assert_eq!(ops.reconciles.load(Ordering::SeqCst), 0);
    }

    /// A job that reached a worker gives the entry's queue slot back, whatever
    /// the entry did while it sat in the channel. A slot standing for a job no
    /// channel holds is an entry every later dispatch refuses, so the work the
    /// entry moved on to would wait behind a job that has already been
    /// discarded.
    #[test]
    fn a_job_the_entry_moved_past_gives_its_queue_slot_back() {
        let ops = Arc::new(FakeOps::default());
        let subject = VaultName::new("a").unwrap();
        let holding = VaultName::new("b").unwrap();
        let host = host_without_ambient_polling(Arc::clone(&ops), &[&subject, &holding], 1);
        let lease = host.demand(&subject).unwrap();
        wait_for_state(&host, &subject, TrustState::Ready);

        // The one worker goes to the other vault, so a job sent for the entry
        // under test stays in the channel until this test lets the worker go.
        ops.block_attach.store(true, Ordering::SeqCst);
        let holding_lease = host.demand(&holding).unwrap();
        wait_for_flag("attach_started", &ops.attach_started);

        let entry = host.shared.entries.get(&subject).unwrap();
        let superseded = entry
            .gate
            .lock()
            .expect("entry gate poisoned")
            .claim
            .epoch();
        // The send a leg makes when it hands the entry its next job: a job the
        // channel really holds, standing in the queue slot the leg took for it.
        entry
            .gate
            .lock()
            .expect("entry gate poisoned")
            .claim
            .take_slot(superseded);
        dispatch_followup(&host.shared, Job::Reconcile(subject.clone(), superseded));
        assert_eq!(
            entry.gate.lock().expect("entry gate poisoned").claim.slot(),
            Some(superseded),
            "a job the channel accepted gave its own queue slot back"
        );

        // The entry moves past that job and schedules work of its own, which
        // the occupied slot defers to a later tick. The other vault's claim is
        // held by the attach blocking the worker, so this tick reaches the
        // entry under test alone.
        report_through_a_driven_poll(&ops, &host, &subject, &ops.off_thread_rescan_poll_batches);
        let (queued, scheduled) = {
            let state = entry.gate.lock().expect("entry gate poisoned");
            (state.claim.slot(), state.claim.marker().map(Job::epoch))
        };
        assert_eq!(queued, Some(superseded));
        assert_eq!(
            scheduled,
            Some(superseded + 1),
            "the entry scheduled nothing to wait on the slot"
        );

        // The worker frees up and takes the superseded job off the channel.
        ops.attach_release.store(true, Ordering::SeqCst);
        wait_until(
            "the arrival at a superseded epoch to give the queue slot back",
            lifecycle_wait_budget(),
            || match entry.gate.lock().expect("entry gate poisoned").claim.slot() {
                None => Observed::Met(()),
                Some(epoch) => Observed::pending(format!("the slot still stands at {epoch}")),
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));

        retry_pending_dispatches(&host.shared);
        wait_for_state(&host, &subject, TrustState::Ready);
        assert_eq!(
            ops.reconciles.load(Ordering::SeqCst),
            1,
            "the job the entry moved past ran anyway"
        );
        drop(lease);
        drop(holding_lease);
    }

    /// A job the entry has moved past gives back its own queue slot and no
    /// other. The slot standing when it arrives may already name the job that
    /// superseded it — a job the channel really holds — and an entry whose
    /// slot is free is one the next dispatcher tick sends that job into the
    /// channel a second time.
    #[test]
    fn an_arrival_at_a_superseded_epoch_leaves_the_slot_of_the_job_that_replaced_it() {
        let ops = Arc::new(FakeOps::default());
        let subject = VaultName::new("a").unwrap();
        let holding = VaultName::new("b").unwrap();
        let attaching = VaultName::new("c").unwrap();
        let host =
            host_without_ambient_polling(Arc::clone(&ops), &[&subject, &holding, &attaching], 2);
        let lease = host.demand(&subject).unwrap();
        wait_for_state(&host, &subject, TrustState::Ready);

        // Both workers go to vaults that are not the one under test: the
        // channel then holds what this test sends for it, and has the room a
        // duplicate would take. Their claims are held by the legs blocking
        // them, so the tick below reaches the entry under test alone.
        ops.block_attach.store(true, Ordering::SeqCst);
        let holding_lease = host.demand(&holding).unwrap();
        let attaching_lease = host.demand(&attaching).unwrap();
        wait_for_attaches(&ops, 3, "both workers to be inside an attach");

        let entry = host.shared.entries.get(&subject).unwrap();
        // The epoch a leg's follow-up was sent at, superseded by the tick
        // below before it reached a worker.
        let superseded = entry
            .gate
            .lock()
            .expect("entry gate poisoned")
            .claim
            .epoch();
        report_through_a_driven_poll(&ops, &host, &subject, &ops.off_thread_rescan_poll_batches);
        assert_eq!(
            entry.gate.lock().expect("entry gate poisoned").claim.slot(),
            Some(superseded + 1),
            "the scheduled reconcile never reached the channel"
        );

        // What a worker does with the superseded follow-up. Driving it here
        // rather than through a worker of its own is what lands the arrival
        // while the job that replaced it is still in the channel.
        run_job(&host.shared, Job::Reconcile(subject.clone(), superseded));
        let (queued, scheduled) = {
            let state = entry.gate.lock().expect("entry gate poisoned");
            (state.claim.slot(), state.claim.marker().map(Job::epoch))
        };
        assert_eq!(
            queued,
            Some(superseded + 1),
            "an arrival at a superseded epoch gave away the slot of the job that replaced it"
        );
        assert_eq!(scheduled, Some(superseded + 1));

        // The tick that would send the job in the channel a second time.
        retry_pending_dispatches(&host.shared);

        ops.attach_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &subject, TrustState::Ready);
        wait_for_state(&host, &holding, TrustState::Ready);
        wait_for_state(&host, &attaching, TrustState::Ready);
        settle();
        assert_eq!(
            ops.reconciles.load(Ordering::SeqCst),
            1,
            "a job the channel held one copy of ran twice"
        );
        let state = entry.gate.lock().expect("entry gate poisoned");
        assert!(
            state.claim.marker().is_none() && !state.claim.is_held(),
            "a second dispatch left a marker over an entry no dispatch reaches"
        );
        drop(state);
        drop(lease);
        drop(holding_lease);
        drop(attaching_lease);
    }

    /// A follow-up stands in the queue slot its own leg took and in no other.
    /// The slot an unclaimed entry holds may name a newer job the channel
    /// really holds, and an entry whose slot is given away under that job is
    /// one the next dispatcher tick sends it into the channel a second time.
    #[test]
    fn a_follow_up_send_leaves_the_slot_of_the_job_already_in_the_channel() {
        let ops = Arc::new(FakeOps::default());
        let subject = VaultName::new("a").unwrap();
        let holding = VaultName::new("b").unwrap();
        let attaching = VaultName::new("c").unwrap();
        let host =
            host_without_ambient_polling(Arc::clone(&ops), &[&subject, &holding, &attaching], 2);
        let lease = host.demand(&subject).unwrap();
        wait_for_state(&host, &subject, TrustState::Ready);

        // Both workers go to vaults that are not the one under test: the
        // channel then holds what this test sends for it, and has the room a
        // second send takes. Their claims are held by the legs blocking them,
        // so the tick below reaches the entry under test alone.
        ops.block_attach.store(true, Ordering::SeqCst);
        let holding_lease = host.demand(&holding).unwrap();
        let attaching_lease = host.demand(&attaching).unwrap();
        wait_for_attaches(&ops, 3, "both workers to be inside an attach");

        let entry = host.shared.entries.get(&subject).unwrap();
        // The epoch a leg's follow-up is sent at, superseded by the tick below
        // while the leg is between its own gate and the channel.
        let superseded = entry
            .gate
            .lock()
            .expect("entry gate poisoned")
            .claim
            .epoch();
        report_through_a_driven_poll(&ops, &host, &subject, &ops.off_thread_rescan_poll_batches);
        assert_eq!(
            entry.gate.lock().expect("entry gate poisoned").claim.slot(),
            Some(superseded + 1),
            "the scheduled reconcile never reached the channel"
        );

        dispatch_followup(&host.shared, Job::Reconcile(subject.clone(), superseded));
        assert_eq!(
            entry.gate.lock().expect("entry gate poisoned").claim.slot(),
            Some(superseded + 1),
            "a follow-up took the slot of the newer job the channel holds"
        );

        // The tick that would send the job in the channel a second time, had
        // the follow-up above left the slot free.
        retry_pending_dispatches(&host.shared);

        ops.attach_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &subject, TrustState::Ready);
        wait_for_state(&host, &holding, TrustState::Ready);
        wait_for_state(&host, &attaching, TrustState::Ready);
        settle();
        assert_eq!(
            ops.reconciles.load(Ordering::SeqCst),
            1,
            "a job the channel held one copy of ran twice"
        );
        drop(lease);
        drop(holding_lease);
        drop(attaching_lease);
    }

    /// A follow-up a full channel refuses takes the marker at the epoch the
    /// entry stands at, whatever else is standing there. A marker survives the
    /// move that hands the follow-up on only by naming work raised under an
    /// epoch the entry has already left, so an entry that keeps it owes a job
    /// every arrival discards: the work the entry really owes is dropped, and
    /// the gate is left to a marker no run gives back.
    #[test]
    fn a_refused_follow_up_takes_the_marker_from_work_the_entry_has_left() {
        let ops = Arc::new(FakeOps::default());
        let subject = VaultName::new("a").unwrap();
        let working = VaultName::new("b").unwrap();
        let waiting = VaultName::new("c").unwrap();
        let host =
            host_without_ambient_polling(Arc::clone(&ops), &[&subject, &working, &waiting], 1);
        let lease = host.demand(&subject).unwrap();
        wait_for_state(&host, &subject, TrustState::Ready);

        // The one worker goes to a vault that is not the one under test, and
        // the channel's one place goes to a job no worker is left to take:
        // every send from here is refused.
        ops.block_attach.store(true, Ordering::SeqCst);
        let working_lease = host.demand(&working).unwrap();
        wait_for_flag("attach_started", &ops.attach_started);
        let waiting_lease = host.demand(&waiting).unwrap();
        let waiting_entry = host.shared.entries.get(&waiting).unwrap();
        assert!(
            waiting_entry
                .gate
                .lock()
                .expect("entry gate poisoned")
                .claim
                .slot()
                .is_some(),
            "the attach that fills the channel never reached it"
        );

        let entry = host.shared.entries.get(&subject).unwrap();
        // The epoch the entry's marker was raised under, left behind by the
        // move that hands the follow-up below on.
        let superseded = entry
            .gate
            .lock()
            .expect("entry gate poisoned")
            .claim
            .epoch();
        {
            // The entry as a handoff leaves it: standing at the job the leg is
            // sending and holding the slot that job took, under a marker the
            // move left standing because it names another job.
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            state
                .claim
                .mark(Job::Reconcile(subject.clone(), superseded));
            state.claim.stand_at(superseded + 1);
            state.claim.take_slot(superseded + 1);
        }

        dispatch_followup(
            &host.shared,
            Job::Reconcile(subject.clone(), superseded + 1),
        );
        let (queued, scheduled) = {
            let state = entry.gate.lock().expect("entry gate poisoned");
            (state.claim.slot(), state.claim.marker().map(Job::epoch))
        };
        assert_eq!(queued, None, "a refused send kept the queue slot it took");
        assert_eq!(
            scheduled,
            Some(superseded + 1),
            "a refused follow-up left the entry owing work it has moved past"
        );

        // The channel drains, and the tick that follows sends the entry the
        // job it owes.
        ops.attach_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &waiting, TrustState::Ready);
        retry_pending_dispatches(&host.shared);
        wait_until(
            "the refused follow-up to reach a worker",
            lifecycle_wait_budget(),
            || {
                let reconciles = ops.reconciles.load(Ordering::SeqCst);
                if reconciles == 1 {
                    Observed::Met(())
                } else {
                    Observed::pending(format!("{reconciles} reconciles so far"))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        settle();
        assert_eq!(
            ops.reconciles.load(Ordering::SeqCst),
            1,
            "the follow-up the entry took its marker back for ran more than once"
        );
        drop(lease);
        drop(working_lease);
        drop(waiting_lease);
    }

    /// A leg holds its claim on the entry until the job it hands off is in the
    /// queue, so there is no instant between the two at which the entry looks
    /// idle. A dispatcher tick arriving in that window finds an entry it
    /// cannot take, and the job the leg sent still finds the attachment it was
    /// dispatched to work on.
    #[test]
    fn a_tick_cannot_claim_an_entry_whose_next_job_is_already_in_the_queue() {
        let ops = Arc::new(FakeOps::default());
        let working = VaultName::new("a").unwrap();
        let first_slot = VaultName::new("b").unwrap();
        let second_slot = VaultName::new("c").unwrap();
        let host = host_without_ambient_polling(
            Arc::clone(&ops),
            &[&working, &first_slot, &second_slot],
            2,
        );
        let lease = host.demand(&working).unwrap();
        wait_for_state(&host, &working, TrustState::Ready);

        // The leg under test: a maintenance whose own handoff drain saturates,
        // so it hands the entry a reconcile carrying no fact of its own — the
        // entry a tick would otherwise find idle and attached.
        ops.maintenance_due.store(true, Ordering::SeqCst);
        ops.block_maintenance.store(true, Ordering::SeqCst);
        poll_watchers(&host.shared);
        wait_for_flag("maintenance_started", &ops.maintenance_started);
        ops.handoff_poll_batches
            .store(HANDOFF_BATCH_LIMIT, Ordering::SeqCst);

        // Both worker slots go to vaults that are not the one under test: one
        // taken now, one waiting in the queue ahead of the handoff below. The
        // handed-off reconcile therefore stays in the queue while the test
        // drives a tick at the entry it was sent against.
        ops.block_attach.store(true, Ordering::SeqCst);
        let holding = host.demand(&first_slot).unwrap();
        wait_for_flag("attach_started", &ops.attach_started);
        ops.attach_started.store(false, Ordering::SeqCst);
        let waiting = host.demand(&second_slot).unwrap();

        ops.maintenance_release.store(true, Ordering::SeqCst);
        wait_for_flag("attach_started", &ops.attach_started);

        let claimed_before = ops.polls.lock().unwrap().get(&working).copied().unwrap();
        poll_watchers(&host.shared);
        assert_eq!(
            ops.polls.lock().unwrap().get(&working).copied().unwrap(),
            claimed_before,
            "a tick took an entry whose next job was already on its way to a worker"
        );
        assert!(
            matches!(host.state(&working), Some(TrustState::Warming { .. })),
            "the entry stopped waiting for the reconcile its leg handed it"
        );

        ops.attach_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &working, TrustState::Ready);
        assert_eq!(ops.reconciles.load(Ordering::SeqCst), 1);
        drop(lease);
        drop(holding);
        drop(waiting);
    }

    /// A rescan a watcher poll reports overflowed the watcher, so what the
    /// entry knows about the vault is unreliable until it is reread: the poll
    /// that reports it publishes the overflow before it gives the entry back,
    /// and schedules the reconcile that clears it. Coverage was never lost, so
    /// nothing recovers.
    #[test]
    fn a_polled_rescan_publishes_the_overflow_and_schedules_its_reconcile() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        // The reconcile the poll schedules is blocked, so the state read below
        // is the one the poll published rather than the one that cleared it.
        ops.block_reconcile.store(true, Ordering::SeqCst);
        report_through_a_driven_poll(&ops, &host, &name, &ops.off_thread_rescan_poll_batches);
        assert_eq!(
            host.state(&name),
            Some(TrustState::untrusted(UntrustedReason::WatcherOverflow)),
            "the reported rescan left the entry trusted while nothing had reread it"
        );

        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.reconciles.load(Ordering::SeqCst), 1);
        assert_eq!(
            ops.recovers.load(Ordering::SeqCst),
            0,
            "the facts were healed by a recovery rather than the reconcile they are owed"
        );
    }

    /// A batch reported without a rescan is work whatever it carries: the poll
    /// hands it to a reconcile, and the entry stops serving until that reconcile
    /// runs — healing, not overflowed. The batch here carries nothing at all,
    /// which is the corner of that arm: the entry publishes the phase and owes
    /// the reconcile on the strength of the report alone.
    #[test]
    fn a_polled_batch_without_a_rescan_publishes_healing() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        // The reconcile the poll schedules is blocked, so the state read below
        // is the one the poll published rather than the one that cleared it.
        ops.block_reconcile.store(true, Ordering::SeqCst);
        report_through_a_driven_poll(&ops, &host, &name, &ops.off_thread_poll_batches);
        assert_eq!(
            host.state(&name),
            Some(TrustState::warming(WarmingPhase::Healing, 0, None)),
            "the entry went on serving against facts nothing had reconciled"
        );

        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.reconciles.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn maintenance_handoff_saturation_stays_warming_until_a_followup_drain() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_maintenance.store(true, Ordering::SeqCst);
        ops.maintenance_due.store(true, Ordering::SeqCst);
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
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let lease = host.demand(&name).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_maintenance.store(true, Ordering::SeqCst);
        ops.maintenance_due.store(true, Ordering::SeqCst);
        wait_for_flag("maintenance_started", &ops.maintenance_started);
        ops.handoff_rescan_poll_batches.store(1, Ordering::SeqCst);
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
