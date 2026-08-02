use std::collections::BTreeMap;
use std::fmt;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use norn_config::VaultName;
use norn_fs::{Batch, RescanScope, WatchError};
use norn_wire::{MaintainerIdentity, TrustState, UntrustedReason};

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
    fn detach(&self, name: &VaultName, attachment: Self::Attachment);
}

/// Epoch-bound, monotonic publication of one lifecycle job's warming progress.
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
    pub fn report(&self, healed: u64, total_estimate: Option<u64>) {
        let Some(entry) = self.entry.upgrade() else {
            return;
        };
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        if state.epoch != self.epoch {
            return;
        }
        let TrustState::Warming {
            healed: prior,
            total_estimate: prior_total,
            ..
        } = &state.trust
        else {
            return;
        };
        let healed = healed.max(*prior);
        let total = match (*prior_total, total_estimate) {
            (Some(left), Some(right)) => Some(left.max(right).max(healed)),
            (Some(left), None) => Some(left.max(healed)),
            (None, Some(right)) => Some(right.max(healed)),
            (None, None) => None,
        };
        state.trust = TrustState::warming(healed, total);
    }
}

fn reporter<A>(entry: &Arc<Entry<A>>, epoch: u64) -> ProgressReporter<A> {
    ProgressReporter {
        entry: Arc::downgrade(entry),
        epoch,
    }
}

/// A lifecycle job's semantic failure class.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JobFailure {
    Environmental(String),
    WatcherTerminal(String),
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
    identity_refused: bool,
    runnable: bool,
    queued: bool,
    active_epoch: Option<u64>,
    pending_dispatch: Option<Job>,
    terminal_watcher: bool,
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

#[derive(Clone)]
enum Job {
    Attach(VaultName, u64),
    Recover(VaultName, u64),
    Reconcile(VaultName, u64),
    Detach(VaultName, u64),
}

impl Job {
    fn epoch(&self) -> u64 {
        match self {
            Self::Attach(_, epoch)
            | Self::Recover(_, epoch)
            | Self::Reconcile(_, epoch)
            | Self::Detach(_, epoch) => *epoch,
        }
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
            if state.pending_dispatch.as_ref().map(Job::epoch) == Some(job.epoch()) {
                state.queued = false;
            }
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
    let mut attachments = Vec::new();
    for ((name, _), state) in entries.iter().zip(&mut states) {
        state.epoch += 1;
        state.queued = false;
        state.pending_dispatch = None;
        state.pending = Batch::default();
        state.recovery_required = false;
        state.identity_refused = false;
        state.duplicate_root = Some(conflict.clone());
        state.trust = TrustState::Unattached;
        if state.active_epoch.is_none() && !state.detach_in_flight {
            state.runnable = false;
            if let Some(attachment) = state.attachment.take() {
                attachments.push(((*name).clone(), attachment));
            }
        }
    }
    drop(states);
    for (name, attachment) in attachments {
        shared.ops.detach(&name, attachment);
    }
}

fn refuse_identity_error<O: EntryOps>(shared: &Arc<Shared<O>>, name: &VaultName) {
    let Some(entry) = shared.entries.get(name) else {
        return;
    };
    let attachment = {
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        state.epoch += 1;
        state.queued = false;
        state.pending_dispatch = None;
        state.pending.merge(Batch::rescan(RescanScope::Vault));
        state.recovery_required = true;
        state.identity_refused = true;
        state.trust = TrustState::untrusted(UntrustedReason::environmental_refusal());
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
        Err(_) => {
            refuse_identity_error(shared, name);
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
    attach_gate: Mutex<()>,
}

pub struct Host<O: EntryOps> {
    shared: Arc<Shared<O>>,
    workers: Vec<thread::JoinHandle<()>>,
    dispatcher_stop: mpsc::Sender<()>,
    dispatcher: Option<thread::JoinHandle<()>>,
}

/// One client operation's lifecycle guard and immediate trust answer.
/// Dropping it ends the demand lease and may release an overdue idle detach.
pub struct DemandLease<O: EntryOps> {
    outcome: Demand,
    held: Option<(Arc<Shared<O>>, VaultName)>,
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
        let schedule = {
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            state.demand_leases = state.demand_leases.saturating_sub(1);
            if state.demand_leases == 0 {
                state.last_demand = Instant::now();
                state.detach_due = false;
            }
            schedule_due_detach(&mut state, &name)
        };
        if let Some(job) = schedule {
            let _ = job;
            let _ = dispatch_pending(&shared, entry);
        }
    }
}

impl<O: EntryOps> Drop for Host<O> {
    fn drop(&mut self) {
        self.shared.shutting_down.store(true, Ordering::SeqCst);
        let _ = self.dispatcher_stop.send(());
        let dispatcher = self.dispatcher.take();
        let mut attached = Vec::new();
        for (name, entry) in &self.shared.entries {
            let attachment = {
                let mut state = entry.gate.lock().expect("entry gate poisoned");
                state.epoch += 1;
                state.runnable = false;
                state.queued = false;
                state.pending_dispatch = None;
                state.pending = Batch::default();
                state.trust = TrustState::Unattached;
                if state.active_epoch.is_none() && !state.detach_in_flight {
                    state.attachment.take()
                } else {
                    None
                }
            };
            if let Some(attachment) = attachment {
                attached.push((name.clone(), attachment));
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
        for (name, attachment) in attached {
            self.shared.ops.detach(&name, attachment);
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
                            identity_refused: false,
                            runnable: false,
                            queued: false,
                            active_epoch: None,
                            pending_dispatch: None,
                            terminal_watcher: false,
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
            attach_gate: Mutex::new(()),
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
            });
        };
        if let Some(conflict) = recheck_and_refuse(&self.shared, name)? {
            return Ok(DemandLease {
                outcome: Demand::DuplicateRoot(conflict),
                held: None,
            });
        }
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        state.demand_leases += 1;
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
            });
        }
        let schedule = matches!(
            state.trust,
            TrustState::Unattached | TrustState::Untrusted { .. }
        ) && !state.runnable
            && !state.identity_refused;
        if schedule {
            state.runnable = true;
            state.trust = TrustState::warming(0, None);
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
            });
        }
        let answer = Demand::State(state.trust.clone());
        drop(state);
        Ok(DemandLease {
            outcome: answer,
            held: Some((Arc::clone(&self.shared), name.clone())),
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
            state.trust = TrustState::warming(0, None);
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
            state.terminal_watcher = true;
            state.recovery_required = true;
            state.pending.merge(Batch::rescan(RescanScope::Vault));
            state.epoch += 1;
            if state.active_epoch.is_none() {
                state.runnable = false;
                state.queued = false;
                state.pending_dispatch = None;
            }
            state.trust = TrustState::untrusted(UntrustedReason::WatcherOverflow);
            let _ = error;
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
        let mut schedule = None;
        let mut detach = None;
        {
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            state.safety_pins = state.safety_pins.saturating_sub(1);
            if state.epoch != epoch {
                detach = Some(attachment);
            } else {
                match result {
                    Ok(None) => {
                        state.active_epoch = None;
                        state.runnable = false;
                        state.attachment = Some(attachment);
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
                                TrustState::warming(0, None)
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
                        state.pending = Batch::default();
                        state.trust = TrustState::Unattached;
                        state.epoch += 1;
                        detach = Some(attachment);
                    }
                    Err(JobFailure::WatcherTerminal(_)) => {
                        state.active_epoch = None;
                        state.runnable = false;
                        state.terminal_watcher = true;
                        state.recovery_required = true;
                        state.pending.merge(Batch::rescan(RescanScope::Vault));
                        state.trust = TrustState::untrusted(UntrustedReason::WatcherOverflow);
                        state.attachment = Some(attachment);
                    }
                    Err(_) => {
                        state.active_epoch = None;
                        state.runnable = false;
                        state.recovery_required = true;
                        state.pending.merge(Batch::rescan(RescanScope::Vault));
                        state.trust =
                            TrustState::untrusted(UntrustedReason::environmental_refusal());
                        state.attachment = Some(attachment);
                    }
                }
                if schedule.is_none() {
                    schedule = schedule_due_detach(&mut state, name);
                }
            }
        }
        if let Some(attachment) = detach {
            shared.ops.detach(name, attachment);
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            if state.active_epoch == Some(epoch) {
                state.active_epoch = None;
                state.runnable = false;
            }
            if state.demand_leases > 0
                && state.attachment.is_none()
                && !state.runnable
                && state.duplicate_root.is_none()
                && !state.identity_refused
            {
                state.trust = TrustState::warming(0, None);
                state.epoch += 1;
                state.runnable = true;
                let job = Job::Attach(name.clone(), state.epoch);
                state.pending_dispatch = Some(job.clone());
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
        | Job::Detach(name, _) => name,
    };
    let Some(entry) = shared.entries.get(name) else {
        return;
    };
    match job {
        Job::Attach(name, epoch) => {
            // Root identity classification, effectful acquisition, and publication are
            // one host-global transaction. A second name cannot slip through a stale
            // classification while this attachment is being acquired.
            let _attach_guard = shared.attach_gate.lock().expect("attach gate poisoned");
            match shared.registry.recheck(&name) {
                Ok(Some(conflict)) => {
                    refuse_conflict(shared, &conflict);
                    let mut state = entry.gate.lock().expect("entry gate poisoned");
                    if state.epoch == epoch {
                        state.runnable = false;
                        state.duplicate_root = Some(conflict);
                        state.trust = TrustState::Unattached;
                    }
                    return;
                }
                Err(_) => {
                    let mut state = entry.gate.lock().expect("entry gate poisoned");
                    if state.epoch == epoch {
                        state.runnable = false;
                        state.trust =
                            TrustState::untrusted(UntrustedReason::environmental_refusal());
                    }
                    return;
                }
                Ok(None) => {}
            }
            let mut result =
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
            let post_conflict = match shared.registry.recheck(&name) {
                Ok(conflict) => conflict,
                Err(_) => {
                    if let Ok((attachment, _, _)) = result {
                        shared.ops.detach(&name, attachment);
                    }
                    result = Err(JobFailure::Environmental(
                        "root identity revalidation failed".into(),
                    ));
                    None
                }
            };
            if let Some(conflict) = post_conflict {
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
                Ok((attachment, observed, _)) => {
                    state.pending.merge(observed);
                    state.attachment = Some(attachment);
                    state.terminal_watcher = false;
                    state.recovery_required = false;
                    state.identity_refused = false;
                    state.maintainer_contended = None;
                    state.duplicate_root = None;
                    if state.pending.is_empty() {
                        state.trust = TrustState::Ready;
                    } else {
                        state.epoch += 1;
                        let epoch = state.epoch;
                        state.runnable = true;
                        drop(state);
                        dispatch_followup(shared, Job::Reconcile(name, epoch));
                    }
                }
                Err(JobFailure::MaintainerContended(incumbent)) => {
                    state.maintainer_contended = Some(incumbent);
                    state.trust = TrustState::Unattached;
                }
                Err(JobFailure::LostMaintainership) => {
                    state.attachment = None;
                    state.pending = Batch::default();
                    state.trust = TrustState::Unattached;
                }
                Err(JobFailure::WatcherTerminal(_)) => {
                    state.terminal_watcher = true;
                    state.trust = TrustState::untrusted(UntrustedReason::WatcherOverflow);
                }
                Err(_) => {
                    state.trust = TrustState::untrusted(UntrustedReason::environmental_refusal());
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
                    state.terminal_watcher = false;
                    state.recovery_required = false;
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
                    state.pending = Batch::default();
                    state.trust = TrustState::Unattached;
                    drop(state);
                    shared.ops.detach(&name, attachment);
                    return;
                }
                Err(JobFailure::WatcherTerminal(_)) => {
                    state.terminal_watcher = true;
                    state.recovery_required = true;
                    state.pending.merge(Batch::rescan(RescanScope::Vault));
                    state.attachment = Some(attachment);
                    state.trust = TrustState::untrusted(UntrustedReason::WatcherOverflow);
                    next = schedule_due_detach(&mut state, &name);
                }
                Err(_) => {
                    state.recovery_required = true;
                    state.pending.merge(Batch::rescan(RescanScope::Vault));
                    state.attachment = Some(attachment);
                    state.trust = TrustState::untrusted(UntrustedReason::environmental_refusal());
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
                if !matches!(state.trust, TrustState::Warming { .. }) {
                    state.trust = TrustState::warming(0, None);
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
                    if state.detach_due {
                        state.runnable = false;
                        let next = schedule_due_detach(&mut state, &name);
                        drop(state);
                        if let Some(job) = next {
                            dispatch_followup(shared, job);
                        }
                        break;
                    } else if state.pending.is_empty() {
                        state.runnable = false;
                        if !state.recovery_required {
                            state.trust = TrustState::Ready;
                        }
                        break;
                    } else if handoff_saturated {
                        state.runnable = false;
                        state.trust = TrustState::warming(0, None);
                        break;
                    }
                }
                Err(JobFailure::LostMaintainership) => {
                    state.pending = Batch::default();
                    state.runnable = false;
                    state.trust = TrustState::Unattached;
                    drop(state);
                    shared.ops.detach(&name, attachment);
                    break;
                }
                Err(JobFailure::WatcherTerminal(_)) => {
                    state.terminal_watcher = true;
                    state.attachment = Some(attachment);
                    state.runnable = false;
                    state.recovery_required = true;
                    state.pending.merge(Batch::rescan(RescanScope::Vault));
                    state.trust = TrustState::untrusted(UntrustedReason::WatcherOverflow);
                    let next = schedule_due_detach(&mut state, &name);
                    drop(state);
                    if let Some(job) = next {
                        dispatch_followup(shared, job);
                    }
                    break;
                }
                Err(_) => {
                    state.attachment = Some(attachment);
                    state.runnable = false;
                    state.recovery_required = true;
                    state.pending.merge(Batch::rescan(RescanScope::Vault));
                    state.trust = TrustState::untrusted(UntrustedReason::environmental_refusal());
                    let next = schedule_due_detach(&mut state, &name);
                    drop(state);
                    if let Some(job) = next {
                        dispatch_followup(shared, job);
                    }
                    break;
                }
            }
        },
        Job::Detach(name, epoch) => {
            let attachment = {
                let mut state = entry.gate.lock().expect("entry gate poisoned");
                if state.epoch != epoch {
                    return;
                }
                let attachment = state.attachment.take();
                state.detach_in_flight = attachment.is_some();
                // Teardown still owns live resources until EntryOps::detach
                // returns. Warming is the existing non-Ready state that keeps
                // demand nonblocking without claiming the entry is detached.
                state.trust = TrustState::warming(0, None);
                attachment
            };
            if let Some(attachment) = attachment {
                shared.ops.detach(&name, attachment);
            }
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            state.detach_in_flight = false;
            state.pending = Batch::default();
            state.recovery_required = false;
            state.runnable = false;
            state.detach_due = false;
            state.detach_scheduled = false;
            state.trust = TrustState::Unattached;
            if state.demand_leases > 0 && state.duplicate_root.is_none() && !state.identity_refused
            {
                state.runnable = true;
                state.trust = TrustState::warming(0, None);
                state.epoch += 1;
                let next = Job::Attach(name.clone(), state.epoch);
                drop(state);
                dispatch_followup(shared, next);
            }
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
/// is occupied, the worker carries its capacity-one follow-up itself before it
/// returns to the queued sibling.
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
        Err(mpsc::TrySendError::Full(job)) => run_job(shared, job),
        Err(mpsc::TrySendError::Disconnected(_)) => {}
    }
}

#[allow(dead_code)]
fn _schema_rescan_is_dominant(batch: &Batch) -> bool {
    batch.schema_dirty() || batch.rescans().contains(&RescanScope::Schema)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // fixtures impersonate external filesystem retargets.
mod tests {
    use super::*;
    use norn_config::registry::{Entry as RegistryEntry, VaultRoot};
    use std::sync::atomic::{AtomicUsize, Ordering};

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
        contend_attach: std::sync::atomic::AtomicBool,
        block_detach: std::sync::atomic::AtomicBool,
        detach_started: std::sync::atomic::AtomicBool,
        detach_release: std::sync::atomic::AtomicBool,
        block_reconcile: std::sync::atomic::AtomicBool,
        reconcile_started: std::sync::atomic::AtomicBool,
        reconcile_release: std::sync::atomic::AtomicBool,
        block_poll: std::sync::atomic::AtomicBool,
        poll_started: std::sync::atomic::AtomicBool,
        poll_release: std::sync::atomic::AtomicBool,
    }

    impl EntryOps for Arc<FakeOps> {
        type Attachment = ();

        fn attach(&self, _: &VaultName, _: &ProgressReporter<()>) -> Result<(), JobFailure> {
            self.attaches.fetch_add(1, Ordering::SeqCst);
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
            self.reconciles.fetch_add(1, Ordering::SeqCst);
            if self.block_reconcile.load(Ordering::SeqCst) {
                self.reconcile_started.store(true, Ordering::SeqCst);
                while !self.reconcile_release.load(Ordering::SeqCst) {
                    thread::yield_now();
                }
            }
            if self.terminal_reconcile.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::WatcherTerminal("lost".into()));
            }
            if self.environmental_reconcile.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::Environmental("refused".into()));
            }
            Ok(())
        }

        fn recover(
            &self,
            _: &VaultName,
            _: &mut (),
            _: &ProgressReporter<()>,
        ) -> Result<(), JobFailure> {
            self.recovers.fetch_add(1, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(20));
            if self.terminal_recover.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::WatcherTerminal("lost".into()));
            }
            if self.environmental_recover.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::Environmental("refused".into()));
            }
            Ok(())
        }

        fn poll(&self, _: &VaultName, _: &mut ()) -> Result<Option<Batch>, JobFailure> {
            if self.block_poll.load(Ordering::SeqCst) {
                self.poll_started.store(true, Ordering::SeqCst);
                while !self.poll_release.load(Ordering::SeqCst) {
                    thread::yield_now();
                }
            }
            Ok(None)
        }

        fn detach(&self, _: &VaultName, _: ()) {
            if self.block_detach.load(Ordering::SeqCst) {
                self.detach_started.store(true, Ordering::SeqCst);
                while !self.detach_release.load(Ordering::SeqCst) {
                    thread::yield_now();
                }
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

    fn wait_for(host: &Host<Arc<FakeOps>>, name: &VaultName, expected: TrustState) {
        for _ in 0..200 {
            if host.state(name) == Some(expected.clone()) {
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("state did not become {expected:?}");
    }

    #[test]
    fn concurrent_demand_is_single_flight() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        for _ in 0..20 {
            let _ = host.demand(&name).unwrap();
        }
        wait_for(&host, &name, TrustState::Ready);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn stored_contention_is_reported_without_scheduling_a_hidden_retry() {
        let ops = Arc::new(FakeOps::default());
        ops.contend_attach.store(true, Ordering::SeqCst);
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let initial = host.demand(&name).unwrap();
        for _ in 0..200 {
            if ops.attaches.load(Ordering::SeqCst) == 1
                && host.state(&name) == Some(TrustState::Unattached)
            {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(matches!(
            initial.completion(),
            Demand::MaintainerContended(_)
        ));
        let lease = host.demand(&name).unwrap();
        assert!(matches!(lease.completion(), Demand::MaintainerContended(_)));
        thread::sleep(Duration::from_millis(10));
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        drop(lease);
        drop(initial);
        drop(host.retry(&name).unwrap());
        wait_for(&host, &name, TrustState::Ready);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn demand_during_in_flight_detach_never_observes_ready_without_attachment() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::ZERO);
        let initial = host.demand(&name).unwrap();
        wait_for(&host, &name, TrustState::Ready);
        drop(initial);
        ops.block_detach.store(true, Ordering::SeqCst);
        host.reap_idle(Instant::now()).unwrap();
        for _ in 0..200 {
            if ops.detach_started.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(ops.detach_started.load(Ordering::SeqCst));
        assert!(matches!(
            host.state(&name),
            Some(TrustState::Warming { .. })
        ));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);
        let lease = host.demand(&name).unwrap();
        assert!(matches!(
            lease.completion(),
            Demand::State(TrustState::Warming { .. })
        ));
        ops.detach_release.store(true, Ordering::SeqCst);
        wait_for(&host, &name, TrustState::Ready);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);
        drop(lease);
    }

    #[test]
    fn idle_deadline_begins_when_the_final_demand_lease_ends() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_millis(20));
        let lease = host.demand(&name).unwrap();
        wait_for(&host, &name, TrustState::Ready);
        thread::sleep(Duration::from_millis(30));
        drop(lease);
        host.reap_idle(Instant::now()).unwrap();
        assert_eq!(host.state(&name), Some(TrustState::Ready));
        thread::sleep(Duration::from_millis(25));
        host.reap_idle(Instant::now()).unwrap();
        wait_for(&host, &name, TrustState::Unattached);
    }

    #[test]
    fn long_held_lease_gets_a_fresh_idle_interval_after_release() {
        let ops = Arc::new(FakeOps::default());
        let idle_after = Duration::from_millis(200);
        let (host, name) = fixture(Arc::clone(&ops), idle_after);
        let lease = host.demand(&name).unwrap();
        wait_for(&host, &name, TrustState::Ready);
        thread::sleep(idle_after + Duration::from_millis(50));
        assert_eq!(host.state(&name), Some(TrustState::Ready));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);

        let released = Instant::now();
        drop(lease);
        host.reap_idle(released + idle_after / 2).unwrap();
        assert_eq!(host.state(&name), Some(TrustState::Ready));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);
        for _ in 0..500 {
            if host.state(&name) == Some(TrustState::Unattached) {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(host.state(&name), Some(TrustState::Unattached));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn idle_reap_releases_the_attachment_and_returns_to_unattached() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::ZERO);
        let lease = host.demand(&name).unwrap();
        wait_for(&host, &name, TrustState::Ready);
        drop(lease);
        host.reap_idle(Instant::now()).unwrap();
        wait_for(&host, &name, TrustState::Unattached);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn dispatcher_reaps_idle_attachment_despite_watcher_churn() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_millis(20));
        let lease = host.demand(&name).unwrap();
        wait_for(&host, &name, TrustState::Ready);
        drop(lease);
        for _ in 0..3 {
            host.accept_batch(&name, Batch::rescan(RescanScope::Vault))
                .unwrap();
            thread::sleep(Duration::from_millis(4));
        }
        wait_for(&host, &name, TrustState::Unattached);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        assert!(ops.reconciles.load(Ordering::SeqCst) > 0);
    }

    #[test]
    fn demand_lease_cancels_queued_detach_until_client_work_ends() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::ZERO);
        let lease = host.demand(&name).unwrap();
        wait_for(&host, &name, TrustState::Ready);
        host.reap_idle(Instant::now()).unwrap();
        thread::sleep(Duration::from_millis(10));
        assert_eq!(host.state(&name), Some(TrustState::Ready));
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 0);
        drop(lease);
        wait_for(&host, &name, TrustState::Unattached);
    }

    #[test]
    fn terminal_watcher_failure_recovers_only_on_demand() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        let _ = host.demand(&name).unwrap();
        wait_for(&host, &name, TrustState::Ready);
        host.watcher_failed(&name, WatchError::Backend("gone".into()));
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 0);
        let _ = host.demand(&name).unwrap();
        wait_for(&host, &name, TrustState::Ready);
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 1);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn recover_drains_pending_invalidations_before_publishing_ready() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for(&host, &name, TrustState::Ready);
        host.watcher_failed(&name, WatchError::Backend("gone".into()));
        let lease = host.demand(&name).unwrap();
        host.accept_batch(&name, Batch::rescan(RescanScope::Vault))
            .unwrap();
        wait_for(&host, &name, TrustState::Ready);
        assert_eq!(ops.reconciles.load(Ordering::SeqCst), 1);
        drop(lease);
    }

    #[test]
    fn terminal_failure_during_recover_stays_watcher_untrusted() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for(&host, &name, TrustState::Ready);
        host.watcher_failed(&name, WatchError::Backend("gone".into()));
        ops.terminal_recover.store(true, Ordering::SeqCst);
        drop(host.demand(&name).unwrap());
        wait_for(
            &host,
            &name,
            TrustState::untrusted(UntrustedReason::WatcherOverflow),
        );
    }

    #[test]
    fn terminal_failure_during_reconcile_stays_watcher_untrusted() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for(&host, &name, TrustState::Ready);
        ops.terminal_reconcile.store(true, Ordering::SeqCst);
        host.accept_batch(&name, Batch::rescan(RescanScope::Vault))
            .unwrap();
        wait_for(
            &host,
            &name,
            TrustState::untrusted(UntrustedReason::WatcherOverflow),
        );
    }

    #[test]
    fn failed_reconcile_requires_demand_recovery_before_later_facts_can_be_ready() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for(&host, &name, TrustState::Ready);
        ops.environmental_reconcile.store(true, Ordering::SeqCst);
        host.accept_batch(&name, Batch::rescan(RescanScope::Vault))
            .unwrap();
        wait_for(
            &host,
            &name,
            TrustState::untrusted(UntrustedReason::environmental_refusal()),
        );
        let failed_count = ops.reconciles.load(Ordering::SeqCst);
        host.accept_batch(&name, Batch::default()).unwrap();
        thread::sleep(Duration::from_millis(20));
        assert_eq!(ops.reconciles.load(Ordering::SeqCst), failed_count);
        assert_eq!(
            host.state(&name),
            Some(TrustState::untrusted(
                UntrustedReason::environmental_refusal()
            ))
        );
        drop(host.demand(&name).unwrap());
        wait_for(&host, &name, TrustState::Ready);
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 1);
        assert!(ops.reconciles.load(Ordering::SeqCst) > failed_count);
    }

    #[test]
    fn failed_recover_cannot_be_bypassed_by_a_later_watcher_fact() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for(&host, &name, TrustState::Ready);
        host.watcher_failed(&name, WatchError::Backend("gone".into()));
        ops.environmental_recover.store(true, Ordering::SeqCst);
        drop(host.demand(&name).unwrap());
        wait_for(
            &host,
            &name,
            TrustState::untrusted(UntrustedReason::environmental_refusal()),
        );
        host.accept_batch(&name, Batch::default()).unwrap();
        thread::sleep(Duration::from_millis(20));
        assert_eq!(ops.reconciles.load(Ordering::SeqCst), 0);
        assert_eq!(
            host.state(&name),
            Some(TrustState::untrusted(
                UntrustedReason::environmental_refusal()
            ))
        );
        drop(host.demand(&name).unwrap());
        wait_for(&host, &name, TrustState::Ready);
        assert_eq!(ops.recovers.load(Ordering::SeqCst), 2);
        assert_eq!(ops.reconciles.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn watcher_failure_invalidates_an_in_flight_reconcile() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for(&host, &name, TrustState::Ready);
        ops.block_reconcile.store(true, Ordering::SeqCst);
        host.accept_batch(&name, Batch::rescan(RescanScope::Vault))
            .unwrap();
        for _ in 0..200 {
            if ops.reconcile_started.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(ops.reconcile_started.load(Ordering::SeqCst));
        host.watcher_failed(&name, WatchError::Backend("lost".into()));
        let raced_demand = host.demand(&name).unwrap();
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        ops.reconcile_release.store(true, Ordering::SeqCst);
        wait_for(
            &host,
            &name,
            TrustState::untrusted(UntrustedReason::WatcherOverflow),
        );
        thread::sleep(Duration::from_millis(10));
        assert_eq!(
            host.state(&name),
            Some(TrustState::untrusted(UntrustedReason::WatcherOverflow))
        );
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        drop(raced_demand);
        drop(host.demand(&name).unwrap());
        wait_for(&host, &name, TrustState::Ready);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn host_drop_waits_for_in_flight_work_and_its_attachment_teardown() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for(&host, &name, TrustState::Ready);
        ops.block_reconcile.store(true, Ordering::SeqCst);
        host.accept_batch(&name, Batch::rescan(RescanScope::Vault))
            .unwrap();
        while !ops.reconcile_started.load(Ordering::SeqCst) {
            thread::yield_now();
        }
        let returned = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let drop_returned = Arc::clone(&returned);
        let dropper = thread::spawn(move || {
            drop(host);
            drop_returned.store(true, Ordering::SeqCst);
        });
        thread::sleep(Duration::from_millis(20));
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
        wait_for(&host, &name, TrustState::Ready);
        drop(host);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn demand_during_invalidated_poll_waits_for_attachment_ownership() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::from_secs(60));
        drop(host.demand(&name).unwrap());
        wait_for(&host, &name, TrustState::Ready);
        ops.block_poll.store(true, Ordering::SeqCst);
        while !ops.poll_started.load(Ordering::SeqCst) {
            thread::yield_now();
        }
        host.watcher_failed(&name, WatchError::Backend("lost".into()));
        let demand = host.demand(&name).unwrap();
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        assert_eq!(
            demand.completion(),
            Demand::State(TrustState::untrusted(UntrustedReason::WatcherOverflow))
        );
        ops.poll_release.store(true, Ordering::SeqCst);
        wait_for(&host, &name, TrustState::Ready);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        drop(demand);
    }

    #[derive(Default)]
    struct PollingOps {
        emit: std::sync::atomic::AtomicBool,
        queued: AtomicUsize,
        reconciles: AtomicUsize,
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
            progress.report(1, Some(2));
            self.reconciles.fetch_add(1, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(30));
            progress.report(2, Some(2));
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
                while !self.release_a.load(Ordering::SeqCst) {
                    thread::yield_now();
                }
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
    fn worker_carries_followup_when_a_sibling_fills_its_only_queue_slot() {
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
        for _ in 0..200 {
            if ops.a_started.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
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
        while !ops.a_started.load(Ordering::SeqCst) {
            thread::yield_now();
        }
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
        for _ in 0..200 {
            if ops.a_started.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        let b_lease = host.demand(&b).unwrap();
        std::fs::remove_dir(&b_root).unwrap();
        symlink(&a_root, &b_root).unwrap();
        ops.release_a.store(true, Ordering::SeqCst);
        for _ in 0..200 {
            if matches!(b_lease.completion(), Demand::DuplicateRoot(_)) {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
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
        while !ops.poll_started.load(Ordering::SeqCst) {
            thread::yield_now();
        }
        std::fs::remove_dir(&b_root).unwrap();
        symlink(&a_root, &b_root).unwrap();
        let refused = host.demand(&b).unwrap();
        assert!(matches!(refused.outcome(), Demand::DuplicateRoot(_)));
        assert!(matches!(a_lease.completion(), Demand::DuplicateRoot(_)));
        assert!(matches!(b_lease.completion(), Demand::DuplicateRoot(_)));
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);

        ops.poll_release.store(true, Ordering::SeqCst);
        for _ in 0..200 {
            if ops.detaches.load(Ordering::SeqCst) == 2 {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
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
        while !ops.poll_started.load(Ordering::SeqCst) {
            thread::yield_now();
        }

        std::fs::remove_dir(&root).unwrap();
        symlink("root", &root).unwrap();
        let demand = host.demand(&name).unwrap();
        assert_eq!(
            demand.completion(),
            Demand::State(TrustState::untrusted(
                UntrustedReason::environmental_refusal()
            ))
        );
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        ops.poll_release.store(true, Ordering::SeqCst);
        for _ in 0..200 {
            if ops.detaches.load(Ordering::SeqCst) == 1 {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
        thread::sleep(Duration::from_millis(10));
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        assert_eq!(
            host.state(&name),
            Some(TrustState::untrusted(
                UntrustedReason::environmental_refusal()
            ))
        );
        drop((demand, host));
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
        ops.emit.store(true, Ordering::SeqCst);
        let mut saw_warming = false;
        let mut saw_progress = false;
        for _ in 0..100 {
            if let Some(TrustState::Warming { healed, .. }) = host.state(&name) {
                saw_warming = true;
                saw_progress |= healed > 0;
                if saw_progress {
                    break;
                }
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(saw_warming, "polled dirtiness never closed Ready");
        assert!(
            saw_progress,
            "warming progress did not advance before Ready"
        );
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
        ops.emit.store(true, Ordering::SeqCst);
        let mut warming = false;
        for _ in 0..200 {
            if matches!(host.state(&name), Some(TrustState::Warming { .. })) {
                warming = true;
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(warming);
        drop(lease);
        host.reap_idle(Instant::now()).unwrap();
        wait_for_state(&host, &name, TrustState::Unattached);
    }

    fn wait_for_state<O: EntryOps>(host: &Host<O>, name: &VaultName, expected: TrustState) {
        for _ in 0..200 {
            if host.state(name) == Some(expected.clone()) {
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("state did not become {expected:?}");
    }
}
