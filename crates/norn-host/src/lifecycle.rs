use std::collections::BTreeMap;
use std::fmt;
use std::ops::Deref;
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
    /// the full hash-authoritative heal. An implementation must drain facts
    /// observed during healing before returning the Ready-capable attachment.
    fn attach(&self, name: &VaultName) -> Result<Self::Attachment, JobFailure>;
    /// Apply one coalesced envelope. Schema dirtiness dominates document roots;
    /// a rescan widens rather than discarding uncertainty.
    fn reconcile(
        &self,
        name: &VaultName,
        attachment: &mut Self::Attachment,
        work: ReconcileWork,
    ) -> Result<(), JobFailure>;
    /// Re-establish trust using resources retained after an environmental or
    /// watcher refusal. Implementations restart watcher coverage before the
    /// full heal when coverage was terminally lost.
    fn recover(
        &self,
        name: &VaultName,
        attachment: &mut Self::Attachment,
    ) -> Result<(), JobFailure>;
    /// Nonblocking read of at most one settled watcher batch.
    fn poll(
        &self,
        name: &VaultName,
        attachment: &mut Self::Attachment,
    ) -> Result<Option<Batch>, JobFailure>;
    fn detach(&self, name: &VaultName, attachment: Self::Attachment);
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
    runnable: bool,
    terminal_watcher: bool,
    maintainer_contended: Option<MaintainerIdentity>,
    last_demand: Instant,
    demand_leases: usize,
    safety_pins: usize,
    detach_due: bool,
    detach_scheduled: bool,
    epoch: u64,
}

enum Job {
    Attach(VaultName, u64),
    Recover(VaultName, u64),
    Reconcile(VaultName, u64),
    Detach(VaultName, u64),
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
        Some(Job::Detach(name.clone(), state.epoch))
    } else {
        None
    }
}

struct Shared<O: EntryOps> {
    registry: ServingRegistry,
    entries: BTreeMap<VaultName, Arc<Entry<O::Attachment>>>,
    ops: Arc<O>,
    jobs: mpsc::SyncSender<Job>,
    idle_after: Duration,
}

pub struct Host<O: EntryOps> {
    shared: Arc<Shared<O>>,
    _workers: Vec<thread::JoinHandle<()>>,
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
            schedule_due_detach(&mut state, &name)
        };
        if let Some(job) = schedule {
            let _ = shared.jobs.send(job);
        }
    }
}

impl<O: EntryOps> Drop for Host<O> {
    fn drop(&mut self) {
        let _ = self.dispatcher_stop.send(());
        if let Some(dispatcher) = self.dispatcher.take() {
            let _ = dispatcher.join();
        }
        let mut attached = Vec::new();
        for (name, entry) in &self.shared.entries {
            let attachment = {
                let mut state = entry.gate.lock().expect("entry gate poisoned");
                state.epoch += 1;
                state.runnable = false;
                state.pending = Batch::default();
                state.trust = TrustState::Unattached;
                state.attachment.take()
            };
            if let Some(attachment) = attachment {
                attached.push((name.clone(), attachment));
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
                            runnable: false,
                            terminal_watcher: false,
                            maintainer_contended: None,
                            last_demand: now,
                            demand_leases: 0,
                            safety_pins: 0,
                            detach_due: false,
                            detach_scheduled: false,
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
            jobs,
            idle_after: policy.idle_after,
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
                poll_watchers(&shared);
            }
        });
        Ok(Self {
            shared,
            _workers: workers,
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
        if let Some(conflict) = self
            .shared
            .registry
            .recheck(name)
            .map_err(|_| HostError::WorkerStopped)?
        {
            return Ok(DemandLease {
                outcome: Demand::DuplicateRoot(conflict),
                held: None,
            });
        }
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        state.last_demand = Instant::now();
        state.demand_leases += 1;
        state.detach_due = false;
        if state.detach_scheduled {
            state.epoch += 1;
            state.runnable = false;
            state.detach_scheduled = false;
        }
        if let Some(incumbent) = state.maintainer_contended.take() {
            drop(state);
            return Ok(DemandLease {
                outcome: Demand::MaintainerContended(incumbent),
                held: Some((Arc::clone(&self.shared), name.clone())),
            });
        }
        let schedule = matches!(
            state.trust,
            TrustState::Unattached | TrustState::Untrusted { .. }
        ) && !state.runnable;
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
            let answer = Demand::State(state.trust.clone());
            drop(state);
            self.shared
                .jobs
                .send(job)
                .map_err(|_| HostError::WorkerStopped)?;
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

    /// Merge watcher facts and schedule at most one runnable job for the entry.
    pub fn accept_batch(&self, name: &VaultName, batch: Batch) -> Result<(), HostError> {
        let Some(entry) = self.shared.entries.get(name) else {
            return Ok(());
        };
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        let rescan = !batch.rescans().is_empty();
        state.pending.merge(batch);
        if rescan {
            state.trust = TrustState::untrusted(UntrustedReason::WatcherOverflow);
        } else if matches!(state.trust, TrustState::Ready) {
            state.trust = TrustState::warming(0, None);
        }
        if !state.runnable && state.attachment.is_some() {
            state.runnable = true;
            state.epoch += 1;
            let epoch = state.epoch;
            drop(state);
            self.shared
                .jobs
                .send(Job::Reconcile(name.clone(), epoch))
                .map_err(|_| HostError::WorkerStopped)?;
        }
        Ok(())
    }

    /// A terminal watcher failure does not autonomously restart coverage.
    pub fn watcher_failed(&self, name: &VaultName, error: WatchError) {
        if let Some(entry) = self.shared.entries.get(name) {
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            state.terminal_watcher = true;
            state.trust = TrustState::untrusted(UntrustedReason::WatcherOverflow);
            let _ = error;
        }
    }

    /// Schedule expired entries for teardown. Safety-pinned work is allowed to
    /// finish; its release performs the expired detach immediately.
    pub fn reap_idle(&self, now: Instant) -> Result<(), HostError> {
        let mut jobs = Vec::new();
        for (name, entry) in &self.shared.entries {
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            if (state.attachment.is_some() || state.safety_pins > 0)
                && now.saturating_duration_since(state.last_demand) >= self.shared.idle_after
            {
                state.detach_due = true;
                if let Some(job) = schedule_due_detach(&mut state, name) {
                    jobs.push(job);
                }
            }
        }
        for job in jobs {
            self.shared
                .jobs
                .send(job)
                .map_err(|_| HostError::WorkerStopped)?;
        }
        Ok(())
    }
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
            if !state.pending.is_empty() && state.attachment.is_some() {
                state.runnable = true;
                state.epoch += 1;
                let epoch = state.epoch;
                drop(state);
                try_schedule_reconcile(shared, name, entry, epoch);
                continue;
            }
            let Some(attachment) = state.attachment.take() else {
                continue;
            };
            (attachment, state.epoch)
        };
        let result = shared.ops.poll(name, &mut attachment);
        let mut schedule = None;
        let mut detach = None;
        {
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            if state.epoch != epoch {
                detach = Some(attachment);
            } else {
                match result {
                    Ok(None) => state.attachment = Some(attachment),
                    Ok(Some(batch)) => {
                        let rescan = !batch.rescans().is_empty();
                        state.pending.merge(batch);
                        state.trust = if rescan {
                            TrustState::untrusted(UntrustedReason::WatcherOverflow)
                        } else {
                            TrustState::warming(0, None)
                        };
                        state.attachment = Some(attachment);
                        state.runnable = true;
                        state.epoch += 1;
                        schedule = Some(Job::Reconcile(name.clone(), state.epoch));
                    }
                    Err(JobFailure::LostMaintainership) => {
                        state.pending = Batch::default();
                        state.trust = TrustState::Unattached;
                        state.epoch += 1;
                        detach = Some(attachment);
                    }
                    Err(JobFailure::WatcherTerminal(_)) => {
                        state.terminal_watcher = true;
                        state.trust = TrustState::untrusted(UntrustedReason::WatcherOverflow);
                        state.attachment = Some(attachment);
                    }
                    Err(_) => {
                        state.trust =
                            TrustState::untrusted(UntrustedReason::environmental_refusal());
                        state.attachment = Some(attachment);
                    }
                }
            }
        }
        if let Some(attachment) = detach {
            shared.ops.detach(name, attachment);
        }
        if let Some(job) = schedule {
            let Job::Reconcile(_, epoch) = job else {
                unreachable!()
            };
            try_schedule_reconcile(shared, name, entry, epoch);
        }
    }
}

fn try_schedule_reconcile<O: EntryOps>(
    shared: &Arc<Shared<O>>,
    name: &VaultName,
    entry: &Arc<Entry<O::Attachment>>,
    epoch: u64,
) {
    if shared
        .jobs
        .try_send(Job::Reconcile(name.clone(), epoch))
        .is_err()
    {
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        if state.epoch == epoch {
            state.runnable = false;
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
    match job {
        Job::Attach(name, epoch) => {
            let result = shared.ops.attach(&name);
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            if state.epoch != epoch {
                if let Ok(attachment) = result {
                    drop(state);
                    shared.ops.detach(&name, attachment);
                }
                return;
            }
            state.runnable = false;
            match result {
                Ok(attachment) => {
                    state.attachment = Some(attachment);
                    state.terminal_watcher = false;
                    state.maintainer_contended = None;
                    if state.pending.is_empty() {
                        state.trust = TrustState::Ready;
                    } else {
                        state.epoch += 1;
                        let epoch = state.epoch;
                        state.runnable = true;
                        drop(state);
                        let _ = shared.jobs.send(Job::Reconcile(name, epoch));
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
            let result = shared.ops.recover(&name, &mut attachment);
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
                    state.attachment = Some(attachment);
                    state.terminal_watcher = false;
                    if state.detach_due {
                        next = schedule_due_detach(&mut state, &name);
                    } else if state.pending.is_empty() {
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
                    state.attachment = Some(attachment);
                    state.trust = TrustState::untrusted(UntrustedReason::WatcherOverflow);
                    next = schedule_due_detach(&mut state, &name);
                }
                Err(_) => {
                    state.attachment = Some(attachment);
                    state.trust = TrustState::untrusted(UntrustedReason::environmental_refusal());
                    next = schedule_due_detach(&mut state, &name);
                }
            }
            drop(state);
            if let Some(job) = next {
                let _ = shared.jobs.send(job);
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
                state.trust = TrustState::warming(0, None);
                (attachment, work)
            };
            let result = shared.ops.reconcile(&name, &mut attachment, work);
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            state.safety_pins -= 1;
            if state.epoch != epoch {
                drop(state);
                shared.ops.detach(&name, attachment);
                return;
            }
            match result {
                Ok(()) => {
                    state.attachment = Some(attachment);
                    if state.detach_due {
                        state.runnable = false;
                        let next = schedule_due_detach(&mut state, &name);
                        drop(state);
                        if let Some(job) = next {
                            let _ = shared.jobs.send(job);
                        }
                        break;
                    } else if state.pending.is_empty() {
                        state.runnable = false;
                        state.trust = TrustState::Ready;
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
                    state.trust = TrustState::untrusted(UntrustedReason::WatcherOverflow);
                    let next = schedule_due_detach(&mut state, &name);
                    drop(state);
                    if let Some(job) = next {
                        let _ = shared.jobs.send(job);
                    }
                    break;
                }
                Err(_) => {
                    state.attachment = Some(attachment);
                    state.runnable = false;
                    state.trust = TrustState::untrusted(UntrustedReason::environmental_refusal());
                    let next = schedule_due_detach(&mut state, &name);
                    drop(state);
                    if let Some(job) = next {
                        let _ = shared.jobs.send(job);
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
                state.attachment.take()
            };
            if let Some(attachment) = attachment {
                shared.ops.detach(&name, attachment);
            }
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            if state.epoch != epoch {
                return;
            }
            state.pending = Batch::default();
            state.runnable = false;
            state.detach_due = false;
            state.detach_scheduled = false;
            state.trust = TrustState::Unattached;
        }
    }
}

#[allow(dead_code)]
fn _schema_rescan_is_dominant(batch: &Batch) -> bool {
    batch.schema_dirty() || batch.rescans().contains(&RescanScope::Schema)
}

#[cfg(test)]
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
        contend_attach: std::sync::atomic::AtomicBool,
    }

    impl EntryOps for Arc<FakeOps> {
        type Attachment = ();

        fn attach(&self, _: &VaultName) -> Result<(), JobFailure> {
            self.attaches.fetch_add(1, Ordering::SeqCst);
            if self.contend_attach.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::MaintainerContended(
                    MaintainerIdentity::unknown(),
                ));
            }
            Ok(())
        }

        fn reconcile(&self, _: &VaultName, _: &mut (), _: ReconcileWork) -> Result<(), JobFailure> {
            self.reconciles.fetch_add(1, Ordering::SeqCst);
            if self.terminal_reconcile.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::WatcherTerminal("lost".into()));
            }
            Ok(())
        }

        fn recover(&self, _: &VaultName, _: &mut ()) -> Result<(), JobFailure> {
            self.recovers.fetch_add(1, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(20));
            if self.terminal_recover.swap(false, Ordering::SeqCst) {
                return Err(JobFailure::WatcherTerminal("lost".into()));
            }
            Ok(())
        }

        fn poll(&self, _: &VaultName, _: &mut ()) -> Result<Option<Batch>, JobFailure> {
            Ok(None)
        }

        fn detach(&self, _: &VaultName, _: ()) {
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
        drop(host.demand(&name).unwrap());
        for _ in 0..200 {
            if ops.attaches.load(Ordering::SeqCst) == 1
                && host.state(&name) == Some(TrustState::Unattached)
            {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        let lease = host.demand(&name).unwrap();
        assert!(matches!(*lease, Demand::MaintainerContended(_)));
        thread::sleep(Duration::from_millis(10));
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 1);
        drop(lease);
        drop(host.demand(&name).unwrap());
        wait_for(&host, &name, TrustState::Ready);
        assert_eq!(ops.attaches.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn idle_reap_releases_the_attachment_and_returns_to_unattached() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::ZERO);
        let _ = host.demand(&name).unwrap();
        wait_for(&host, &name, TrustState::Ready);
        host.reap_idle(Instant::now()).unwrap();
        wait_for(&host, &name, TrustState::Unattached);
        assert_eq!(ops.detaches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn demand_lease_cancels_queued_detach_until_client_work_ends() {
        let ops = Arc::new(FakeOps::default());
        let (host, name) = fixture(Arc::clone(&ops), Duration::ZERO);
        drop(host.demand(&name).unwrap());
        wait_for(&host, &name, TrustState::Ready);
        let lease = host.demand(&name).unwrap();
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

    #[derive(Default)]
    struct PollingOps {
        emit: std::sync::atomic::AtomicBool,
    }

    impl EntryOps for Arc<PollingOps> {
        type Attachment = ();
        fn attach(&self, _: &VaultName) -> Result<(), JobFailure> {
            Ok(())
        }
        fn reconcile(&self, _: &VaultName, _: &mut (), _: ReconcileWork) -> Result<(), JobFailure> {
            thread::sleep(Duration::from_millis(30));
            Ok(())
        }
        fn recover(&self, _: &VaultName, _: &mut ()) -> Result<(), JobFailure> {
            Ok(())
        }
        fn poll(&self, _: &VaultName, _: &mut ()) -> Result<Option<Batch>, JobFailure> {
            Ok(self.emit.swap(false, Ordering::SeqCst).then(Batch::default))
        }
        fn detach(&self, _: &VaultName, _: ()) {}
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
        for _ in 0..100 {
            if matches!(host.state(&name), Some(TrustState::Warming { .. })) {
                saw_warming = true;
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(saw_warming, "polled dirtiness never closed Ready");
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
        drop(host.demand(&name).unwrap());
        wait_for_state(&host, &name, TrustState::Ready);
        ops.emit.store(true, Ordering::SeqCst);
        wait_for_state(&host, &name, TrustState::warming(0, None));
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
