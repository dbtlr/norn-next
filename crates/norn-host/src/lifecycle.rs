use std::collections::BTreeMap;
use std::fmt;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use norn_config::registry::Entry as Registration;
use norn_fs::{Batch, Identity, RescanScope, WatchError};
use norn_wire::{
    AttachMode, ErrorEnvelope, MaintainerIdentity, TrustState, UntrustedReason, VaultName,
    WarmingPhase, WatcherLossCause,
};

use crate::registry::{AliasConflict, RegistryRead};
use crate::reload::ReloadCandidate;
use crate::{
    ActiveFingerprints, AuthoredDrift, ReloadError, ReloadOutcome, ReloadRefusal, VaultInspection,
};

mod claim;
mod serving;

use claim::{Claim, Coverage, Leg};
use serving::{ServingRefusal, ServingSet};

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

/// The coverage a vault entry is served from, and the source of the read-only
/// snapshot handle its reads run on.
///
/// A reader is opened from live coverage and from nothing else, which is what
/// binds the handle's lifetime to the store behind it: an entry holding no
/// coverage has no way to make one, and coverage on its way back to
/// [`EntryOps::detach`] is coverage whose reader the entry has already let go
/// of.
pub trait SnapshotSource: Send + 'static {
    /// The read-only snapshot handle one entry's reads run on. The entry holds
    /// one and every read against that entry shares it, so what concurrent
    /// reads serialize against is inside the handle rather than around it.
    type Reader: Send + Sync + 'static;

    /// Open the handle this coverage serves reads from. Coverage that mints
    /// none is coverage no read reaches.
    ///
    /// This runs under the entry gate lock, on the attach leg's publication,
    /// and the lock is what the contract here is about. It blocks on no I/O
    /// beyond the open the attach has already paid for: every other holder of
    /// that entry — every demand, every reap, every read — waits behind it. And
    /// it does not panic: an unwind here poisons the gate, and a poisoned gate
    /// is an entry whose coverage reaches the ops by `Drop` rather than through
    /// [`EntryOps::detach`], with the maintainer lock and the watcher given back
    /// out of order.
    fn open_reader(&self) -> Option<Self::Reader>;
}

/// The effectful half of an entry lifecycle.
pub trait EntryOps: Send + Sync + 'static {
    type Attachment: SnapshotSource;

    /// Acquire maintainership, establish watcher coverage, and only then run
    /// one full hash-authoritative heal. After this returns, the lifecycle
    /// performs one nonblocking [`EntryOps::poll`] before it may publish Ready;
    /// raced and continuing facts then use ordinary coalesced reconciliation.
    ///
    /// The [`Registration`] is handed down rather than looked up: the vault's
    /// name, its root and its schema source arrive from the entry the lifecycle
    /// is attaching, so an implementation keeps no account of the serving set
    /// and has none to disagree with. What an attachment needs after this call
    /// — the root a reconcile scopes against and the active controls a reload
    /// compares — it keeps from what it is given here.
    fn attach(
        &self,
        registration: &Registration,
        progress: &ProgressReporter<Self::Attachment>,
    ) -> Result<Self::Attachment, JobFailure>;
    /// Apply one coalesced document envelope. A rescan widens rather than
    /// discarding uncertainty. Control-file facts do not reach this method.
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
    /// Read, validate, and apply the two authored vault control files. The
    /// default reports unsupported, which does not change the vault state.
    fn reload(
        &self,
        _: &VaultName,
        _: &mut Self::Attachment,
        _: &ProgressReporter<Self::Attachment>,
    ) -> Result<ReloadOutcome, EntryReloadFailure> {
        Err(EntryReloadFailure::Unsupported)
    }
    /// The core fingerprints active in this attachment, where it has them.
    fn active_fingerprints(&self, _: &Self::Attachment) -> Option<ActiveFingerprints> {
        None
    }
    /// Discard damaged derived state and build it from the vault again — heal
    /// rung 3, reached where an implementation reported
    /// [`JobFailure::StoreDamaged`].
    ///
    /// The attachment arrives by value because the store inside it is consumed:
    /// the connection to the damaged database closes, the file goes, and what
    /// the attachment holds afterwards is the database a create produces with
    /// the vault derived into it. Watcher coverage and maintainership stand
    /// through it — neither is what was damaged — so this is not a re-attach.
    ///
    /// A failure here gives nothing back: the implementation releases what the
    /// attachment held, and the entry publishes over coverage it no longer has.
    fn rebuild(
        &self,
        name: &VaultName,
        attachment: Self::Attachment,
        progress: &ProgressReporter<Self::Attachment>,
    ) -> Result<Self::Attachment, JobFailure>;
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
pub struct ProgressReporter<A: SnapshotSource> {
    entry: std::sync::Weak<Entry<A>>,
    epoch: u64,
}

impl<A: SnapshotSource> ProgressReporter<A> {
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

    /// Enter Lane 1 warming after a reload candidate passes core validation.
    ///
    /// This transition replaces the current trust label at the reporter's
    /// epoch. Call it only from a reload leg that was admitted while the vault
    /// was Ready. A call from another state would overwrite its trust reason.
    pub fn begin_schema_reload(&self) {
        let Some(entry) = self.entry.upgrade() else {
            return;
        };
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        if !state.claim.stands_at(self.epoch) {
            return;
        }
        state.close_reader();
        state.trust = TrustState::warming(WarmingPhase::Healing, 0, None);
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
pub struct Healing<'a, A: SnapshotSource>(&'a ProgressReporter<A>);

impl<A: SnapshotSource> Healing<'_, A> {
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

fn reporter<A: SnapshotSource>(entry: &Arc<Entry<A>>, epoch: u64) -> ProgressReporter<A> {
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
    /// A vault control file failed at a typed core reload boundary.
    Reload(ReloadError),
    /// The derived state is damaged, and no repetition of the work that met it
    /// resolves that. The store the entry holds is not a store any read can be
    /// answered from, so what the entry owes is the database-side heal rung —
    /// discard, and derive again from the vault — rather than the recovery a
    /// broken environment owes.
    StoreDamaged(String),
    WatcherTerminal(WatchError),
    LostMaintainership,
    MaintainerContended(MaintainerIdentity),
}

/// A reload adapter's answer before the lifecycle applies failure policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EntryReloadFailure {
    Unsupported,
    Runtime(JobFailure),
}

impl From<JobFailure> for EntryReloadFailure {
    fn from(failure: JobFailure) -> Self {
        Self::Runtime(failure)
    }
}

/// The immediate answer to client demand. Warming never blocks the caller.
///
/// The three park variants are what an entry nothing re-attaches answers, and
/// they answer it whatever trust state stands beside them: a park outlives the
/// release that publishes Unattached over it, so the answer names what keeps
/// the entry from re-arming rather than what its resources are doing.
///
/// Two variants name no entry at all: a demand for a name the registry does not
/// hold, and a demand for a mode this host has no lifecycle for. Both are read
/// before an entry is touched, and both answer through the one mapping every
/// other demand answers through, so a refusal the host can make is a refusal
/// the vocabulary spells.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Demand {
    State(TrustState),
    MaintainerContended(MaintainerIdentity),
    DuplicateRoot(AliasConflict),
    /// The registry's own account of a root it cannot read.
    IdentityRefused(String),
    UnknownVault,
    /// The mode the demand named, which this host holds no lifecycle for.
    UnsupportedMode(AttachMode),
}

/// The one thing a client can be told the host has gone.
///
/// Every trigger is written inside `Drop for Host`, which takes `&mut self`
/// while [`Host::demand`] takes `&self`, so no owned-host caller can observe
/// it: what rides this channel is the host being gone, never a request being
/// bad. A refusal a request earns is a [`Demand`] and is answered in the wire
/// vocabulary; a policy that describes no host is [`LifecyclePolicyError`] and
/// is answered at construction.
#[derive(Debug)]
pub enum HostError {
    WorkerStopped,
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkerStopped => f.write_str("the host worker pool stopped"),
        }
    }
}

impl std::error::Error for HostError {}

/// A [`LifecyclePolicy`] that describes no host.
///
/// These are read once, at construction, and no request can reach them: a
/// policy is the composition root's, so a caller that gets a host holds one
/// built from a policy that admitted it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecyclePolicyError {
    NoWorkerSlots,
    ZeroWatchPollInterval,
}

impl fmt::Display for LifecyclePolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoWorkerSlots => f.write_str("the host requires at least one worker slot"),
            Self::ZeroWatchPollInterval => {
                f.write_str("the host requires a nonzero watcher poll interval")
            }
        }
    }
}

impl std::error::Error for LifecyclePolicyError {}

/// One vault this host serves.
///
/// The registration is the entry's own and never read from anywhere else: what
/// the host serves, and at which root, is one fact carried beside the state
/// that serves it. Every effect the entry needs a root for — the attach, the
/// classification a recheck runs, the refusal that names an alias — reads it
/// here.
struct Entry<A: SnapshotSource> {
    /// The name this entry is served under, the root it serves, and where its
    /// schema is read from. It stands outside the gate because nothing changes
    /// it: an entry serves the registration it was inserted with for as long as
    /// the set serves the entry.
    registration: Registration,
    gate: Mutex<EntryState<A>>,
}

impl<A: SnapshotSource> Entry<A> {
    /// An entry serving `registration` and holding nothing, which is what a
    /// vault the host has never attached stands at.
    fn unattached(registration: Registration) -> Self {
        Self {
            registration,
            gate: Mutex::new(EntryState {
                trust: TrustState::Unattached,
                coverage: Coverage::none(),
                reader: None,
                pending: Batch::default(),
                active_fingerprints: None,
                last_reload_error: None,
                recovery_required: false,
                rebuild_required: false,
                recovery_demands: 0,
                recovery_generation: 0,
                identity_refused: None,
                claim: Claim::default(),
                maintainer_contended: None,
                duplicate_root: None,
                last_demand: Instant::now(),
                demand_leases: 0,
                safety_pins: 0,
                detach_due: false,
                detach_scheduled: false,
                detach_in_flight: false,
            }),
        }
    }

    /// The name this entry is served under.
    fn name(&self) -> &VaultName {
        &self.registration.name
    }
}

struct EntryState<A: SnapshotSource> {
    trust: TrustState,
    /// The entry's coverage, and who holds it.
    coverage: Coverage<A>,
    /// The read-only snapshot handle this entry's reads run on, where its
    /// coverage minted one.
    ///
    /// It sits beside the coverage rather than inside it, so a read proceeds
    /// while a lifecycle job holds the attachment: a leg takes what
    /// [`EntryState::coverage`] holds, and the handle here is untouched by
    /// that taking. What bounds its life instead is the store it was minted
    /// from — [`EntryState::install_coverage`] mints it under the lock that
    /// installs that store, and [`EntryState::close_reader`] drops it under
    /// the lock that starts the store on its way to [`EntryOps::detach`].
    ///
    /// The handle is shared rather than owned outright because a read runs
    /// outside the entry's lock: [`Host::begin_read`] clones it under the
    /// lock, and the read holds that clone until it ends. A clone in a read's
    /// hands therefore outlives the entry's own, so a read in flight goes on
    /// running against the handle it started on; the pin the same hold takes
    /// defers only the scheduling of an idle detach, and a teardown already
    /// moving runs under the read.
    reader: Option<Arc<A::Reader>>,
    pending: Batch,
    /// The core fingerprints active in the attached runtime.
    active_fingerprints: Option<ActiveFingerprints>,
    /// The last typed core error from reading, parsing, or applying vault
    /// control files. A successful candidate clears it.
    last_reload_error: Option<ReloadError>,
    recovery_required: bool,
    /// Whether the entry's derived state is damaged and owes the database-side
    /// heal rung.
    ///
    /// It is set beside every publication of
    /// [`UntrustedReason::StoreDamagedRebuilding`] — the reason an entry
    /// holding the damaged database publishes — and cleared by the rebuild that
    /// resolves it. The other damage reason,
    /// [`UntrustedReason::StoreDamagedAwaitingDemand`], sets nothing here,
    /// because the attach that publishes it acquired no store: there is nothing
    /// to discard until a demand opens the file again.
    ///
    /// The reason does not stand in for the flag. A reason is what the entry
    /// publishes at one instant and is overwritten by the warming state the
    /// rebuild runs under, while the requirement stands until the rebuild that
    /// clears it lands, and it is what every producer of work reads to learn
    /// which rung is owed. It dominates
    /// `recovery_required` wherever both stand: a store that will not answer
    /// answers no better after coverage is installed over it again, so a
    /// recovery run against damaged state is the loop this flag exists to keep
    /// the entry out of.
    rebuild_required: bool,
    /// The live demand leases asking for the recovery the entry currently owes.
    recovery_demands: usize,
    /// Which recovery requirement the demands above were raised against. A
    /// requirement that replaces another retires its demands by moving on from
    /// their generation, so a lease that asked for an earlier recovery neither
    /// satisfies this one nor discounts a lease that did ask for it.
    recovery_generation: u64,
    /// The registry's account of a root it cannot read, from the
    /// classification that refused it. While it is set the entry is parked, and
    /// the detail is what the park answers with. A demand an acquisition can
    /// follow withdraws it, and that acquisition's own classification is what
    /// decides whether it stands again.
    identity_refused: Option<String>,
    /// The entry's hold on itself: the epoch its work stands at, what holds its
    /// scheduling gate, the leg running against it, and the job holding its one
    /// queue slot.
    claim: Claim,
    /// The incumbent another process reported while holding this vault's
    /// maintainer lock. While it is set the entry is parked, and only
    /// [`Host::retry`] clears it.
    maintainer_contended: Option<MaintainerIdentity>,
    /// The conflict a classification found over this entry's root. While it is
    /// set the entry is parked, and a demand an acquisition can follow
    /// withdraws it so that acquisition can classify the root again.
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

impl<A: SnapshotSource> EntryState<A> {
    fn record_reload_error(&mut self, error: ReloadError) -> String {
        let detail = error.to_string();
        self.last_reload_error = Some(error);
        detail
    }

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

    /// Owe the database-side heal rung, and no recovery: a rebuild is what
    /// resolves damaged state, and a recovery run beside it would re-install
    /// coverage over a store that answers nothing.
    fn require_rebuild(&mut self) {
        self.rebuild_required = true;
        self.recovery_required = false;
        self.retire_recovery_demands();
    }

    /// Whether the entry owes a rung of the ladder, which is work no ordinary
    /// reconcile stands in for.
    ///
    /// The readers of this are the sites that would otherwise publish `Ready`,
    /// publish a warming state, or schedule ordinary work over an entry whose
    /// coverage or whose derived state is not what those states claim.
    fn owes_a_rung(&self) -> bool {
        self.recovery_required || self.rebuild_required
    }

    /// Owe no rebuild. Called where the derived state the entry holds is one a
    /// build from the vault has just produced.
    fn clear_rebuild(&mut self) {
        self.rebuild_required = false;
    }

    /// Owe no recovery. The demands that were waiting on one retire with it.
    fn clear_recovery(&mut self) {
        self.recovery_required = false;
        self.retire_recovery_demands();
    }

    /// Owe neither rung. Called where the coverage the requirements were raised
    /// against is not the coverage the entry holds any more: it has gone back
    /// to [`EntryOps::detach`], or an attach installed one nothing has said
    /// anything about yet.
    ///
    /// The two flags clear together because they are set against one thing. A
    /// rebuild requirement left standing across such a move names a store this
    /// entry no longer holds, and [`EntryState::owes_a_rung`] is what every
    /// publisher of `Ready` and every producer of ordinary work reads: an entry
    /// publishing `Ready` while a stale requirement stands is one no watcher
    /// poll and no maintenance tick ever schedules against again.
    fn clear_rung_requirements(&mut self) {
        self.clear_recovery();
        self.clear_rebuild();
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

    /// Install the coverage an attach acquired, and mint the reader that
    /// coverage serves reads from.
    ///
    /// The mint is this move rather than a step beside it. The lock that
    /// installs the coverage is the one that publishes the trust label the
    /// entry answers with, so the handle and the label a read pairs it with
    /// are installed together: coverage installed with an empty slot is an
    /// entry publishing a trust label no read can answer under, and a handle
    /// minted under a later lock is one minted from coverage the entry may
    /// already have given back.
    fn install_coverage(&mut self, attachment: A) {
        debug_assert!(
            self.reader.is_none(),
            "a reader stands over coverage the entry never installed"
        );
        self.reader = attachment.open_reader().map(Arc::new);
        self.coverage.install(attachment);
    }

    /// Park the coverage a rung-3 rebuild handed back, and mint the reader that
    /// coverage serves reads from.
    ///
    /// The store inside this coverage is not the store the standing handle was
    /// minted from: the rung discards the damaged database and opens the one
    /// that replaces it, so a handle kept across it reads a file nothing links
    /// to any more. Letting go and minting again are one move here for the
    /// reason [`EntryState::install_coverage`] gives: the lock that installs
    /// the coverage is the lock that publishes the trust label a read pairs the
    /// handle with.
    fn remint_coverage(&mut self, leg: u64, attachment: A) {
        self.close_reader();
        self.reader = attachment.open_reader().map(Arc::new);
        self.coverage.park_by(leg, attachment);
    }

    /// Let go of the reader this entry's coverage minted, because that
    /// coverage is on its way to [`EntryOps::detach`]. A handle the entry
    /// keeps past that is one every later read runs against a closed store.
    ///
    /// A read in flight holds its own clone of the handle and the pin that
    /// says it is running, so what this ends is the entry's hold rather than
    /// the read's.
    fn close_reader(&mut self) {
        self.reader = None;
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

    /// Whether the entry holds anything, or anything holds the entry.
    ///
    /// Every hold either side of the lifecycle can take is named here:
    /// coverage the entry holds or that is out with a leg, a claim on its
    /// scheduling gate, a leg registered against it, a job waiting in its
    /// queue slot, a release in flight, a read pinning it, and a demand lease
    /// recorded against it. An entry none of these stands over owes nothing
    /// and is owed nothing, which is what makes it removable from the serving
    /// set.
    ///
    /// Six of the limbs are reachable as the sole hold, and a case reaches
    /// each: coverage in hand, coverage out with a leg, the gate, the leg
    /// registration, the queue slot and the lease.
    /// [`EntryState::detach_in_flight`] and [`EntryState::pinned`] are defence
    /// in depth over states the other six already answer — a release in flight
    /// was begun over coverage the entry was holding, and a pin is taken either
    /// beside the coverage in the entry's own hand or by the leg that took it
    /// out — so no state this crate reaches turns on either one alone.
    ///
    /// The queue slot is a limb of its own because a slot can outlive the gate
    /// that stood when it was taken. A job leg makes its next job under
    /// [`Claim::hand_on`], which holds the gate, and the entry's lock goes back
    /// before `dispatch_handoff` takes it again for [`Claim::hand_off`]. A
    /// refusal reaching the entry in that window — `refuse_identity_error`,
    /// which the dispatcher thread reaches through
    /// `park_on_current_classification` — opens the gate, so the hand-off takes
    /// the slot through its open-gate arm and the job entering the channel is
    /// held here by the slot alone.
    fn held_by_anything(&self) -> bool {
        self.coverage.in_hand()
            || self.coverage.out_with_leg()
            || self.claim.is_held()
            || self.claim.leg().is_some()
            || self.claim.slot_taken()
            || self.detach_in_flight
            || self.pinned()
            || self.demand_leases > 0
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

    /// Withdraw the parks the registry raised, leaving the entry free to be
    /// worked again.
    ///
    /// Both are statements about roots, and a root is read by the acquisition
    /// that classifies it. Withdrawing them is therefore the caller's way of
    /// asking for that acquisition rather than a claim that the roots have
    /// changed: the attach that follows reads them and writes back whichever
    /// refusal still stands.
    ///
    /// Maintainer contention stays. No read of the registry says whether
    /// another process still holds this entry's lock, so nothing an
    /// acquisition could answer is being asked for here.
    fn retire_registry_parks(&mut self) {
        self.identity_refused = None;
        self.duplicate_root = None;
    }

    /// What a caller reads off this entry: the park it stands on, or its trust
    /// state where nothing parks it.
    ///
    /// A park outranks the label underneath it unconditionally. The park is
    /// what says whether anything more is coming, and a trust state published
    /// over one says nothing about that — a release in flight under a duplicate
    /// root publishes `Warming`, and polling that window walks out of nothing.
    /// Reading the park first is therefore what keeps the status surface and
    /// the refusal surface from describing one instant differently: both render
    /// this one demand through [`Demand::answer`], which carries no wildcard, so
    /// a park variant minted without a stance in the vocabulary does not
    /// compile rather than falling through to a label.
    fn published_demand(&self) -> Demand {
        self.parked()
            .unwrap_or_else(|| Demand::State(self.trust.clone()))
    }
}

#[derive(Clone)]
enum Job {
    Attach(VaultName, u64),
    Recover(VaultName, u64),
    Rebuild(VaultName, u64),
    Reconcile(VaultName, u64),
    Maintenance(VaultName, u64),
    Reload(VaultName, u64, mpsc::SyncSender<Result<(), ReloadRefusal>>),
    ReloadReconcile(VaultName, u64, mpsc::SyncSender<Result<(), ReloadRefusal>>),
    Detach(VaultName, u64),
}

impl Job {
    fn epoch(&self) -> u64 {
        match self {
            Self::Attach(_, epoch)
            | Self::Recover(_, epoch)
            | Self::Rebuild(_, epoch)
            | Self::Reconcile(_, epoch)
            | Self::Maintenance(_, epoch)
            | Self::Reload(_, epoch, _)
            | Self::ReloadReconcile(_, epoch, _)
            | Self::Detach(_, epoch) => *epoch,
        }
    }

    fn name(&self) -> &VaultName {
        match self {
            Self::Attach(name, _)
            | Self::Recover(name, _)
            | Self::Rebuild(name, _)
            | Self::Reconcile(name, _)
            | Self::Maintenance(name, _)
            | Self::Reload(name, _, _)
            | Self::ReloadReconcile(name, _, _)
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
        WatchError::SynchronizationExpired => WatcherLossCause::SynchronizationExpired,
    };
    UntrustedReason::watcher_lost(cause, detail)
}

/// Whether a terminal watch failure says the ground under the entry moved.
///
/// Coverage that ended because the root stopped being covered is the one
/// watcher fact about the registry rather than about the vault's documents: the
/// path the entry is registered at is not the path the coverage was installed
/// over any more, so what that path resolves to now, and which other names
/// reach it, are open questions. A classification is what asks them, and every
/// caller that reads this fact runs one. Every other watch failure is about
/// coverage over a root that is still the entry's own.
fn root_moved(error: &WatchError) -> bool {
    matches!(error, WatchError::CoverageLost(_))
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

/// The work a demand lease is owed, read off what the entry holds.
///
/// The job follows what the entry needs rather than which door the demand came
/// through, which is why one choice serves [`Host::demand`] and
/// [`schedule_demanded_work`] alike: two spellings of it are two predicates to
/// keep in step, and an entry answered one way at one door and another way at
/// the next is the divergence itself.
///
/// Coverage the entry holds is coverage its work runs against. A lease raised
/// over facts such an entry is holding is owed the reconcile that derives
/// them: the watcher, the store, the maintainer lock and the progress counted
/// against them all stand while it runs, and the reconcile is what drains the
/// entry's pending facts. Coverage the entry does not hold is what an attach
/// installs. Coverage a recovery is owed over is coverage the entry holds and
/// cannot trust, and a recover is what makes it trustworthy again — it is the
/// leg that restarts coverage the ops report terminally lost.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DemandedWork {
    Attach,
    Recover,
    Rebuild,
    Reconcile,
}

impl DemandedWork {
    /// The work this entry owes a demand lease.
    ///
    /// The reconcile arm rests on where the untrusted reasons are written:
    /// every writer of a watcher loss or an environmental refusal against an
    /// entry holding coverage sets `recovery_required` beside it, and every
    /// writer of damaged derived state over an entry holding its store sets
    /// `rebuild_required`. An overflow is
    /// the one reason that stands without either, and rereading the facts is
    /// what clears an overflow. The assertion says that invariant out loud, so
    /// a writer that stops pairing a reason with the work it owes is caught
    /// here rather than by an entry a reconcile drove to Ready over a cause
    /// nothing addressed.
    ///
    /// Damage is read before a recovery because it dominates one: a store that
    /// will not answer answers no better after coverage is installed over it
    /// again, so an entry owing both owes the rung that resolves the store.
    fn owed_by<A: SnapshotSource>(state: &EntryState<A>) -> Self {
        if !state.coverage.in_hand() {
            Self::Attach
        } else if state.rebuild_required {
            Self::Rebuild
        } else if state.recovery_required {
            Self::Recover
        } else {
            debug_assert!(
                match &state.trust {
                    TrustState::Untrusted { reason, .. } =>
                        matches!(reason, UntrustedReason::WatcherOverflow),
                    _ => true,
                },
                "an entry holding coverage and owing no recovery stands at {:?}, \
                 which a reconcile does not clear",
                state.trust
            );
            Self::Reconcile
        }
    }

    fn job(self, name: &VaultName, epoch: u64) -> Job {
        match self {
            Self::Attach => Job::Attach(name.clone(), epoch),
            Self::Recover => Job::Recover(name.clone(), epoch),
            Self::Rebuild => Job::Rebuild(name.clone(), epoch),
            Self::Reconcile => Job::Reconcile(name.clone(), epoch),
        }
    }

    /// What the entry publishes while this work is scheduled and running, or
    /// `None` where what already stands is what the work runs under.
    ///
    /// An attach and a recover both establish coverage before they read a
    /// document, so the phase they warm under is the prologue that counts
    /// nothing. A reconcile runs against coverage that is already installed and
    /// reads documents from the first instant, so what it warms out of is the
    /// facts standing in the entry — an overflow until the rescan among them is
    /// reread, and the document side of the ladder otherwise.
    ///
    /// A rebuild publishes nothing of its own, and that is the honest reading:
    /// the entry said its derived state was damaged, and it is damaged until
    /// the rung that resolves it has. Warming over the verdict would retire the
    /// reason before anything addressed it, and leave a caller unable to tell a
    /// vault being derived again from one that never lost its store.
    fn scheduled_state(self, pending: &Batch) -> Option<TrustState> {
        match self {
            Self::Attach | Self::Recover => Some(TrustState::warming(
                WarmingPhase::InstallingCoverage,
                0,
                None,
            )),
            Self::Rebuild => None,
            Self::Reconcile => Some(trust_for_pending_reconcile(pending)),
        }
    }
}

/// Schedule the work a demand lease is owed against an entry free to run it,
/// publishing the state that work warms under.
fn schedule_demand<A: SnapshotSource>(state: &mut EntryState<A>, name: &VaultName) -> Job {
    let work = DemandedWork::owed_by(state);
    if let Some(scheduled) = work.scheduled_state(&state.pending) {
        state.trust = scheduled;
    }
    state.claim.schedule(|epoch| work.job(name, epoch))
}

fn schedule_due_detach<A: SnapshotSource>(
    state: &mut EntryState<A>,
    name: &VaultName,
) -> Option<Job> {
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
/// that ends a claim without a terminal verdict answers those leases here,
/// which is what makes the claim's own completion the moment the demand is
/// honored rather than some later one. A job that fails terminally publishes
/// its refusal or park instead and deliberately schedules nothing: the held
/// lease's completion is the caller's handle on that outcome.
/// The job is [`DemandedWork::owed_by`]'s, which is the one [`Host::demand`]
/// schedules too: what the entry needs is the same fact at either door.
///
/// An entry that is serving, already working, giving its resources back,
/// parked, or owed a recovery no live lease has demanded owes an outstanding
/// lease nothing. A recovery runs only where a lease has demanded it, because a
/// terminal failure does not autonomously restart coverage. A release in flight
/// owes the lease the re-attach [`finish_release`] ends with, which is why
/// nothing is scheduled here against one.
fn schedule_demanded_work<A: SnapshotSource>(
    state: &mut EntryState<A>,
    name: &VaultName,
) -> Option<Job> {
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
    Some(schedule_demand(state, name))
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
///
/// The reader goes here, at the start of the window, rather than where the
/// resources reach the ops: this is the instant the entry stops being readable,
/// and the store the handle was minted from closes inside the window. Every
/// teardown enters here, so one site is what carries the rule for all of them.
fn begin_release<A: SnapshotSource>(state: &mut EntryState<A>) {
    // A job that lost the attachment to this leg left its marker behind for a
    // later tick, and the resources it was scheduled against are going back:
    // the marker ends with the gate, because a job nothing holds the entry for
    // is one no dispatch reaches.
    state.claim.open();
    state.close_reader();
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
///
/// The reader was let go of at the window's start and nothing minted one inside
/// it: the one mint site publishes under [`Claim::stands_at`] at its own epoch,
/// and a window is opened either by a move that superseded every such epoch
/// first or by the single leg standing at the entry's current one. The
/// assertion below is where that says itself out loud.
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
    debug_assert!(
        state.reader.is_none(),
        "a reader stands over coverage the release has already given back"
    );
    let reattach_requested = !state.recovery_required || state.recovery_demanded();
    state.claim.end_leg(epoch);
    // The coverage this leg was handed is with the ops now, so the entry holds
    // none: what a leg parked back before it reached here is the entry's own
    // and stands.
    state.coverage.released_by(epoch);
    state.detach_in_flight = false;
    state.pending = Batch::default();
    state.active_fingerprints = None;
    // The derived state a damage verdict was about is with the ops, and an
    // attach opens the database again from nothing: a requirement kept here
    // would name a store this entry no longer holds.
    state.clear_rung_requirements();
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
fn restore_lost_claim<A: SnapshotSource>(state: &mut EntryState<A>, job: Job) {
    if !state.pinned() {
        state.claim.open();
        return;
    }
    state.claim.restore(job);
}

/// Give back the queue slot a send took, where the entry still holds it for
/// that send.
fn release_queue_slot<A: SnapshotSource>(entry: &Arc<Entry<A>>, epoch: u64) {
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
    dispatch_taken_job(shared, entry, job)
}

/// Send a job whose queue slot the caller already took.
///
/// Keeping refusal cleanup with the send makes the take-then-shutdown window
/// directly exercisable: every exit on which no channel accepted the job gives
/// back only the slot still naming that job's epoch.
fn dispatch_taken_job<O: EntryOps>(
    shared: &Arc<Shared<O>>,
    entry: &Arc<Entry<O::Attachment>>,
    job: Job,
) -> Result<(), HostError> {
    if shared.shutting_down.load(Ordering::SeqCst) {
        release_queue_slot(entry, job.epoch());
        return Err(HostError::WorkerStopped);
    }
    let jobs = shared.jobs.lock().expect("job sender poisoned");
    let Some(jobs) = jobs.as_ref() else {
        drop(jobs);
        release_queue_slot(entry, job.epoch());
        return Err(HostError::WorkerStopped);
    };
    match jobs.try_send(job.clone()) {
        Ok(()) => Ok(()),
        Err(mpsc::TrySendError::Full(_)) => {
            release_queue_slot(entry, job.epoch());
            Ok(())
        }
        Err(mpsc::TrySendError::Disconnected(_)) => {
            release_queue_slot(entry, job.epoch());
            Err(HostError::WorkerStopped)
        }
    }
}

fn retry_pending_dispatches<O: EntryOps>(shared: &Arc<Shared<O>>) {
    for entry in shared.entries.snapshot() {
        let _ = dispatch_pending(shared, &entry);
    }
}

/// Park every alias of a root reached under more than one name, and give back
/// what each of them is holding.
///
/// The refusal routes on who holds the entry's resources, because that is who
/// gives them back:
///
/// - The entry holds its own coverage. The refusal takes it and is the release
///   that gives it back, whatever leg is registered against the entry — a leg
///   that no longer holds the entry's coverage is not what ends it, and the
///   registration ends here with the taking. Leaving the release to that leg
///   would leave a refused entry holding coverage every producer that reads
///   parked coverage goes on taking.
/// - A leg holds the resources, or is about to: the refusal opens the release
///   window and the leg's own end closes it. The window is what says the
///   resources are coming back, so nothing re-arms over them and no lease is
///   answered before they are.
/// - Neither holds anything. The entry is released, and says so.
///
/// A release already in flight is left alone: it has published the phase that
/// names it and publishes Unattached itself once the resources are back.
///
/// A window opened over a leg is closed by the leg it was opened over, so
/// opening one where no leg is registered would leave a window nothing can
/// close. Coverage out with a leg stands under that leg's own registration:
/// every taker registers one under the lock it takes the coverage under, and
/// the two releases that take coverage without registering — the first route
/// above and [`Job::Detach`] — set the flag this pass has already returned on.
/// The assertion below is where that invariant is stated.
fn refuse_conflict<O: EntryOps>(shared: &Arc<Shared<O>>, conflict: &AliasConflict) {
    let entries = conflict
        .aliases()
        .iter()
        .filter_map(|name| shared.entries.get(name))
        .collect::<Vec<_>>();
    let mut states = entries
        .iter()
        .map(|entry| entry.gate.lock().expect("entry gate poisoned"))
        .collect::<Vec<_>>();
    let mut releasing = Vec::new();
    for (entry, state) in entries.iter().zip(&mut states) {
        state.claim.invalidate();
        state.pending = Batch::default();
        // Every route below ends with the entry's coverage back at the ops, so
        // neither rung is owed against the store the conflict took away.
        state.clear_rung_requirements();
        state.identity_refused = None;
        state.duplicate_root = Some(conflict.clone());
        if state.detach_in_flight {
            continue;
        }
        // The gate goes back on every route below: a refused entry owes no
        // work, so a job it had scheduled or sent is one nothing is left
        // holding the entry for, and a gate left standing for it is an entry
        // no later dispatch reaches.
        state.claim.open();
        let epoch = state.claim.epoch();
        debug_assert!(
            !state.coverage.out_with_leg() || state.claim.leg().is_some(),
            "coverage out with a leg no registration names, so no leg closes the window below"
        );
        match state.coverage.take(epoch) {
            // The entry holds the coverage the conflict invalidates, so this is
            // a teardown like any other: the release below is what holds it
            // from here, the resources go back, and Unattached is published
            // after they have.
            Some(attachment) => {
                state.claim.end_running_leg();
                begin_release(state);
                releasing.push((Arc::clone(entry), epoch, attachment));
            }
            // The resources are out with a leg, or with one that has yet to
            // take them. The window opens here and the leg's own end closes it,
            // so nothing is published as released before the leg gives them
            // back.
            None if state.coverage.out_with_leg() || state.claim.leg().is_some() => {
                begin_release(state);
            }
            // Nothing is held, so the entry is already released and can say so.
            None => state.trust = TrustState::Unattached,
        }
    }
    drop(states);
    for (entry, epoch, attachment) in releasing {
        finish_release(shared, &entry, entry.name(), epoch, Some(attachment));
    }
}

/// Park an entry on the registry's own account of a root it cannot read, and
/// give back whatever coverage the entry is still holding.
///
/// The park is [`park_identity_refusal`]'s write, so one fact has one spelling
/// however it is found; what this path adds around it is the give-back below.
///
/// A conflict park standing over the entry stands through this one. The read
/// that reaches here classified nothing — it failed on this root before it
/// could say what any root resolves to — so it neither confirms nor contradicts
/// a conflict, and [`EntryState::parked`] ranks the conflict first for exactly
/// that reason. A read that can reach the root again is what retires it.
///
/// What the refusal gives back is what the entry itself holds, whatever leg is
/// registered against it: a leg standing over parked coverage holds none, so
/// leaving the release to that leg would leave a refused entry serving the
/// coverage every producer that reads parked coverage goes on taking. The
/// registration ends with the taking, for the reason [`Claim::end_running_leg`]
/// gives.
///
/// Coverage out with a leg is that leg's, and comes back where the leg ends:
/// the invalidation above moves the entry past every leg's epoch, so each one
/// hands its coverage up rather than parking it back here. A release already in
/// flight holds the coverage the same way and publishes its own end.
///
/// The entry stays untrusted and owing a recovery once the coverage is back,
/// which is why the release here is the ops alone: this is a park with the
/// resources given back under it, not a teardown, and a demand asking for
/// coverage again is what serves the entry.
fn refuse_identity_error<O: EntryOps>(shared: &Arc<Shared<O>>, name: &VaultName, detail: String) {
    let Some(entry) = shared.entries.get(name) else {
        return;
    };
    let attachment = {
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        state.claim.invalidate();
        state.pending.merge(Batch::rescan(RescanScope::Vault));
        state.require_recovery();
        // The coverage reaches the ops on both routes below, so a rebuild the
        // entry owed is owed against a store it is not holding any more. What
        // it owes from here is the recovery raised above, and a requirement
        // left standing beside that one is one nothing clears: this path gives
        // its coverage back without the release that would.
        state.clear_rebuild();
        park_identity_refusal(&mut state, detail);
        // The refusal opens no release window, so the reader is let go of
        // here: the coverage reaches the ops on both routes below — given up
        // to them under this lock, or handed to them where the leg holding it
        // ends — and a handle the entry kept would outlast the store either
        // way.
        state.close_reader();
        match state.coverage.give_up() {
            // The entry holds its own coverage, whatever leg is registered
            // against it. The refusal gives it back, and the registration ends
            // with the taking: what would otherwise stand is a leg recorded as
            // holding coverage the entry no longer has.
            Some(attachment) => {
                state.claim.end_running_leg();
                state.claim.open();
                Some(attachment)
            }
            // Coverage out with a leg is that leg's, and comes back where the
            // leg ends. The gate stays where it is: a leg running against the
            // entry is holding it, and a release in flight is holding the entry
            // for the publication it ends with.
            None => {
                if state.claim.leg().is_none() && !state.detach_in_flight {
                    state.claim.open();
                }
                None
            }
        }
    };
    if let Some(attachment) = attachment {
        shared.ops.detach(name, attachment);
    }
}

/// Park an entry on the registry's own account of a root it cannot read, where
/// the entry holds nothing for the refusal to give back.
///
/// One fact takes one park wherever it is found. The detail is what
/// [`EntryState::parked`] answers with and what the trust state beside it
/// carries, so a lease reports the refusal the same way whether the read that
/// found it was an attach's or a watcher signal's. While the park stands
/// nothing schedules against the entry, so an attach a registry refusal ended
/// is not one the next ambient poll tick arms again; the demand that withdraws
/// the park is what asks for the root to be read again.
///
/// A leg publishing under its own claim writes the park here rather than
/// through [`refuse_identity_error`], which invalidates the claim and leaves
/// the leg's own [`Claim::stands_at`] false for the publication it is in the
/// middle of.
fn park_identity_refusal<A: SnapshotSource>(state: &mut EntryState<A>, detail: String) {
    state.identity_refused = Some(detail.clone());
    state.trust = TrustState::untrusted(UntrustedReason::environmental_refusal(detail));
}

/// Classify one entry's root against every root the host serves, and park the
/// entry on whatever the read refuses.
///
/// This raises parks and retires none. A park is a statement that the entry
/// may not be served, and what withdraws it is the acquisition that reads the
/// root again under the gate it claims an identity in: [`Host::demand`]
/// retires the registry's parks and schedules that acquisition, and the
/// acquisition either leaves them retired by reaching Ready or writes back the
/// refusal that still stands. A sweep that retired a park here would answer a
/// question no acquisition asked, on a read no acquisition is bound by.
///
/// Two legs acquire coverage. [`Job::Attach`] installs it where the entry
/// holds none, reading the root under the gate itself so the identity it
/// claims and the conflict it is judged for come from one read.
/// [`Job::Recover`] installs it again over coverage the ops reported
/// terminally lost, and reads the root through this. Neither meets a standing
/// registry park — a park gives the entry's coverage back, so the work a
/// parked entry is owed once the park is withdrawn is always the attach.
///
/// Both refusals the read can raise are acted on under the attach gate, so a
/// classification and the acquisition claims it is judged against cannot
/// interleave: the conflict this refuses is the conflict every alias in it is
/// refused for, whichever thread found it.
///
/// Maintainer contention is untouched: the registry says nothing about another
/// process's lock, so nothing read here can raise or retire it.
///
/// The park this leaves behind is the whole of the answer, so nothing is
/// reported back: the caller reads [`EntryState::parked`] like every other
/// reader does, and one park is answered by one predicate.
fn park_on_current_classification<O: EntryOps>(shared: &Arc<Shared<O>>, name: &VaultName) {
    let _attach_guard = shared.attach_gate.lock().expect("attach gate poisoned");
    match shared.entries.recheck(name) {
        Ok(reading) => {
            if let Some(conflict) = &reading.conflict {
                refuse_conflict(shared, conflict);
            }
        }
        Err(refusal) => refuse_identity_error(shared, name, refusal.to_string()),
    }
}

/// Serve one more vault, and classify the root it arrives with against the
/// roots already served.
///
/// The classification is part of the join rather than a step after it: a name
/// arriving over a root another name already reaches puts both of them in a
/// conflict, and the incumbent is serving that root right now — nothing else
/// would read it again until that entry re-attached, which a served entry has
/// no reason to do.
///
/// A departure needs no such read. Taking a name out of an alias group only
/// shrinks it, so no departure can raise a refusal, and the parks the group
/// still stands under are retired by the acquisitions the remaining names'
/// demands schedule.
///
/// This is the seam the registration verb Layer 3's product surface offers
/// lands on; nothing in this crate outside its own cases reaches it yet.
#[cfg_attr(not(test), allow(dead_code))]
fn serve<O: EntryOps>(
    shared: &Arc<Shared<O>>,
    registration: Registration,
) -> Result<(), ServingRefusal> {
    let name = registration.name.clone();
    shared.entries.insert(registration)?;
    park_on_current_classification(shared, &name);
    Ok(())
}

struct Shared<O: EntryOps> {
    /// The one collection of vaults this host serves. It answers which names
    /// exist and where their roots are, so nothing here reads a registration
    /// off a second account that could disagree with it.
    ///
    /// The set's lock is never taken while an entry gate is held. Every read
    /// here clones the entry out and lets the set's lock go before it takes
    /// that entry's gate, and [`ServingSet::remove`] is the one move that holds
    /// both — in that order, the set and then the gate.
    entries: ServingSet<O::Attachment>,
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

/// One read's hold on a vault entry.
///
/// The handle and the trust label come out of one hold of the entry gate lock,
/// so the label a read answers under and the snapshot it answers from describe
/// one instant. Which labels answer and which refuse is
/// [`TrustState::refusal`]'s answer, given beside the states themselves in
/// `norn-wire`, and it is read there rather than restated here.
///
/// The hold pins the entry the way a leg running outside the lock does, so a
/// teardown that reads the pin schedules nothing while a read is in flight, and
/// the pin goes back where the hold is dropped — a read that ends, and a read
/// that unwinds, end their hold on the entry alike.
pub struct ReadHold<A: SnapshotSource> {
    entry: Arc<Entry<A>>,
    reader: Arc<A::Reader>,
    trust: TrustState,
}

impl<A: SnapshotSource> ReadHold<A> {
    /// The trust label the entry stood at when this read took its hold.
    pub fn trust(&self) -> &TrustState {
        &self.trust
    }

    /// The handle this read runs on. It is the entry's own, shared with every
    /// other read against that entry, and it stays open for as long as this
    /// hold does.
    pub fn reader(&self) -> &A::Reader {
        &self.reader
    }
}

impl<A: SnapshotSource> Drop for ReadHold<A> {
    fn drop(&mut self) {
        let mut state = self.entry.gate.lock().expect("entry gate poisoned");
        state.unpin();
    }
}

/// One client operation's lifecycle guard and immediate trust answer.
/// Dropping it ends the demand lease and starts a fresh idle interval.
pub struct DemandLease<O: EntryOps> {
    outcome: Demand,
    /// The vault this lease was demanded under. It is the lease's own whether
    /// or not an entry stands behind it, so a refusal answered here names the
    /// vault that was asked for rather than a name supplied beside it.
    name: VaultName,
    /// The host the lease is recorded against, and nothing where the name
    /// reaches no entry: an unregistered name records no lease and gives none
    /// back.
    held: Option<Arc<Shared<O>>>,
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
        let Some(shared) = &self.held else {
            return self.outcome.clone();
        };
        let Some(entry) = shared.entries.get(&self.name) else {
            return Demand::UnknownVault;
        };
        entry
            .gate
            .lock()
            .expect("entry gate poisoned")
            .published_demand()
    }

    /// This lease's current completion in the wire vocabulary: the trust state
    /// it answers with, or the refusal it is.
    ///
    /// The name a refusal is answered under is the lease's own, so a client
    /// reading `host/unknown-vault` reads the name this lease was demanded
    /// under and no other. [`Demand::answer`] takes that name as an argument
    /// because a demand carries no entry to read it off; a lease does, which is
    /// why this is the entry point a caller holding one uses.
    pub fn answer(&self) -> Result<TrustState, ErrorEnvelope> {
        self.completion().answer(&self.name)
    }
}

impl<O: EntryOps> Drop for DemandLease<O> {
    fn drop(&mut self) {
        let Some(shared) = self.held.take() else {
            return;
        };
        let Some(entry) = shared.entries.get(&self.name) else {
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
        for entry in self.shared.entries.snapshot() {
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            state.claim.invalidate();
            state.claim.open();
            state.pending = Batch::default();
            if state.detach_in_flight {
                // A release is already under way with this entry's resources.
                // The joins below wait for the leg running it, and whatever it
                // leaves in the entry is given back after them.
                begin_release(&mut state);
                releasing.push((Arc::clone(&entry), None));
                continue;
            }
            match state.coverage.give_up() {
                // The entry holds its own coverage, whatever leg is registered
                // against it: a leg standing over coverage it has not taken
                // gives none back, so the destruction takes it and the pass
                // below is what gives it back.
                Some(attachment) => {
                    state.claim.end_running_leg();
                    begin_release(&mut state);
                    releasing.push((Arc::clone(&entry), Some(attachment)));
                }
                // A leg is holding this entry's resources, or is about to take
                // them. The joins below wait for it, and whatever it leaves in
                // the entry is given back after them.
                None if state.coverage.out_with_leg() || state.claim.leg().is_some() => {
                    begin_release(&mut state);
                    releasing.push((Arc::clone(&entry), None));
                }
                // Nothing is held, so the entry is already released and can
                // say so.
                None => state.trust = TrustState::Unattached,
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
        for (entry, attachment) in releasing {
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
                self.shared.ops.detach(entry.name(), attachment);
            }
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            // Coverage still out with a leg is that leg's to give back, and the
            // window stays open for it: the leg reaches the same release every
            // other one does and publishes there, once the resources are back.
            // The joins above are what leaves nothing out on the ordinary path
            // — a leg that has ended holds nothing — so what stands here is a
            // leg destruction cannot wait for, which is destruction re-entered
            // from an [`EntryOps`] callback on the very thread that leg runs
            // on.
            if state.coverage.out_with_leg() {
                continue;
            }
            state.detach_in_flight = false;
            state.trust = TrustState::Unattached;
        }
    }
}

impl<O: EntryOps> Host<O> {
    pub fn new(
        registry: RegistryRead,
        ops: O,
        policy: LifecyclePolicy,
    ) -> Result<Self, LifecyclePolicyError> {
        if policy.worker_slots == 0 {
            return Err(LifecyclePolicyError::NoWorkerSlots);
        }
        if policy.watch_poll_interval.is_zero() {
            return Err(LifecyclePolicyError::ZeroWatchPollInterval);
        }
        // Startup takes the same seam a later registration does: every vault in
        // the set arrived through one insertion, so a vault gained while the
        // host runs is served exactly as a vault read at startup is.
        let entries = ServingSet::new();
        for registration in registry.into_entries() {
            entries
                .insert(registration)
                .expect("the registrations are keyed by name, so no two of them name one entry");
        }
        let (jobs, receiver) = mpsc::sync_channel(policy.worker_slots);
        let receiver = Arc::new(Mutex::new(receiver));
        let shared = Arc::new(Shared {
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

    /// Where one entry stands: the trust state it answers with, or the refusal
    /// it is.
    ///
    /// A name the host serves no entry under is `host/unknown-vault`, which is
    /// the vocabulary's own spelling for the ask having no entry behind it.
    /// Every other answer is `EntryState::published_demand` rendered through
    /// [`Demand::answer`] — the same demand, through the same mapping, that a
    /// lease answers with. That shared rendering is what keeps this surface
    /// from contradicting the one a lease reports: an entry standing on a park
    /// refuses here with the park's own code and typed detail, rather than with
    /// a reason read off the label underneath it.
    ///
    /// Reading the settled label from underneath a park is therefore not
    /// something this answers. A park is a fact about whether the entry may be
    /// served at all, and the label beneath it is a separate question this
    /// surface does not take.
    pub fn state(&self, name: &VaultName) -> Result<TrustState, ErrorEnvelope> {
        self.shared
            .entries
            .get(name)
            .map_or(Demand::UnknownVault, |entry| {
                entry
                    .gate
                    .lock()
                    .expect("entry gate poisoned")
                    .published_demand()
            })
            .answer(name)
    }

    /// The retained internal status facts for one served vault.
    pub fn inspect(&self, name: &VaultName) -> Option<VaultInspection> {
        self.shared.entries.get(name).map(|entry| {
            let state = entry.gate.lock().expect("entry gate poisoned");
            VaultInspection {
                trust: state.trust.clone(),
                active_fingerprints: state.active_fingerprints,
                last_reload_error: state.last_reload_error.clone(),
            }
        })
    }

    /// Compare the authored control files with the active fingerprints now.
    pub fn authored_drift(&self, name: &VaultName) -> Option<AuthoredDrift> {
        let entry = self.shared.entries.get(name)?;
        let active = entry
            .gate
            .lock()
            .expect("entry gate poisoned")
            .active_fingerprints;
        let Some(active) = active else {
            return Some(AuthoredDrift::Inactive);
        };
        Some(
            match ReloadCandidate::authored_fingerprints(&entry.registration) {
                Ok(authored) if authored == active => AuthoredDrift::Current,
                Ok(_) => AuthoredDrift::ReloadPending,
                Err(error) => AuthoredDrift::Unreadable(error),
            },
        )
    }

    /// Request one explicit reload and wait until the vault has its outcome.
    pub fn reload(&self, name: &VaultName) -> Result<(), ReloadRefusal> {
        let Some(entry) = self.shared.entries.get(name) else {
            return Err(ReloadRefusal::UnknownVault);
        };
        let (reply, answer) = mpsc::sync_channel(1);
        {
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            if state.trust != TrustState::Ready
                || !state.coverage.in_hand()
                || state.claim.is_held()
                || state.detach_in_flight
            {
                return Err(ReloadRefusal::Unavailable(state.trust.clone()));
            }
            state
                .claim
                .schedule(|epoch| Job::Reload(name.clone(), epoch, reply));
        }
        dispatch_pending(&self.shared, &entry).map_err(|_| ReloadRefusal::HostStopped)?;
        answer.recv().unwrap_or(Err(ReloadRefusal::HostStopped))
    }

    /// How many classifications this host has run against its serving set.
    ///
    /// One classification stats every root the host serves, so this is the whole
    /// filesystem cost the registry itself carries: a path that moves the
    /// counter spent those stats, and a path that leaves it standing spent none.
    /// It is cumulative, so what one act cost is the difference between a
    /// reading before it and one after.
    ///
    /// **Behind `induced-failure`, with the rest of the harness-reachable
    /// surface.** Nothing a client asks for is answered from this number: it
    /// says what a host did rather than what a vault holds, and the suites that
    /// read it are the ones that assert an act really stated the roots it
    /// claims to have stated.
    #[cfg(feature = "induced-failure")]
    pub fn classifications(&self) -> usize {
        self.shared.entries.classifications()
    }

    /// How many live demand leases are waiting on the recovery `name`'s entry
    /// owes, and nothing where the host serves no such name.
    ///
    /// A recovery is asked for rather than scheduled outright, and this is what
    /// says how many callers are asking. Zero beside an entry that owes one is a
    /// requirement nobody is waiting on; zero beside an entry that owes none is
    /// an entry with nothing to ask of it, and the two are told apart by
    /// [`Host::state`].
    ///
    /// **Behind `induced-failure`.** A client that wants to know what an entry
    /// owes reads [`Host::state`]; the count of who is waiting is an internal
    /// bookkeeping fact, opened here for the suites that assert on it.
    #[cfg(feature = "induced-failure")]
    pub fn recovery_demands(&self, name: &VaultName) -> Option<usize> {
        self.shared.entries.get(name).map(|entry| {
            entry
                .gate
                .lock()
                .expect("entry gate poisoned")
                .recovery_demands
        })
    }

    /// Record client demand and, where necessary, start one asynchronous job
    /// against the entry. The call never waits on that job: what it answers
    /// with is the state the work it started runs under.
    ///
    /// **The call stats nothing.** It reads the entry's own lock and the
    /// serving set's, and no root — not this entry's, and not the registry's.
    /// Identity is classified where coverage is acquired and where a signal
    /// says the ground under live coverage moved, and both of those are off
    /// this path. The cost of a demand is therefore flat in the number of
    /// vaults the host serves.
    ///
    /// The registry's parks do not answer here either. A demand over an entry
    /// parked on a duplicate root or a root the registry could not read is a
    /// demand for that root to be read again: the park is withdrawn, an attach
    /// is scheduled, and the classification that attach runs under the identity
    /// gate is what clears the park or writes back the refusal that still
    /// stands. So the refusal a caller reads is one a read established rather
    /// than one a cache kept.
    ///
    /// Maintainer contention is the park that does answer here. No read this
    /// host performs says whether another process still holds the lock, so
    /// there is no acquisition to schedule and nothing for one to adjudicate;
    /// [`Host::retry`] is what a caller says otherwise with. A contended entry
    /// therefore keeps the registry's parks too: they are withdrawn for the
    /// acquisition that reads those roots again, and no acquisition follows a
    /// demand the contention answers.
    ///
    /// The mode says how the derived state this demand asks for is held, and it
    /// is answered before any lease is recorded: a demand this host has no
    /// lifecycle for is refused rather than served under another mode, and it
    /// leaves the entry standing exactly where it found it, parks included. The
    /// refusal is a lease holding nothing, on the same terms an unregistered
    /// name is answered under: nothing is recorded against an entry, so nothing
    /// is withdrawn when the lease is dropped.
    pub fn demand(&self, name: &VaultName, mode: AttachMode) -> Result<DemandLease<O>, HostError> {
        if !matches!(mode, AttachMode::Durable) {
            return Ok(DemandLease {
                outcome: Demand::UnsupportedMode(mode),
                name: name.clone(),
                held: None,
                recovery_demand: None,
            });
        }
        let Some(entry) = self.shared.entries.get(name) else {
            return Ok(DemandLease {
                outcome: Demand::UnknownVault,
                name: name.clone(),
                held: None,
                recovery_demand: None,
            });
        };
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        state.demand_leases += 1;
        let recovery_demand = state.demand_recovery();
        state.detach_due = false;
        if state.detach_scheduled && !state.detach_in_flight {
            state.claim.invalidate();
            state.claim.open();
            state.detach_scheduled = false;
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
        // The registry's parks are withdrawn here, so that the acquisition this
        // demand asks for is the read that answers them. The acquisition runs
        // from this call where the schedule above admits it, and from the
        // release in flight otherwise: `finish_release` re-arms an attach for a
        // standing lease over an entry no park is left on.
        //
        // A contended entry keeps every park it stands on. The contention
        // answers this demand and no acquisition follows it, so a withdrawal
        // here would drop a conflict nothing is coming to read again — and
        // leave the aliases of one root disagreeing about it.
        if state.maintainer_contended.is_none() {
            state.retire_registry_parks();
        }
        // A parked entry is one nothing re-attaches, so the lease is recorded
        // and answered with the park itself rather than with a trust state that
        // says nothing about why no work follows it. The arm is the park's own,
        // because the lease it returns is the one shape this call has that
        // schedules nothing; the two arms below answer through
        // `EntryState::published_demand`, so which of a park and a label a
        // caller is handed is settled in the one place that ranks them.
        if let Some(park) = state.parked() {
            drop(state);
            return Ok(DemandLease {
                outcome: park,
                name: name.clone(),
                held: Some(Arc::clone(&self.shared)),
                recovery_demand,
            });
        }
        if schedule {
            // The job is the one `DemandedWork::owed_by` names, and
            // `schedule_demanded_work` schedules that same job: what the entry
            // needs is one fact, read the same way at either door.
            schedule_demand(&mut state, name);
            let answer = state.published_demand();
            drop(state);
            dispatch_pending(&self.shared, &entry)?;
            return Ok(DemandLease {
                outcome: answer,
                name: name.clone(),
                held: Some(Arc::clone(&self.shared)),
                recovery_demand,
            });
        }
        let answer = state.published_demand();
        drop(state);
        Ok(DemandLease {
            outcome: answer,
            name: name.clone(),
            held: Some(Arc::clone(&self.shared)),
            recovery_demand,
        })
    }

    /// Explicitly retry a demand whose prior completion reported a park.
    ///
    /// Contention is the park nothing else retires: no read this host performs
    /// says whether another process still holds this entry's maintainer lock, so
    /// a caller asking again is the whole of the evidence that it may be tried.
    /// The parks the registry raises are the demand's own to withdraw, and the
    /// attach it schedules is the read that adjudicates them, so this call adds
    /// nothing for them.
    ///
    /// The retry carries the mode the demand it stands in for would carry: a
    /// park is retired for the demand that follows it, and that demand asks for
    /// its derived state the same way any other does. A mode this host has no
    /// lifecycle for is refused here before the contention park is retired, so
    /// a retry the mode refuses retires nothing.
    pub fn retry(&self, name: &VaultName, mode: AttachMode) -> Result<DemandLease<O>, HostError> {
        if !matches!(mode, AttachMode::Durable) {
            return Ok(DemandLease {
                outcome: Demand::UnsupportedMode(mode),
                name: name.clone(),
                held: None,
                recovery_demand: None,
            });
        }
        if let Some(entry) = self.shared.entries.get(name) {
            entry
                .gate
                .lock()
                .expect("entry gate poisoned")
                .maintainer_contended = None;
        }
        self.demand(name, mode)
    }

    /// Take one read's hold on an entry: the handle its reads run on, the
    /// trust label it answers under, and the pin that keeps the entry standing
    /// while it runs.
    ///
    /// All three come out of one hold of the entry gate lock. That is what
    /// couples the label to the handle — a label read under a second lock
    /// describes a different instant from the snapshot beside it — and what
    /// makes the pin cover the whole of the read: the hold is taken before the
    /// lock goes back, so no teardown reads an unpinned entry between the two.
    ///
    /// An entry with no handle in its slot answers none. It is holding no
    /// coverage, its coverage is on its way back, or the coverage it holds
    /// mints no reader.
    pub fn begin_read(&self, name: &VaultName) -> Option<ReadHold<O::Attachment>> {
        let entry = self.shared.entries.get(name)?;
        let (reader, trust) = {
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            let reader = Arc::clone(state.reader.as_ref()?);
            let trust = state.trust.clone();
            state.pin();
            (reader, trust)
        };
        Some(ReadHold {
            entry,
            reader,
            trust,
        })
    }

    /// Schedule expired entries for teardown. Safety-pinned work is allowed to
    /// finish; its release performs the expired detach immediately.
    pub fn reap_idle(&self, now: Instant) -> Result<(), HostError> {
        reap_idle_shared(&self.shared, now)
    }
}

fn reap_idle_shared<O: EntryOps>(shared: &Arc<Shared<O>>, now: Instant) -> Result<(), HostError> {
    let mut entries = Vec::new();
    for entry in shared.entries.snapshot() {
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        if state.demand_leases == 0
            && (state.coverage.in_hand() || state.pinned())
            && now.saturating_duration_since(state.last_demand) >= shared.idle_after
        {
            state.detach_due = true;
            if schedule_due_detach(&mut state, entry.name()).is_some() {
                drop(state);
                entries.push(entry);
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
    for entry in shared.entries.snapshot() {
        let entry = &entry;
        let name = entry.name();
        let (mut attachment, epoch) = {
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            if state.claim.is_held() {
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
        // The clock this reads runs whether or not the watcher has anything to
        // say, so the question is asked on every poll the coverage survived and
        // both arms below act on the answer. Maintenance a poll only scheduled
        // where the watcher reported nothing would be maintenance an entry
        // under sustained traffic never runs.
        let maintenance_due = result.is_ok() && shared.ops.maintenance_due(name, &attachment);
        let mut schedule = None;
        let mut stale = None;
        let mut release = None;
        let mut reclassify = false;
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
                        if maintenance_due && !state.owes_a_rung() {
                            schedule = Some(
                                state
                                    .claim
                                    .schedule(|epoch| Job::Maintenance(name.clone(), epoch)),
                            );
                        }
                    }
                    Ok(Some(batch)) => {
                        state.claim.end_poll(epoch);
                        let rescan = !batch.rescans().is_empty();
                        state.pending.merge(batch);
                        if !state.owes_a_rung() {
                            state.trust = if rescan {
                                TrustState::untrusted(UntrustedReason::WatcherOverflow)
                            } else {
                                TrustState::warming(WarmingPhase::Healing, 0, None)
                            };
                        }
                        state.coverage.park_by(epoch, attachment);
                        if !state.owes_a_rung() {
                            // Due maintenance takes the claim ahead of the
                            // reconcile, and the facts this poll carried wait
                            // for it in the pending set: the maintenance leg
                            // reads that set when it ends and hands the
                            // reconcile on. One claim carries both, so a poll
                            // that carried facts schedules the maintenance it
                            // found due rather than dropping the verdict. The
                            // polls that never see the entry at all — the ones
                            // a held claim skips — are answered by the
                            // reconcile leg, which reads the same clocks on
                            // every turn it takes.
                            let due = maintenance_due;
                            schedule = Some(state.claim.schedule(|epoch| {
                                if due {
                                    Job::Maintenance(name.clone(), epoch)
                                } else {
                                    Job::Reconcile(name.clone(), epoch)
                                }
                            }));
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
                        reclassify = root_moved(&error);
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
                    Err(JobFailure::Reload(error)) => {
                        state.claim.drop_marker();
                        state.claim.end_poll(epoch);
                        state.require_recovery_keeping_demands();
                        state.pending.merge(Batch::rescan(RescanScope::Vault));
                        let detail = state.record_reload_error(error);
                        state.trust =
                            TrustState::untrusted(UntrustedReason::environmental_refusal(detail));
                        state.coverage.park_by(epoch, attachment);
                    }
                    // Damaged derived state is not what a poll retries into.
                    // The entry keeps its coverage — the watcher and the
                    // maintainer lock are not what was damaged — publishes the
                    // damage, and schedules the rung that resolves it. No
                    // rescan is merged and no recovery is owed: the rebuild's
                    // own heal reads the whole vault, which is more than any
                    // rescan asks for.
                    //
                    // No poll implementation reports damage today:
                    // `ProductionEntryOps::poll` reads the maintainer lock and
                    // the watcher queue and touches no store, so the fake ops
                    // are what exercise this route. It stands because the
                    // verdict is the seam's, not one leg's: `JobFailure` is one
                    // vocabulary across every `EntryOps` method, and the
                    // lifecycle decides what each word means wherever it can
                    // arrive. Folding damage into the environmental arm above
                    // would be the decision, and it is the wrong one — a poll
                    // that ever reported damage would be answered by the
                    // recovery ladder that re-installs coverage over the same
                    // database.
                    Err(JobFailure::StoreDamaged(detail)) => {
                        state.claim.drop_marker();
                        state.claim.end_poll(epoch);
                        state.require_rebuild();
                        state.trust = TrustState::untrusted(
                            UntrustedReason::store_damaged_rebuilding(detail),
                        );
                        state.coverage.park_by(epoch, attachment);
                        schedule = Some(
                            state
                                .claim
                                .schedule(|epoch| Job::Rebuild(name.clone(), epoch)),
                        );
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
            // A release window standing open over this poll is one whatever
            // moved the entry on opened for the coverage the poll is holding:
            // the poll hands it to the release, which is what publishes.
            let releasing = {
                let state = entry.gate.lock().expect("entry gate poisoned");
                state.detach_in_flight && state.claim.leg() == Some(Leg::Poll(epoch))
            };
            if releasing {
                finish_release(shared, entry, name, epoch, Some(attachment));
                continue;
            }
            // The entry moved on while this poll held it, so the poll owns
            // nothing but the attachment it took: it gives that back and
            // publishes nothing, because whatever moved the entry on has
            // already said where the entry stands.
            shared.ops.detach(name, attachment);
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            // The coverage reached the ops through neither a release window nor
            // the route around one, so the close it was owed came from whatever
            // moved the entry past this poll's epoch.
            debug_assert!(
                state.reader.is_none(),
                "a reader stands over coverage a superseded poll has given back"
            );
            state.coverage.released_by(epoch);
            if state.claim.leg() == Some(Leg::Poll(epoch)) {
                if state.detach_in_flight {
                    // The window opened while the coverage was going back, so
                    // it closes on nothing — which is what it is owed, and the
                    // release is still what publishes.
                    drop(state);
                    finish_release(shared, entry, name, epoch, None);
                    continue;
                }
                state.claim.end_poll(epoch);
            }
            if let Some(job) = schedule_demanded_work(&mut state, name) {
                schedule = Some(job);
            }
        }
        // The root under this entry's coverage moved, so it is read again
        // here. A refusal found here supersedes whatever was scheduled above,
        // and the dispatch below then reaches an entry holding no job.
        if reclassify {
            park_on_current_classification(shared, name);
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
    let name = job.name().clone();
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
    let held = run_job_inner(shared, job);
    end_job_leg(shared, &entry, &name, epoch, held);
}

/// End the leg a worker ran, and give back whatever it is still holding.
///
/// A leg ending while the entry's release window is open is what closes that
/// window: the window was opened over this leg, so the coverage the leg hands
/// up is the coverage the release owes back, and [`finish_release`] is what
/// publishes the release rather than the leg. The window opens under the
/// entry's lock and this leg takes that lock at either side of the detach
/// below, so it is read at both: a window that opens while the coverage is
/// already going back closes on nothing, which is what it is owed.
///
/// Where no window is open, this ends the leg's claim on the entry. A claim
/// the leg gave up itself — handed to the job it dispatched, or ended by the
/// release it ran — is already over, and the epoch it moved on to is that
/// job's, not a supersession to open the entry's gate over. What this ends is
/// a claim held to the last: an entry whose epoch moved on under a leg was
/// invalidated by something that left the scheduling to the leg's end, so the
/// gate goes back with the claim.
///
/// Coverage the leg still holds ends with it, and reaches the ops before that
/// gate goes back: work scheduled against an entry whose resources are still
/// going back is work running beside them.
///
/// The work an outstanding demand lease is owed is scheduled where such a claim
/// ends. The entry accounts for no coverage once this leg's has gone to the
/// ops, and a watcher poll passes over an entry holding none, so a lease left
/// standing here is one no later dispatcher tick reaches: the claim's own end
/// is the moment the demand is honored, as it is at every other site that ends
/// one. A leg still standing at the entry's own epoch is not that: it published
/// its verdict under its own claim and scheduled whatever follows it there, and
/// a terminal failure among those verdicts restarts no coverage of its own.
///
/// The epoch is the whole of that separation, and it carries it because a leg
/// publishes a verdict only while standing at the entry's own epoch: every arm
/// of [`run_job_inner`] reads that epoch before it writes one. A leg reaching
/// the schedule below published none, so what the lease is answered with there
/// is work the entry still owes rather than coverage restarted over a verdict
/// already given.
fn end_job_leg<O: EntryOps>(
    shared: &Arc<Shared<O>>,
    entry: &Arc<Entry<O::Attachment>>,
    name: &VaultName,
    epoch: u64,
    held: Option<O::Attachment>,
) {
    let releasing = {
        let state = entry.gate.lock().expect("entry gate poisoned");
        state.detach_in_flight && state.claim.leg() == Some(Leg::Job(epoch))
    };
    if releasing {
        finish_release(shared, entry, name, epoch, held);
        return;
    }
    let handed_back = held.is_some();
    if let Some(attachment) = held {
        shared.ops.detach(name, attachment);
    }
    let mut state = entry.gate.lock().expect("entry gate poisoned");
    // Coverage that reached the ops here reached them through neither a release
    // window nor the route around one, so the close it was owed came from
    // whatever moved the entry past this leg's epoch.
    debug_assert!(
        !handed_back || state.reader.is_none(),
        "a reader stands over coverage a superseded leg has given back"
    );
    if state.claim.end_job_leg(epoch) {
        state.coverage.released_by(epoch);
        if state.detach_in_flight {
            drop(state);
            finish_release(shared, entry, name, epoch, None);
            return;
        }
        if !state.claim.stands_at(epoch) {
            state.claim.release();
            if schedule_demanded_work(&mut state, name).is_some() {
                drop(state);
                let _ = dispatch_pending(shared, entry);
            }
        }
    }
}

/// Run one job's work, answering with the coverage its leg still holds where
/// the leg ends holding any.
///
/// A leg that finds the entry has moved on from the work it carries hands its
/// coverage up rather than giving it back itself: what the entry owes for that
/// coverage — a release window to close, or nothing but the ops — is read
/// where the leg ends, and one caller reads it for every job.
fn run_job_inner<O: EntryOps>(shared: &Arc<Shared<O>>, job: Job) -> Option<O::Attachment> {
    let name = match &job {
        Job::Attach(name, _)
        | Job::Recover(name, _)
        | Job::Rebuild(name, _)
        | Job::Reconcile(name, _)
        | Job::Maintenance(name, _)
        | Job::Reload(name, _, _)
        | Job::ReloadReconcile(name, _, _)
        | Job::Detach(name, _) => name,
    };
    let entry = shared.entries.get(name)?;
    let entry = &entry;
    match job {
        Job::Attach(name, epoch) => {
            // Classification and the identity claim are atomic, but the heal is
            // deliberately outside this gate so unrelated vaults can attach in
            // parallel. Publication revalidates under the same gate.
            let mut attach_claims = shared.attach_gate.lock().expect("attach gate poisoned");
            // One read of the root answers both questions the acquisition asks
            // of it: whether another name reaches it, and which root it is. The
            // identity the claim below is filed under is the one this recheck
            // resolved, so the two facts cannot disagree and one refusal covers
            // them.
            let reading = match shared.entries.recheck(&name) {
                Ok(reading) => reading,
                Err(refusal) => {
                    let mut state = entry.gate.lock().expect("entry gate poisoned");
                    if state.claim.stands_at(epoch) {
                        state.claim.release();
                        park_identity_refusal(&mut state, refusal.to_string());
                    }
                    return None;
                }
            };
            if let Some(conflict) = reading.conflict {
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
                return None;
            }
            let claim_identity = reading.identity;
            if let Some(identity) = claim_identity {
                if let Some(owner) = attach_claims.get(&identity).filter(|owner| *owner != &name) {
                    let conflict = AliasConflict::new([owner.clone(), name.clone()]);
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
                    return None;
                }
                attach_claims.insert(identity, name.clone());
            }
            drop(attach_claims);
            let result = shared
                .ops
                .attach(&entry.registration, &reporter(entry, epoch))
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
            let post_reading = match shared.entries.recheck(&name) {
                Ok(reading) => reading,
                Err(refusal) => {
                    drop(attach_claims);
                    if let Ok((attachment, _, _)) = result {
                        shared.ops.detach(&name, attachment);
                    }
                    let mut state = entry.gate.lock().expect("entry gate poisoned");
                    if state.claim.stands_at(epoch) {
                        state.claim.release();
                        park_identity_refusal(&mut state, refusal.to_string());
                    }
                    return None;
                }
            };
            let mut post_conflict = post_reading.conflict;
            if post_conflict.is_none()
                && let Some(identity) = post_reading.identity
                && let Some(owner) = attach_claims.get(&identity).filter(|owner| *owner != &name)
            {
                post_conflict = Some(AliasConflict::new([owner.clone(), name.clone()]));
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
                return None;
            }
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            if !state.claim.stands_at(epoch) {
                // The entry moved on while this attach ran, so what it
                // acquired is coverage the entry never installed: it goes back
                // where the leg ends, with the release the entry owes it read
                // there.
                drop(state);
                return result.ok().map(|(attachment, _, _)| attachment);
            }
            state.claim.release();
            match result {
                Ok((attachment, observed, handoff_saturated)) => {
                    state.pending.merge(observed);
                    state.active_fingerprints = shared.ops.active_fingerprints(&attachment);
                    state.last_reload_error = None;
                    state.install_coverage(attachment);
                    // The coverage is this attach's, and what any earlier
                    // requirement was raised against went back with the release
                    // that preceded it. Both clear here so nothing the entry
                    // owed about a store it no longer holds stands beside a
                    // trust label published over a store it does.
                    state.clear_rung_requirements();
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
                Err(JobFailure::Reload(error)) => {
                    let detail = state.record_reload_error(error);
                    state.trust =
                        TrustState::untrusted(UntrustedReason::environmental_refusal(detail));
                }
                // An attach runs rung 3 against the database it opened, so
                // damage reaching here is damage that rung could not resolve.
                // The attach acquired nothing and there is no store to rebuild
                // against: what stands is the verdict, and a demand answers it
                // with another attach that opens the file again. That is the
                // reason the entry publishes — the one that says a client's
                // demand is what resumes it, rather than the one that says the
                // entry is already rebuilding.
                Err(JobFailure::StoreDamaged(detail)) => {
                    state.trust = TrustState::untrusted(
                        UntrustedReason::store_damaged_awaiting_demand(detail),
                    );
                }
            }
            None
        }
        Job::Recover(name, epoch) => {
            // A recovery installs watcher coverage over the registered root
            // again, so it acquires coverage the way an attach does and reads
            // the root the same way before it does: nothing watched that path
            // while the entry stood without coverage, so what it resolves to
            // now, and which other names reach it, are open questions here. A
            // refusal moves the entry past this epoch and gives the parked
            // coverage back, so the leg below finds nothing left to run.
            park_on_current_classification(shared, &name);
            let mut attachment = {
                let mut state = entry.gate.lock().expect("entry gate poisoned");
                if !state.claim.stands_at(epoch) {
                    return None;
                }
                state.pin();
                let Some(attachment) = state.coverage.take(epoch) else {
                    state.unpin();
                    restore_lost_claim(&mut state, Job::Recover(name.clone(), epoch));
                    return None;
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
                // The entry moved on while this leg ran, so the coverage it
                // took goes back where the leg ends: the release the entry
                // owes it is read there.
                drop(state);
                return Some(attachment);
            }
            state.claim.release();
            let mut next = None;
            let mut reclassify = false;
            match result {
                Ok(()) => {
                    state.pending.merge(observed);
                    state.active_fingerprints = shared.ops.active_fingerprints(&attachment);
                    state.last_reload_error = None;
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
                    return None;
                }
                Err(JobFailure::MaintainerContended(incumbent)) => {
                    state.maintainer_contended = Some(incumbent);
                    begin_release(&mut state);
                    drop(state);
                    finish_release(shared, entry, &name, epoch, Some(attachment));
                    return None;
                }
                Err(JobFailure::WatcherTerminal(error)) => {
                    state.require_recovery();
                    state.pending.merge(Batch::rescan(RescanScope::Vault));
                    state.coverage.park_by(epoch, attachment);
                    reclassify = root_moved(&error);
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
                Err(JobFailure::Reload(error)) => {
                    state.require_recovery();
                    state.pending.merge(Batch::rescan(RescanScope::Vault));
                    state.coverage.park_by(epoch, attachment);
                    let detail = state.record_reload_error(error);
                    state.trust =
                        TrustState::untrusted(UntrustedReason::environmental_refusal(detail));
                    next = schedule_due_detach(&mut state, &name);
                }
                Err(JobFailure::StoreDamaged(detail)) => {
                    state.require_rebuild();
                    state.coverage.park_by(epoch, attachment);
                    state.trust =
                        TrustState::untrusted(UntrustedReason::store_damaged_rebuilding(detail));
                    next = Some(
                        state
                            .claim
                            .hand_on(|epoch| Job::Rebuild(name.clone(), epoch)),
                    );
                }
            }
            drop(state);
            // The watcher this leg drained reported that the root stopped
            // being covered, and this leg is the one caller that consumed the
            // report: the classification rides the fact rather than the caller.
            if reclassify {
                park_on_current_classification(shared, &name);
            }
            if let Some(job) = next {
                dispatch_handoff(shared, entry, epoch, job);
            }
            None
        }
        Job::Rebuild(name, epoch) => {
            // Rung 3 runs against coverage the entry already holds: the watcher
            // and the maintainer lock are not what was damaged, so this leg
            // neither installs them nor reads the root again. What it replaces
            // is the derived state between them.
            let attachment = {
                let mut state = entry.gate.lock().expect("entry gate poisoned");
                if !state.claim.stands_at(epoch) {
                    return None;
                }
                state.pin();
                let Some(attachment) = state.coverage.take(epoch) else {
                    state.unpin();
                    restore_lost_claim(&mut state, Job::Rebuild(name.clone(), epoch));
                    return None;
                };
                attachment
            };
            let mut observed = Batch::default();
            let mut handoff_saturated = false;
            let rebuilt = match shared
                .ops
                .rebuild(&name, attachment, &reporter(entry, epoch))
            {
                Ok(mut attachment) => match drain_observed(&shared.ops, &name, &mut attachment) {
                    Ok((batch, saturated)) => {
                        observed = batch;
                        handoff_saturated = saturated;
                        Ok(attachment)
                    }
                    Err(_) => Err(Some(attachment)),
                },
                // The rebuild consumed the attachment and the store inside it,
                // so what the entry holds is nothing.
                Err(_) => Err(None),
            };
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            state.unpin();
            if !state.claim.stands_at(epoch) {
                // The entry moved on while this leg ran. Coverage it still has
                // goes back where the leg ends; coverage the rebuild consumed
                // is accounted for here, because no leg-end will find it to
                // give back.
                return match rebuilt {
                    Ok(attachment) => Some(attachment),
                    Err(held @ Some(_)) => held,
                    Err(None) => {
                        state.coverage.released_by(epoch);
                        None
                    }
                };
            }
            match rebuilt {
                Ok(attachment) => {
                    state.claim.release();
                    state.pending.merge(observed);
                    state.active_fingerprints = shared.ops.active_fingerprints(&attachment);
                    // The store inside this coverage is not the store the
                    // entry's reader was minted from, so the handle is minted
                    // again here: this is the one leg that swaps an attached
                    // entry's coverage for another.
                    state.remint_coverage(epoch, attachment);
                    // The derived state the entry holds is one this leg built
                    // from the vault, so the damage the verdict named is gone
                    // with the file it was in. The verdict is retired here
                    // whatever follows: an entry that went on saying it was
                    // damaged while a detach queued behind it would be stating
                    // a fact about a file that no longer exists.
                    state.clear_rebuild();
                    state.trust = TrustState::Ready;
                    let next = if state.detach_due {
                        schedule_due_detach(&mut state, &name)
                    } else if state.pending.is_empty() && !handoff_saturated {
                        None
                    } else {
                        state.trust = trust_for_pending_reconcile(&state.pending);
                        Some(
                            state
                                .claim
                                .hand_on(|epoch| Job::Reconcile(name.clone(), epoch)),
                        )
                    };
                    drop(state);
                    if let Some(job) = next {
                        dispatch_handoff(shared, entry, epoch, job);
                    }
                }
                // Rung 3 could not run, or the watcher could not be drained
                // afterwards. Either way this leg is holding an entry it cannot
                // put back into service, so the resources go back and the entry
                // says it is holding nothing. A demand answers that with an
                // attach, which opens the database again and reports for itself
                // what it finds there.
                Err(held) => {
                    begin_release(&mut state);
                    drop(state);
                    finish_release(shared, entry, &name, epoch, held);
                }
            }
            None
        }
        Job::Reconcile(name, epoch) => loop {
            let (mut attachment, work) = {
                let mut state = entry.gate.lock().expect("entry gate poisoned");
                if !state.claim.stands_at(epoch) {
                    return None;
                }
                state.pin();
                let Some(attachment) = state.coverage.take(epoch) else {
                    state.unpin();
                    restore_lost_claim(&mut state, Job::Reconcile(name.clone(), epoch));
                    return None;
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
            // Asked off the gate, like the watcher poll asks it, and asked on
            // every turn of this loop: a vault whose facts never stop arriving
            // keeps this leg turning under a claim no watcher poll can look
            // past, so this is the only place its clocks are read.
            let maintenance_due = result.is_ok() && shared.ops.maintenance_due(&name, &attachment);
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            state.unpin();
            if !state.claim.stands_at(epoch) {
                // The entry moved on while this leg ran, so the coverage it
                // took goes back where the leg ends: the release the entry
                // owes it is read there.
                drop(state);
                return Some(attachment);
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
                        break None;
                    } else if maintenance_due && !state.owes_a_rung() {
                        // Due maintenance takes the claim ahead of another
                        // reconcile turn, and the facts this turn observed wait
                        // for it in the pending set: the maintenance leg reads
                        // that set when it ends and hands the reconcile on. The
                        // maintenance an entry owes is therefore bounded by one
                        // reconcile turn rather than by the arrival of a quiet
                        // moment the entry may never get.
                        let next = state
                            .claim
                            .hand_on(|epoch| Job::Maintenance(name.clone(), epoch));
                        drop(state);
                        dispatch_handoff(shared, entry, epoch, next);
                        break None;
                    } else if handoff_saturated {
                        let next = state
                            .claim
                            .hand_on(|epoch| Job::Reconcile(name.clone(), epoch));
                        drop(state);
                        dispatch_handoff(shared, entry, epoch, next);
                        break None;
                    } else if state.pending.is_empty() {
                        state.claim.release();
                        if !state.owes_a_rung() {
                            state.trust = TrustState::Ready;
                        }
                        break None;
                    }
                }
                Err(JobFailure::LostMaintainership) => {
                    begin_release(&mut state);
                    drop(state);
                    finish_release(shared, entry, &name, epoch, Some(attachment));
                    break None;
                }
                Err(JobFailure::MaintainerContended(incumbent)) => {
                    state.maintainer_contended = Some(incumbent);
                    begin_release(&mut state);
                    drop(state);
                    finish_release(shared, entry, &name, epoch, Some(attachment));
                    break None;
                }
                Err(JobFailure::WatcherTerminal(error)) => {
                    state.coverage.park_by(epoch, attachment);
                    state.claim.release();
                    state.require_recovery();
                    state.pending.merge(Batch::rescan(RescanScope::Vault));
                    let reclassify = root_moved(&error);
                    state.trust = TrustState::untrusted(watcher_lost(error));
                    let next = schedule_due_detach(&mut state, &name);
                    drop(state);
                    // The watcher this leg drained reported that the root
                    // stopped being covered, and this leg is the one caller
                    // that consumed the report: the classification rides the
                    // fact rather than the caller.
                    if reclassify {
                        park_on_current_classification(shared, &name);
                    }
                    if let Some(job) = next {
                        dispatch_handoff(shared, entry, epoch, job);
                    }
                    break None;
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
                    break None;
                }
                Err(JobFailure::Reload(error)) => {
                    state.coverage.park_by(epoch, attachment);
                    state.claim.release();
                    state.require_recovery();
                    state.pending.merge(Batch::rescan(RescanScope::Vault));
                    let detail = state.record_reload_error(error);
                    state.trust =
                        TrustState::untrusted(UntrustedReason::environmental_refusal(detail));
                    let next = schedule_due_detach(&mut state, &name);
                    drop(state);
                    if let Some(job) = next {
                        dispatch_handoff(shared, entry, epoch, job);
                    }
                    break None;
                }
                // The facts this reconcile took are gone with it, and nothing
                // is merged back for them: the rebuild's heal reads the vault
                // whole, so every path the lost batch named is read again by
                // the leg that follows this one.
                Err(JobFailure::StoreDamaged(detail)) => {
                    state.coverage.park_by(epoch, attachment);
                    state.require_rebuild();
                    state.trust =
                        TrustState::untrusted(UntrustedReason::store_damaged_rebuilding(detail));
                    let next = state
                        .claim
                        .hand_on(|epoch| Job::Rebuild(name.clone(), epoch));
                    drop(state);
                    dispatch_handoff(shared, entry, epoch, next);
                    break None;
                }
            }
        },
        Job::Reload(name, epoch, reply) => {
            run_reload_job(shared, entry, name, epoch, reply, ReloadStep::Activate)
        }
        Job::ReloadReconcile(name, epoch, reply) => {
            run_reload_job(shared, entry, name, epoch, reply, ReloadStep::Reconcile)
        }
        Job::Maintenance(name, epoch) => {
            let mut attachment = {
                let mut state = entry.gate.lock().expect("entry gate poisoned");
                if !state.claim.stands_at(epoch) {
                    return None;
                }
                state.pin();
                let Some(attachment) = state.coverage.take(epoch) else {
                    state.unpin();
                    restore_lost_claim(&mut state, Job::Maintenance(name.clone(), epoch));
                    return None;
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
                // The entry moved on while this leg ran, so the coverage it
                // took goes back where the leg ends: the release the entry
                // owes it is read there.
                drop(state);
                return Some(attachment);
            }
            let mut next = None;
            let mut reclassify = false;
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
                    } else if !state.owes_a_rung() {
                        // The claim this leg took can be the one a watcher's
                        // facts arrived under, so the label those facts warmed
                        // the entry to is one this leg ends: a maintenance that
                        // left nothing to derive is an entry serving the vault,
                        // and no leg follows to say so.
                        state.trust = TrustState::Ready;
                    }
                }
                Err(JobFailure::LostMaintainership) => {
                    begin_release(&mut state);
                    drop(state);
                    finish_release(shared, entry, &name, epoch, Some(attachment));
                    return None;
                }
                Err(JobFailure::MaintainerContended(incumbent)) => {
                    state.maintainer_contended = Some(incumbent);
                    begin_release(&mut state);
                    drop(state);
                    finish_release(shared, entry, &name, epoch, Some(attachment));
                    return None;
                }
                Err(JobFailure::WatcherTerminal(error)) => {
                    state.coverage.park_by(epoch, attachment);
                    state.claim.release();
                    state.require_recovery();
                    state.pending.merge(Batch::rescan(RescanScope::Vault));
                    reclassify = root_moved(&error);
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
                Err(JobFailure::Reload(error)) => {
                    state.coverage.park_by(epoch, attachment);
                    state.claim.release();
                    state.require_recovery();
                    state.pending.merge(Batch::rescan(RescanScope::Vault));
                    let detail = state.record_reload_error(error);
                    state.trust =
                        TrustState::untrusted(UntrustedReason::environmental_refusal(detail));
                    next = schedule_due_detach(&mut state, &name);
                }
                // Maintenance is where a verification the warm path never runs
                // asks the database about itself, so this is the arm damage
                // nothing else meets arrives through.
                Err(JobFailure::StoreDamaged(detail)) => {
                    state.coverage.park_by(epoch, attachment);
                    state.require_rebuild();
                    state.trust =
                        TrustState::untrusted(UntrustedReason::store_damaged_rebuilding(detail));
                    next = Some(
                        state
                            .claim
                            .hand_on(|epoch| Job::Rebuild(name.clone(), epoch)),
                    );
                }
            }
            drop(state);
            // The watcher this leg drained reported that the root stopped
            // being covered, and this leg is the one caller that consumed the
            // report: the classification rides the fact rather than the caller.
            if reclassify {
                park_on_current_classification(shared, &name);
            }
            if let Some(job) = next {
                dispatch_handoff(shared, entry, epoch, job);
            }
            None
        }
        Job::Detach(name, epoch) => {
            let attachment = {
                let mut state = entry.gate.lock().expect("entry gate poisoned");
                if !state.claim.stands_at(epoch) {
                    return None;
                }
                let attachment = state.coverage.take(epoch);
                begin_release(&mut state);
                attachment
            };
            finish_release(shared, entry, &name, epoch, attachment);
            None
        }
    }
}

#[derive(Clone, Copy)]
enum ReloadStep {
    Activate,
    Reconcile,
}

/// Run one bounded turn of an explicit reload.
///
/// A schema reload keeps its reply while watcher facts remain. Each saturated
/// drain hands the claim to a new turn, so unrelated vault work can run before
/// this vault continues. The reply succeeds only after this vault is Ready.
fn run_reload_job<O: EntryOps>(
    shared: &Arc<Shared<O>>,
    entry: &Arc<Entry<O::Attachment>>,
    name: VaultName,
    epoch: u64,
    reply: mpsc::SyncSender<Result<(), ReloadRefusal>>,
    step: ReloadStep,
) -> Option<O::Attachment> {
    let (mut attachment, work) = {
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        if !state.claim.stands_at(epoch) {
            let _ = reply.send(Err(ReloadRefusal::Unavailable(state.trust.clone())));
            return None;
        }
        state.pin();
        let Some(attachment) = state.coverage.take(epoch) else {
            state.unpin();
            state.claim.release();
            let _ = reply.send(Err(ReloadRefusal::Unavailable(state.trust.clone())));
            return None;
        };
        let work = match step {
            ReloadStep::Activate => Batch::default(),
            ReloadStep::Reconcile => std::mem::take(&mut state.pending),
        };
        (attachment, work)
    };

    let mut unsupported = false;
    let mut result = match step {
        ReloadStep::Activate => {
            match shared
                .ops
                .reload(&name, &mut attachment, &reporter(entry, epoch))
            {
                Ok(outcome) => Ok(outcome),
                Err(EntryReloadFailure::Unsupported) => {
                    unsupported = true;
                    Ok(ReloadOutcome::ConfigOnly)
                }
                Err(EntryReloadFailure::Runtime(failure)) => Err(failure),
            }
        }
        ReloadStep::Reconcile if work.is_empty() => Ok(ReloadOutcome::SchemaChanged),
        ReloadStep::Reconcile => shared
            .ops
            .reconcile(
                &name,
                &mut attachment,
                ReconcileWork { batch: work },
                &reporter(entry, epoch),
            )
            .map(|()| ReloadOutcome::SchemaChanged),
    };
    let mut observed = Batch::default();
    let mut handoff_saturated = false;
    if !unsupported && matches!(result, Ok(ReloadOutcome::SchemaChanged)) {
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
        let _ = reply.send(Err(ReloadRefusal::Unavailable(TrustState::Unattached)));
        return Some(attachment);
    }
    if state.detach_in_flight {
        let trust = state.trust.clone();
        drop(state);
        let _ = reply.send(Err(ReloadRefusal::Unavailable(trust)));
        return Some(attachment);
    }
    if unsupported {
        state.coverage.park_by(epoch, attachment);
        state.claim.release();
        drop(state);
        let _ = reply.send(Err(ReloadRefusal::Unsupported));
        return None;
    }
    if matches!(result, Ok(ReloadOutcome::SchemaChanged))
        && (handoff_saturated || !observed.is_empty())
    {
        state.pending.merge(observed);
        state.coverage.park_by(epoch, attachment);
        let next = state
            .claim
            .hand_on(|epoch| Job::ReloadReconcile(name.clone(), epoch, reply));
        drop(state);
        dispatch_handoff(shared, entry, epoch, next);
        return None;
    }

    let response = match result {
        Ok(outcome) => {
            state.active_fingerprints = shared.ops.active_fingerprints(&attachment);
            state.last_reload_error = None;
            state.clear_rung_requirements();
            match outcome {
                ReloadOutcome::ConfigOnly => {
                    state.coverage.park_by(epoch, attachment);
                }
                ReloadOutcome::SchemaChanged => {
                    state.remint_coverage(epoch, attachment);
                }
            }
            state.trust = TrustState::Ready;
            Ok(())
        }
        Err(JobFailure::Reload(error)) => {
            state.active_fingerprints = shared.ops.active_fingerprints(&attachment);
            state.coverage.park_by(epoch, attachment);
            let ready = state.trust == TrustState::Ready;
            let detail = state.record_reload_error(error.clone());
            if !ready {
                state.require_recovery();
                state.pending.merge(Batch::rescan(RescanScope::Vault));
                state.trust = TrustState::untrusted(UntrustedReason::environmental_refusal(detail));
            }
            Err(ReloadRefusal::Core(error))
        }
        Err(error @ JobFailure::LostMaintainership) => {
            begin_release(&mut state);
            drop(state);
            finish_release(shared, entry, &name, epoch, Some(attachment));
            let _ = reply.send(Err(ReloadRefusal::Runtime(error)));
            return None;
        }
        Err(JobFailure::MaintainerContended(incumbent)) => {
            let error = JobFailure::MaintainerContended(incumbent.clone());
            state.maintainer_contended = Some(incumbent);
            begin_release(&mut state);
            drop(state);
            finish_release(shared, entry, &name, epoch, Some(attachment));
            let _ = reply.send(Err(ReloadRefusal::Runtime(error)));
            return None;
        }
        Err(JobFailure::WatcherTerminal(error)) => {
            let failure = JobFailure::WatcherTerminal(error.clone());
            let reclassify = root_moved(&error);
            state.active_fingerprints = shared.ops.active_fingerprints(&attachment);
            state.coverage.park_by(epoch, attachment);
            state.require_recovery();
            state.pending.merge(Batch::rescan(RescanScope::Vault));
            state.trust = TrustState::untrusted(watcher_lost(error));
            state.claim.release();
            drop(state);
            if reclassify {
                park_on_current_classification(shared, &name);
            }
            let _ = reply.send(Err(ReloadRefusal::Runtime(failure)));
            return None;
        }
        Err(JobFailure::Environmental(detail)) => {
            let failure = JobFailure::Environmental(detail.clone());
            state.active_fingerprints = shared.ops.active_fingerprints(&attachment);
            state.coverage.park_by(epoch, attachment);
            state.require_recovery();
            state.pending.merge(Batch::rescan(RescanScope::Vault));
            state.trust = TrustState::untrusted(UntrustedReason::environmental_refusal(detail));
            Err(ReloadRefusal::Runtime(failure))
        }
        Err(JobFailure::StoreDamaged(detail)) => {
            let failure = JobFailure::StoreDamaged(detail.clone());
            state.active_fingerprints = shared.ops.active_fingerprints(&attachment);
            state.coverage.park_by(epoch, attachment);
            state.require_rebuild();
            state.trust = TrustState::untrusted(UntrustedReason::store_damaged_rebuilding(detail));
            let next = state
                .claim
                .hand_on(|epoch| Job::Rebuild(name.clone(), epoch));
            drop(state);
            let _ = reply.send(Err(ReloadRefusal::Runtime(failure)));
            dispatch_handoff(shared, entry, epoch, next);
            return None;
        }
    };
    state.claim.release();
    drop(state);
    let _ = reply.send(response);
    None
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
///
/// The lookup this send opens with is for those three paths alone and gates
/// nothing: the job goes into the channel whether the set still serves its name
/// or not, and it is the worker's own re-read of the set in [`run_job`] that
/// answers a job for a name the set has stopped serving.
fn dispatch_followup<O: EntryOps>(shared: &Arc<Shared<O>>, job: Job) {
    let epoch = job.epoch();
    let entry = shared.entries.get(job.name());
    if shared.shutting_down.load(Ordering::SeqCst) {
        if let Some(entry) = &entry {
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
            if let Some(entry) = &entry {
                release_queue_slot(entry, epoch);
            }
        }
    }
}

/// One trust state as a surface answers it, for a case that names the state it
/// expects rather than the envelope that state becomes: the state itself where
/// a poll walks out of it, and the envelope its reason is spelled in where one
/// does not.
///
/// Every suite in this crate that reads [`Host::state`] renders its expectation
/// through this, so a case names a state and the surface's own mapping decides
/// how that state crosses.
///
/// The name is a placeholder: [`Demand::State`] reads nothing off it, and only
/// [`Demand::UnknownVault`] echoes one.
#[cfg(test)]
pub(crate) fn answered(state: TrustState) -> Result<TrustState, ErrorEnvelope> {
    Demand::State(state).answer(&VaultName::new("answered").expect("a legal vault name"))
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // fixtures impersonate external filesystem retargets.
mod tests {
    use super::*;
    use norn_config::registry::{Entry as RegistryEntry, VaultRoot};
    use norn_testkit::wait::{Budget, Observed, wait_until};
    use norn_wire::ErrorDetail;
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

    /// The coverage every fake in this suite installs, and the ledger the
    /// reader it mints reports itself through.
    ///
    /// A fake with a subject in the reader keeps the ledger it hands out and
    /// reads both ends of the handle's life off it; a fake with no such
    /// subject installs a ledger nothing reads. Coverage minting a reader is
    /// the default here, so the slot an entry publishes is occupied wherever a
    /// case looks at it; `mints` false is the other configuration a coverage
    /// can be in, and the one production stands in today.
    struct FakeCoverage {
        readers: Arc<ReaderLedger>,
        mints: bool,
    }

    impl Default for FakeCoverage {
        fn default() -> Self {
            Self {
                readers: Arc::default(),
                mints: true,
            }
        }
    }

    /// What a case reads about an entry's readers: one open counted where the
    /// coverage mints the handle, one close where the last holder of that
    /// handle drops it.
    #[derive(Default)]
    struct ReaderLedger {
        opened: AtomicUsize,
        closed: AtomicUsize,
    }

    /// The snapshot handle a fake coverage mints. It reads nothing; what it
    /// carries is its own open and close.
    struct FakeReader(Arc<ReaderLedger>);

    impl SnapshotSource for FakeCoverage {
        type Reader = FakeReader;

        fn open_reader(&self) -> Option<FakeReader> {
            if !self.mints {
                return None;
            }
            self.readers.opened.fetch_add(1, Ordering::SeqCst);
            Some(FakeReader(Arc::clone(&self.readers)))
        }
    }

    impl Drop for FakeReader {
        fn drop(&mut self) {
            self.0.closed.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// The reader slot an entry is holding, where it is holding one.
    fn reader_stands<O: EntryOps>(host: &Host<O>, name: &VaultName) -> bool {
        host.shared
            .entries
            .get(name)
            .expect("the vault is registered")
            .gate
            .lock()
            .expect("entry gate poisoned")
            .reader
            .is_some()
    }

    #[derive(Default)]
    struct FakeOps {
        /// The ledger every coverage this fake installs mints its reader
        /// through, so a case reads the readers of every entry the fake serves
        /// off one place.
        readers: Arc<ReaderLedger>,
        /// Install coverage that mints no reader, which is the configuration
        /// production stands in: a store is attached and the entry beside it
        /// serves no reads.
        coverage_mints_no_reader: std::sync::atomic::AtomicBool,
        attaches: AtomicUsize,
        /// The root every attach was handed, filed under the name the
        /// registration it was handed carries.
        ///
        /// The fake records what it is given rather than what a case expects it
        /// to be called for, so an attach handed another entry's registration is
        /// recorded under that entry's name and root: which registration reached
        /// the ops is read here rather than inferred from the attach happening
        /// at all.
        attach_roots: Mutex<BTreeMap<VaultName, VaultRoot>>,
        detaches: AtomicUsize,
        recovers: AtomicUsize,
        rebuilds: AtomicUsize,
        reconciles: AtomicUsize,
        reload_supported: std::sync::atomic::AtomicBool,
        reload_schema_changed: std::sync::atomic::AtomicBool,
        block_reload: std::sync::atomic::AtomicBool,
        reload_started: std::sync::atomic::AtomicBool,
        reload_release: std::sync::atomic::AtomicBool,
        terminal_attach: std::sync::atomic::AtomicBool,
        terminal_recover: std::sync::atomic::AtomicBool,
        terminal_reconcile: std::sync::atomic::AtomicBool,
        environmental_recover: std::sync::atomic::AtomicBool,
        environmental_reconcile: std::sync::atomic::AtomicBool,
        /// The vault whose next reconcile reports damaged derived state, and
        /// the vault whose next maintenance does. Each names one vault and is
        /// taken by the leg that reports it, so a case reads what one entry
        /// does about a verdict against it — and what its siblings do about a
        /// verdict that was never theirs.
        damaged_reconcile_at: Mutex<Option<VaultName>>,
        damaged_maintenance_at: Mutex<Option<VaultName>>,
        /// Report the rebuild itself as damaged, which is rung 3 failing to
        /// resolve what it was scheduled for.
        damaged_rebuild: std::sync::atomic::AtomicBool,
        /// Report the attach as damaged: the rung 3 an attach runs against the
        /// database it opened met damage it could not resolve. It is the one
        /// leg whose damage lands on an entry holding no store, so it is the
        /// only producer of the reason that waits for a demand. One-shot, so
        /// the demand that answers the verdict finds the leg sound.
        damaged_attach: std::sync::atomic::AtomicBool,
        /// The vaults a rebuild has run against, in the order it ran.
        rebuilt_vaults: Mutex<Vec<VaultName>>,
        /// Hold the rebuild open, so the state the entry publishes while rung 3
        /// runs is a state a case can read rather than one it has to catch.
        block_rebuild: std::sync::atomic::AtomicBool,
        rebuild_started: std::sync::atomic::AtomicBool,
        rebuild_release: std::sync::atomic::AtomicBool,
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
        /// The one vault whose poll blocks, where `block_poll` blocks every
        /// vault's. A case holding one alias inside a poll while it drives the
        /// entry beside it needs the block to name which alias it is on.
        poll_gate: Mutex<Option<VaultName>>,
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
        continuous_handoff_poll_for: Mutex<Option<VaultName>>,
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

    impl FakeOps {
        /// Coverage over this fake's ledger, in the configuration the case
        /// asked for. Every leg that hands coverage out builds it here, so a
        /// case reads one ledger whichever leg minted the handle.
        fn coverage(&self) -> FakeCoverage {
            FakeCoverage {
                readers: Arc::clone(&self.readers),
                mints: !self.coverage_mints_no_reader.load(Ordering::SeqCst),
            }
        }
    }

    impl EntryOps for Arc<FakeOps> {
        type Attachment = FakeCoverage;

        fn attach(
            &self,
            registration: &Registration,
            progress: &ProgressReporter<FakeCoverage>,
        ) -> Result<FakeCoverage, JobFailure> {
            ON_JOB_THREAD.with(|flag| flag.set(true));
            self.attaches.fetch_add(1, Ordering::SeqCst);
            self.attach_roots
                .lock()
                .expect("attach roots poisoned")
                .insert(registration.name.clone(), registration.root.clone());
            if self.heal_in_attach.load(Ordering::SeqCst) {
                progress.healing().report(1, Some(2));
            }
            if self.block_attach.load(Ordering::SeqCst) {
                self.attach_started.store(true, Ordering::SeqCst);
                wait_for_release("attach_release", &self.attach_release);
            }
            if self.terminal_attach.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::WatcherTerminal(WatchError::Backend(
                    "lost".into(),
                )));
            }
            if self.contend_attach.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::MaintainerContended(
                    MaintainerIdentity::unknown(),
                ));
            }
            if self.damaged_attach.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::StoreDamaged(
                    "the database disk image is malformed".into(),
                ));
            }
            Ok(self.coverage())
        }

        fn reconcile(
            &self,
            name: &VaultName,
            _: &mut FakeCoverage,
            _: ReconcileWork,
            _: &ProgressReporter<FakeCoverage>,
        ) -> Result<(), JobFailure> {
            ON_JOB_THREAD.with(|flag| flag.set(true));
            let reconcile = self.reconciles.fetch_add(1, Ordering::SeqCst) + 1;
            if self.block_reconcile.load(Ordering::SeqCst)
                || self.block_reconcile_at.load(Ordering::SeqCst) == reconcile
            {
                self.reconcile_started.store(true, Ordering::SeqCst);
                wait_for_release("reconcile_release", &self.reconcile_release);
            }
            if self.terminal_reconcile.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::WatcherTerminal(WatchError::Backend(
                    "lost".into(),
                )));
            }
            if self.environmental_reconcile.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::Environmental("refused".into()));
            }
            if takes_the_vault(&self.damaged_reconcile_at, name) {
                return Err(JobFailure::StoreDamaged(
                    "the database disk image is malformed".into(),
                ));
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

        fn rebuild(
            &self,
            name: &VaultName,
            attachment: FakeCoverage,
            progress: &ProgressReporter<FakeCoverage>,
        ) -> Result<FakeCoverage, JobFailure> {
            ON_JOB_THREAD.with(|flag| flag.set(true));
            self.rebuilds.fetch_add(1, Ordering::SeqCst);
            self.rebuilt_vaults
                .lock()
                .expect("rebuilt vaults poisoned")
                .push(name.clone());
            progress.installing_coverage();
            if self.block_rebuild.load(Ordering::SeqCst) {
                self.rebuild_started.store(true, Ordering::SeqCst);
                wait_for_release("rebuild_release", &self.rebuild_release);
            }
            if self.damaged_rebuild.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::StoreDamaged("still damaged".into()));
            }
            // The rung consumes the coverage it was handed and answers with
            // coverage over the database that replaced the damaged one, which
            // is what the production rung does to the store inside its
            // attachment. What the caller gets back therefore mints a reader
            // of its own, and the one it was handed mints nothing again.
            drop(attachment);
            Ok(self.coverage())
        }

        fn recover(
            &self,
            _: &VaultName,
            _: &mut FakeCoverage,
            _: &ProgressReporter<FakeCoverage>,
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

        fn reload(
            &self,
            _: &VaultName,
            _: &mut FakeCoverage,
            progress: &ProgressReporter<FakeCoverage>,
        ) -> Result<ReloadOutcome, EntryReloadFailure> {
            ON_JOB_THREAD.with(|flag| flag.set(true));
            if !self.reload_supported.load(Ordering::SeqCst) {
                return Err(EntryReloadFailure::Unsupported);
            }
            if self.block_reload.load(Ordering::SeqCst) {
                self.reload_started.store(true, Ordering::SeqCst);
                wait_for_release("reload_release", &self.reload_release);
            }
            if self.reload_schema_changed.load(Ordering::SeqCst) {
                progress.begin_schema_reload();
                Ok(ReloadOutcome::SchemaChanged)
            } else {
                Ok(ReloadOutcome::ConfigOnly)
            }
        }

        fn poll(
            &self,
            name: &VaultName,
            _: &mut FakeCoverage,
        ) -> Result<Option<Batch>, JobFailure> {
            *self
                .polls
                .lock()
                .expect("poll counts poisoned")
                .entry(name.clone())
                .or_default() += 1;
            let gated = self
                .poll_gate
                .lock()
                .expect("poll gate poisoned")
                .as_ref()
                .is_some_and(|gated| gated == name);
            if self.block_poll.load(Ordering::SeqCst) || gated {
                self.poll_started.store(true, Ordering::SeqCst);
                wait_for_release("poll_release", &self.poll_release);
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
            if on_job_thread
                && self
                    .continuous_handoff_poll_for
                    .lock()
                    .expect("continuous handoff poll poisoned")
                    .as_ref()
                    == Some(name)
            {
                return Ok(Some(Batch::default()));
            }
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

        fn maintenance_due(&self, _: &VaultName, _: &FakeCoverage) -> bool {
            self.maintenance_due.swap(false, Ordering::SeqCst)
        }

        fn maintain(&self, name: &VaultName, _: &mut FakeCoverage) -> Result<(), JobFailure> {
            ON_JOB_THREAD.with(|flag| flag.set(true));
            self.maintenances.fetch_add(1, Ordering::SeqCst);
            if self.block_maintenance.load(Ordering::SeqCst) {
                self.maintenance_started.store(true, Ordering::SeqCst);
                wait_for_release("maintenance_release", &self.maintenance_release);
            }
            if self.lost_maintenance.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::LostMaintainership);
            }
            if takes_the_vault(&self.damaged_maintenance_at, name) {
                return Err(JobFailure::StoreDamaged(
                    "the full-text index disagrees with the documents it indexes".into(),
                ));
            }
            if self.contend_maintenance.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::MaintainerContended(
                    MaintainerIdentity::unknown(),
                ));
            }
            Ok(())
        }

        fn detach(&self, _: &VaultName, _: FakeCoverage) {
            if self.block_detach.load(Ordering::SeqCst) {
                self.detach_started.store(true, Ordering::SeqCst);
                wait_for_release("detach_release", &self.detach_release);
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
        let registry = RegistryRead::from_entries([entry]);
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
        let registry = RegistryRead::from_entries([entry]);
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
        let registry = RegistryRead::from_entries(names.iter().map(|name| {
            RegistryEntry::new(
                (*name).clone(),
                VaultRoot::new(format!("/tmp/norn-host-lifecycle-{name}")).unwrap(),
            )
        }));
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

    /// A host over roots the filesystem answers for. Every root is created
    /// before the host reads the registry.
    fn rooted_host(
        ops: Arc<FakeOps>,
        roots: &[(&VaultName, &std::path::Path)],
        worker_slots: usize,
        watch_poll_interval: Duration,
    ) -> Host<Arc<FakeOps>> {
        for (_, root) in roots {
            std::fs::create_dir_all(root).unwrap();
        }
        let registry = RegistryRead::from_entries(roots.iter().map(|(name, root)| {
            RegistryEntry::new((*name).clone(), VaultRoot::new(root).unwrap())
        }));
        Host::new(
            registry,
            ops,
            LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots,
                watch_poll_interval,
            },
        )
        .unwrap()
    }

    /// A host over roots the filesystem answers for, with the dispatcher
    /// ticking.
    fn host_over_roots(
        ops: Arc<FakeOps>,
        roots: &[(&VaultName, &std::path::Path)],
        worker_slots: usize,
    ) -> Host<Arc<FakeOps>> {
        rooted_host(ops, roots, worker_slots, Duration::from_millis(2))
    }

    /// A host over roots the filesystem answers for, without ambient polling.
    ///
    /// A case that drives a watcher signal itself needs the dispatcher's own
    /// tick out of the way: the fake holds one terminal report, and which leg
    /// consumes it is what such a case pins.
    fn quiet_host_over_roots(
        ops: Arc<FakeOps>,
        roots: &[(&VaultName, &std::path::Path)],
    ) -> Host<Arc<FakeOps>> {
        rooted_host(ops, roots, 2, Duration::from_secs(60))
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

    /// The widest sequence of this suite's own waits that a held-open gate
    /// brackets.
    ///
    /// A case holding a job at a gate runs its own waits while the gate is
    /// held: the case that proves a blocked maintenance stalls nothing waits
    /// for another vault to be polled and then for that vault to be reaped,
    /// and the case that proves two attaches take distinct worker slots waits
    /// for the second to start and then for its state. Two is what the widest
    /// of them runs, and the gate's bound is derived over it.
    ///
    /// **This is a census, and no test binds it.** Nothing here can read how
    /// many waits a case runs inside a gate it is holding, so a case that
    /// grows a third wait makes this number wrong without failing anything.
    /// The derivation above is what keeps it honest: it names the two cases it
    /// was counted over, so a reader can recount it, and a case that adds a
    /// wait inside a gate updates it here. What *is* bound is everything
    /// downstream of it — that the gate's budget derives from this number
    /// rather than repeating the flat one, and that the derivation dominates
    /// the sequence.
    const HELD_OPEN_OUTER_WAITS: u32 = 2;

    /// The budget a gate held open across a case's own waits obeys.
    ///
    /// It is derived from the budget those waits obey rather than written
    /// beside it, because the two must move together: a gate that shares one
    /// flat constant with the sequence it brackets lets go partway through
    /// that sequence, and the job it was holding resumes under a case still
    /// asserting it is parked. See [`norn_testkit::wait::Budget::dominating`].
    fn held_open_wait_budget() -> Budget {
        lifecycle_wait_budget().dominating(HELD_OPEN_OUTER_WAITS)
    }

    /// Wait for the surface to answer one exact trust state.
    ///
    /// `expected` is the state, and what is waited for is that state as
    /// [`Host::state`] answers it — a state a poll walks out of crosses as
    /// itself, and one it does not crosses as the envelope its reason is
    /// spelled in. Rendering the expectation the same way the surface renders
    /// its answer is what lets a case name the state it means either way, and
    /// what makes a park standing over the entry a mismatch rather than a
    /// match on the label underneath it.
    ///
    /// The failure is the testkit's own: which bound it passed, how long it
    /// ran, how many times it asked, and the state it last saw. A wait that
    /// expires because one probe was slow and a wait that expires because the
    /// state never came are different diagnoses, and only that report tells
    /// them apart.
    fn wait_for_state<O: EntryOps>(host: &Host<O>, name: &VaultName, expected: TrustState) {
        let expected = answered(expected);
        wait_until(
            &format!("the trust state to become {expected:?}"),
            lifecycle_wait_budget(),
            || {
                let state = host.state(name);
                if state == expected {
                    Observed::Met(())
                } else {
                    Observed::pending(format!("the state is {state:?}"))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
    }

    /// The label the entry itself published, read underneath the park
    /// [`Host::state`] answers in its place.
    ///
    /// A case about when a leg publishes reads it here. What such a case pins
    /// is the order of two acts — the resources reaching the ops, then the
    /// label saying they are back — and a park standing over the entry is a
    /// separate fact about whether the entry may be served at all.
    fn published_label<O: EntryOps>(host: &Host<O>, name: &VaultName) -> Option<TrustState> {
        host.shared.entries.get(name).map(|entry| {
            entry
                .gate
                .lock()
                .expect("entry gate poisoned")
                .trust
                .clone()
        })
    }

    /// Wait for the entry to publish one exact trust state, read underneath any
    /// park, reporting the last label observed.
    fn wait_for_published_label<O: EntryOps>(
        host: &Host<O>,
        name: &VaultName,
        expected: TrustState,
    ) {
        wait_until(
            &format!("the entry to publish {expected:?}"),
            lifecycle_wait_budget(),
            || match published_label(host, name) {
                Some(state) if state == expected => Observed::Met(()),
                state => Observed::pending(format!("the label is {state:?}")),
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
    }

    /// What the entry stands parked on, where it stands on anything.
    fn entry_park<O: EntryOps>(host: &Host<O>, name: &VaultName) -> Option<Demand> {
        host.shared
            .entries
            .get(name)
            .and_then(|entry| entry.gate.lock().expect("entry gate poisoned").parked())
    }

    /// Wait for the entry to stand on one exact park, on the one budget,
    /// reporting the last park observed.
    fn wait_for_park<O: EntryOps>(host: &Host<O>, name: &VaultName, expected: Demand) {
        wait_until(
            &format!("the entry to park on {expected:?}"),
            lifecycle_wait_budget(),
            || match entry_park(host, name) {
                Some(park) if park == expected => Observed::Met(()),
                park => Observed::pending(format!("the park is {park:?}")),
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

    /// Whether what a surface answered is an environmental refusal, judged by
    /// the reason alone. The detail beside it is the platform's own account of
    /// the refusal, which is prose rather than a value to match — but a refusal
    /// carries an account of itself, so the prose is there.
    ///
    /// The argument is what [`Host::state`] answers, so an entry standing
    /// environmentally refused is read here through the envelope it answers
    /// with rather than through a label a caller would have to inspect for a
    /// reason.
    fn refuses_environmentally(answer: &Result<TrustState, ErrorEnvelope>) -> bool {
        let Err(envelope) = answer else {
            return false;
        };
        let ErrorDetail::EntryUntrusted {
            reason: UntrustedReason::EnvironmentalRefusal { detail, .. },
            ..
        } = envelope.detail()
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

    /// Read the identity park at both surfaces it is written across: the
    /// refusal [`Host::state`] answers, and the untrusted label the entry
    /// publishes underneath it.
    ///
    /// [`park_identity_refusal`] writes the two together, out of one account
    /// of the root, and a client reads both. The label is what the entry
    /// answers with in the window a demand opens between retiring the park and
    /// the attach it schedules publishing again, so a label carrying another
    /// reason than the park beside it is a fact a client can observe. Reading
    /// them together is what holds them to being one write: a park spelled
    /// without its label, or a label spelled with another reason, fails here
    /// rather than passing on the surface that still agrees.
    ///
    /// A case with a leg in flight over the park reads the two surfaces apart
    /// instead. The park is the entry standing still; a release running
    /// underneath one publishes its own end, and the label it leaves is that
    /// leg's rather than the park's.
    fn assert_the_identity_park_stands_on_both_surfaces<O: EntryOps>(
        host: &Host<O>,
        name: &VaultName,
    ) {
        let park = entry_park(host, name);
        let Some(Demand::IdentityRefused(detail)) = park.clone() else {
            panic!("the entry stands on no identity park: {park:?}");
        };
        assert!(
            !detail.is_empty(),
            "an identity park reported no account of itself"
        );
        let refusal = UntrustedReason::environmental_refusal(detail);
        assert_eq!(
            host.state(name)
                .as_ref()
                .expect_err("a parked entry refuses")
                .detail(),
            &ErrorDetail::entry_untrusted(refusal.clone()),
            "the status surface answered the identity park with another refusal"
        );
        assert_eq!(
            published_label(host, name),
            Some(TrustState::untrusted(refusal)),
            "the label under the identity park carries another reason than the park does"
        );
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
                if refuses_environmentally(&state) {
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
    fn wait_for_flag(label: &str, flag: &std::sync::atomic::AtomicBool) -> Budget {
        wait_for_marker(label, flag, lifecycle_wait_budget())
    }

    /// Hold a fake's job at a gate until the case releases it.
    ///
    /// This is the wait a case's own sequence runs inside, so it obeys
    /// [`held_open_wait_budget`] rather than the budget those waits obey: a
    /// gate that expired first would let the job run on under a case still
    /// asserting it is parked, and the failure would land on whatever the job
    /// touched next rather than here.
    /// Whether an arrangement naming one vault names this one, taking it where
    /// it does. One-shot, so a leg reports the verdict once and the entry's
    /// answer to it is what the rest of the case reads.
    fn takes_the_vault(arranged: &Mutex<Option<VaultName>>, name: &VaultName) -> bool {
        let mut arranged = arranged.lock().expect("an arranged vault poisoned");
        if arranged.as_ref() == Some(name) {
            *arranged = None;
            return true;
        }
        false
    }

    fn wait_for_release(label: &str, flag: &std::sync::atomic::AtomicBool) -> Budget {
        wait_for_marker(label, flag, held_open_wait_budget())
    }

    /// **The bar on the gates here.** The wait a gate is held open by obeys a
    /// budget that outlives the sequence of this suite's own waits it brackets,
    /// and the wait those outer waits obey is the one it dominates.
    ///
    /// The forbidden shape is the two wrappers passing the same flat budget,
    /// which is invisible until a case brackets more than one wait: the gate
    /// then expires inside the sequence, the job it held runs on, and the
    /// failure lands on whatever that job touched rather than on the gate.
    ///
    /// **What this reads is the budget each wrapper hands to the wait**, not
    /// the two budget functions side by side. Comparing those would pass
    /// unchanged while a wrapper quietly took the other one, which is the whole
    /// defect: the relation between the numbers was never in doubt, the wiring
    /// was. Both markers are already set, so each wait is met on its first look
    /// and reports the bound it was obeying without spending any of it.
    #[test]
    fn a_gate_held_open_obeys_the_budget_that_outlives_the_waits_it_brackets() {
        let set = std::sync::atomic::AtomicBool::new(true);
        let gate = wait_for_release("a gate the case has already released", &set);
        let outer = wait_for_flag("a marker the fake has already set", &set);

        assert_eq!(
            gate,
            held_open_wait_budget(),
            "a gate is held open under {gate:?}, which is not the budget derived to outlive the \
             waits it brackets"
        );
        assert_eq!(
            outer,
            lifecycle_wait_budget(),
            "an outer wait obeys {outer:?} rather than this suite's own budget"
        );

        let bracketed = outer.work() * HELD_OPEN_OUTER_WAITS;
        assert!(
            gate.work() > bracketed,
            "a gate gets {:?}, which does not outlive the {bracketed:?} a case's own \
             {HELD_OPEN_OUTER_WAITS} waits may take",
            gate.work()
        );
    }

    /// Wait for a marker under `budget`, and hand back the budget it obeyed.
    ///
    /// The return is what lets a case state which of this suite's two budgets a
    /// wait actually took, rather than which one a reader of the wrapper
    /// expects it to take.
    fn wait_for_marker(
        label: &str,
        flag: &std::sync::atomic::AtomicBool,
        budget: Budget,
    ) -> Budget {
        wait_until(label, budget, || {
            if flag.load(Ordering::SeqCst) {
                Observed::Met(())
            } else {
                Observed::pending("not set")
            }
        })
        .unwrap_or_else(|failure| panic!("{failure}"));
        budget
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
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
            answered(TrustState::warming(
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
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_flag("attach_started", &ops.attach_started);
        assert_eq!(
            host.state(&name),
            answered(TrustState::warming(WarmingPhase::Healing, 1, Some(2)))
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
            let _ = host.demand(&name, AttachMode::Durable).unwrap();
        }
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn unsupported_reload_leaves_a_ready_vault_unchanged() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let _lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);

        assert_eq!(host.reload(&name), Err(ReloadRefusal::Unsupported));
        assert_eq!(host.state(&name), answered(TrustState::Ready));
        let entry = host.shared.entries.get(&name).unwrap();
        let state = entry.gate.lock().unwrap();
        assert!(!state.recovery_required);
        assert!(state.pending.is_empty());
        assert!(state.coverage.in_hand());
    }

    #[test]
    fn a_release_window_opened_over_reload_closes_at_the_job_epilogue() {
        let ops = Arc::new(FakeOps::default());
        ops.reload_supported.store(true, Ordering::SeqCst);
        ops.block_reload.store(true, Ordering::SeqCst);
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let host = Arc::new(host);
        let _lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);

        let reloading = Arc::clone(&host);
        let reload_name = name.clone();
        let reload = thread::spawn(move || reloading.reload(&reload_name));
        wait_for_flag("reload_started", &ops.reload_started);
        let entry = host.shared.entries.get(&name).unwrap();
        begin_release(&mut entry.gate.lock().unwrap());
        ops.reload_release.store(true, Ordering::SeqCst);

        assert_eq!(
            reload.join().unwrap(),
            Err(ReloadRefusal::Unavailable(releasing()))
        );
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        let state = entry.gate.lock().unwrap();
        assert!(!state.detach_in_flight);
        assert!(state.coverage.in_hand());
    }

    #[test]
    fn saturated_reload_hands_the_claim_on_before_it_continues() {
        let ops = Arc::new(FakeOps::default());
        ops.reload_supported.store(true, Ordering::SeqCst);
        ops.reload_schema_changed.store(true, Ordering::SeqCst);
        ops.block_reload.store(true, Ordering::SeqCst);
        let reloaded = VaultName::new("reloaded").unwrap();
        let sibling = VaultName::new("sibling").unwrap();
        let base = temp_base("reload-handoff");
        let reloaded_root = base.join("reloaded");
        let sibling_root = base.join("sibling");
        let host = Arc::new(host_over_roots(
            Arc::clone(&ops),
            &[
                (&reloaded, reloaded_root.as_path()),
                (&sibling, sibling_root.as_path()),
            ],
            1,
        ));
        let _reloaded_lease = host.demand(&reloaded, AttachMode::Durable).unwrap();
        wait_for_state(&host, &reloaded, TrustState::Ready);
        *ops.continuous_handoff_poll_for.lock().unwrap() = Some(reloaded.clone());

        let reloading = Arc::clone(&host);
        let reload_name = reloaded.clone();
        let reload = thread::spawn(move || reloading.reload(&reload_name));
        wait_for_flag("reload_started", &ops.reload_started);
        let _sibling_lease = host.demand(&sibling, AttachMode::Durable).unwrap();
        ops.reload_release.store(true, Ordering::SeqCst);

        let sibling_ready = wait_until(
            "the sibling to attach between bounded reload turns",
            lifecycle_wait_budget(),
            || match host.state(&sibling) {
                Ok(TrustState::Ready) => Observed::Met(()),
                state => Observed::pending(format!("the sibling state is {state:?}")),
            },
        );
        assert!(matches!(
            host.state(&reloaded),
            Ok(TrustState::Warming { .. })
        ));
        *ops.continuous_handoff_poll_for.lock().unwrap() = None;
        reload.join().unwrap().expect("the reload to become Ready");
        sibling_ready.unwrap_or_else(|failure| panic!("{failure}"));
        assert_eq!(host.state(&reloaded), answered(TrustState::Ready));
        assert_eq!(
            ops.reconciles.load(Ordering::SeqCst),
            0,
            "an empty saturated drain ran an empty reconcile"
        );

        drop(host);
        std::fs::remove_dir_all(base).unwrap();
    }

    /// A contended attach parks the entry and schedules nothing behind it.
    ///
    /// The status surface answers the contention, not the label the park
    /// stands over. The label underneath really is `Unattached` — the attach
    /// took nothing — but `Unattached` is a state that crosses, so answering
    /// it would invite the very demand the contention refuses.
    #[test]
    fn stored_contention_is_reported_without_scheduling_a_hidden_retry() {
        let ops = Arc::new(FakeOps::default());
        ops.contend_attach.store(true, Ordering::SeqCst);
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let initial = host.demand(&name, AttachMode::Durable).unwrap();
        let contended = ErrorDetail::maintainer_contended(MaintainerIdentity::unknown());
        wait_until(
            "the contended attach to park the entry",
            lifecycle_wait_budget(),
            || {
                let attaches = ops.attaches.load(Ordering::SeqCst);
                let state = host.state(&name);
                let refuses = state.as_ref().err().map(ErrorEnvelope::detail) == Some(&contended);
                if attaches == 1 && refuses {
                    Observed::Met(())
                } else {
                    Observed::pending(format!("{attaches} attaches, state is {state:?}"))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        assert_eq!(
            published_label(&host, &name),
            Some(TrustState::Unattached),
            "the label the contention parks over is not the settled one"
        );
        assert!(matches!(
            initial.completion(),
            Demand::MaintainerContended(_)
        ));
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        assert!(matches!(lease.completion(), Demand::MaintainerContended(_)));
        settle();
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        drop(lease);
        drop(initial);
        drop(host.retry(&name, AttachMode::Durable).unwrap());
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
        let initial = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        drop(initial);
        ops.block_detach.store(true, Ordering::SeqCst);
        host.reap_idle(Instant::now()).unwrap();
        wait_for_flag("detach_started", &ops.detach_started);
        let releasing = TrustState::warming(WarmingPhase::ReleasingCoverage, 0, None);
        assert_eq!(host.state(&name), answered(releasing.clone()));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_detach.store(true, Ordering::SeqCst);
        arm(&ops);
        let lease = provoke(&ops, &host, &name);
        wait_for_flag("detach_started", &ops.detach_started);
        assert_eq!(
            published_label(&host, &name),
            Some(releasing()),
            "the entry reported its resources released while they were still out"
        );
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);
        drop(lease);

        ops.detach_release.store(true, Ordering::SeqCst);
        wait_for_published_label(&host, &name, TrustState::Unattached);
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
        Some(host.demand(name, AttachMode::Durable).unwrap())
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
    /// Without ambient polling: a dispatcher tick arriving on its own is a
    /// second leg over the entries these cases hold themselves, and which of
    /// the two reaches an alias first is what a case here is pinning.
    fn two_alias_host(ops: Arc<FakeOps>) -> (Host<Arc<FakeOps>>, VaultName, VaultName) {
        let a = VaultName::new("a").unwrap();
        let b = VaultName::new("b").unwrap();
        let registry = RegistryRead::from_entries([
            RegistryEntry::new(
                a.clone(),
                VaultRoot::new("/tmp/norn-host-refused-a").unwrap(),
            ),
            RegistryEntry::new(
                b.clone(),
                VaultRoot::new("/tmp/norn-host-refused-b").unwrap(),
            ),
        ]);
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
        drop(host.demand(&a, AttachMode::Durable).unwrap());
        drop(host.demand(&b, AttachMode::Durable).unwrap());
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
        let conflict = AliasConflict::new([a.clone(), b.clone()]);
        // The refusal runs off the test thread because it releases both
        // aliases inline, and this test reads the entries from inside that
        // release.
        let refusal = thread::spawn(move || refuse_conflict(&shared, &conflict));
        wait_for_flag("detach_started", &ops.detach_started);
        assert_eq!(published_label(&host, &a), Some(releasing()));
        assert_eq!(published_label(&host, &b), Some(releasing()));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);

        ops.detach_release.store(true, Ordering::SeqCst);
        refusal.join().unwrap();
        wait_for_published_label(&host, &a, TrustState::Unattached);
        wait_for_published_label(&host, &b, TrustState::Unattached);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 2);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);
    }

    /// A refusal reaching an entry whose coverage is parked releases it there
    /// and then, whatever leg is registered against the entry.
    ///
    /// A leg is registered from the instant it begins and holds the coverage
    /// only from the instant it takes it, so a leg standing over parked
    /// coverage gives nothing back: leaving the release to it would leave a
    /// refused entry holding the coverage it was refused over, and every
    /// producer that reads parked coverage would go on taking it.
    #[test]
    fn a_refusal_over_a_leg_standing_at_parked_coverage_releases_it_inline() {
        let ops = Arc::new(FakeOps::default());
        let (host, a, b) = two_alias_host(Arc::clone(&ops));

        let entry = host.shared.entries.get(&a).expect("alias a is registered");
        {
            // The window between a leg's registration and its taking the
            // entry's coverage, reached under the lock a running leg would
            // reach it under.
            let mut state = entry.gate.lock().unwrap();
            let epoch = state.claim.epoch();
            state.claim.begin_job_leg(epoch);
            assert!(state.coverage.in_hand(), "the entry parked its coverage");
        }
        let conflict = AliasConflict::new([a.clone(), b.clone()]);
        refuse_conflict(&host.shared, &conflict);

        assert_eq!(
            ops.detaches.load(Ordering::SeqCst),
            2,
            "a refused alias kept the coverage no leg was holding"
        );
        let state = entry.gate.lock().unwrap();
        assert!(
            !state.coverage.in_hand(),
            "a refused entry is still serving coverage to whatever polls it"
        );
        assert!(
            state.claim.leg().is_none(),
            "a leg holding none of the entry's coverage is still registered as what holds it"
        );
        assert_eq!(state.trust, TrustState::Unattached);
    }

    /// A refusal reaching an entry whose registered leg holds none of its
    /// coverage opens the window all the same, and that leg's end closes it on
    /// nothing.
    ///
    /// This is the window every attach-time refusal opens: the attach's own leg
    /// is registered and the coverage it would install is not there to give
    /// back. The window is what keeps anything from re-arming while the leg is
    /// still entitled to install coverage, and Unattached is published where
    /// the leg ends holding none.
    #[test]
    fn a_refusal_over_a_leg_holding_none_of_the_coverage_closes_its_window_at_the_leg() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        let entry = host
            .shared
            .entries
            .get(&name)
            .expect("the vault is registered");

        let epoch = {
            let mut state = entry.gate.lock().unwrap();
            let epoch = state.claim.epoch();
            let attachment = state.coverage.give_up();
            assert!(attachment.is_some(), "the entry parked its coverage");
            state.claim.begin_job_leg(epoch);
            drop(state);
            if let Some(attachment) = attachment {
                ops.detach(&name, attachment);
            }
            epoch
        };
        refuse_conflict(&host.shared, &AliasConflict::new([name.clone()]));

        {
            let state = entry.gate.lock().unwrap();
            assert_eq!(
                state.trust,
                releasing(),
                "the refusal published released over a leg that could still install coverage"
            );
            assert!(state.detach_in_flight, "no window is open over the leg");
        }

        end_job_leg(&host.shared, &entry, &name, epoch, None);
        let state = entry.gate.lock().unwrap();
        assert!(
            !state.detach_in_flight,
            "the window the refusal opened outlived the leg it was opened over"
        );
        assert_eq!(state.trust, TrustState::Unattached);
        assert!(state.claim.leg().is_none());
    }

    /// The refusal lands while a stale watcher poll's coverage is already on its
    /// way back to the ops: the window opens over a leg holding coverage the
    /// ops have, so it closes on nothing, and the release is still what
    /// publishes.
    #[test]
    fn a_window_opened_over_a_stale_poll_already_detaching_closes_on_nothing() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        let entry = host
            .shared
            .entries
            .get(&name)
            .expect("the vault is registered");

        *ops.poll_gate.lock().unwrap() = Some(name.clone());
        let polling = Arc::clone(&host.shared);
        let poll = thread::spawn(move || poll_watchers(&polling));
        wait_for_flag("poll_started", &ops.poll_started);
        // The entry moves on under the poll with no window open, so the poll is
        // stale and gives its coverage straight back to the ops.
        entry.gate.lock().unwrap().claim.supersede();

        ops.block_detach.store(true, Ordering::SeqCst);
        *ops.poll_gate.lock().unwrap() = None;
        ops.poll_release.store(true, Ordering::SeqCst);
        wait_for_flag("detach_started", &ops.detach_started);

        refuse_conflict(&host.shared, &AliasConflict::new([name.clone()]));
        assert_eq!(published_label(&host, &name), Some(releasing()));

        ops.detach_release.store(true, Ordering::SeqCst);
        poll.join().unwrap();
        wait_for_published_label(&host, &name, TrustState::Unattached);
        assert_eq!(
            ops.detaches.load(Ordering::SeqCst),
            1,
            "the coverage went back twice"
        );
        let state = entry.gate.lock().unwrap();
        assert!(
            !state.detach_in_flight,
            "the window opened over a detach already under way never closed"
        );
        assert!(!state.coverage.in_hand());
        assert!(state.claim.leg().is_none());
    }

    /// The same instant on the job epilogue: a stale job leg is already handing
    /// its coverage to the ops when the refusal opens the window over it.
    #[test]
    fn a_window_opened_over_a_stale_job_leg_already_detaching_closes_on_nothing() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        let entry = host
            .shared
            .entries
            .get(&name)
            .expect("the vault is registered");

        ops.block_reconcile.store(true, Ordering::SeqCst);
        ops.off_thread_poll_batches.store(1, Ordering::SeqCst);
        poll_watchers(&host.shared);
        wait_for_reconciles(&ops, 1, "the reconcile leg to take the coverage");
        // The entry moves on under the leg with no window open, so the leg is
        // stale and its coverage goes straight back to the ops.
        entry.gate.lock().unwrap().claim.supersede();

        ops.block_detach.store(true, Ordering::SeqCst);
        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for_flag("detach_started", &ops.detach_started);

        refuse_conflict(&host.shared, &AliasConflict::new([name.clone()]));
        assert_eq!(published_label(&host, &name), Some(releasing()));

        ops.detach_release.store(true, Ordering::SeqCst);
        wait_for_published_label(&host, &name, TrustState::Unattached);
        assert_eq!(
            ops.detaches.load(Ordering::SeqCst),
            1,
            "the coverage went back twice"
        );
        let state = entry.gate.lock().unwrap();
        assert!(
            !state.detach_in_flight,
            "the window opened over a detach already under way never closed"
        );
        assert!(!state.coverage.in_hand());
        assert!(state.claim.leg().is_none());
    }

    /// A refusal reaching an alias whose coverage is out with a watcher poll
    /// opens the release window and leaves the giving back to that poll, while
    /// the alias beside it is released inline. Neither says released before its
    /// own resources are, and the lease held across the window reads the
    /// conflict throughout.
    #[test]
    fn a_refused_alias_held_in_a_poll_releases_through_the_poll_that_holds_it() {
        let ops = Arc::new(FakeOps::default());
        let (host, a, b) = two_alias_host(Arc::clone(&ops));
        let entry = host.shared.entries.get(&a).expect("alias a is registered");

        // One alias is held inside a watcher poll, which is what puts its
        // coverage out with a leg while the refusal lands.
        *ops.poll_gate.lock().unwrap() = Some(a.clone());
        let polling = Arc::clone(&host.shared);
        let poll = thread::spawn(move || poll_watchers(&polling));
        wait_for_flag("poll_started", &ops.poll_started);

        let lease = host.demand(&a, AttachMode::Durable).unwrap();
        assert_eq!(*lease.outcome(), Demand::State(TrustState::Ready));
        ops.block_detach.store(true, Ordering::SeqCst);
        let shared = Arc::clone(&host.shared);
        let conflict = AliasConflict::new([a.clone(), b.clone()]);
        // The refusal runs off the test thread because it releases the alias it
        // finds idle inline, and this test reads both entries from inside that
        // release.
        let refusal = thread::spawn(move || refuse_conflict(&shared, &conflict));
        wait_for_flag("detach_started", &ops.detach_started);
        assert_eq!(
            (published_label(&host, &a), published_label(&host, &b)),
            (Some(releasing()), Some(releasing())),
            "a refused alias published released over resources that were still out"
        );
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);
        assert!(matches!(lease.completion(), Demand::DuplicateRoot(_)));

        // The poll finds the entry moved on and hands its coverage to the
        // release the refusal opened, which is what publishes.
        ops.poll_release.store(true, Ordering::SeqCst);
        wait_until(
            "the poll to give the entry's claim back",
            lifecycle_wait_budget(),
            || {
                if entry.gate.lock().unwrap().pinned() {
                    Observed::pending("the poll still holds the entry".to_string())
                } else {
                    Observed::Met(())
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);
        assert_eq!(
            published_label(&host, &a),
            Some(releasing()),
            "the refused alias published released while its poll's coverage was still out"
        );

        ops.detach_release.store(true, Ordering::SeqCst);
        poll.join().unwrap();
        refusal.join().unwrap();
        wait_for_published_label(&host, &a, TrustState::Unattached);
        wait_for_published_label(&host, &b, TrustState::Unattached);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 2);
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            2,
            "the release re-armed over a conflict that still parks the entry"
        );
        assert!(matches!(lease.completion(), Demand::DuplicateRoot(_)));
        drop((lease, host));
    }

    /// A refused entry holds nothing, so nothing takes anything from it: the
    /// ambient scan that reads parked coverage passes over it, and the facts it
    /// was holding went back with the refusal.
    #[test]
    fn a_refused_entry_is_polled_by_nothing_afterwards() {
        let ops = Arc::new(FakeOps::default());
        let (host, a, b) = two_alias_host(Arc::clone(&ops));
        let entry = host.shared.entries.get(&a).expect("alias a is registered");
        {
            // A leg registered over parked coverage, with facts standing
            // beside it: the shape a refusal has to leave holding neither.
            let mut state = entry.gate.lock().unwrap();
            let epoch = state.claim.epoch();
            state.claim.begin_job_leg(epoch);
            state.pending.merge(Batch::rescan(RescanScope::Vault));
        }
        let conflict = AliasConflict::new([a.clone(), b.clone()]);
        refuse_conflict(&host.shared, &conflict);
        let polls = ops.polls.lock().unwrap().get(&a).copied().unwrap_or(0);

        poll_watchers(&host.shared);
        poll_watchers(&host.shared);

        assert_eq!(
            ops.polls.lock().unwrap().get(&a).copied().unwrap_or(0),
            polls,
            "a refused entry served its coverage to a watcher poll"
        );
        assert_eq!(
            ops.reconciles.load(Ordering::SeqCst),
            0,
            "a refused entry scheduled work against the coverage it was refused over"
        );
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            2,
            "a refused entry re-attached"
        );
        assert_eq!(published_label(&host, &a), Some(TrustState::Unattached));
        let state = entry.gate.lock().unwrap();
        assert!(!state.coverage.in_hand());
        assert!(state.pending.is_empty());
    }

    /// An identity refusal reaching an entry whose coverage is parked gives it
    /// back too, whatever leg is registered against the entry, and the refused
    /// entry is polled by nothing afterwards.
    ///
    /// The refusal is a park with the resources given back under it: the entry
    /// stays untrusted and owing a recovery, and a demand asking for that
    /// recovery is what serves it again.
    #[test]
    fn an_identity_refusal_over_a_leg_standing_at_parked_coverage_releases_it() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        let entry = host
            .shared
            .entries
            .get(&name)
            .expect("the vault is registered");
        {
            // The window between a leg's registration and its taking the
            // entry's coverage, reached under the lock a running leg would
            // reach it under.
            let mut state = entry.gate.lock().unwrap();
            let epoch = state.claim.epoch();
            state.claim.begin_job_leg(epoch);
            assert!(state.coverage.in_hand(), "the entry parked its coverage");
        }

        refuse_identity_error(&host.shared, &name, "root unreadable".to_string());

        assert_eq!(
            ops.detaches.load(Ordering::SeqCst),
            1,
            "a refused entry kept the coverage no leg was holding"
        );
        {
            let state = entry.gate.lock().unwrap();
            assert!(
                !state.coverage.in_hand(),
                "a refused entry is still serving coverage to whatever polls it"
            );
            assert!(
                state.claim.leg().is_none(),
                "a leg holding none of the entry's coverage is still registered as what holds it"
            );
            assert!(
                state.recovery_required,
                "the refused entry owes no recovery"
            );
            assert!(
                !state.claim.is_held(),
                "a refused entry holds its own gate against the recovery a demand asks for"
            );
            assert!(matches!(state.trust, TrustState::Untrusted { .. }));
        }

        let polls = ops.polls.lock().unwrap().get(&name).copied().unwrap_or(0);
        poll_watchers(&host.shared);
        poll_watchers(&host.shared);
        assert_eq!(
            ops.polls.lock().unwrap().get(&name).copied().unwrap_or(0),
            polls,
            "a refused entry served its coverage to a watcher poll"
        );
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            1,
            "a refused entry re-attached"
        );
    }

    /// **A rung requirement does not outlive the store it was raised against.**
    /// A damage verdict arms a rebuild against the coverage the entry holds. A
    /// registry refusal then hands that coverage straight to the ops without
    /// the release that would retire the requirement, and what the entry owes
    /// from there is the recovery the refusal raises: no rebuild reaches a
    /// store no entry is holding.
    ///
    /// The requirement is what every producer of ordinary work reads. An entry
    /// that carried it across the give-back would re-attach, publish Ready over
    /// a database nothing has said anything about, and never schedule a
    /// reconcile or a maintenance leg again, because both producers ask
    /// `owes_a_rung` first.
    #[test]
    fn a_rebuild_requirement_does_not_outlive_the_coverage_a_refusal_gives_back() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        let entry = host
            .shared
            .entries
            .get(&name)
            .expect("the vault is registered");
        {
            // The state a maintenance or poll verdict of damage leaves behind:
            // the entry holds its coverage and owes the rung against it.
            let mut state = entry.gate.lock().unwrap();
            state.require_rebuild();
            state.trust =
                TrustState::untrusted(UntrustedReason::store_damaged_rebuilding("malformed"));
        }

        refuse_identity_error(&host.shared, &name, "root unreadable".to_string());
        {
            let mut state = entry.gate.lock().unwrap();
            assert!(
                state.recovery_required,
                "the refused entry owes no recovery"
            );
            assert!(
                !state.rebuild_required,
                "the entry owes a rung against a store the refusal handed to the ops"
            );
            state.retire_registry_parks();
        }

        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        {
            let state = entry.gate.lock().unwrap();
            assert!(
                !state.owes_a_rung(),
                "an entry publishing Ready owes a rung no producer of ordinary work runs past"
            );
        }

        let reconciles = ops.reconciles.load(Ordering::SeqCst);
        ops.off_thread_poll_batches.store(1, Ordering::SeqCst);
        poll_watchers(&host.shared);
        wait_until(
            "the re-attached entry to absorb a watcher batch",
            lifecycle_wait_budget(),
            || {
                if ops.reconciles.load(Ordering::SeqCst) > reconciles {
                    Observed::Met(())
                } else {
                    Observed::pending(format!(
                        "{} reconciles, standing at {:?}",
                        ops.reconciles.load(Ordering::SeqCst),
                        host.state(&name)
                    ))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        drop(lease);
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
        let conflict = AliasConflict::new([a.clone(), b.clone()]);
        let second = conflict.clone();
        // The first refusal runs off the test thread because it releases both
        // aliases inline, and the second one below runs from inside that
        // release.
        let refusal = thread::spawn(move || refuse_conflict(&shared, &conflict));
        wait_for_flag("detach_started", &ops.detach_started);

        refuse_conflict(&host.shared, &second);
        assert_eq!(
            (published_label(&host, &a), published_label(&host, &b)),
            (Some(releasing()), Some(releasing())),
            "a refusal published released over a release that had given nothing back"
        );
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);

        ops.detach_release.store(true, Ordering::SeqCst);
        refusal.join().unwrap();
        wait_for_published_label(&host, &a, TrustState::Unattached);
        wait_for_published_label(&host, &b, TrustState::Unattached);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 2);
    }

    /// The demand that withdraws the conflict park is what makes the release
    /// the lease is recorded behind honor it. One predicate reads the park on
    /// both sides of the window: a lease the demand path admitted is never a
    /// lease the release path refuses.
    #[test]
    fn a_lease_admitted_over_a_withdrawn_park_is_honored_by_the_release() {
        let ops = Arc::new(FakeOps::default());
        let (host, a, b) = two_alias_host(Arc::clone(&ops));

        ops.block_detach.store(true, Ordering::SeqCst);
        let shared = Arc::clone(&host.shared);
        let conflict = AliasConflict::new([a.clone(), b.clone()]);
        // The refusal runs off the test thread because it releases both
        // aliases inline, and this test demands from inside that release.
        let refusal = thread::spawn(move || refuse_conflict(&shared, &conflict));
        wait_for_flag("detach_started", &ops.detach_started);

        // The demand withdraws the conflict park, so the lease is recorded
        // against an entry whose release is still in flight and free to
        // re-acquire when it ends.
        let lease = host.demand(&a, AttachMode::Durable).unwrap();
        assert_eq!(*lease.outcome(), Demand::State(releasing()));

        ops.detach_release.store(true, Ordering::SeqCst);
        refusal.join().unwrap();
        wait_for_state(&host, &a, TrustState::Ready);
        assert_eq!(lease.completion(), Demand::State(TrustState::Ready));
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            3,
            "the release refused the re-arm for a park the demand had withdrawn"
        );
        drop((lease, host));
    }

    /// The answer a lease reports is the answer the demand that raised it acted
    /// on: the demand withdraws the conflict park before it schedules against
    /// the entry, so no lease reports a conflict for the length of the work
    /// that was scheduled over one.
    #[test]
    fn a_demand_scheduled_over_a_withdrawn_conflict_answers_the_work_it_scheduled() {
        let ops = Arc::new(FakeOps::default());
        let (host, a, b) = two_alias_host(Arc::clone(&ops));
        let conflict = AliasConflict::new([a.clone(), b.clone()]);
        refuse_conflict(&host.shared, &conflict);
        wait_for_published_label(&host, &a, TrustState::Unattached);

        ops.block_attach.store(true, Ordering::SeqCst);
        let lease = host.demand(&a, AttachMode::Durable).unwrap();
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

    /// A lease raised inside a release window is honored by the release, and
    /// the attach that honors it is where a root the registry cannot read is
    /// found. The lease then reports that refusal rather than the released
    /// state the window ended at, and the attach acquires nothing.
    #[cfg(unix)]
    #[test]
    fn a_release_re_arming_over_an_unreadable_root_reports_the_refusal() {
        let base = temp_base("released-identity-park");
        let root = base.join("root");
        let ops = Arc::new(FakeOps::default());
        let name = VaultName::new("notes").unwrap();
        let host = host_over_roots(Arc::clone(&ops), &[(&name, &root)], 1);
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_detach.store(true, Ordering::SeqCst);
        ops.lost_poll.store(true, Ordering::SeqCst);
        wait_for_flag("detach_started", &ops.detach_started);

        refuse_root_identity(&root);
        let lease = host.demand(&name, AttachMode::Durable).unwrap();

        ops.detach_release.store(true, Ordering::SeqCst);
        wait_until(
            "the lease to answer the identity park",
            lifecycle_wait_budget(),
            || {
                let completion = lease.completion();
                if refuses_identity(&completion) {
                    Observed::Met(())
                } else {
                    Observed::pending(format!("{completion:?}"))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        assert!(
            refuses_environmentally(&host.state(&name)),
            "the entry published no refusal over the root the registry cannot read"
        );
        settle();
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            1,
            "the re-attach acquired coverage over a root the registry cannot read"
        );

        drop((lease, host));
        let _ = std::fs::remove_dir_all(base);
    }

    /// One park is answered in one order however many stand at once: a
    /// maintainer another process holds outranks a root reached under more than
    /// one name, which outranks a root the registry cannot read.
    ///
    /// The status surface answers each of them too, and answers the park
    /// rather than the label underneath it. The entry below is being attached
    /// while the parks are written over it, so the label under every one of
    /// them is a state that crosses — and a surface reading that label would
    /// invite the very demand each park refuses.
    #[test]
    fn every_surface_answers_the_widest_park_standing_over_the_entry() {
        let ops = Arc::new(FakeOps::default());
        let (host, a, b) = two_alias_host(Arc::clone(&ops));
        let lease = host.demand(&a, AttachMode::Durable).unwrap();
        let entry = host.shared.entries.get(&a).unwrap();
        let conflict = AliasConflict::new([a.clone(), b.clone()]);
        let refused_detail = |detail: &ErrorDetail| {
            assert_eq!(
                host.state(&a)
                    .as_ref()
                    .expect_err("a parked entry refuses")
                    .detail(),
                detail,
                "the status surface answered the park standing over the entry another way"
            );
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
        refused_detail(&ErrorDetail::maintainer_contended(
            MaintainerIdentity::unknown(),
        ));

        entry.gate.lock().unwrap().maintainer_contended = None;
        assert_eq!(lease.completion(), Demand::DuplicateRoot(conflict));
        refused_detail(&ErrorDetail::duplicate_root([a.clone(), b.clone()]));

        entry.gate.lock().unwrap().duplicate_root = None;
        assert_eq!(
            lease.completion(),
            Demand::IdentityRefused("the root cannot be read".into())
        );
        refused_detail(&ErrorDetail::entry_untrusted(
            UntrustedReason::environmental_refusal("the root cannot be read"),
        ));

        drop((lease, host));
    }

    /// A watcher poll reads the same park the demand path does: an entry
    /// holding coverage a conflict has refused is one no ambient tick schedules
    /// against, however long that coverage stays in the entry.
    #[test]
    fn a_poll_schedules_no_demanded_work_against_a_parked_entry() {
        let ops = Arc::new(FakeOps::default());
        let (host, a, b) = two_alias_host(Arc::clone(&ops));
        let lease = host.demand(&a, AttachMode::Durable).unwrap();

        // The entry a refusal leaves behind where a leg is registered: the
        // coverage stays parked in the entry, the conflict park stands, and
        // Unattached is published over both.
        {
            let entry = host.shared.entries.get(&a).unwrap();
            let mut state = entry.gate.lock().unwrap();
            state.duplicate_root = Some(AliasConflict::new([a.clone(), b.clone()]));
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
        // The work this entry would be owed: coverage in hand and no recovery
        // owed is a reconcile, and the park is the whole of why the poll
        // scheduled none.
        assert_eq!(
            ops.reconciles.load(Ordering::SeqCst),
            0,
            "a poll reconciled an entry a conflict has parked"
        );
        assert!(matches!(lease.completion(), Demand::DuplicateRoot(_)));
        drop((lease, host));
    }

    /// A retry recovers an entry from a conflict the registry no longer
    /// reports, and it recovers it through the attach the demand inside it
    /// schedules: the registry's parks are retired by the acquisition that
    /// adjudicates them, not by the caller asking again.
    #[test]
    fn a_retry_over_a_resolved_conflict_reaches_ready() {
        let ops = Arc::new(FakeOps::default());
        let (host, a, b) = two_alias_host(Arc::clone(&ops));
        refuse_conflict(&host.shared, &AliasConflict::new([a.clone(), b.clone()]));
        wait_for_published_label(&host, &a, TrustState::Unattached);

        let retried = host.retry(&a, AttachMode::Durable).unwrap();
        wait_for_state(&host, &a, TrustState::Ready);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 3);
        drop((retried, host));
    }

    /// The whole of the park's life, driven from the disk: an alias of a served
    /// root parks the entry, the root stops being an alias, and the next demand
    /// recovers the entry.
    ///
    /// Nothing between those two demands reads a root. The park stands on the
    /// classification the first attach ran, and it is withdrawn by the second
    /// demand so that demand's own attach can classify the roots as they are
    /// now. A lease held across both is what says so: the answer it reports
    /// changes from the refusal to the state, on the same lease.
    #[cfg(unix)]
    #[test]
    fn a_park_raised_by_an_alias_is_retired_by_the_demand_after_the_root_is_resolved() {
        use std::os::unix::fs::symlink;

        let base = temp_base("retired-conflict-lease");
        let a_root = base.join("a");
        let b_root = base.join("b");
        let ops = Arc::new(FakeOps::default());
        let a = VaultName::new("a").unwrap();
        let b = VaultName::new("b").unwrap();
        let host = host_over_roots(Arc::clone(&ops), &[(&a, &a_root), (&b, &b_root)], 2);

        // b's root becomes a second name for a's, so the attach b's demand
        // schedules is the classification that refuses it.
        std::fs::remove_dir(&b_root).unwrap();
        symlink(&a_root, &b_root).unwrap();
        let lease = host.demand(&b, AttachMode::Durable).unwrap();
        wait_until(
            "the lease to answer the duplicate root",
            lifecycle_wait_budget(),
            || {
                let completion = lease.completion();
                if matches!(completion, Demand::DuplicateRoot(_)) {
                    Observed::Met(())
                } else {
                    Observed::pending(format!("{completion:?}"))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            0,
            "an alias of a served root acquired coverage before it was refused"
        );

        // The roots are two again, so the attach the next demand schedules is
        // the read that retires the park.
        std::fs::remove_file(&b_root).unwrap();
        std::fs::create_dir(&b_root).unwrap();
        let recovered = host.demand(&b, AttachMode::Durable).unwrap();
        wait_for_state(&host, &b, TrustState::Ready);
        assert_eq!(
            lease.completion(),
            Demand::State(TrustState::Ready),
            "the lease kept answering a conflict the next attach retired"
        );

        drop((lease, recovered, host));
        let _ = std::fs::remove_dir_all(base);
    }

    /// A demand against a warm entry classifies nothing.
    ///
    /// The counter is the serving set's own and moves once per classification,
    /// and one classification stats every root the host serves. So a request
    /// path that leaves it unmoved is a request path whose cost does not grow
    /// with the registry — which is the whole of what this pins.
    ///
    /// The warm-up attach is what says the counter is live. An unmoved counter
    /// over a seam that stopped counting would read the same as a request path
    /// that spends nothing, so the reading below is taken against a counter
    /// this case has already watched move.
    #[test]
    fn a_warm_demand_classifies_no_root() {
        let ops = Arc::new(FakeOps::default());
        let base = temp_base("warm-demand-classifications");
        let names = ["alpha", "beta", "gamma", "delta"]
            .map(|name| VaultName::new(name).expect("a legal vault name"));
        let roots = names.each_ref().map(|name| base.join(name.as_str()));
        let served = names
            .iter()
            .zip(roots.iter())
            .map(|(name, root)| (name, root.as_path()))
            .collect::<Vec<_>>();
        let host = host_over_roots(Arc::clone(&ops), &served, 2);
        let warm = &names[0];
        drop(host.demand(warm, AttachMode::Durable).unwrap());
        wait_for_state(&host, warm, TrustState::Ready);

        let before = host.shared.entries.classifications();
        assert!(
            before > 0,
            "the attach that warmed the entry classified nothing, so an unmoved \
             counter below pins nothing"
        );
        for _ in 0..8 {
            let lease = host.demand(warm, AttachMode::Durable).unwrap();
            assert_eq!(*lease.outcome(), Demand::State(TrustState::Ready));
        }
        assert_eq!(
            host.shared.entries.classifications(),
            before,
            "a demand against a warm entry classified the roots the host serves"
        );

        drop(host);
        let _ = std::fs::remove_dir_all(base);
    }

    /// The status surface and the refusal surface cannot describe one instant
    /// differently. An entry a duplicate root has parked refuses, and the state
    /// a caller polls says so rather than reporting the settled label the park
    /// stands over — a label that would invite exactly the demand the park
    /// refuses.
    ///
    /// What is pinned is the code and its typed payload, not merely that some
    /// refusal stands: the two surfaces render one demand through one mapping,
    /// so an entry whose park carries a branchable alias list answers that list
    /// on both, and neither may spell the fact in a vocabulary of its own.
    #[test]
    fn a_parked_entry_refuses_at_the_status_surface_too() {
        let ops = Arc::new(FakeOps::default());
        let (host, a, b) = two_alias_host(Arc::clone(&ops));
        let lease = host.demand(&a, AttachMode::Durable).unwrap();
        refuse_conflict(&host.shared, &AliasConflict::new([a.clone(), b.clone()]));
        wait_for_published_label(&host, &a, TrustState::Unattached);

        assert!(matches!(lease.completion(), Demand::DuplicateRoot(_)));
        let answered = host.state(&a);
        assert_eq!(
            answered
                .as_ref()
                .expect_err("a parked entry refuses")
                .detail(),
            &ErrorDetail::duplicate_root([a.clone(), b.clone()]),
            "the status surface spelled the park in a vocabulary of its own"
        );
        assert_eq!(
            answered,
            lease.answer(),
            "the two surfaces describe one instant differently"
        );

        drop((lease, host));
    }

    /// A contended entry refuses at the status surface with the incumbent its
    /// park carries.
    ///
    /// Contention leaves the settled label `Unattached`, and the entry really
    /// does hold nothing — but `Unattached` is a state that crosses, so a
    /// surface answering it invites the demand no acquisition can serve while
    /// another process holds the lock. The park is what both surfaces answer,
    /// and the incumbent rides along as the branchable half of it.
    #[test]
    fn a_contended_entry_refuses_at_the_status_surface_with_its_incumbent() {
        let ops = Arc::new(FakeOps::default());
        ops.contend_attach.store(true, Ordering::SeqCst);
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_park(
            &host,
            &name,
            Demand::MaintainerContended(MaintainerIdentity::unknown()),
        );

        assert_eq!(
            published_label(&host, &name),
            Some(TrustState::Unattached),
            "the label the contention parks over is not the settled one"
        );
        let answered = host.state(&name);
        assert_eq!(
            answered
                .as_ref()
                .expect_err("a contended entry refuses")
                .detail(),
            &ErrorDetail::maintainer_contended(MaintainerIdentity::unknown()),
            "the status surface answered the label under the contention"
        );
        assert_eq!(
            answered,
            lease.answer(),
            "the two surfaces describe one instant differently"
        );

        drop((lease, host));
    }

    /// The release a refusal opens is a window inside the park, not a gap in
    /// it.
    ///
    /// `refuse_conflict` parks the entry and then hands its resources back, and
    /// for the length of that release the label is `ReleasingCoverage` — a
    /// warming state, and warming is answered rather than refused. A surface
    /// reading the label would therefore stop refusing for exactly as long as
    /// the release runs, on an entry no demand may be raised against. The park
    /// outranks the label unconditionally, so the window answers the park.
    #[test]
    fn a_park_answers_through_the_release_window_it_opened() {
        let ops = Arc::new(FakeOps::default());
        let (host, a, b) = two_alias_host(Arc::clone(&ops));

        ops.block_detach.store(true, Ordering::SeqCst);
        let shared = Arc::clone(&host.shared);
        let conflict = AliasConflict::new([a.clone(), b.clone()]);
        // The refusal runs off the test thread because it releases both
        // aliases inline, and this case reads the entry from inside that
        // release.
        let refusal = thread::spawn(move || refuse_conflict(&shared, &conflict));
        wait_for_flag("detach_started", &ops.detach_started);

        assert_eq!(
            published_label(&host, &a),
            Some(releasing()),
            "the entry is not in the release window this case is about"
        );
        assert_eq!(
            host.state(&a)
                .expect_err("a parked entry refuses through its release")
                .detail(),
            &ErrorDetail::duplicate_root([a.clone(), b.clone()]),
            "the release window answered the warming label under the park"
        );

        ops.detach_release.store(true, Ordering::SeqCst);
        refusal.join().unwrap();
        drop(host);
    }

    /// The watcher signal that a root stopped being covered is what puts a live
    /// entry's root back under classification.
    ///
    /// Nothing reads a served entry's root while it is being served, so a
    /// retarget that makes one vault's root a second name for another's is
    /// caught by this signal or not at all. The refusal reaches both names,
    /// and the coverage the retargeted entry was holding goes back.
    ///
    /// The report is consumed here by the ambient watcher poll. A job leg's
    /// own drain is the other caller that can consume it, and
    /// `a_job_leg_that_drains_coverage_lost_reclassifies_the_root` is where
    /// that one is driven.
    #[cfg(unix)]
    #[test]
    fn coverage_lost_at_the_root_reclassifies_every_alias_of_it() {
        use std::os::unix::fs::symlink;

        let base = temp_base("coverage-lost-reclassify");
        let a_root = base.join("a");
        let b_root = base.join("b");
        let ops = Arc::new(FakeOps::default());
        let a = VaultName::new("a").unwrap();
        let b = VaultName::new("b").unwrap();
        let host = quiet_host_over_roots(
            Arc::clone(&ops),
            &[(&a, a_root.as_path()), (&b, b_root.as_path())],
        );
        // Only b is attached, so the coverage the poll below reports on is b's.
        let lease = host.demand(&b, AttachMode::Durable).unwrap();
        wait_for_state(&host, &b, TrustState::Ready);

        // b's registered root becomes a second name for a's, and the watcher
        // reports that the coverage it had over that root has ended.
        std::fs::remove_dir(&b_root).unwrap();
        symlink(&a_root, &b_root).unwrap();
        *ops.terminal_poll.lock().unwrap() = Some(WatchError::CoverageLost(b_root.clone()));
        poll_watchers(&host.shared);

        let conflict = AliasConflict::new([a.clone(), b.clone()]);
        assert_eq!(
            entry_park(&host, &b),
            Some(Demand::DuplicateRoot(conflict.clone())),
            "the retargeted entry went on unclassified"
        );
        assert_eq!(
            entry_park(&host, &a),
            Some(Demand::DuplicateRoot(conflict.clone())),
            "the name the retargeted root now reaches was not refused with it"
        );
        assert_eq!(lease.completion(), Demand::DuplicateRoot(conflict));
        assert_eq!(
            ops.detaches.load(Ordering::SeqCst),
            1,
            "the refused entry kept the coverage it was refused over"
        );

        drop((lease, host));
        let _ = std::fs::remove_dir_all(base);
    }

    /// A watcher report is spent by whichever caller drains the subscription
    /// first, and a job leg's own drain is such a caller. The classification
    /// rides the report rather than the caller, so the leg that consumes it
    /// reads the root the ambient poll would have read.
    #[cfg(unix)]
    #[test]
    fn a_job_leg_that_drains_coverage_lost_reclassifies_the_root() {
        use std::os::unix::fs::symlink;

        let base = temp_base("job-leg-coverage-lost");
        let a_root = base.join("a");
        let b_root = base.join("b");
        let ops = Arc::new(FakeOps::default());
        let a = VaultName::new("a").unwrap();
        let b = VaultName::new("b").unwrap();
        let host = quiet_host_over_roots(
            Arc::clone(&ops),
            &[(&a, a_root.as_path()), (&b, b_root.as_path())],
        );
        let lease = host.demand(&b, AttachMode::Durable).unwrap();
        wait_for_state(&host, &b, TrustState::Ready);

        // A maintenance leg is held open over b, so the drain that ends it is
        // what reaches the watcher next.
        ops.maintenance_due.store(true, Ordering::SeqCst);
        ops.block_maintenance.store(true, Ordering::SeqCst);
        poll_watchers(&host.shared);
        wait_for_flag("maintenance_started", &ops.maintenance_started);

        // b's registered root becomes a second name for a's, and the watcher
        // reports that the coverage it had over that root has ended.
        std::fs::remove_dir(&b_root).unwrap();
        symlink(&a_root, &b_root).unwrap();
        *ops.terminal_poll.lock().unwrap() = Some(WatchError::CoverageLost(b_root.clone()));
        ops.maintenance_release.store(true, Ordering::SeqCst);

        let conflict = AliasConflict::new([a.clone(), b.clone()]);
        wait_for_park(&host, &b, Demand::DuplicateRoot(conflict.clone()));
        assert_eq!(
            entry_park(&host, &a),
            Some(Demand::DuplicateRoot(conflict)),
            "the name the retargeted root now reaches was not refused with it"
        );
        wait_for_detaches(&ops, 1, "the refused entry to give its coverage back");

        drop((lease, host));
        let _ = std::fs::remove_dir_all(base);
    }

    /// A recovery installs coverage over the registered root again, so it
    /// classifies that root the way every acquisition does.
    ///
    /// The failure that left the entry owing the recovery is a fact about the
    /// watch rather than about the root, and nothing watched that path while
    /// the entry stood without coverage. A retarget inside that window is read
    /// here or not at all.
    #[cfg(unix)]
    #[test]
    fn a_recovery_classifies_the_root_it_installs_coverage_over() {
        use std::os::unix::fs::symlink;

        let base = temp_base("recovery-classifies-root");
        let a_root = base.join("a");
        let b_root = base.join("b");
        let ops = Arc::new(FakeOps::default());
        let a = VaultName::new("a").unwrap();
        let b = VaultName::new("b").unwrap();
        let host = quiet_host_over_roots(
            Arc::clone(&ops),
            &[(&a, a_root.as_path()), (&b, b_root.as_path())],
        );
        let lease = host.demand(&b, AttachMode::Durable).unwrap();
        wait_for_state(&host, &b, TrustState::Ready);

        // The watcher backend stops. The entry parks its coverage and owes a
        // recovery, and nothing here has said anything about the root.
        *ops.terminal_poll.lock().unwrap() = Some(WatchError::Backend("lost".into()));
        poll_watchers(&host.shared);
        wait_for_state(&host, &b, backend_lost());
        assert_eq!(
            entry_park(&host, &b),
            None,
            "a watcher backend that stopped parked the entry on the registry"
        );

        // b's registered root becomes a second name for a's while nothing
        // covers it.
        std::fs::remove_dir(&b_root).unwrap();
        symlink(&a_root, &b_root).unwrap();

        let recovery = host.demand(&b, AttachMode::Durable).unwrap();
        let conflict = AliasConflict::new([a.clone(), b.clone()]);
        wait_for_park(&host, &b, Demand::DuplicateRoot(conflict.clone()));
        assert_eq!(
            entry_park(&host, &a),
            Some(Demand::DuplicateRoot(conflict)),
            "the name the retargeted root now reaches was not refused with it"
        );
        assert_eq!(
            ops.recovers.load(Ordering::SeqCst),
            0,
            "the recovery installed coverage over a root two names reach"
        );
        settle();
        assert_eq!(
            host.state(&b),
            host.state(&a),
            "one root's two names stand at different states"
        );

        drop((lease, recovery, host));
        let _ = std::fs::remove_dir_all(base);
    }

    /// A demand a contention answers withdraws no park.
    ///
    /// Withdrawing the registry's parks is how a demand asks for the
    /// acquisition that reads those roots again, and a contended entry
    /// acquires nothing. A withdrawal here would drop a conflict nothing is
    /// coming to read again, leaving the two names of one root disagreeing
    /// about it — and the retry below is what does ask for that read.
    #[test]
    fn a_demand_a_contention_answers_leaves_the_conflict_park_standing() {
        let ops = Arc::new(FakeOps::default());
        let (host, a, b) = two_alias_host(Arc::clone(&ops));
        let conflict = AliasConflict::new([a.clone(), b.clone()]);
        refuse_conflict(&host.shared, &conflict);
        wait_for_published_label(&host, &a, TrustState::Unattached);
        {
            let entry = host
                .shared
                .entries
                .get(&a)
                .expect("the vault is registered");
            entry
                .gate
                .lock()
                .expect("entry gate poisoned")
                .maintainer_contended = Some(MaintainerIdentity::unknown());
        }

        let contended = host.demand(&a, AttachMode::Durable).unwrap();
        assert_eq!(
            *contended.outcome(),
            Demand::MaintainerContended(MaintainerIdentity::unknown()),
            "the contention did not answer the demand it stands over"
        );
        let standing = host.shared.entries.get(&a).map(|entry| {
            entry
                .gate
                .lock()
                .expect("entry gate poisoned")
                .duplicate_root
                .clone()
        });
        assert_eq!(
            standing,
            Some(Some(conflict)),
            "the demand dropped a conflict no acquisition is coming to read"
        );
        assert_eq!(
            host.state(&a)
                .as_ref()
                .expect_err("a contended entry refuses")
                .detail(),
            &ErrorDetail::maintainer_contended(MaintainerIdentity::unknown()),
            "the status surface answered past the contention that outranks the conflict"
        );
        settle();
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            2,
            "the contended entry re-attached"
        );

        // The retry retires the contention, so the demand inside it withdraws
        // the conflict park and the attach it schedules reads the roots again.
        let retried = host.retry(&a, AttachMode::Durable).unwrap();
        wait_for_state(&host, &a, TrustState::Ready);

        drop((contended, retried, host));
    }

    /// A lease answers its own completion in the wire vocabulary, under the
    /// name it was demanded with. The name is the lease's rather than a
    /// caller's: the one refusal that echoes a name — a vault the registry does
    /// not hold — names the vault this lease was taken over, so a client
    /// reading it cannot be told about a vault nobody asked for.
    #[test]
    fn a_lease_answers_its_completion_under_the_name_it_holds() {
        let ops = Arc::new(FakeOps::default());
        let (host, a, b) = two_alias_host(Arc::clone(&ops));
        let lease = host.demand(&a, AttachMode::Durable).unwrap();
        {
            let entry = host.shared.entries.get(&a).unwrap();
            let mut state = entry.gate.lock().unwrap();
            state.duplicate_root = Some(AliasConflict::new([a.clone(), b.clone()]));
        }
        let parked = lease.answer().expect_err("a parked entry refuses");
        assert_eq!(
            parked.detail(),
            &ErrorDetail::duplicate_root([a.clone(), b.clone()]),
            "the lease answered its park with another refusal"
        );

        let missing = VaultName::new("ledger").unwrap();
        let unknown = host.demand(&missing, AttachMode::Durable).unwrap();
        let refusal = unknown.answer().expect_err("an unregistered vault refuses");
        assert_eq!(
            refusal.detail(),
            &ErrorDetail::unknown_vault(missing.clone()),
            "the refusal echoes a name this lease was not demanded under"
        );

        drop((lease, unknown, host));
    }

    /// A demand raised while an entry is giving its resources back defers to
    /// the release and is honored by it: the entry is warming, so nothing is
    /// scheduled against resources still on their way out, and the re-attach
    /// the lease is owed runs when they are back.
    #[test]
    fn a_demand_raised_during_a_teardown_is_honored_when_the_release_finishes() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_detach.store(true, Ordering::SeqCst);
        ops.lost_poll.store(true, Ordering::SeqCst);
        wait_for_flag("detach_started", &ops.detach_started);

        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_detach.store(true, Ordering::SeqCst);
        ops.lost_recover.store(true, Ordering::SeqCst);
        lose_coverage_through_a_driven_poll(&ops, &host, &name, "gone");
        let recovering = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_flag("detach_started", &ops.detach_started);

        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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

    /// A release that re-arms takes the entry's queue slot for the attach it
    /// sends, and that slot is what refuses a second send while the channel
    /// still carries the attach: the re-arm plants no marker of its own, so
    /// [`Claim::take_slot_for_marked`] has the slot alone to weigh a marker
    /// recorded beside it against. An entry left with a free slot behind a job
    /// the channel really holds is one a tick sends a second job for, and the
    /// arrival that frees the slot then frees one naming work the entry never
    /// sent.
    ///
    /// The producer that records the marker is written out here. Every
    /// producer the call graph reaches today either reads an unheld gate
    /// before it schedules or frees the slot under the lock it marks in, and
    /// the hand-on holds the gate for the whole window the re-arm's slot
    /// stands, so no run reaches this state. Layer 4's request-driven work
    /// submission is the producer that will: it names the work an entry owes
    /// at the epoch the entry stands at, held gate or not. The carrier is kept
    /// for it and covered here at its seam.
    ///
    /// The queue has room for the second send: both workers are held inside
    /// another vault's attach, so the channel carries the re-armed attach
    /// alone and takes one more job. What refuses the send is the slot, not a
    /// full queue. The poll that reports the maintainer lost is driven from
    /// here, so the release and its re-arm run on this thread.
    #[test]
    fn a_release_re_arm_holds_the_queue_slot_against_a_second_send() {
        let ops = Arc::new(FakeOps::default());
        let working = VaultName::new("a").unwrap();
        let holding = VaultName::new("b").unwrap();
        let holding_too = VaultName::new("c").unwrap();
        let host =
            host_without_ambient_polling(Arc::clone(&ops), &[&working, &holding, &holding_too], 2);
        let working_lease = host.demand(&working, AttachMode::Durable).unwrap();
        wait_for_state(&host, &working, TrustState::Ready);

        // Both workers inside an attach they cannot leave: nothing takes a job
        // out of the channel from here, so what the channel holds below is
        // what this case put in it.
        ops.block_attach.store(true, Ordering::SeqCst);
        let holding_lease = host.demand(&holding, AttachMode::Durable).unwrap();
        let holding_too_lease = host.demand(&holding_too, AttachMode::Durable).unwrap();
        wait_until(
            "both workers to reach an attach that blocks",
            lifecycle_wait_budget(),
            || {
                let attaches = ops.attaches.load(Ordering::SeqCst);
                if attaches == 3 {
                    Observed::Met(())
                } else {
                    Observed::pending(format!("{attaches} attaches so far"))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));

        // The teardown: the poll reports the maintainer lost, the release
        // gives the coverage back, and the lease standing over the entry is
        // what it re-arms for. The entries beside it are skipped by the round,
        // their own attaches holding their gates.
        ops.lost_poll.store(true, Ordering::SeqCst);
        poll_watchers(&host.shared);

        let entry = host.shared.entries.get(&working).unwrap();
        assert_eq!(
            ops.detaches.load(Ordering::SeqCst),
            1,
            "the release gave nothing back, so nothing re-armed"
        );

        // The work a producer records against the entry as it stands. The
        // reader below refuses on a marker of the entry's own or a leg
        // registered against it as readily as on the slot, and the re-arm left
        // neither, so the slot is what answers for the refusal.
        let attach = {
            let mut state = entry.gate.lock().unwrap();
            assert!(
                state.claim.marker().is_none(),
                "the re-arm scheduled work of its own, so the marker below is not what the tick reads"
            );
            assert!(
                state.claim.leg().is_none(),
                "a leg stands over the entry, so the slot is not what refuses below"
            );
            let attach = state.claim.epoch();
            state.claim.mark(Job::Reconcile(working.clone(), attach));
            attach
        };

        dispatch_pending(&host.shared, &entry).unwrap();
        assert!(
            entry.gate.lock().unwrap().claim.marker().is_some(),
            "nothing is scheduled against the entry, so the tick read no marker"
        );
        // The room left in the channel is what says the tick sent nothing. The
        // re-armed attach is the one job in it, so a second send would have
        // taken the room this job finds; the name it carries is served by no
        // entry, so the worker that reaches it answers nothing.
        let room = {
            let jobs = host.shared.jobs.lock().unwrap();
            jobs.as_ref()
                .expect("the host is running")
                .try_send(Job::Reconcile(VaultName::new("d").unwrap(), attach))
        };
        // The workers are let go before the read is judged, so a case that
        // reads a second send reports it rather than timing out behind the
        // attaches it left blocked.
        ops.attach_release.store(true, Ordering::SeqCst);
        assert!(
            room.is_ok(),
            "a tick sent a second job against an entry whose attach is already in the channel"
        );

        drop((working_lease, holding_lease, holding_too_lease, host));
    }

    /// A teardown that parks the entry on a contended maintainer honors the
    /// demand raised inside it by reporting the contention: the release owes
    /// the lease an answer, and re-attaching against another process's lock is
    /// not one.
    #[test]
    fn a_demand_raised_during_a_contended_teardown_stays_parked() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_detach.store(true, Ordering::SeqCst);
        ops.contend_poll.store(true, Ordering::SeqCst);
        wait_for_flag("detach_started", &ops.detach_started);

        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        assert!(matches!(lease.outcome(), Demand::MaintainerContended(_)));

        ops.detach_release.store(true, Ordering::SeqCst);
        wait_for_published_label(&host, &name, TrustState::Unattached);
        settle();
        let answered = host.state(&name);
        assert_eq!(
            answered
                .as_ref()
                .expect_err("a contended entry refuses")
                .detail(),
            &ErrorDetail::maintainer_contended(MaintainerIdentity::unknown()),
            "the status surface invited the demand the contention refuses"
        );
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        assert!(matches!(lease.completion(), Demand::MaintainerContended(_)));
        assert_eq!(
            answered,
            lease.answer(),
            "the two surfaces describe one instant differently"
        );
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
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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

    /// Destruction and the leg it waits for converge on one release window:
    /// destruction opens it over a leg holding the entry's coverage, the leg's
    /// own end closes it through the release, and the pass after the joins
    /// finds nothing left to give back. Two writers, one release.
    #[test]
    fn destruction_and_the_leg_it_waits_for_converge_on_one_release() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        // A reconcile leg holding the entry's coverage, blocked inside the ops
        // so destruction reaches the entry while the leg still holds it.
        ops.block_reconcile.store(true, Ordering::SeqCst);
        ops.off_thread_poll_batches.store(1, Ordering::SeqCst);
        poll_watchers(&host.shared);
        wait_for_reconciles(&ops, 1, "the reconcile leg to take the entry's coverage");

        let shared = Arc::clone(&host.shared);
        let entry = shared.entries.get(&name).expect("the vault is registered");
        let released = Arc::clone(&ops);
        let unblock = thread::spawn(move || {
            wait_until(
                "destruction to open the release window over the leg",
                lifecycle_wait_budget(),
                || {
                    if entry.gate.lock().unwrap().detach_in_flight {
                        Observed::Met(())
                    } else {
                        Observed::pending("no window is open yet".to_string())
                    }
                },
            )
            .unwrap_or_else(|failure| panic!("{failure}"));
            released.reconcile_release.store(true, Ordering::SeqCst);
        });
        drop(host);
        unblock.join().unwrap();

        assert_eq!(
            ops.detaches.load(Ordering::SeqCst),
            1,
            "the entry's coverage went back twice, or not at all"
        );
        let entry = shared.entries.get(&name).expect("the vault is registered");
        let state = entry.gate.lock().unwrap();
        assert_eq!(state.trust, TrustState::Unattached);
        assert!(!state.detach_in_flight, "the release window is still open");
        assert!(!state.coverage.in_hand());
    }

    /// Destruction leaves coverage still out with a leg to that leg, and the
    /// window stays open until the leg's own release publishes.
    ///
    /// The joins wait for every leg destruction owns, so a leg still holding
    /// coverage after them is one destruction cannot wait for — destruction
    /// re-entered from an [`EntryOps`] callback, on the very thread that leg
    /// runs on. Unattached is published where the resources are back and
    /// nowhere else, so it is that leg's release that publishes it.
    #[test]
    fn destruction_leaves_coverage_out_with_a_leg_to_that_leg() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        let shared = Arc::clone(&host.shared);
        let entry = shared.entries.get(&name).expect("the vault is registered");

        let (epoch, attachment) = {
            let mut state = entry.gate.lock().unwrap();
            let epoch = state.claim.epoch();
            state.claim.begin_job_leg(epoch);
            let attachment = state.coverage.take(epoch);
            assert!(attachment.is_some(), "the entry parked its coverage");
            (epoch, attachment)
        };

        drop(host);
        {
            let state = entry.gate.lock().unwrap();
            assert_eq!(
                state.trust,
                releasing(),
                "destruction published released over coverage a leg still held"
            );
            assert!(
                state.detach_in_flight,
                "the window the leg has to close is not open"
            );
        }
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);

        end_job_leg(&shared, &entry, &name, epoch, attachment);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        let state = entry.gate.lock().unwrap();
        assert_eq!(state.trust, TrustState::Unattached);
        assert!(!state.detach_in_flight, "the release window is still open");
        assert!(!state.coverage.in_hand());
    }

    /// A leg reaching for coverage another leg holds takes none, and the record
    /// of who holds it stands over the refusal. Custody is read rather than
    /// inferred, so an entry with an empty attachment beside a registered leg
    /// is not what says the coverage is out — the record is.
    ///
    /// Destruction is where the record answers alone. The pass that runs after
    /// the joins has no registration to fall back on: it reads custody bare and
    /// leaves the window open for whatever is still out. A refused take that
    /// recorded the entry as accounting for nothing would put this entry on
    /// that pass's other route, and Unattached would be published over coverage
    /// the first leg is still holding.
    #[test]
    fn a_refused_take_leaves_the_record_of_the_leg_holding_the_coverage() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        let shared = Arc::clone(&host.shared);
        let entry = shared.entries.get(&name).expect("the vault is registered");

        // The poll holding the entry's coverage.
        let (holder, attachment) = {
            let mut state = entry.gate.lock().unwrap();
            let holder = state.claim.epoch();
            state.claim.begin_poll(holder);
            let attachment = state.coverage.take(holder);
            (holder, attachment)
        };
        assert!(attachment.is_some(), "the entry parked its coverage");

        // The job dispatched at that same epoch, registering its own leg over
        // the poll's and reaching for coverage the entry does not have in its
        // own hand.
        let took = {
            let mut state = entry.gate.lock().unwrap();
            state.claim.begin_job_leg(holder);
            state.coverage.take(holder)
        };
        assert!(
            took.is_none(),
            "a leg took the coverage another leg is holding"
        );

        // Destruction reads the record, so the record is read here through
        // what destruction does with it rather than off the field.
        drop(host);
        let (trust, in_flight) = {
            let state = entry.gate.lock().unwrap();
            (state.trust.clone(), state.detach_in_flight)
        };
        assert_eq!(
            trust,
            releasing(),
            "destruction published released over coverage a leg still held"
        );
        assert!(in_flight, "the window the leg has to close is not open");
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);

        // The leg that holds it, reaching the release every leg reaches.
        finish_release(&shared, &entry, &name, holder, attachment);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        let state = entry.gate.lock().unwrap();
        assert_eq!(state.trust, TrustState::Unattached);
        assert!(!state.detach_in_flight, "the release window is still open");
        assert!(!state.coverage.out_with_leg());
    }

    /// A job the joins wait for may have given its attachment back to the entry
    /// before its claim ended, so destruction gives back what it finds in the
    /// entry as well as what it took itself.
    #[test]
    fn destruction_gives_back_an_attachment_a_finished_job_left_behind() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
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
        drop(host.demand(&name, AttachMode::Durable).unwrap());
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

        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        // Elapsed time is the subject here, so these two sleeps are the test's
        // own clock rather than a wait: the first spends more than the idle
        // interval while the lease is held, and the second spends it again
        // after the lease ends, when it is allowed to count.
        thread::sleep(Duration::from_millis(30));
        drop(lease);
        host.reap_idle(Instant::now()).unwrap();
        assert_eq!(host.state(&name), answered(TrustState::Ready));
        thread::sleep(Duration::from_millis(25));
        host.reap_idle(Instant::now()).unwrap();
        wait_for_state(&host, &name, TrustState::Unattached);
    }

    #[test]
    fn long_held_lease_gets_a_fresh_idle_interval_after_release() {
        let ops = Arc::new(FakeOps::default());
        let idle_after = Duration::from_millis(200);
        let (host, name) = fixture(Arc::clone(&ops), idle_after);
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        // The test's own clock, not a wait: it spends the whole idle interval
        // and more while the lease is held, so the reap that follows is the
        // one a fresh interval has to survive.
        thread::sleep(idle_after + Duration::from_millis(50));
        assert_eq!(host.state(&name), answered(TrustState::Ready));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);

        let released = Instant::now();
        drop(lease);
        host.reap_idle(released + idle_after / 2).unwrap();
        assert_eq!(host.state(&name), answered(TrustState::Ready));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);
        wait_for_state(&host, &name, TrustState::Unattached);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn idle_reap_releases_the_attachment_and_returns_to_unattached() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::ZERO);
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        host.reap_idle(Instant::now()).unwrap();
        settle();
        assert_eq!(host.state(&name), answered(TrustState::Ready));
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
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 1);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        drop(lease);
    }

    /// The watch error a terminal failure carried reaches the trust state: a
    /// backend that stopped, a vault root that left coverage, and a
    /// synchronization boundary that never arrived are three causes with three
    /// states, and the error's own account of itself is the state's detail — a
    /// sentence a person reads, not a value shaped to be parsed.
    ///
    /// **The table is every arm [`watcher_lost`] maps.** A cause the mapping
    /// carries and this table does not is a transition nothing here drives, so
    /// the row set is the match's row set rather than a sample of it.
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
            (
                WatchError::SynchronizationExpired,
                lost(
                    WatcherLossCause::SynchronizationExpired,
                    "filesystem watcher synchronization expired",
                ),
            ),
        ] {
            let ops = Arc::new(FakeOps::default());
            let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
            let held = host.demand(&name, AttachMode::Durable).unwrap();
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
        let held = host.demand(&name, AttachMode::Durable).unwrap();
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
        assert_eq!(host.state(&name), answered(expected.clone()));

        report_through_an_ambient_poll(&ops.off_thread_rescan_poll_batches);
        settle();
        assert_eq!(
            host.state(&name),
            answered(expected),
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
    /// does: the entry gives its resources back and settles on Unattached
    /// underneath the park, nothing re-attaches against the lock another
    /// process holds, and every surface keeps answering the contention until a
    /// retry clears it.
    #[test]
    fn contention_reported_by_a_poll_parks_the_entry_until_retry() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let held = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        ops.contend_poll.store(true, Ordering::SeqCst);
        wait_for_published_label(&host, &name, TrustState::Unattached);
        settle();
        assert_eq!(
            host.state(&name)
                .expect_err("a contended entry refuses")
                .detail(),
            &ErrorDetail::maintainer_contended(MaintainerIdentity::unknown()),
            "the status surface invited the demand the contention refuses"
        );
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        assert!(matches!(
            host.demand(&name, AttachMode::Durable).unwrap().outcome(),
            Demand::MaintainerContended(_)
        ));
        settle();
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);

        let retried = host.retry(&name, AttachMode::Durable).unwrap();
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
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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

        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_flag("reconcile_started", &ops.reconcile_started);
        assert!(matches!(host.state(&name), Ok(TrustState::Warming { .. })));

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

        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_flag("reconcile_started", &ops.reconcile_started);
        assert_eq!(
            host.state(&name),
            answered(TrustState::untrusted(UntrustedReason::WatcherOverflow))
        );

        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        drop(lease);
    }

    #[test]
    fn reconcile_handoff_saturation_requires_an_additional_reconcile_before_ready() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
            answered(TrustState::untrusted(UntrustedReason::WatcherOverflow))
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
        drop(host.demand(&name, AttachMode::Durable).unwrap());
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
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, backend_lost());
        drop(lease);
    }

    #[test]
    fn terminal_failure_during_reconcile_stays_watcher_untrusted() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
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
        drop(host.demand(&name, AttachMode::Durable).unwrap());
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
            answered(TrustState::untrusted(
                UntrustedReason::environmental_refusal("refused")
            ))
        );
        // Held across the wait; see the sibling recover-side test below for
        // why an immediately dropped lease here can wedge the entry.
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 1);
        assert!(ops.reconciles.load(Ordering::SeqCst) > failed_count);
        drop(lease);
    }

    /// **A damaged store reaches rung 3 without passing through the
    /// environmental ladder.** The sequence is the whole assertion: Ready, then
    /// the damage published under its own reason, then the prologue of the
    /// rebuild, then Ready again — and no recovery anywhere in it.
    ///
    /// A recovery re-installs coverage and re-heals against the same database,
    /// so an entry answering damage that way meets the same verdict every time
    /// round. What makes that impossible is not the ordering below but the
    /// requirement the verdict sets: `rebuild_required` dominates
    /// `recovery_required`, so the work the entry owes is the rebuild whichever
    /// door asks for it.
    #[test]
    fn a_damaged_store_reaches_rung_three_and_never_the_recovery_ladder() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        *ops.damaged_reconcile_at
            .lock()
            .expect("an arranged vault poisoned") = Some(name.clone());
        ops.block_rebuild.store(true, Ordering::SeqCst);
        report_through_an_ambient_poll(&ops.off_thread_rescan_poll_batches);

        wait_for_state(
            &host,
            &name,
            TrustState::untrusted(UntrustedReason::store_damaged_rebuilding(
                "the database disk image is malformed",
            )),
        );
        // The rebuild is the entry's own work rather than work a demand has to
        // ask for: nothing was demanded between the verdict and this leg.
        wait_for_flag("rebuild_started", &ops.rebuild_started);
        assert_eq!(
            host.state(&name),
            answered(TrustState::untrusted(
                UntrustedReason::store_damaged_rebuilding("the database disk image is malformed")
            )),
            "the entry retired the damage verdict before the rung resolving it had"
        );
        ops.rebuild_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);

        assert_eq!(ops.rebuilds.load(Ordering::SeqCst), 1);
        assert_eq!(
            ops.recovers.load(Ordering::SeqCst),
            0,
            "the damaged store was answered by the ladder that cannot resolve it"
        );
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            1,
            "rung 3 tore the attachment down and built another"
        );
    }

    /// The verdict maintenance reports reaches the same rung. Maintenance is
    /// where the verification the warm path never runs asks the database about
    /// itself, so it is the leg silent damage arrives through.
    #[test]
    fn damage_found_by_scheduled_maintenance_reaches_rung_three() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        *ops.damaged_maintenance_at
            .lock()
            .expect("an arranged vault poisoned") = Some(name.clone());
        ops.maintenance_due.store(true, Ordering::SeqCst);
        wait_until(
            "the entry to reach Ready through the rung the verdict schedules",
            lifecycle_wait_budget(),
            || {
                if ops.rebuilds.load(Ordering::SeqCst) == 1
                    && host.state(&name) == answered(TrustState::Ready)
                {
                    Observed::Met(())
                } else {
                    Observed::pending(format!(
                        "{} rebuilds, standing at {:?}",
                        ops.rebuilds.load(Ordering::SeqCst),
                        host.state(&name)
                    ))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 0);
    }

    /// **The verdict an attach reports is the one that waits for a demand, and
    /// the entry then waits.** The attach acquired nothing, so there is no
    /// database for rung 3 to discard and no work the entry can owe itself.
    /// What stands is the verdict, and a demand is what opens a file to
    /// discard.
    ///
    /// The counts beside the reason are what make it the reason rather than
    /// the rebuilding one: one attach and no rebuild once the queue drains, no
    /// requirement recorded against the entry, and no job left claimed. An
    /// entry publishing the rebuilding reason here would be publishing a
    /// promise that it resumes on its own, which nothing below is doing. The
    /// second demand is the other half — the entry is not wedged, and the
    /// attach that demand schedules is the one that reaches Ready.
    #[test]
    fn damage_an_attach_reports_waits_for_the_demand_that_opens_a_store_to_discard() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let entry = host.shared.entries.get(&name).unwrap();
        let awaiting = TrustState::untrusted(UntrustedReason::store_damaged_awaiting_demand(
            "the database disk image is malformed",
        ));

        ops.damaged_attach.store(true, Ordering::SeqCst);
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, awaiting.clone());
        settle();

        assert_eq!(
            host.state(&name),
            answered(awaiting),
            "the entry moved off a verdict nothing had answered"
        );
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            1,
            "the entry resumed itself against a verdict that waits for a demand"
        );
        assert_eq!(
            ops.rebuilds.load(Ordering::SeqCst),
            0,
            "rung 3 ran against a database the attach never opened"
        );
        assert_eq!(
            ops.recovers.load(Ordering::SeqCst),
            0,
            "the damage was answered by the ladder that cannot resolve it"
        );
        {
            let state = entry.gate.lock().unwrap();
            assert!(
                !state.rebuild_required,
                "the entry owes a rebuild over a database it does not hold"
            );
            assert!(
                !state.claim.is_held() && state.claim.leg().is_none(),
                "work stands scheduled against an entry that waits for a demand"
            );
        }

        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            2,
            "the demand answered the verdict with something other than an attach"
        );
        assert_eq!(
            ops.rebuilds.load(Ordering::SeqCst),
            0,
            "the demand asked for a rung against a database nothing had opened"
        );
        drop((lease, host));
    }

    /// **The handle an entry serves reads from is minted from the store the
    /// rung left standing.** Rung 3 is the one leg that swaps an attached
    /// entry's coverage for other coverage, so the reader minted over the
    /// discarded database ends where that database does and the replacement
    /// mints the handle the entry publishes Ready over. A handle kept across
    /// the rung answers every later read from a file the rung unlinked.
    #[test]
    fn a_rebuild_ends_the_reader_over_the_discarded_store_and_mints_another() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.readers.opened.load(Ordering::SeqCst), 1);
        assert_eq!(ops.readers.closed.load(Ordering::SeqCst), 0);

        *ops.damaged_maintenance_at
            .lock()
            .expect("an arranged vault poisoned") = Some(name.clone());
        ops.maintenance_due.store(true, Ordering::SeqCst);
        wait_until(
            "the entry to reach Ready through the rung the verdict schedules",
            lifecycle_wait_budget(),
            || {
                if ops.rebuilds.load(Ordering::SeqCst) == 1
                    && host.state(&name) == answered(TrustState::Ready)
                {
                    Observed::Met(())
                } else {
                    Observed::pending(format!(
                        "{} rebuilds, standing at {:?}",
                        ops.rebuilds.load(Ordering::SeqCst),
                        host.state(&name)
                    ))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));

        assert_eq!(
            ops.readers.closed.load(Ordering::SeqCst),
            1,
            "the handle over the database the rung discarded still stands"
        );
        assert_eq!(
            ops.readers.opened.load(Ordering::SeqCst),
            2,
            "the entry publishes Ready over coverage that minted no handle"
        );
        assert!(
            reader_stands(&host, &name),
            "an entry publishing Ready holds no reader for a read to run on"
        );
        drop(lease);
    }

    /// **Damage is contained to the entry it was found in.** One vault's
    /// derived state is one file, and a sibling serving from its own is not
    /// what a corrupt page is about: the sibling stays Ready, and no rung runs
    /// against it.
    #[test]
    fn a_damaged_store_leaves_its_sibling_ready() {
        let ops = Arc::new(FakeOps::default());
        let damaged = VaultName::new("damaged").unwrap();
        let sibling = VaultName::new("sibling").unwrap();
        let registry = RegistryRead::from_entries([
            RegistryEntry::new(
                damaged.clone(),
                VaultRoot::new("/tmp/norn-host-damaged-vault").unwrap(),
            ),
            RegistryEntry::new(
                sibling.clone(),
                VaultRoot::new("/tmp/norn-host-sibling-vault").unwrap(),
            ),
        ]);
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
        drop(host.demand(&damaged, AttachMode::Durable).unwrap());
        drop(host.demand(&sibling, AttachMode::Durable).unwrap());
        wait_for_state(&host, &damaged, TrustState::Ready);
        wait_for_state(&host, &sibling, TrustState::Ready);

        *ops.damaged_reconcile_at
            .lock()
            .expect("an arranged vault poisoned") = Some(damaged.clone());
        report_through_an_ambient_poll(&ops.off_thread_rescan_poll_batches);
        wait_until(
            "the damaged entry to come back through its rebuild",
            lifecycle_wait_budget(),
            || {
                if ops.rebuilds.load(Ordering::SeqCst) >= 1
                    && host.state(&damaged) == answered(TrustState::Ready)
                {
                    Observed::Met(())
                } else {
                    Observed::pending(format!(
                        "{} rebuilds, the damaged entry at {:?}",
                        ops.rebuilds.load(Ordering::SeqCst),
                        host.state(&damaged)
                    ))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));

        assert_eq!(
            *ops.rebuilt_vaults.lock().expect("rebuilt vaults poisoned"),
            vec![damaged.clone()],
            "a rung ran against a vault whose derived state nothing said was damaged"
        );
        assert_eq!(
            host.state(&sibling),
            answered(TrustState::Ready),
            "one vault's damaged store withdrew another vault"
        );
    }

    /// **Rung 3 that cannot resolve the damage gives the entry back rather than
    /// running again.** A rebuild that meets damage in the database it just
    /// created has nothing left to discard, and the resources it holds go back:
    /// the entry says it is holding nothing, and the attach a demand asks for is
    /// what opens a database again.
    #[test]
    fn a_rebuild_that_is_still_damaged_releases_the_entry_rather_than_looping() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        *ops.damaged_reconcile_at
            .lock()
            .expect("an arranged vault poisoned") = Some(name.clone());
        ops.damaged_rebuild.store(true, Ordering::SeqCst);
        report_through_an_ambient_poll(&ops.off_thread_rescan_poll_batches);

        wait_for_state(&host, &name, TrustState::Unattached);
        settle();
        assert_eq!(
            ops.rebuilds.load(Ordering::SeqCst),
            1,
            "the failed rung ran again against an entry holding nothing"
        );
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 0);
        assert_eq!(
            ops.detaches.load(Ordering::SeqCst),
            0,
            "the rebuild consumed the attachment itself"
        );
    }

    #[test]
    fn failed_recover_cannot_be_bypassed_by_a_later_watcher_fact() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = attached_awaiting_recovery(&ops);
        ops.environmental_recover.store(true, Ordering::SeqCst);
        // Held across both waits below: a lease dropped before either state
        // is reached would withdraw the recovery request it raised.
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
            answered(TrustState::untrusted(
                UntrustedReason::environmental_refusal("refused")
            ))
        );
        drop(lease);
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 2);
        assert_eq!(ops.reconciles.load(Ordering::SeqCst), 1);
        drop(lease);
    }

    /// A classification that refuses the root supersedes the reconcile in
    /// flight against it: the entry publishes the refusal at once, and the
    /// reconcile gives its attachment back rather than restoring it into an
    /// entry that has moved past it. Nothing re-attaches against a root the
    /// registry is refusing.
    #[cfg(unix)]
    #[test]
    fn an_identity_refusal_invalidates_an_in_flight_reconcile() {
        let base = temp_base("reconcile-identity-refusal");
        let root = base.join("root");
        let ops = Arc::new(FakeOps::default());
        let name = VaultName::new("notes").unwrap();
        let host = host_over_roots(Arc::clone(&ops), &[(&name, &root)], 1);
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_reconcile.store(true, Ordering::SeqCst);
        // A dispatcher tick reporting facts is what puts a reconcile in
        // flight; which tick it was is not this case's subject.
        report_through_an_ambient_poll(&ops.off_thread_rescan_poll_batches);
        wait_for_flag("reconcile_started", &ops.reconcile_started);

        refuse_root_identity(&root);
        park_on_current_classification(&host.shared, &name);
        assert_the_identity_park_stands_on_both_surfaces(&host, &name);
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
        assert_the_identity_park_stands_on_both_surfaces(&host, &name);
        drop(host);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn host_drop_waits_for_in_flight_work_and_its_attachment_teardown() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
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
        drop(host.demand(&name, AttachMode::Durable).unwrap());
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
        drop(host.demand(&name, AttachMode::Durable).unwrap());
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
        let lease = host.demand(name, AttachMode::Durable).unwrap();
        assert_eq!(
            host.state(name),
            answered(backend_lost()),
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
        let held = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        claim_for_a_poll(&ops);
        *ops.terminal_poll.lock().unwrap() = Some(WatchError::Backend("lost".into()));

        ops.poll_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, backend_lost());
        settle();
        assert_eq!(host.state(&name), answered(backend_lost()));
        assert_eq!(
            ops.recovers.load(Ordering::SeqCst),
            0,
            "a lease that demanded no recovery got one anyway"
        );
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);

        let retry = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 1);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        drop((held, retry));
    }

    /// A release reads whether its lost coverage was demanded before clearing
    /// that recovery requirement. A lease predating the loss keeps the entry
    /// alive, but it asked for no recovery and therefore schedules no attach.
    #[test]
    fn a_release_owing_undemanded_recovery_schedules_no_reattach() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let held = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        let entry = host.shared.entries.get(&name).unwrap();
        let (epoch, attachment) = {
            let mut state = entry.gate.lock().unwrap();
            let epoch = state.claim.epoch();
            state.claim.begin_job_leg(epoch);
            let attachment = state
                .coverage
                .take(epoch)
                .expect("the ready entry holds its attachment");
            state.require_recovery();
            assert_eq!(state.recovery_demands, 0);
            begin_release(&mut state);
            (epoch, attachment)
        };

        finish_release(&host.shared, &entry, &name, epoch, Some(attachment));

        assert_eq!(host.state(&name), answered(TrustState::Unattached));
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            1,
            "a lease that demanded no recovery scheduled a re-attach"
        );
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        let state = entry.gate.lock().unwrap();
        assert!(state.claim.marker().is_none());
        assert!(state.claim.slot().is_none());
        drop(state);
        drop(held);
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
        let held = host.demand(&name, AttachMode::Durable).unwrap();
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
        assert_eq!(host.state(&name), answered(backend_lost()));

        let retry = host.demand(&name, AttachMode::Durable).unwrap();
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
        let spent = host.demand(&name, AttachMode::Durable).unwrap();
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
        type Attachment = FakeCoverage;
        fn attach(
            &self,
            _: &Registration,
            _: &ProgressReporter<FakeCoverage>,
        ) -> Result<FakeCoverage, JobFailure> {
            Ok(FakeCoverage::default())
        }
        /// No case here damages a store, so the coverage comes straight
        /// back: rung 3 over derived state nothing damaged is the identity.
        fn rebuild(
            &self,
            _: &VaultName,
            attachment: FakeCoverage,
            _: &ProgressReporter<FakeCoverage>,
        ) -> Result<FakeCoverage, JobFailure> {
            Ok(attachment)
        }
        fn reconcile(
            &self,
            _: &VaultName,
            _: &mut FakeCoverage,
            _: ReconcileWork,
            progress: &ProgressReporter<FakeCoverage>,
        ) -> Result<(), JobFailure> {
            let healing = progress.healing();
            healing.report(1, Some(2));
            self.reconciles.fetch_add(1, Ordering::SeqCst);
            if self.hold_reconcile.load(Ordering::SeqCst) {
                self.reconcile_started.store(true, Ordering::SeqCst);
                wait_for_release("reconcile_release", &self.reconcile_release);
            }
            healing.report(2, Some(2));
            Ok(())
        }
        fn recover(
            &self,
            _: &VaultName,
            _: &mut FakeCoverage,
            _: &ProgressReporter<FakeCoverage>,
        ) -> Result<(), JobFailure> {
            Ok(())
        }
        fn poll(&self, _: &VaultName, _: &mut FakeCoverage) -> Result<Option<Batch>, JobFailure> {
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
        fn detach(&self, _: &VaultName, _: FakeCoverage) {}
    }

    #[test]
    fn attach_handoff_drains_two_already_queued_batches_before_ready() {
        let ops = Arc::new(PollingOps::default());
        ops.queued.store(2, Ordering::SeqCst);
        let name = VaultName::new("notes").unwrap();
        let registry = RegistryRead::from_entries([RegistryEntry::new(
            name.clone(),
            VaultRoot::new("/tmp/norn-host-two-batches").unwrap(),
        )]);
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
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
        type Attachment = FakeCoverage;
        fn attach(
            &self,
            registration: &Registration,
            _: &ProgressReporter<FakeCoverage>,
        ) -> Result<FakeCoverage, JobFailure> {
            let name = &registration.name;
            if name.as_str() == "a" {
                self.a_started.store(true, Ordering::SeqCst);
                wait_for_release("release_a", &self.release_a);
            } else if name.as_str() == "b" {
                self.b_started.store(true, Ordering::SeqCst);
            }
            Ok(FakeCoverage::default())
        }
        /// No case here damages a store, so the coverage comes straight
        /// back: rung 3 over derived state nothing damaged is the identity.
        fn rebuild(
            &self,
            _: &VaultName,
            attachment: FakeCoverage,
            _: &ProgressReporter<FakeCoverage>,
        ) -> Result<FakeCoverage, JobFailure> {
            Ok(attachment)
        }
        fn reconcile(
            &self,
            _: &VaultName,
            _: &mut FakeCoverage,
            _: ReconcileWork,
            _: &ProgressReporter<FakeCoverage>,
        ) -> Result<(), JobFailure> {
            Ok(())
        }
        fn recover(
            &self,
            _: &VaultName,
            _: &mut FakeCoverage,
            _: &ProgressReporter<FakeCoverage>,
        ) -> Result<(), JobFailure> {
            Ok(())
        }
        fn poll(
            &self,
            name: &VaultName,
            _: &mut FakeCoverage,
        ) -> Result<Option<Batch>, JobFailure> {
            Ok(
                (name.as_str() == "a" && !self.a_polled.swap(true, Ordering::SeqCst))
                    .then(|| Batch::rescan(RescanScope::Vault)),
            )
        }
        fn detach(&self, _: &VaultName, _: FakeCoverage) {}
    }

    #[test]
    fn unrelated_attaches_use_distinct_worker_slots() {
        let ops = Arc::new(QueueFullOps::default());
        let a = VaultName::new("a").unwrap();
        let b = VaultName::new("b").unwrap();
        let registry = RegistryRead::from_entries([
            RegistryEntry::new(
                a.clone(),
                VaultRoot::new("/tmp/norn-host-parallel-a").unwrap(),
            ),
            RegistryEntry::new(
                b.clone(),
                VaultRoot::new("/tmp/norn-host-parallel-b").unwrap(),
            ),
        ]);
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

        let a_lease = host.demand(&a, AttachMode::Durable).unwrap();
        wait_for_flag("a_started", &ops.a_started);
        let b_lease = host.demand(&b, AttachMode::Durable).unwrap();
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
        let registry = RegistryRead::from_entries([
            RegistryEntry::new(a.clone(), VaultRoot::new("/tmp/norn-host-queue-a").unwrap()),
            RegistryEntry::new(b.clone(), VaultRoot::new("/tmp/norn-host-queue-b").unwrap()),
        ]);
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
        let a_lease = host.demand(&a, AttachMode::Durable).unwrap();
        wait_for_flag("a_started", &ops.a_started);
        let b_lease = host.demand(&b, AttachMode::Durable).unwrap();
        ops.release_a.store(true, Ordering::SeqCst);
        wait_for_state(&host, &a, TrustState::Ready);
        wait_for_state(&host, &b, TrustState::Ready);
        drop((a_lease, b_lease));
    }

    #[test]
    fn public_demand_returns_warming_when_the_bounded_queue_is_full() {
        let ops = Arc::new(QueueFullOps::default());
        let names = ["a", "b", "c"].map(|name| VaultName::new(name).unwrap());
        let registry = RegistryRead::from_entries(names.iter().map(|name| {
            RegistryEntry::new(
                name.clone(),
                VaultRoot::new(format!("/tmp/norn-host-full-queue-{name}")).unwrap(),
            )
        }));
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
        let a = host.demand(&names[0], AttachMode::Durable).unwrap();
        wait_for_flag("a_started", &ops.a_started);
        let b = host.demand(&names[1], AttachMode::Durable).unwrap();
        let started = Instant::now();
        let c = host.demand(&names[2], AttachMode::Durable).unwrap();
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
        let registry = RegistryRead::from_entries(names.iter().map(|name| {
            RegistryEntry::new(
                name.clone(),
                VaultRoot::new(format!("/tmp/norn-host-send-race-{name}")).unwrap(),
            )
        }));
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
        let a = host.demand(&names[0], AttachMode::Durable).unwrap();
        wait_for_flag("a_started", &ops.a_started);
        let b = host.demand(&names[1], AttachMode::Durable).unwrap();

        let entry = host.shared.entries.get(&names[2]).unwrap();
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

    /// A dispatcher tick takes the queue slot before it observes shutdown. A
    /// refused send gives that slot back, because no channel holds the job it
    /// names even though the terminal host will never dispatch it again.
    #[test]
    fn shutdown_after_a_dispatch_takes_the_slot_gives_the_slot_back() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let entry = host.shared.entries.get(&name).unwrap();
        let epoch = {
            let mut state = entry.gate.lock().unwrap();
            let job = state
                .claim
                .schedule(|epoch| Job::Attach(name.clone(), epoch));
            job.epoch()
        };

        let job = {
            let mut state = entry.gate.lock().unwrap();
            state.claim.take_slot_for_marked().unwrap()
        };
        host.shared.shutting_down.store(true, Ordering::SeqCst);
        assert!(matches!(
            dispatch_taken_job(&host.shared, &entry, job),
            Err(HostError::WorkerStopped)
        ));
        assert_eq!(
            entry.gate.lock().unwrap().claim.slot(),
            None,
            "a shutdown-refused dispatch kept the queue slot it took at epoch {epoch}"
        );
    }

    /// A dispatcher tick can likewise take the queue slot before discovering
    /// that the worker channel is gone. No channel accepted the job, so the
    /// refused dispatch gives the slot back before reporting the stopped pool.
    #[test]
    fn a_disconnected_dispatch_gives_its_queue_slot_back() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let entry = host.shared.entries.get(&name).unwrap();
        {
            let mut state = entry.gate.lock().unwrap();
            state
                .claim
                .schedule(|epoch| Job::Attach(name.clone(), epoch));
        }
        let (jobs, receiver) = mpsc::sync_channel(1);
        drop(receiver);
        *host.shared.jobs.lock().unwrap() = Some(jobs);

        assert!(matches!(
            dispatch_pending(&host.shared, &entry),
            Err(HostError::WorkerStopped)
        ));
        assert_eq!(
            entry.gate.lock().unwrap().claim.slot(),
            None,
            "a disconnected dispatch kept a queue slot for a job no channel holds"
        );
    }

    /// A missing sender is the stopped-pool spelling reached after host
    /// teardown has taken the sender but a caller still holds the shared state.
    /// The caller took a slot before observing that absence, so it gives it back.
    #[test]
    fn a_dispatch_with_no_sender_gives_its_queue_slot_back() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let entry = host.shared.entries.get(&name).unwrap();
        {
            let mut state = entry.gate.lock().unwrap();
            state
                .claim
                .schedule(|epoch| Job::Attach(name.clone(), epoch));
        }
        host.shared.jobs.lock().unwrap().take();

        assert!(matches!(
            dispatch_pending(&host.shared, &entry),
            Err(HostError::WorkerStopped)
        ));
        assert_eq!(
            entry.gate.lock().unwrap().claim.slot(),
            None,
            "a dispatch with no sender kept a queue slot for a job no channel holds"
        );
    }

    /// A leg's follow-up has already taken the entry's queue slot when the
    /// worker channel disappears. The refusal gives the slot back because no
    /// channel holds the job it names.
    #[test]
    fn a_disconnected_follow_up_gives_its_queue_slot_back() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let entry = host.shared.entries.get(&name).unwrap();
        let job = {
            let mut state = entry.gate.lock().unwrap();
            let job = state
                .claim
                .hand_on(|epoch| Job::Attach(name.clone(), epoch));
            state.claim.take_slot(job.epoch());
            job
        };
        host.shared.jobs.lock().unwrap().take();

        dispatch_followup(&host.shared, job);

        assert_eq!(
            entry.gate.lock().unwrap().claim.slot(),
            None,
            "a disconnected follow-up kept a queue slot for a job no channel holds"
        );
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
        let registry = RegistryRead::from_entries([
            RegistryEntry::new(a.clone(), VaultRoot::new(&a_root).unwrap()),
            RegistryEntry::new(b.clone(), VaultRoot::new(&b_root).unwrap()),
        ]);
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
        let a_lease = host.demand(&a, AttachMode::Durable).unwrap();
        wait_for_flag("a_started", &ops.a_started);
        let b_lease = host.demand(&b, AttachMode::Durable).unwrap();
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
        let registry = RegistryRead::from_entries([
            RegistryEntry::new(a.clone(), VaultRoot::new(&a_root).unwrap()),
            RegistryEntry::new(b.clone(), VaultRoot::new(&b_root).unwrap()),
        ]);
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
        let a_lease = host.demand(&a, AttachMode::Durable).unwrap();
        let b_lease = host.demand(&b, AttachMode::Durable).unwrap();
        wait_for_state(&host, &a, TrustState::Ready);
        wait_for_state(&host, &b, TrustState::Ready);

        ops.block_poll.store(true, Ordering::SeqCst);
        wait_for_flag("poll_started", &ops.poll_started);
        std::fs::remove_dir(&b_root).unwrap();
        symlink(&a_root, &b_root).unwrap();
        park_on_current_classification(&host.shared, &b);
        assert!(matches!(a_lease.completion(), Demand::DuplicateRoot(_)));
        assert!(matches!(b_lease.completion(), Demand::DuplicateRoot(_)));
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);

        ops.poll_release.store(true, Ordering::SeqCst);
        wait_for_detaches(&ops, 2, "both aliases to detach");
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 2);
        assert!(matches!(a_lease.completion(), Demand::DuplicateRoot(_)));
        assert!(matches!(b_lease.completion(), Demand::DuplicateRoot(_)));
        drop((a_lease, b_lease, host));
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn identity_refusal_invalidates_and_detaches_a_live_entry() {
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
        let registry = RegistryRead::from_entries([RegistryEntry::new(
            name.clone(),
            VaultRoot::new(&root).unwrap(),
        )]);
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
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        ops.block_poll.store(true, Ordering::SeqCst);
        wait_for_flag("poll_started", &ops.poll_started);

        std::fs::remove_dir(&root).unwrap();
        symlink("root", &root).unwrap();
        park_on_current_classification(&host.shared, &name);
        assert_the_identity_park_stands_on_both_surfaces(&host, &name);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        ops.poll_release.store(true, Ordering::SeqCst);
        wait_for_detaches(&ops, 1, "the refused alias to detach");
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        settle();
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        assert_the_identity_park_stands_on_both_surfaces(&host, &name);
        drop(host);
        let _ = std::fs::remove_dir_all(base);
    }

    /// A poll the entry has moved past gives back everything it took: the
    /// attachment goes to [`EntryOps::detach`], and the claim and the marker go
    /// back to the entry. The lease standing against the entry by then is
    /// scheduled there, because the poll is the last thing holding the entry
    /// and the dispatcher reaches an entry holding no attachment for nothing.
    ///
    /// The invalidator is an identity refusal, which parks the entry against
    /// its own root and takes the coverage back. The demand below withdraws
    /// that park — so the lease it returns is raised against an entry that owes
    /// work and has a poll in flight over it.
    #[cfg(unix)]
    #[test]
    fn a_superseded_poll_gives_the_entry_back_and_schedules_the_lease_on_it() {
        let base = temp_base("superseded-poll-give-back");
        let root = base.join("root");
        let ops = Arc::new(FakeOps::default());
        let name = VaultName::new("notes").unwrap();
        let host = host_over_roots(Arc::clone(&ops), &[(&name, &root)], 1);
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_poll.store(true, Ordering::SeqCst);
        wait_for_flag("poll_started", &ops.poll_started);
        refuse_root_identity(&root);
        park_on_current_classification(&host.shared, &name);
        assert_the_identity_park_stands_on_both_surfaces(&host, &name);

        // The root answers again. The poll still holds the entry, so the lease
        // the demand below returns is recorded against an entry nothing can
        // schedule against yet.
        std::fs::remove_file(&root).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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

    /// A job leg the entry has moved past ends the same way a superseded poll
    /// does: the coverage it took goes to [`EntryOps::detach`], the claim goes
    /// back to the entry, and the lease standing against the entry by then is
    /// scheduled there. The entry holds no coverage once the leg's is back, so
    /// a watcher poll passes over it and no later tick reaches the lease.
    ///
    /// The invalidator is an identity refusal, which parks the entry and leaves
    /// the coverage the reconcile is holding to that leg's end. The demand
    /// below withdraws the park and asks for the recovery the refusal left the
    /// entry owing, so the lease it returns is raised against an entry that
    /// owes work and has a leg in flight over it.
    #[test]
    fn a_superseded_job_leg_gives_the_entry_back_and_schedules_the_lease_on_it() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        // Watcher facts schedule the reconcile, and blocking it is what holds
        // the entry's coverage out with a job leg.
        ops.block_reconcile.store(true, Ordering::SeqCst);
        report_through_a_driven_poll(&ops, &host, &name, &ops.off_thread_rescan_poll_batches);
        wait_for_flag("reconcile_started", &ops.reconcile_started);

        refuse_identity_error(&host.shared, &name, "root unreadable".to_string());
        assert!(
            refuses_identity(&entry_park(&host, &name).expect("the entry stands on a park")),
            "the refusal raised no park"
        );

        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        assert!(
            entry_park(&host, &name).is_none(),
            "the demand left the registry's park standing over the entry"
        );
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            1,
            "a lease raised under a live job leg scheduled work against it"
        );

        ops.block_reconcile.store(false, Ordering::SeqCst);
        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(
            ops.detaches.load(Ordering::SeqCst),
            1,
            "the superseded leg kept the coverage it took"
        );
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            2,
            "the lease waited on a second demand for the acquisition it was owed"
        );
        assert_eq!(lease.completion(), Demand::State(TrustState::Ready));
        drop((lease, host));
    }

    /// The other arm of the same epilogue: a leg that ends still standing at
    /// the entry's own epoch schedules nothing for the lease standing over it,
    /// whatever that lease is owed.
    ///
    /// The attach below fails terminally, which leaves the entry untrusted
    /// holding no coverage and on no park — the shape
    /// [`DemandedWork::owed_by`] answers with another attach, and the one
    /// [`schedule_demanded_work`] admits. What holds the acquisition back is
    /// the epoch alone: a terminal failure restarts no coverage of its own,
    /// and a fresh demand is what the entry waits for.
    ///
    /// The lease is held across the wait. Dropping it before the leg ends
    /// takes the demand out of the entry, and with it the thing this rules
    /// out.
    #[test]
    fn a_lease_held_across_a_terminal_attach_does_not_re_acquire_coverage() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        ops.terminal_attach.store(true, Ordering::SeqCst);

        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, backend_lost());
        settle();
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            1,
            "the terminally failed attach acquired coverage again on its own"
        );
        assert_eq!(
            host.state(&name),
            answered(backend_lost()),
            "the entry moved off the failure nothing addressed"
        );
        assert!(
            entry_park(&host, &name).is_none(),
            "the entry stands on a park, so the epoch is not what held the \
             acquisition back"
        );

        drop(lease);
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            2,
            "the demand that followed the failure acquired nothing"
        );
        drop((lease, host));
    }

    /// An invalidation takes the work scheduled against the poll it moves past.
    /// The marker below is what a follow-up leaves when a full queue refuses
    /// its send: it names the poll's own epoch and holds the gate until the
    /// invalidator drops it. If it survives, the stale poll gives that marker
    /// back with the gate still held and the demanded re-attach cannot run.
    #[test]
    fn an_identity_refusal_drops_work_scheduled_against_the_poll_it_invalidates() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_poll.store(true, Ordering::SeqCst);
        let shared = Arc::clone(&host.shared);
        let poll = thread::spawn(move || poll_watchers(&shared));
        wait_for_flag("poll_started", &ops.poll_started);

        let entry = host.shared.entries.get(&name).unwrap();
        {
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            let epoch = state.claim.epoch();
            state.claim.restore(Job::Reconcile(name.clone(), epoch));
            assert_eq!(state.claim.marker().map(Job::epoch), Some(epoch));
        }

        refuse_identity_error(&host.shared, &name, "root unreadable".to_string());
        assert!(
            entry
                .gate
                .lock()
                .expect("entry gate poisoned")
                .claim
                .marker()
                .is_none(),
            "the invalidation left work scheduled against the poll it superseded"
        );

        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        ops.poll_release.store(true, Ordering::SeqCst);
        poll.join().unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);
        drop(lease);
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
    /// The recheck is the arm every refusing root reaches before the
    /// acquisition: it is the only read the attach makes of the root's identity
    /// on this side of the heal, and the claim the attach files is filed under
    /// what it resolved.
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
        let holding_lease = host.demand(&holding, AttachMode::Durable).unwrap();
        wait_for_flag("attach_started", &ops.attach_started);
        let lease = host.demand(&subject, AttachMode::Durable).unwrap();

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
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
        assert_the_identity_park_stands_on_both_surfaces(&host, &name);

        drop((lease, host));
        let _ = std::fs::remove_dir_all(base);
    }

    /// The demand seam carries the attach mode, and the mode it holds no
    /// lifecycle for is answered at the seam: the lease that comes back holds
    /// nothing, carries the refusal, and leaves the entry as it stood.
    ///
    /// The refusal rides the demand vocabulary, so it is read where every other
    /// refusal is read — off the lease's completion, and in the wire vocabulary
    /// through the envelope that completion answers with, carrying the mode
    /// that was named.
    ///
    /// The parks below are what say the refusal comes before the entry is
    /// touched, one for each door. The fixture's root is one the registry reads
    /// without complaint, so the recheck [`Host::demand`] runs retires an
    /// identity park: a park still standing after a refused demand is a demand
    /// that answered the mode before it read the registry, and the lease count
    /// beside it says the same about the entry's own state. [`Host::retry`]
    /// retires the contention park on its way to the demand behind it, so a
    /// park still standing after a refused retry is a retry that answered the
    /// mode first.
    #[test]
    fn a_throwaway_demand_is_refused_before_the_entry_is_touched() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let entry = host.shared.entries.get(&name).unwrap();

        entry.gate.lock().unwrap().identity_refused = Some("the root cannot be read".into());
        let refused = host
            .demand(&name, AttachMode::Throwaway)
            .expect("the mode is refused through the demand vocabulary, not the error channel");
        assert_eq!(
            refused.completion(),
            Demand::UnsupportedMode(AttachMode::Throwaway),
            "the seam admitted a mode it holds no lifecycle for"
        );
        let envelope = refused
            .answer()
            .expect_err("a mode the host holds no lifecycle for is a refusal");
        assert_eq!(
            envelope.detail(),
            &ErrorDetail::unsupported_attach_mode(AttachMode::Throwaway),
            "the refusal names another mode"
        );
        {
            let state = entry.gate.lock().unwrap();
            assert_eq!(
                state.identity_refused.as_deref(),
                Some("the root cannot be read"),
                "the refused demand rechecked the registry over the entry"
            );
            assert_eq!(
                state.demand_leases, 0,
                "the refused demand recorded a lease against the entry"
            );
        }
        // The refused lease is dropped here rather than at the end, so the
        // rest of the case reads the entry's count with nothing outstanding.
        // What its drop withdraws is read below, against a lease the entry
        // holds: a withdrawal against a count already at zero is invisible,
        // because the count saturates at its floor.
        drop(refused);
        settle();
        // Read underneath the identity park the case installed: what is pinned
        // is that the entry never left the state the refusal found it in.
        assert_eq!(published_label(&host, &name), Some(TrustState::Unattached));
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            0,
            "the refused mode scheduled work against the entry"
        );

        {
            let mut state = entry.gate.lock().unwrap();
            state.identity_refused = None;
            state.maintainer_contended = Some(MaintainerIdentity::unknown());
        }
        let refused_retry = host
            .retry(&name, AttachMode::Throwaway)
            .expect("the mode is refused through the demand vocabulary, not the error channel");
        assert_eq!(
            refused_retry.completion(),
            Demand::UnsupportedMode(AttachMode::Throwaway),
            "the retry admitted a mode it holds no lifecycle for"
        );
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        assert!(
            matches!(*lease.outcome(), Demand::MaintainerContended(_)),
            "the refused retry retired the park it never reached"
        );
        // A lease holding nothing withdraws nothing on its way out, and the
        // durable lease above is what makes that readable: the entry counts
        // one, the refused lease is dropped against that count, and the count
        // still stands at one. Against a count of zero the same drop reads the
        // same whether it withdrew nothing or withdrew below a floor that
        // saturates.
        assert_eq!(
            entry.gate.lock().unwrap().demand_leases,
            1,
            "the durable demand recorded no lease to withdraw against"
        );
        drop(refused_retry);
        assert_eq!(
            entry.gate.lock().unwrap().demand_leases,
            1,
            "dropping the refused lease withdrew the lease standing beside it"
        );
        drop(lease);
        assert_eq!(
            entry.gate.lock().unwrap().demand_leases,
            0,
            "the durable lease withdrew nothing on its way out"
        );

        drop(host);
    }

    /// The registry refusal an attach finds before it acquires anything parks
    /// the entry on that refusal, and the lease standing over the attach
    /// reports the park.
    ///
    /// A root the registry cannot read is one fact whichever read found it: the
    /// recheck a demand runs answers it as the identity park, and so does the
    /// recheck an attach runs. The lease is what says so, because the park is
    /// what it reports and a trust state beside one says nothing about whether
    /// anything more is coming.
    ///
    /// The window is the queue: the one worker is inside another vault's
    /// attach, so the job scheduled for the entry under test waits in the
    /// channel while its root is retargeted.
    #[cfg(unix)]
    #[test]
    fn an_attach_time_registry_refusal_parks_the_entry() {
        let base = temp_base("attach-recheck-park");
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
        let holding_lease = host.demand(&holding, AttachMode::Durable).unwrap();
        wait_for_flag("attach_started", &ops.attach_started);
        let lease = host.demand(&subject, AttachMode::Durable).unwrap();

        refuse_root_identity(&subject_root);
        ops.attach_release.store(true, Ordering::SeqCst);
        wait_for_environmental_refusal(&host, &subject);
        assert!(
            refuses_identity(&lease.completion()),
            "the lease over the refused attach reported no park"
        );

        settle();
        assert!(
            refuses_identity(&lease.completion()),
            "a tick retired the park the refused attach raised"
        );

        drop((lease, holding_lease, host));
        let _ = std::fs::remove_dir_all(base);
    }

    /// The registry refusal an attach finds after its heal parks the entry it
    /// gave the attachment back from, and the entry stands still under that
    /// park: the lease answers the refusal rather than the trust state beside
    /// it, and nothing arms the attach again while the park stands.
    ///
    /// The window is the attach itself: the demand is raised, the root is
    /// retargeted while the heal is blocked inside the fake, and the
    /// revalidation that runs when the heal is released is what refuses.
    #[cfg(unix)]
    #[test]
    fn a_post_heal_registry_refusal_parks_the_entry_and_arms_nothing() {
        let base = temp_base("post-heal-recheck-park");
        let root = base.join("root");
        let ops = Arc::new(FakeOps::default());
        let name = VaultName::new("notes").unwrap();
        let host = host_over_roots(Arc::clone(&ops), &[(&name, &root)], 1);

        ops.block_attach.store(true, Ordering::SeqCst);
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_flag("attach_started", &ops.attach_started);
        refuse_root_identity(&root);
        ops.attach_release.store(true, Ordering::SeqCst);

        wait_for_environmental_refusal(&host, &name);
        assert!(
            refuses_identity(&lease.completion()),
            "the lease over the refused attach reported no park"
        );

        settle();
        assert!(
            refuses_identity(&lease.completion()),
            "a tick retired the park the refused attach raised"
        );
        assert_eq!(
            ops.attaches.load(Ordering::SeqCst),
            1,
            "an ambient tick armed the attach again over the park"
        );
        {
            // The refused attach gave the gate back and left nothing armed
            // behind it: an entry standing still under the park is one no
            // dispatch reaches, and a claim or a marker left here is work the
            // park was supposed to have ended.
            let entry = host.shared.entries.get(&name).unwrap();
            let state = entry.gate.lock().unwrap();
            assert!(
                !state.claim.is_held(),
                "the refused attach left the entry claimed under the park"
            );
            assert!(
                state.claim.marker().is_none(),
                "the refused attach left a job armed under the park"
            );
        }

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
        let registry = RegistryRead::from_entries([
            RegistryEntry::new(healthy.clone(), VaultRoot::new(&healthy_root).unwrap()),
            RegistryEntry::new(refused.clone(), VaultRoot::new(&refused_root).unwrap()),
        ]);
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
        let healthy_lease = host.demand(&healthy, AttachMode::Durable).unwrap();
        let refused_lease = host.demand(&refused, AttachMode::Durable).unwrap();
        wait_for_state(&host, &healthy, TrustState::Ready);
        wait_for_state(&host, &refused, TrustState::Ready);
        std::fs::remove_dir(&refused_root).unwrap();
        symlink("refused", &refused_root).unwrap();

        let renewed_healthy = host.demand(&healthy, AttachMode::Durable).unwrap();
        assert_eq!(
            renewed_healthy.completion(),
            Demand::State(TrustState::Ready)
        );
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);

        park_on_current_classification(&host.shared, &refused);
        assert_the_identity_park_stands_on_both_surfaces(&host, &refused);
        assert_eq!(host.state(&healthy), answered(TrustState::Ready));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);

        drop((renewed_healthy, refused_lease, healthy_lease, host));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn dispatcher_closes_ready_before_reconciling_a_polled_batch() {
        let ops = Arc::new(PollingOps::default());
        let name = VaultName::new("notes").unwrap();
        let registry = RegistryRead::from_entries([RegistryEntry::new(
            name.clone(),
            VaultRoot::new("/tmp/norn-host-poll-fixture").unwrap(),
        )]);
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
        let _ = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        ops.hold_reconcile.store(true, Ordering::SeqCst);
        ops.emit.store(true, Ordering::SeqCst);
        wait_for_flag("reconcile_started", &ops.reconcile_started);
        wait_until(
            "warming progress to advance past zero",
            lifecycle_wait_budget(),
            || match host.state(&name) {
                Ok(TrustState::Warming { healed, .. }) if healed > 0 => Observed::Met(()),
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
        let registry = RegistryRead::from_entries([RegistryEntry::new(
            name.clone(),
            VaultRoot::new("/tmp/norn-host-pinned-idle").unwrap(),
        )]);
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
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
                Ok(TrustState::Warming { .. }) => Observed::Met(()),
                state => Observed::pending(format!("the state is {state:?}")),
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        drop(lease);
        host.reap_idle(Instant::now()).unwrap();
        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Unattached);
    }

    /// A fixture whose entries fall idle the instant their last lease ends,
    /// and whose dispatcher never ticks inside a case's own run: the reap
    /// under test is the one the case calls, at the moment it calls it.
    fn fixture_reaped_on_demand(ops: Arc<FakeOps>) -> (Host<Arc<FakeOps>>, VaultName) {
        let name = VaultName::new("notes").unwrap();
        let entry = RegistryEntry::new(
            name.clone(),
            VaultRoot::new("/tmp/norn-host-reader-slot-fixture").unwrap(),
        );
        let registry = RegistryRead::from_entries([entry]);
        let host = Host::new(
            registry,
            ops,
            LifecyclePolicy {
                idle_after: Duration::ZERO,
                worker_slots: 1,
                watch_poll_interval: Duration::from_secs(60),
            },
        )
        .unwrap();
        (host, name)
    }

    /// The reader an entry serves reads from is minted where its coverage is
    /// installed, so the handle and the trust label the entry publishes over
    /// that coverage land under one lock. A read takes both out of one lock
    /// too, which is what makes the label it answers under and the handle it
    /// answers from describe the same instant.
    #[test]
    fn an_attach_publishes_a_reader_beside_the_coverage_it_installs() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        assert_eq!(
            ops.readers.opened.load(Ordering::SeqCst),
            1,
            "the coverage the attach installed minted no reader"
        );
        assert_eq!(ops.readers.closed.load(Ordering::SeqCst), 0);
        assert!(
            reader_stands(&host, &name),
            "an entry publishing Ready holds no reader for a read to run on"
        );

        let hold = host
            .begin_read(&name)
            .expect("an entry holding a reader answers a read");
        assert_eq!(
            hold.trust(),
            &TrustState::Ready,
            "the hold reports a label the entry does not stand at"
        );
        assert!(
            Arc::ptr_eq(&hold.reader().0, &ops.readers),
            "the read runs on a handle the entry's own coverage did not mint"
        );
    }

    /// An attach that installs no coverage mints no reader. The mint is a limb
    /// of the install rather than a step beside it, so an attach that acquired
    /// resources and gave them back leaves the slot exactly as it found it.
    #[cfg(unix)]
    #[test]
    fn an_attach_that_installs_no_coverage_mints_no_reader() {
        let base = temp_base("reader-slot-uninstalled-coverage");
        let root = base.join("root");
        let ops = Arc::new(FakeOps::default());
        let name = VaultName::new("notes").unwrap();
        let host = host_over_roots(Arc::clone(&ops), &[(&name, &root)], 1);

        ops.block_attach.store(true, Ordering::SeqCst);
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_flag("attach_started", &ops.attach_started);
        refuse_root_identity(&root);
        ops.attach_release.store(true, Ordering::SeqCst);

        wait_for_environmental_refusal(&host, &name);
        assert_eq!(
            ops.detaches.load(Ordering::SeqCst),
            1,
            "the refused attach kept the resources its heal acquired"
        );
        assert_eq!(
            ops.readers.opened.load(Ordering::SeqCst),
            0,
            "an attach that installed no coverage minted a reader from it"
        );
        assert!(!reader_stands(&host, &name));
        assert!(host.begin_read(&name).is_none());

        drop((lease, host));
        let _ = std::fs::remove_dir_all(base);
    }

    /// An attach the entry moved on from installs nothing, so it mints
    /// nothing. What that attach acquired goes back where its leg ends, and a
    /// handle minted from it would be one the entry answers reads on over a
    /// store already on its way to the ops.
    #[test]
    fn an_attach_the_entry_moved_on_from_mints_no_reader() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        ops.block_attach.store(true, Ordering::SeqCst);
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_flag("attach_started", &ops.attach_started);

        // The refusal moves the entry past the epoch the attach carries, so
        // the publication below finds an entry that has left the work it ran.
        refuse_conflict(&host.shared, &AliasConflict::new([name.clone()]));
        ops.attach_release.store(true, Ordering::SeqCst);

        wait_for_detaches(
            &ops,
            1,
            "the attach to give back the coverage the entry moved on from",
        );
        assert_eq!(
            ops.readers.opened.load(Ordering::SeqCst),
            0,
            "an attach the entry moved on from minted a reader from what it acquired"
        );
        assert!(!reader_stands(&host, &name));

        drop(lease);
    }

    /// The reader is let go of at the start of the release window rather than
    /// at its end: the store the handle was minted from closes inside that
    /// window, and an entry still holding the handle when it does is one every
    /// later read runs against a closed store.
    #[test]
    fn a_teardown_closes_the_reader_before_the_store_goes_back() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_reaped_on_demand(Arc::clone(&ops));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        ops.block_detach.store(true, Ordering::SeqCst);
        host.reap_idle(Instant::now()).unwrap();
        wait_for_flag("detach_started", &ops.detach_started);

        assert_eq!(
            ops.readers.closed.load(Ordering::SeqCst),
            1,
            "the entry was still holding its reader while the store went back"
        );
        assert!(!reader_stands(&host, &name));
        assert!(
            host.begin_read(&name).is_none(),
            "an entry whose coverage is going back answered a read"
        );

        ops.detach_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Unattached);
    }

    /// A refusal that opens a release window over the leg holding the entry's
    /// coverage closes the reader at the window, not at the leg's own end. The
    /// window is where the entry stops being readable, and every teardown
    /// enters there.
    #[test]
    fn a_refusal_over_a_leg_holding_the_coverage_closes_the_reader_at_the_window() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        let entry = host
            .shared
            .entries
            .get(&name)
            .expect("the vault is registered");

        let (epoch, attachment) = {
            let mut state = entry.gate.lock().unwrap();
            let epoch = state.claim.epoch();
            state.claim.begin_job_leg(epoch);
            let attachment = state
                .coverage
                .take(epoch)
                .expect("the entry parked its coverage");
            (epoch, attachment)
        };
        refuse_conflict(&host.shared, &AliasConflict::new([name.clone()]));

        {
            let state = entry.gate.lock().unwrap();
            assert!(state.detach_in_flight, "no window is open over the leg");
            assert!(
                state.reader.is_none(),
                "a refused entry is still serving reads from a store the leg is taking back"
            );
        }
        assert_eq!(ops.readers.closed.load(Ordering::SeqCst), 1);

        end_job_leg(&host.shared, &entry, &name, epoch, Some(attachment));
        wait_for_published_label(&host, &name, TrustState::Unattached);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
    }

    /// The identity refusal opens no release window, and closes the reader all
    /// the same. Both routes it takes end with the coverage at the ops — given
    /// up under its own lock, or handed up where the leg holding it ends — so
    /// a handle the entry kept would outlast the store on either.
    #[test]
    fn an_identity_refusal_closes_the_reader_it_gives_the_coverage_back_with() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        refuse_identity_error(&host.shared, &name, "the root cannot be read".into());

        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        assert_eq!(
            ops.readers.closed.load(Ordering::SeqCst),
            1,
            "a refused entry kept the reader minted from the store it gave back"
        );
        assert!(!reader_stands(&host, &name));
    }

    /// The refusal's other route: the coverage is out with a leg, so the
    /// refusal gives back nothing itself and the leg's own end is what reaches
    /// the ops. The reader is closed at the refusal all the same — the entry
    /// stops being readable where the refusal lands, and the leg that follows
    /// hands the store to [`EntryOps::detach`] with no close of its own.
    #[test]
    fn an_identity_refusal_over_a_leg_holding_the_coverage_closes_the_reader() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        let entry = host
            .shared
            .entries
            .get(&name)
            .expect("the vault is registered");

        let (epoch, attachment) = {
            let mut state = entry.gate.lock().unwrap();
            let epoch = state.claim.epoch();
            state.claim.begin_job_leg(epoch);
            let attachment = state
                .coverage
                .take(epoch)
                .expect("the entry parked its coverage");
            (epoch, attachment)
        };
        refuse_identity_error(&host.shared, &name, "the root cannot be read".into());

        assert_eq!(
            ops.detaches.load(Ordering::SeqCst),
            0,
            "the refusal gave back coverage the leg is holding"
        );
        assert_eq!(
            ops.readers.closed.load(Ordering::SeqCst),
            1,
            "the entry kept its reader over coverage the leg below gives back"
        );
        assert!(!reader_stands(&host, &name));
        assert!(host.begin_read(&name).is_none());

        end_job_leg(&host.shared, &entry, &name, epoch, Some(attachment));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
    }

    /// An in-flight read holds the entry the way a leg running outside the
    /// entry's lock does: a reap that finds the entry idle schedules no
    /// teardown while the read stands, and the teardown it was owed runs once
    /// the read gives the entry back.
    #[test]
    fn a_read_in_flight_holds_the_entry_against_an_idle_teardown() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_reaped_on_demand(Arc::clone(&ops));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        let hold = host
            .begin_read(&name)
            .expect("an entry holding a reader answers a read");
        host.reap_idle(Instant::now()).unwrap();
        settle();
        assert_eq!(
            ops.detaches.load(Ordering::SeqCst),
            0,
            "a teardown ran under a read still holding the entry"
        );
        assert_eq!(host.state(&name), answered(TrustState::Ready));

        drop(hold);
        host.reap_idle(Instant::now()).unwrap();
        wait_for_state(&host, &name, TrustState::Unattached);
        assert_eq!(ops.readers.closed.load(Ordering::SeqCst), 1);
    }

    /// The other order, and the gap the contract admits. `schedule_due_detach`
    /// reads the pin where it schedules and nowhere after, so a teardown
    /// already scheduled is a teardown a later read does not hold back: the
    /// read is answered, and the entry is torn down while the hold stands.
    ///
    /// What the read goes on holding is its own clone of the handle, which is
    /// the whole of what it has. Nothing here says the store behind that handle
    /// is still open, and nothing pins that it is.
    #[test]
    fn a_detach_scheduled_before_a_read_tears_the_entry_down_under_it() {
        let ops = Arc::new(FakeOps::default());
        let working = VaultName::new("working").unwrap();
        let occupied = VaultName::new("occupied").unwrap();
        let host = host_without_ambient_polling(Arc::clone(&ops), &[&working, &occupied], 1);
        drop(host.demand(&working, AttachMode::Durable).unwrap());
        wait_for_state(&host, &working, TrustState::Ready);

        // The one worker, held inside another vault's attach: the detach the
        // reap below dispatches sits in the channel while this case reads the
        // entry it was scheduled against.
        ops.block_attach.store(true, Ordering::SeqCst);
        let occupier = host.demand(&occupied, AttachMode::Durable).unwrap();
        wait_for_flag("attach_started", &ops.attach_started);

        host.reap_idle(Instant::now() + Duration::from_secs(61))
            .unwrap();
        assert!(
            !reader_stands(&host, &occupied),
            "the occupier's attach published before the reap"
        );

        let hold = host
            .begin_read(&working)
            .expect("a scheduled teardown is not what a read consults");
        assert_eq!(hold.trust(), &TrustState::Ready);

        ops.attach_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &working, TrustState::Unattached);
        assert_eq!(
            ops.detaches.load(Ordering::SeqCst),
            1,
            "the scheduled teardown did not run"
        );
        assert!(!reader_stands(&host, &working));
        assert_eq!(
            ops.readers.closed.load(Ordering::SeqCst),
            0,
            "the entry was torn down and the read's own handle went with it"
        );

        drop(hold);
        assert_eq!(ops.readers.closed.load(Ordering::SeqCst), 1);
        drop(occupier);
    }

    /// Coverage that mints no reader leaves the slot empty, and the entry
    /// beside it answers no read while publishing the trust its coverage earns.
    ///
    /// That is the configuration production stands in: `ProductionAttachment`
    /// mints nothing, so a vault attaches, heals and reports Ready with no
    /// handle in its slot and every read against it refused for want of one.
    #[test]
    fn coverage_that_mints_no_reader_leaves_an_entry_no_read_reaches() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        ops.coverage_mints_no_reader.store(true, Ordering::SeqCst);
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        assert_eq!(
            ops.readers.opened.load(Ordering::SeqCst),
            0,
            "coverage that mints no reader minted one"
        );
        assert!(
            !reader_stands(&host, &name),
            "an entry over coverage that mints no reader is holding a handle"
        );
        assert!(
            host.begin_read(&name).is_none(),
            "an entry with an empty slot answered a read"
        );
    }

    /// Every read against one entry runs on that entry's one handle. A read in
    /// flight leaves the handle where the next read finds it, so what
    /// concurrent reads contend for is inside the handle rather than a slot
    /// they take turns emptying.
    #[test]
    fn concurrent_reads_share_the_entrys_one_reader() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        let first = host
            .begin_read(&name)
            .expect("an entry holding a reader answers a read");
        let second = host
            .begin_read(&name)
            .expect("a read in flight left the entry with no reader for the next");

        assert!(
            std::ptr::eq(first.reader(), second.reader()),
            "two reads against one entry ran on two handles"
        );
        assert_eq!(
            ops.readers.opened.load(Ordering::SeqCst),
            1,
            "a read minted a handle of its own rather than taking the entry's"
        );
    }

    /// The handle a read is running on outlives the entry's own hold on it. A
    /// teardown lets go of the slot; what closes a handle a read still has is
    /// that read ending.
    ///
    /// This characterizes the sharing decision rather than pinning a move: the
    /// entry hands a read a clone at [`Host::begin_read`] instead of a way to
    /// look the handle up per use, and what the case asserts below follows from
    /// that choice by [`Arc`]'s own accounting. Its ledger counts redden on no
    /// mutation of the lifecycle — a redesign of the slot's type is what would
    /// — while its refusal assertion re-pins a close the identity-refusal row
    /// already carries.
    #[test]
    fn a_reader_a_read_is_running_on_outlives_the_entrys_own() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        let hold = host
            .begin_read(&name)
            .expect("an entry holding a reader answers a read");
        refuse_identity_error(&host.shared, &name, "the root cannot be read".into());

        assert!(!reader_stands(&host, &name));
        assert_eq!(
            ops.readers.closed.load(Ordering::SeqCst),
            0,
            "the entry closed a handle a read was still running on"
        );

        drop(hold);
        assert_eq!(
            ops.readers.closed.load(Ordering::SeqCst),
            1,
            "the handle outlived the read that was running on it"
        );
    }

    #[test]
    fn quiet_attached_entry_eventually_runs_scheduled_maintenance() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
        assert_eq!(host.state(&name), answered(TrustState::Ready));
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
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
        let lease_a = host.demand(&a, AttachMode::Durable).unwrap();
        wait_for_state(&host, &a, TrustState::Ready);
        let lease_b = host.demand(&b, AttachMode::Durable).unwrap();
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
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
        assert!(refuses_environmentally(&host.state(&name)));
        assert_eq!(ops.reconciles.load(Ordering::SeqCst), 0);

        drop(lease);
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
        drop(host.demand(&name, AttachMode::Durable).unwrap());
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
        let lease = host.demand(&subject, AttachMode::Durable).unwrap();
        wait_for_state(&host, &subject, TrustState::Ready);

        // The one worker goes to the other vault, so a job sent for the entry
        // under test stays in the channel until this test lets the worker go.
        ops.block_attach.store(true, Ordering::SeqCst);
        let holding_lease = host.demand(&holding, AttachMode::Durable).unwrap();
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
        let lease = host.demand(&subject, AttachMode::Durable).unwrap();
        wait_for_state(&host, &subject, TrustState::Ready);

        // Both workers go to vaults that are not the one under test: the
        // channel then holds what this test sends for it, and has the room a
        // duplicate would take. Their claims are held by the legs blocking
        // them, so the tick below reaches the entry under test alone.
        ops.block_attach.store(true, Ordering::SeqCst);
        let holding_lease = host.demand(&holding, AttachMode::Durable).unwrap();
        let attaching_lease = host.demand(&attaching, AttachMode::Durable).unwrap();
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
        let lease = host.demand(&subject, AttachMode::Durable).unwrap();
        wait_for_state(&host, &subject, TrustState::Ready);

        // Both workers go to vaults that are not the one under test: the
        // channel then holds what this test sends for it, and has the room a
        // second send takes. Their claims are held by the legs blocking them,
        // so the tick below reaches the entry under test alone.
        ops.block_attach.store(true, Ordering::SeqCst);
        let holding_lease = host.demand(&holding, AttachMode::Durable).unwrap();
        let attaching_lease = host.demand(&attaching, AttachMode::Durable).unwrap();
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
        let lease = host.demand(&subject, AttachMode::Durable).unwrap();
        wait_for_state(&host, &subject, TrustState::Ready);

        // The one worker goes to a vault that is not the one under test, and
        // the channel's one place goes to a job no worker is left to take:
        // every send from here is refused.
        ops.block_attach.store(true, Ordering::SeqCst);
        let working_lease = host.demand(&working, AttachMode::Durable).unwrap();
        wait_for_flag("attach_started", &ops.attach_started);
        let waiting_lease = host.demand(&waiting, AttachMode::Durable).unwrap();
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
        let lease = host.demand(&working, AttachMode::Durable).unwrap();
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
        let holding = host.demand(&first_slot, AttachMode::Durable).unwrap();
        wait_for_flag("attach_started", &ops.attach_started);
        ops.attach_started.store(false, Ordering::SeqCst);
        let waiting = host.demand(&second_slot, AttachMode::Durable).unwrap();

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
            matches!(host.state(&working), Ok(TrustState::Warming { .. })),
            "the entry stopped waiting for the reconcile its leg handed it"
        );

        ops.attach_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &working, TrustState::Ready);
        assert_eq!(ops.reconciles.load(Ordering::SeqCst), 1);
        drop(lease);
        drop(holding);
        drop(waiting);
    }

    /// A watcher poll's end reaches an entry a job leg is running against. The
    /// poll is not the leg registered, so it ends nothing: the registration a
    /// release window was opened over stands, and the leg's own end is what
    /// closes that window.
    ///
    /// An end blind to the kind would clear the registration here, and the
    /// window opened over the leg would then close on nothing — the entry would
    /// stand in the releasing phase with its resources already back.
    #[test]
    fn a_poll_end_leaves_the_job_leg_a_release_window_waits_on() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        let entry = host.shared.entries.get(&name).unwrap();
        let (epoch, attachment) = {
            let mut state = entry.gate.lock().unwrap();
            let epoch = state.claim.epoch();
            state.claim.begin_job_leg(epoch);
            let attachment = state.coverage.take(epoch);
            assert!(attachment.is_some(), "the entry parked its coverage");
            // The window this leg's own end is what closes.
            begin_release(&mut state);
            (epoch, attachment)
        };

        // A poll standing at the same epoch, reaching the end of its own tick.
        entry.gate.lock().unwrap().claim.end_poll(epoch);
        assert!(
            matches!(
                entry.gate.lock().unwrap().claim.leg(),
                Some(Leg::Job(held)) if held == epoch
            ),
            "a poll's end cleared the job leg the release window waits on"
        );

        end_job_leg(&host.shared, &entry, &name, epoch, attachment);
        let state = entry.gate.lock().unwrap();
        assert_eq!(
            state.trust,
            TrustState::Unattached,
            "the release window never closed"
        );
        assert!(!state.detach_in_flight, "the release window is still open");
        assert!(!state.coverage.in_hand());
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
    }

    /// A job arriving at an epoch the entry has left gives back the marker that
    /// named it, and the gate goes to the leg running against the entry rather
    /// than open. Nothing else opens the gate on that path, so the derived gate
    /// is the whole of what a later tick reads: an open one would let the tick
    /// claim an entry a leg is already running against.
    #[test]
    fn a_stale_arrival_gives_its_marker_back_to_the_leg_running_against_the_entry() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);

        let entry = host.shared.entries.get(&name).unwrap();
        let stale = {
            let mut state = entry.gate.lock().unwrap();
            let stale = state.claim.epoch();
            // The job in the channel, the entry moved on from it, and a leg
            // running against the entry at the epoch it moved on to.
            state.claim.take_slot(stale);
            state.claim.supersede();
            let running = state.claim.epoch();
            state.claim.begin_job_leg(running);
            // The marker that job left behind when it lost its coverage.
            state.claim.restore(Job::Reconcile(name.clone(), stale));
            stale
        };

        run_job(&host.shared, Job::Reconcile(name.clone(), stale));
        {
            let state = entry.gate.lock().unwrap();
            assert!(
                state.claim.marker().is_none(),
                "a job the entry moved past left its marker standing"
            );
            assert!(
                state.claim.is_held(),
                "the gate went back over a leg still running against the entry"
            );
        }

        let polled_before = polls_of(&ops, &name);
        poll_watchers(&host.shared);
        assert_eq!(
            polls_of(&ops, &name),
            polled_before,
            "a tick claimed an entry a leg is running against"
        );

        // The leg the case stood in for, ending.
        {
            let mut state = entry.gate.lock().unwrap();
            state.claim.end_running_leg();
            state.claim.open();
        }
        drop(lease);
    }

    /// A dispatcher tick takes no queue slot for the job a running leg is about
    /// to send itself. The leg gave the gate back with a marker standing, so
    /// what the tick reads is scheduled work over a free slot, and the
    /// registration is the whole of what says the send is the leg's.
    #[test]
    fn a_tick_takes_no_slot_for_a_marker_the_running_leg_sends_itself() {
        let ops = Arc::new(FakeOps::default());
        let working = VaultName::new("a").unwrap();
        let holding = VaultName::new("b").unwrap();
        let host = host_without_ambient_polling(Arc::clone(&ops), &[&working, &holding], 1);
        let working_lease = host.demand(&working, AttachMode::Durable).unwrap();
        wait_for_state(&host, &working, TrustState::Ready);
        let holding_lease = host.demand(&holding, AttachMode::Durable).unwrap();
        wait_for_state(&host, &holding, TrustState::Ready);

        // The one worker, held inside the other vault's job: a job the tick
        // below sends stays in the queue, so the slot it took is the whole of
        // what the tick leaves behind.
        ops.block_reconcile.store(true, Ordering::SeqCst);
        report_through_a_driven_poll(&ops, &host, &holding, &ops.off_thread_rescan_poll_batches);
        wait_for_flag("reconcile_started", &ops.reconcile_started);

        let entry = host.shared.entries.get(&working).unwrap();
        {
            let mut state = entry.gate.lock().unwrap();
            let epoch = state.claim.epoch();
            // A leg whose job lost the coverage it was dispatched against: the
            // marker it recorded holds the gate, the leg is still registered,
            // and that leg is what sends the job the marker names.
            state.claim.begin_job_leg(epoch);
            state.claim.restore(Job::Reconcile(working.clone(), epoch));
            state.claim.release();
            assert!(state.claim.marker().is_some(), "nothing is scheduled");
            assert!(state.claim.slot().is_none(), "the slot is already taken");
        }

        dispatch_pending(&host.shared, &entry).unwrap();
        assert!(
            entry.gate.lock().unwrap().claim.slot().is_none(),
            "a tick sent the job the leg running against the entry sends itself"
        );

        // The leg the case stood in for, ending.
        {
            let mut state = entry.gate.lock().unwrap();
            state.claim.end_running_leg();
            state.claim.open();
        }
        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &holding, TrustState::Ready);
        drop(working_lease);
        drop(holding_lease);
    }

    /// A marker holds an entry's gate in its own right, so a tick passes over
    /// an entry with a job scheduled against it: nothing claims the entry, and
    /// the job it owes is still the job it owes when the tick has gone by.
    #[test]
    fn a_tick_passes_over_an_entry_a_marker_holds() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);

        let entry = host.shared.entries.get(&name).unwrap();
        let scheduled = {
            let mut state = entry.gate.lock().unwrap();
            // The entry's next job, waiting on a tick to send it: no leg is
            // running, so the marker is the whole of what holds the entry.
            let job = state
                .claim
                .schedule(|epoch| Job::Reconcile(name.clone(), epoch));
            assert!(
                state.claim.leg().is_none(),
                "a leg is holding the entry beside the marker"
            );
            job.epoch()
        };

        let polled_before = polls_of(&ops, &name);
        poll_watchers(&host.shared);
        assert_eq!(
            polls_of(&ops, &name),
            polled_before,
            "a tick claimed an entry a marker holds"
        );
        assert_eq!(
            entry.gate.lock().unwrap().claim.marker().map(Job::epoch),
            Some(scheduled),
            "a tick took the entry out from under the job it owes"
        );
        drop(lease);
    }

    /// A job that lost its coverage records itself for a later tick, and a
    /// marker already standing keeps the gate: that marker names work raised
    /// under an epoch the entry moved on to, and the job recording itself is
    /// putting back work the entry has left.
    #[test]
    fn a_job_that_lost_its_coverage_leaves_a_newer_marker_standing() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);

        let entry = host.shared.entries.get(&name).unwrap();
        let newer = {
            let mut state = entry.gate.lock().unwrap();
            let stale = Job::Reconcile(name.clone(), state.claim.epoch());
            // The work the entry moved on to while the job above was in the
            // channel.
            let newer = state
                .claim
                .schedule(|epoch| Job::Maintenance(name.clone(), epoch));
            // A claim is holding the coverage the stale job was dispatched
            // against, so the job records itself rather than ending here.
            state.pin();
            restore_lost_claim(&mut state, stale);
            state.unpin();
            assert_eq!(
                state.claim.marker().map(Job::epoch),
                Some(newer.epoch()),
                "a job the entry moved past took the marker of the work that replaced it"
            );
            newer
        };

        dispatch_pending(&host.shared, &entry).unwrap();
        wait_until(
            "the work the entry moved on to to run",
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
        assert_eq!(ops.reconciles.load(Ordering::SeqCst), 0);
        assert_eq!(host.state(&name), answered(TrustState::Ready));
        let _ = newer;
        drop(lease);
    }

    /// An invalidation stops the entry waiting on the job it supersedes. The
    /// queue slot that job holds names work the entry has moved on from, and an
    /// entry still waiting on it is one every later dispatch refuses: the work
    /// that replaced it never reaches a worker.
    #[test]
    fn an_invalidation_stops_the_entry_waiting_on_the_job_it_supersedes() {
        let ops = Arc::new(FakeOps::default());
        let first = VaultName::new("a").unwrap();
        let second = VaultName::new("b").unwrap();
        let working = VaultName::new("c").unwrap();
        let host = host_without_ambient_polling(Arc::clone(&ops), &[&first, &second, &working], 2);
        for name in [&first, &second, &working] {
            drop(host.demand(name, AttachMode::Durable).unwrap());
            wait_for_state(&host, name, TrustState::Ready);
        }

        // Both workers, held inside the other vaults' jobs: a job sent against
        // the entry under test waits in the queue, where the case reads the
        // entry around it. A tick claims neither of them again while they run,
        // so the batches below land on the entry the case names.
        ops.block_reconcile.store(true, Ordering::SeqCst);
        report_through_a_driven_poll(&ops, &host, &first, &ops.off_thread_rescan_poll_batches);
        wait_for_flag("reconcile_started", &ops.reconcile_started);
        ops.reconcile_started.store(false, Ordering::SeqCst);
        report_through_a_driven_poll(&ops, &host, &second, &ops.off_thread_rescan_poll_batches);
        wait_for_flag("reconcile_started", &ops.reconcile_started);

        // The teardown the entry owes once it has gone idle, sent and waiting
        // in the queue: the slot names it, and the entry is waiting on it. Only
        // this entry is marked due, so a teardown anywhere below is this one.
        let entry = host.shared.entries.get(&working).unwrap();
        {
            let mut state = entry.gate.lock().unwrap();
            state.detach_due = true;
            assert!(
                schedule_due_detach(&mut state, &working).is_some(),
                "the entry scheduled no teardown for this case to supersede"
            );
        }
        dispatch_pending(&host.shared, &entry).unwrap();
        assert!(
            entry.gate.lock().unwrap().claim.slot().is_some(),
            "the teardown this case supersedes never reached the queue"
        );

        // A demand takes the teardown back, superseding the job already in the
        // queue: the entry stops waiting on it here.
        let lease = host.demand(&working, AttachMode::Durable).unwrap();
        assert!(
            entry.gate.lock().unwrap().claim.slot().is_none(),
            "the entry is still waiting on the job the demand superseded"
        );

        // The work the entry owes now, which reaches a worker only through the
        // slot the invalidation gave back.
        report_through_a_driven_poll(&ops, &host, &working, &ops.off_thread_rescan_poll_batches);
        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_until(
            "the work the entry owes after the invalidation to run",
            lifecycle_wait_budget(),
            || {
                let reconciles = ops.reconciles.load(Ordering::SeqCst);
                if reconciles == 3 {
                    Observed::Met(())
                } else {
                    Observed::pending(format!("{reconciles} reconciles so far"))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        wait_for_state(&host, &working, TrustState::Ready);
        assert_eq!(
            ops.detaches.load(Ordering::SeqCst),
            0,
            "the teardown the demand superseded ran anyway"
        );
        drop(lease);
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
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        // The reconcile the poll schedules is blocked, so the state read below
        // is the one the poll published rather than the one that cleared it.
        ops.block_reconcile.store(true, Ordering::SeqCst);
        report_through_a_driven_poll(&ops, &host, &name, &ops.off_thread_rescan_poll_batches);
        assert_eq!(
            host.state(&name),
            answered(TrustState::untrusted(UntrustedReason::WatcherOverflow)),
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
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);

        // The reconcile the poll schedules is blocked, so the state read below
        // is the one the poll published rather than the one that cleared it.
        ops.block_reconcile.store(true, Ordering::SeqCst);
        report_through_a_driven_poll(&ops, &host, &name, &ops.off_thread_poll_batches);
        assert_eq!(
            host.state(&name),
            answered(TrustState::warming(WarmingPhase::Healing, 0, None)),
            "the entry went on serving against facts nothing had reconciled"
        );

        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(ops.reconciles.load(Ordering::SeqCst), 1);
    }

    /// The work a scheduled job names, for cases whose subject is which of the
    /// three a demand site chose.
    fn scheduled_work(job: &Job) -> Option<DemandedWork> {
        match job {
            Job::Attach(..) => Some(DemandedWork::Attach),
            Job::Recover(..) => Some(DemandedWork::Recover),
            Job::Rebuild(..) => Some(DemandedWork::Rebuild),
            Job::Reconcile(..) => Some(DemandedWork::Reconcile),
            Job::Maintenance(..) | Job::Reload(..) | Job::ReloadReconcile(..) | Job::Detach(..) => {
                None
            }
        }
    }

    /// The work an entry has scheduled against it and not yet begun.
    fn marked_work<O: EntryOps>(host: &Host<O>, name: &VaultName) -> Option<DemandedWork> {
        let entry = host
            .shared
            .entries
            .get(name)
            .expect("the vault is registered");
        let state = entry.gate.lock().expect("entry gate poisoned");
        state.claim.marker().and_then(scheduled_work)
    }

    /// Wait for the entry to hold a detach it has not yet handed to a worker.
    fn wait_for_a_scheduled_detach<O: EntryOps>(host: &Host<O>, name: &VaultName) {
        let entry = host
            .shared
            .entries
            .get(name)
            .expect("the vault is registered");
        wait_until(
            "a detach scheduled against the idle entry",
            lifecycle_wait_budget(),
            || {
                let state = entry.gate.lock().expect("entry gate poisoned");
                if state.detach_scheduled && matches!(state.claim.marker(), Some(Job::Detach(..))) {
                    Observed::Met(())
                } else {
                    Observed::pending(format!(
                        "the entry holds {:?}",
                        state.claim.marker().and_then(scheduled_work)
                    ))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
    }

    /// A demand that finds an entry holding its coverage, with an overflow
    /// standing and no recovery owed, schedules the reconcile those facts are
    /// owed.
    ///
    /// The coverage, the maintainer lock and the counted progress all stand
    /// through it: nothing is torn down and nothing re-installed, and what the
    /// lease is answered with is the overflow itself rather than a coverage
    /// installation over coverage the entry never lost. The rescan the entry is
    /// holding is drained by the reconcile that owns it.
    ///
    /// A queued detach is what opens the window: a reconcile ending against an
    /// idle entry schedules the detach, the demand takes that scheduling back,
    /// and the entry it then reads is attached, unclaimed and untrusted with no
    /// recovery owed.
    #[test]
    fn a_demand_over_a_cancelled_detach_reconciles_the_facts_the_entry_holds() {
        let ops = Arc::new(FakeOps::default());
        let working = VaultName::new("working").unwrap();
        let occupied = VaultName::new("occupied").unwrap();
        let host = host_without_ambient_polling(Arc::clone(&ops), &[&working, &occupied], 1);
        drop(host.demand(&working, AttachMode::Durable).unwrap());
        wait_for_state(&host, &working, TrustState::Ready);

        // The first rescan, held open in the reconcile it schedules: the entry
        // is claimed for the length of the reap below.
        ops.block_reconcile_at.store(1, Ordering::SeqCst);
        report_through_a_driven_poll(&ops, &host, &working, &ops.off_thread_rescan_poll_batches);
        wait_for_flag("reconcile_started", &ops.reconcile_started);
        // A second rescan, for that reconcile's own handoff drain: it is what
        // stands in the entry once the claim ends, and what the demand below
        // finds the entry holding.
        ops.handoff_rescan_poll_batches.store(1, Ordering::SeqCst);
        // The entry falls idle while its reconcile runs, so the detach is
        // scheduled by the leg that ends rather than by a reap over a free
        // entry.
        host.reap_idle(Instant::now() + Duration::from_secs(61))
            .unwrap();

        // The one queue slot, taken for a vault that is not the one under
        // test: the detach the reconcile schedules is refused the channel and
        // stays a marker on the entry, and the worker that then takes this
        // attach is held in it while the demand below runs.
        ops.block_attach.store(true, Ordering::SeqCst);
        ops.attach_started.store(false, Ordering::SeqCst);
        let occupier = host.demand(&occupied, AttachMode::Durable).unwrap();
        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for_flag("attach_started", &ops.attach_started);
        wait_for_a_scheduled_detach(&host, &working);
        assert_eq!(
            host.state(&working),
            answered(TrustState::untrusted(UntrustedReason::WatcherOverflow)),
            "the entry the demand below reads is not holding an overflow"
        );

        let lease = host.demand(&working, AttachMode::Durable).unwrap();
        assert_eq!(
            lease.outcome(),
            &Demand::State(TrustState::untrusted(UntrustedReason::WatcherOverflow)),
            "the demand published a coverage installation over coverage the entry still holds"
        );
        assert_eq!(
            marked_work(&host, &working),
            Some(DemandedWork::Reconcile),
            "the demand scheduled something other than the reconcile the facts are owed"
        );

        ops.attach_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &working, TrustState::Ready);
        assert_eq!(
            ops.recovers.load(Ordering::SeqCst),
            0,
            "the demand recovered coverage the entry had never lost"
        );
        assert_eq!(
            ops.detaches.load(Ordering::SeqCst),
            0,
            "the demand gave back coverage the entry was serving from"
        );
        assert_eq!(
            ops.reconciles.load(Ordering::SeqCst),
            2,
            "the rescan standing in the entry reached no reconcile of its own"
        );
        drop(lease);
        drop(occupier);
    }

    /// Put an entry in the shape a demand site reads, and take its queue slot
    /// so the job a site schedules stays a marker: nothing dispatches it, and
    /// no worker begins the leg that would clear it.
    ///
    /// The untrusted state is the one both sites schedule against, and it is
    /// written here whatever the coverage, so the two inputs the choice reads
    /// are the only two a case varies.
    fn entry_awaiting_demand(
        ops: &Arc<FakeOps>,
        coverage: bool,
        recovery: bool,
    ) -> (Host<Arc<FakeOps>>, VaultName) {
        let (host, name) = fixture_without_ambient_polling(Arc::clone(ops));
        if coverage {
            drop(host.demand(&name, AttachMode::Durable).unwrap());
            wait_for_state(&host, &name, TrustState::Ready);
        }
        {
            let entry = host.shared.entries.get(&name).unwrap();
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            assert_eq!(
                state.coverage.in_hand(),
                coverage,
                "the entry does not hold the coverage this shape is about"
            );
            state.trust = TrustState::untrusted(UntrustedReason::WatcherOverflow);
            state.pending = Batch::rescan(RescanScope::Vault);
            if recovery {
                state.require_recovery();
            }
            let epoch = state.claim.epoch();
            state.claim.take_slot(epoch);
        }
        (host, name)
    }

    /// What a demand site left on an entry: the work it scheduled, and the
    /// state the entry publishes while that work stands. The two travel
    /// together because the site chooses them together.
    #[derive(Debug, Eq, PartialEq)]
    struct Scheduled {
        work: Option<DemandedWork>,
        published: Result<TrustState, ErrorEnvelope>,
    }

    /// What a client demand leaves on an entry in this shape.
    fn scheduled_by_a_client_demand(coverage: bool, recovery: bool) -> Scheduled {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = entry_awaiting_demand(&ops, coverage, recovery);
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        let scheduled = Scheduled {
            work: marked_work(&host, &name),
            published: host.state(&name),
        };
        drop(lease);
        scheduled
    }

    /// What [`schedule_demanded_work`] leaves on an entry in this shape for a
    /// lease already standing against it — the answer a claim's own end owes
    /// it. The call is direct: no claim ends and no poll runs here.
    fn scheduled_for_a_standing_lease(coverage: bool, recovery: bool) -> Scheduled {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = entry_awaiting_demand(&ops, coverage, recovery);
        let entry = host.shared.entries.get(&name).unwrap();
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        // The lease a claim's end answers: recorded while the claim held the
        // entry, and asking for whatever recovery the entry owes.
        state.demand_leases += 1;
        state.demand_recovery();
        schedule_demanded_work(&mut state, &name);
        Scheduled {
            work: state.claim.marker().and_then(scheduled_work),
            published: state.published_demand().answer(&name),
        }
    }

    /// One choice serves both demand sites, and it is a choice of two facts:
    /// the job scheduled and the state published while it stands. The job
    /// follows what the entry needs rather than which door the demand came
    /// through, so an entry admitted at one site and answered at the other is
    /// left in the same place — and a shape either site reads differently is
    /// the divergence itself.
    #[test]
    fn both_demand_sites_answer_an_entry_the_same_way() {
        let coverage_prologue = TrustState::warming(WarmingPhase::InstallingCoverage, 0, None);
        for (shape, coverage, recovery, work, published) in [
            (
                "no coverage in hand",
                false,
                false,
                DemandedWork::Attach,
                coverage_prologue.clone(),
            ),
            (
                "no coverage in hand and a recovery owed",
                false,
                true,
                DemandedWork::Attach,
                coverage_prologue.clone(),
            ),
            (
                "coverage in hand and a recovery owed over it",
                true,
                true,
                DemandedWork::Recover,
                coverage_prologue.clone(),
            ),
            (
                // The entry holds a rescan in every shape, so the state a
                // reconcile warms out of is the overflow those facts are.
                "coverage in hand and no recovery owed",
                true,
                false,
                DemandedWork::Reconcile,
                TrustState::untrusted(UntrustedReason::WatcherOverflow),
            ),
        ] {
            let expected = Scheduled {
                work: Some(work),
                published: answered(published),
            };
            let by_demand = scheduled_by_a_client_demand(coverage, recovery);
            let for_a_standing_lease = scheduled_for_a_standing_lease(coverage, recovery);
            assert_eq!(
                by_demand, expected,
                "a client demand over an entry with {shape} left it {by_demand:?}"
            );
            assert_eq!(
                for_a_standing_lease, by_demand,
                "the two demand sites answered an entry with {shape} differently"
            );
        }
    }

    /// **A vault that never stops reporting facts still runs its maintenance.**
    /// The clock maintenance is due on is the wall clock, and the poll that
    /// reads it carries a watcher's facts as readily as it carries none: an
    /// entry under sustained traffic answers every poll with a batch, so a
    /// maintenance the poll only schedules where it found nothing is a
    /// maintenance a busy vault never runs.
    ///
    /// The facts the poll carried are not spent by the maintenance that took
    /// the claim ahead of them: they stay in the entry's pending set, and the
    /// maintenance leg hands the reconcile on.
    #[test]
    fn a_poll_carrying_facts_runs_the_maintenance_it_found_due() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        let reconciles = ops.reconciles.load(Ordering::SeqCst);

        // One poll that answers with facts and finds maintenance due — the
        // busy vault's poll, driven rather than waited for.
        ops.off_thread_rescan_poll_batches
            .store(1, Ordering::SeqCst);
        ops.maintenance_due.store(true, Ordering::SeqCst);
        poll_watchers(&host.shared);

        wait_for_maintenance(&ops);
        wait_for_state(&host, &name, TrustState::Ready);
        assert!(
            ops.reconciles.load(Ordering::SeqCst) > reconciles,
            "the facts the poll carried were dropped by the maintenance beside them"
        );
        drop(lease);
    }

    /// A maintenance that took the claim a poll's facts arrived under is the
    /// leg that ends the label those facts warmed the entry to: nothing is
    /// pending behind it, so no reconcile follows to publish `Ready`.
    #[test]
    fn maintenance_that_derived_nothing_publishes_ready() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);

        ops.off_thread_poll_batches.store(1, Ordering::SeqCst);
        ops.maintenance_due.store(true, Ordering::SeqCst);
        poll_watchers(&host.shared);

        wait_for_maintenance(&ops);
        wait_for_state(&host, &name, TrustState::Ready);
        drop(lease);
    }

    /// **A reconcile that keeps finding facts gives the claim to due
    /// maintenance rather than taking another turn.** The reconcile leg loops
    /// while its own handoff drain keeps observing facts, and it holds the
    /// entry's claim across every turn — the watcher poll that reads the
    /// maintenance clocks skips a held entry, so an entry whose facts never
    /// stop arriving is one no poll ever looks at. The leg reads those clocks
    /// itself, so the maintenance an entry owes waits for one reconcile turn
    /// rather than for a quiet moment a busy vault never has.
    ///
    /// The clocks come due while the leg is running, which is the case the poll
    /// cannot cover: the poll that scheduled this reconcile found nothing due.
    #[test]
    fn a_reconcile_that_keeps_finding_facts_yields_to_due_maintenance() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
        wait_for_state(&host, &name, TrustState::Ready);
        let reconciles = ops.reconciles.load(Ordering::SeqCst);

        // The reconcile leg's own drain observes a fact short of saturating,
        // which is the shape that sends the leg around the loop again holding
        // the claim. Its own batch source, so the driven poll below cannot
        // spend the batch the drain depends on.
        ops.handoff_rescan_poll_batches.store(1, Ordering::SeqCst);
        ops.block_reconcile_at
            .store(reconciles + 1, Ordering::SeqCst);
        report_through_a_driven_poll(&ops, &host, &name, &ops.off_thread_poll_batches);

        wait_for_flag("reconcile_started", &ops.reconcile_started);
        ops.maintenance_due.store(true, Ordering::SeqCst);
        ops.reconcile_release.store(true, Ordering::SeqCst);

        wait_for_state(&host, &name, TrustState::Ready);
        assert_eq!(
            ops.maintenances.load(Ordering::SeqCst),
            1,
            "the reconcile turned again under a claim the maintenance clocks were due behind"
        );
        assert!(
            ops.reconciles.load(Ordering::SeqCst) > reconciles + 1,
            "the facts the yielding turn observed were dropped by the maintenance beside them"
        );
        drop(lease);
    }

    /// Wait for one maintenance leg to have run.
    fn wait_for_maintenance(ops: &Arc<FakeOps>) {
        wait_until(
            "the maintenance a poll found due to run",
            lifecycle_wait_budget(),
            || {
                let ran = ops.maintenances.load(Ordering::SeqCst);
                if ran > 0 {
                    Observed::Met(())
                } else {
                    Observed::pending("no maintenance has run")
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
    }

    #[test]
    fn maintenance_handoff_saturation_stays_warming_until_a_followup_drain() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
        assert!(matches!(host.state(&name), Ok(TrustState::Warming { .. })));

        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::Ready);
        drop(lease);
    }

    #[test]
    fn maintenance_handoff_saturation_preserves_untrusted_rescan_state() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let lease = host.demand(&name, AttachMode::Durable).unwrap();
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
            answered(TrustState::untrusted(UntrustedReason::WatcherOverflow))
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
        let registry = RegistryRead::from_entries([
            RegistryEntry::new(a.clone(), VaultRoot::new("/tmp/norn-host-maint-a").unwrap()),
            RegistryEntry::new(b.clone(), VaultRoot::new("/tmp/norn-host-maint-b").unwrap()),
        ]);
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
        let lease_a = host.demand(&a, AttachMode::Durable).unwrap();
        let lease_b = host.demand(&b, AttachMode::Durable).unwrap();
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

    /// A vault the set gains while the host runs, and a vault it loses.
    ///
    /// The seam under these cases is [`ServingSet::insert`] and
    /// [`ServingSet::remove`]. Nothing outside this crate reaches either: what
    /// Layer 3's product surface offers is a registration verb, and these two
    /// moves are what such a verb lands on.
    mod serving_set {
        use super::*;
        use crate::lifecycle::serving::ServingRefusal;

        fn registration(name: &VaultName, root: &std::path::Path) -> RegistryEntry {
            RegistryEntry::new(name.clone(), VaultRoot::new(root).unwrap())
        }

        /// A vault inserted into a running host attaches on the demand that
        /// follows, the way a vault read at startup does — and the attach is
        /// handed the registration the insertion carried.
        ///
        /// The root the ops receive is the whole of what the inserted entry
        /// contributes to its own attach: an attach handed the incumbent's
        /// registration would reach Ready just as this one does, over the wrong
        /// vault.
        #[test]
        fn an_inserted_vault_attaches_like_a_registered_one() {
            let ops = Arc::new(FakeOps::default());
            let (host, served) = fixture_without_ambient_polling(Arc::clone(&ops));
            let joined = VaultName::new("joined").unwrap();
            assert_eq!(
                host.demand(&joined, AttachMode::Durable).unwrap().outcome(),
                &Demand::UnknownVault,
                "a name the set does not serve was demandable before it joined"
            );

            let base = temp_base("serving-set-insert");
            let root = base.join("joined");
            std::fs::create_dir_all(&root).unwrap();
            host.shared
                .entries
                .insert(registration(&joined, &root))
                .expect("the set serves no such name");

            drop(host.demand(&joined, AttachMode::Durable).unwrap());
            wait_for_state(&host, &joined, TrustState::Ready);
            assert_eq!(
                host.state(&served),
                answered(TrustState::Unattached),
                "the insertion disturbed the entry that was already served"
            );
            let attached = ops.attach_roots.lock().unwrap();
            assert_eq!(
                attached.get(&joined),
                Some(&VaultRoot::new(&root).unwrap()),
                "the attach was handed a registration other than the inserted vault's"
            );
            assert!(
                !attached.contains_key(&served),
                "the attach was handed the incumbent's registration"
            );
            drop(attached);

            drop(host);
            let _ = std::fs::remove_dir_all(base);
        }

        /// An inserted root another served name already reaches is classified
        /// as part of the join, and both names are refused for it.
        ///
        /// The incumbent is the reason the classification runs here rather than
        /// at the newcomer's first demand: it is serving that root at this
        /// instant and has no other occasion to read it again, so an alias
        /// admitted unclassified would leave one root served under two names.
        #[test]
        fn an_inserted_alias_of_a_served_root_is_refused_as_a_duplicate() {
            let ops = Arc::new(FakeOps::default());
            let base = temp_base("serving-set-alias");
            let root = base.join("root");
            std::fs::create_dir_all(&root).unwrap();
            let served = VaultName::new("served").unwrap();
            let alias = VaultName::new("alias").unwrap();
            let host = host_over_roots(Arc::clone(&ops), &[(&served, root.as_path())], 2);
            let incumbent = host.demand(&served, AttachMode::Durable).unwrap();
            wait_for_state(&host, &served, TrustState::Ready);

            serve(&host.shared, registration(&alias, &root.join(".")))
                .expect("the set serves no such name");

            let conflict = AliasConflict::new([alias.clone(), served.clone()]);
            assert_eq!(
                entry_park(&host, &alias),
                Some(Demand::DuplicateRoot(conflict.clone())),
                "the alias was admitted against a root the host already serves"
            );
            assert_eq!(
                incumbent.completion(),
                Demand::DuplicateRoot(conflict),
                "the incumbent went on serving a root a second name now reaches"
            );
            wait_for_detaches(&ops, 1, "the incumbent to give its coverage back");

            drop((incumbent, host));
            let _ = std::fs::remove_dir_all(base);
        }

        /// A name the set already serves keeps the entry standing under it: the
        /// insertion is refused rather than replacing that entry.
        #[test]
        fn a_name_the_set_serves_is_not_inserted_over() {
            let ops = Arc::new(FakeOps::default());
            let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
            let entry = host.shared.entries.get(&name).expect("the vault is served");

            assert_eq!(
                host.shared.entries.insert(registration(
                    &name,
                    std::path::Path::new("/tmp/norn-host-elsewhere")
                )),
                Err(ServingRefusal::AlreadyServed)
            );
            let standing = host.shared.entries.get(&name).expect("the vault is served");
            assert!(
                Arc::ptr_eq(&entry, &standing),
                "the refused insertion replaced the entry standing under the name"
            );
            assert_eq!(
                standing.registration.root, entry.registration.root,
                "the refused insertion moved the served root"
            );
        }

        /// A vault removed while it holds nothing is unknown to the demand that
        /// follows it.
        #[test]
        fn a_removed_vault_is_unknown_to_the_next_demand() {
            let ops = Arc::new(FakeOps::default());
            let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));

            host.shared
                .entries
                .remove(&name)
                .expect("the entry holds nothing");

            assert_eq!(
                host.demand(&name, AttachMode::Durable).unwrap().outcome(),
                &Demand::UnknownVault
            );
            assert_eq!(
                host.state(&name)
                    .expect_err("a removed vault refuses")
                    .detail(),
                &ErrorDetail::unknown_vault(name.clone()),
                "the status surface answered for a name the host serves nothing under"
            );
            settle();
            assert_eq!(
                ops.attaches.load(Ordering::SeqCst),
                0,
                "the demand after the removal attached the vault it removed"
            );
        }

        /// A job dispatched against a name the set then stops serving does
        /// nothing when it arrives, and the worker that answered it goes on to
        /// the next vault's work.
        ///
        /// The job in the channel carries a name and an epoch, so the worker
        /// resolves it through the set again and finds nothing there. That
        /// second read is what admits a removal while a job stands in the
        /// channel, and
        /// `an_arrival_at_a_current_epoch_for_an_unserved_name_reaches_no_entry`
        /// is what isolates it: the epoch guard answers this arrival first, so
        /// this case stays green with the by-name re-reads replaced.
        ///
        /// The refusal is what supersedes the queued job and gives the entry's
        /// gate back, which is the window the removal is answered in — an
        /// entry with a job scheduled against it holds its own gate and is
        /// refused removal.
        #[test]
        fn a_job_arriving_for_a_name_the_set_stopped_serving_does_nothing() {
            let ops = Arc::new(FakeOps::default());
            let leaving = VaultName::new("leaving").unwrap();
            let occupied = VaultName::new("occupied").unwrap();
            let after = VaultName::new("after").unwrap();
            let host =
                host_without_ambient_polling(Arc::clone(&ops), &[&leaving, &occupied, &after], 1);

            // The one worker, held inside an attach: the job the demand below
            // dispatches waits in the channel for the whole of the sequence.
            ops.block_attach.store(true, Ordering::SeqCst);
            let occupier = host.demand(&occupied, AttachMode::Durable).unwrap();
            wait_for_flag("attach_started", &ops.attach_started);

            drop(host.demand(&leaving, AttachMode::Durable).unwrap());
            refuse_identity_error(&host.shared, &leaving, "root unreadable".to_string());
            host.shared
                .entries
                .remove(&leaving)
                .expect("the refused entry holds nothing");

            ops.block_attach.store(false, Ordering::SeqCst);
            ops.attach_release.store(true, Ordering::SeqCst);
            wait_for_state(&host, &occupied, TrustState::Ready);

            // The channel holds one job at a time here, so the vault demanded
            // now reaches it only once the arrival for the removed name has
            // been taken off. Reaching Ready is therefore both the arrival
            // having happened and the worker being free to run the job after
            // it. The retry is the dispatcher's duty, driven here because this
            // fixture's tick is a minute away.
            drop(host.demand(&after, AttachMode::Durable).unwrap());
            wait_until(
                "the vault demanded after the arrival to attach",
                lifecycle_wait_budget(),
                || {
                    retry_pending_dispatches(&host.shared);
                    match host.state(&after) {
                        Ok(TrustState::Ready) => Observed::Met(()),
                        state => Observed::pending(format!("the state is {state:?}")),
                    }
                },
            )
            .unwrap_or_else(|failure| panic!("{failure}"));

            assert!(
                !ops.attach_roots.lock().unwrap().contains_key(&leaving),
                "the job that arrived for the removed name attached it"
            );
            assert_eq!(
                ops.attaches.load(Ordering::SeqCst),
                2,
                "the attaches are the two vaults the set still serves and no other"
            );
            assert_eq!(
                host.state(&leaving)
                    .expect_err("a removed vault refuses")
                    .detail(),
                &ErrorDetail::unknown_vault(leaving.clone()),
                "the arrival put back the entry the removal took out"
            );
            drop(occupier);
        }

        /// The arrival above with the epoch guard behind it taken away: the job
        /// stands at the epoch its entry stood at when the set stopped serving
        /// the name, so the name is the whole of what answers it.
        ///
        /// The lifecycle reaches no such arrival on its own — a job in the
        /// channel holds its entry's gate through its marker, and the gate goes
        /// back only where something supersedes the job first, so every removal
        /// a dispatch races is admitted over work already left behind. The
        /// arrival is driven directly for that reason: what it isolates is the
        /// resolution by name, and a dispatch that carried its entry instead
        /// would attach a vault the set no longer serves at an epoch that is
        /// still current.
        #[test]
        fn an_arrival_at_a_current_epoch_for_an_unserved_name_reaches_no_entry() {
            let ops = Arc::new(FakeOps::default());
            let leaving = VaultName::new("leaving").unwrap();
            let bystander = VaultName::new("bystander").unwrap();
            let host = host_without_ambient_polling(Arc::clone(&ops), &[&leaving, &bystander], 1);
            let epoch = host
                .shared
                .entries
                .get(&leaving)
                .expect("the vault is served")
                .gate
                .lock()
                .unwrap()
                .claim
                .epoch();

            host.shared
                .entries
                .remove(&leaving)
                .expect("the entry holds nothing");
            run_job(&host.shared, Job::Attach(leaving.clone(), epoch));

            assert_eq!(
                ops.attaches.load(Ordering::SeqCst),
                0,
                "the arrival for an unserved name reached an entry and attached it"
            );
            assert_eq!(
                host.state(&bystander),
                answered(TrustState::Unattached),
                "the arrival for an unserved name ran against the entry beside it"
            );
        }

        /// A name the set never served is already not served: removing it
        /// answers the way removing a served name that holds nothing does, and
        /// takes nothing out from under the names the set does serve.
        #[test]
        fn a_name_the_set_never_served_is_removed_by_answering() {
            let ops = Arc::new(FakeOps::default());
            let (host, served) = fixture_without_ambient_polling(ops);
            let absent = VaultName::new("absent").unwrap();

            assert_eq!(
                host.shared.entries.remove(&absent),
                Ok(()),
                "a name the set never served was refused removal"
            );
            assert!(
                host.shared.entries.get(&served).is_some(),
                "removing a name the set never served took out the one it does"
            );
        }

        /// An entry holding its coverage stays in the set. Removal refuses
        /// rather than tearing that coverage down under the set's own lock, and
        /// the vault goes on being served.
        ///
        /// The coverage is the only hold standing here: the lease is withdrawn
        /// before the removal, and the fixture's dispatcher never ticks, so no
        /// poll is pinning the entry either.
        #[test]
        fn an_attached_vault_is_refused_removal_and_goes_on_being_served() {
            let ops = Arc::new(FakeOps::default());
            let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
            drop(host.demand(&name, AttachMode::Durable).unwrap());
            wait_for_state(&host, &name, TrustState::Ready);

            assert_eq!(
                host.shared.entries.remove(&name),
                Err(ServingRefusal::Held),
                "an entry holding its coverage left the set"
            );
            assert_eq!(host.state(&name), answered(TrustState::Ready));
            assert_eq!(
                host.demand(&name, AttachMode::Durable).unwrap().outcome(),
                &Demand::State(TrustState::Ready)
            );
        }

        /// An entry holding its scheduling gate for work it has scheduled
        /// stays in the set: the job the marker stands for is work the entry
        /// owes, and a removal under it takes the entry out from under a
        /// dispatch that is still to come.
        ///
        /// The marker is planted directly, so the gate is the only hold: no leg
        /// is registered, no coverage is held and no lease is recorded.
        #[test]
        fn an_entry_holding_its_gate_stays_in_the_set() {
            let ops = Arc::new(FakeOps::default());
            let (host, name) = fixture_without_ambient_polling(ops);
            let entry = host.shared.entries.get(&name).expect("the vault is served");
            {
                let mut state = entry.gate.lock().unwrap();
                state
                    .claim
                    .schedule(|epoch| Job::Attach(name.clone(), epoch));
            }

            assert_eq!(
                host.shared.entries.remove(&name),
                Err(ServingRefusal::Held),
                "an entry holding its gate for a job it has scheduled left the set"
            );

            entry.gate.lock().unwrap().claim.open();
            assert_eq!(
                host.shared.entries.remove(&name),
                Ok(()),
                "the entry the withdrawn job was holding stayed in the set"
            );
        }

        /// An entry whose coverage is out with a leg stays in the set. The
        /// entry holds nothing of its own, and what the leg holds comes back
        /// where the leg ends — to an entry the set must still be serving.
        #[test]
        fn an_entry_whose_coverage_is_out_with_a_leg_stays_in_the_set() {
            let ops = Arc::new(FakeOps::default());
            let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
            drop(host.demand(&name, AttachMode::Durable).unwrap());
            wait_for_state(&host, &name, TrustState::Ready);
            let entry = host.shared.entries.get(&name).expect("the vault is served");
            // The coverage taken out of the entry's hand and no registration
            // beside it: the one shape that leaves out-with-a-leg as the whole
            // of what holds the entry.
            let (coverage, epoch) = {
                let mut state = entry.gate.lock().unwrap();
                let epoch = state.claim.epoch();
                let coverage = state
                    .coverage
                    .take(epoch)
                    .expect("the attached entry holds its coverage");
                (coverage, epoch)
            };

            assert_eq!(
                host.shared.entries.remove(&name),
                Err(ServingRefusal::Held),
                "an entry whose coverage is out with a leg left the set"
            );

            // The leg ends the coverage it took: the ops have it back, and the
            // entry accounts for none.
            host.shared.ops.detach(&name, coverage);
            entry.gate.lock().unwrap().coverage.released_by(epoch);
            assert_eq!(
                host.shared.entries.remove(&name),
                Ok(()),
                "the entry the ended leg was holding stayed in the set"
            );
        }

        /// A leg registered against an entry holds it in the set with the gate
        /// already back: the leg outlives the gate it took, and it is the leg
        /// rather than the gate that says work is still standing against the
        /// entry.
        #[test]
        fn an_entry_a_registered_leg_stands_against_stays_in_the_set() {
            let ops = Arc::new(FakeOps::default());
            let (host, name) = fixture_without_ambient_polling(ops);
            let entry = host.shared.entries.get(&name).expect("the vault is served");
            let epoch = {
                let mut state = entry.gate.lock().unwrap();
                let epoch = state.claim.epoch();
                state.claim.begin_job_leg(epoch);
                state.claim.release();
                epoch
            };

            assert_eq!(
                host.shared.entries.remove(&name),
                Err(ServingRefusal::Held),
                "an entry a registered leg stands against left the set"
            );

            entry.gate.lock().unwrap().claim.end_job_leg(epoch);
            assert_eq!(
                host.shared.entries.remove(&name),
                Ok(()),
                "the entry the ended leg was holding stayed in the set"
            );
        }

        /// A job waiting in an entry's queue slot holds it in the set with the
        /// gate already open: the job is entering the channel, and a removal
        /// under it takes the entry out from under a dispatch the worker is
        /// about to resolve by name.
        ///
        /// The slot is driven to being the sole hold the way the lifecycle
        /// reaches it. A job leg makes its next job under the entry's lock,
        /// that lock goes back before `dispatch_handoff` takes it again for the
        /// hand-off, and a refusal in the window between the two opens the
        /// gate — so the hand-off ends the leg and takes the slot with nothing
        /// else left standing.
        #[test]
        fn an_entry_holding_its_queue_slot_stays_in_the_set() {
            let ops = Arc::new(FakeOps::default());
            let (host, name) = fixture_without_ambient_polling(ops);
            let entry = host.shared.entries.get(&name).expect("the vault is served");
            let (leg, job) = {
                let mut state = entry.gate.lock().unwrap();
                let leg = state.claim.epoch();
                state.claim.begin_job_leg(leg);
                let job = state
                    .claim
                    .hand_on(|epoch| Job::Attach(name.clone(), epoch));
                (leg, job)
            };
            // The window the refusal reaches, and the hand-off that runs after
            // it with the gate already back.
            entry.gate.lock().unwrap().claim.open();
            entry.gate.lock().unwrap().claim.hand_off(leg, &job);
            {
                let state = entry.gate.lock().unwrap();
                assert_eq!(
                    state.claim.slot(),
                    Some(job.epoch()),
                    "the hand-off left no job waiting in the entry's slot"
                );
                assert!(
                    !state.claim.is_held() && state.claim.leg().is_none(),
                    "a gate or a leg stands beside the slot, so the slot is not the sole hold"
                );
            }

            assert_eq!(
                host.shared.entries.remove(&name),
                Err(ServingRefusal::Held),
                "an entry with a job waiting in its queue slot left the set"
            );

            entry.gate.lock().unwrap().claim.free_slot(job.epoch());
            assert_eq!(
                host.shared.entries.remove(&name),
                Ok(()),
                "the entry the freed slot was holding stayed in the set"
            );
        }

        /// A lease recorded against an entry holds it in the set even where the
        /// entry holds nothing else. The lease is a caller waiting on the
        /// vault, and a removal under it would leave that caller reading an
        /// entry the host has stopped serving.
        ///
        /// The lease is made the sole hold by waiting for the contended leg to
        /// let go of the entry, not by the park alone. The park is published
        /// under the first of the two locks that leg takes and its registration
        /// ends under the second, so a removal run between them is refused for
        /// the leg — which would answer this case's first assertion without the
        /// lease and leave its second one racing the leg to the answer.
        #[test]
        fn a_lease_recorded_against_an_entry_holds_it_in_the_set() {
            let ops = Arc::new(FakeOps::default());
            ops.contend_attach.store(true, Ordering::SeqCst);
            let (host, name) = fixture_without_ambient_polling(Arc::clone(&ops));
            let lease = host.demand(&name, AttachMode::Durable).unwrap();
            wait_until(
                "the contended attach to park the entry",
                lifecycle_wait_budget(),
                || match lease.completion() {
                    Demand::MaintainerContended(_) => Observed::Met(()),
                    other => Observed::pending(format!("the lease reports {other:?}")),
                },
            )
            .unwrap_or_else(|failure| panic!("{failure}"));
            let entry = host.shared.entries.get(&name).expect("the vault is served");
            wait_until(
                "the contended leg to end its hold on the entry",
                lifecycle_wait_budget(),
                || {
                    let state = entry.gate.lock().expect("entry gate poisoned");
                    if state.claim.leg().is_none() && !state.claim.is_held() {
                        Observed::Met(())
                    } else {
                        Observed::pending(format!(
                            "a leg stands: {}, the gate is held: {}",
                            state.claim.leg().is_some(),
                            state.claim.is_held()
                        ))
                    }
                },
            )
            .unwrap_or_else(|failure| panic!("{failure}"));

            assert_eq!(
                host.shared.entries.remove(&name),
                Err(ServingRefusal::Held),
                "an entry a lease is recorded against left the set"
            );
            drop(lease);
            assert_eq!(
                host.shared.entries.remove(&name),
                Ok(()),
                "the entry the withdrawn lease was holding stayed in the set"
            );
            assert_eq!(
                host.demand(&name, AttachMode::Durable).unwrap().outcome(),
                &Demand::UnknownVault
            );
        }
    }
}
