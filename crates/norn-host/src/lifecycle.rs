use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use norn_config::VaultName;
use norn_fs::{Batch, RescanScope, WatchError};
use norn_wire::{TrustState, UntrustedReason};

use crate::registry::{AliasConflict, ServingRegistry};

/// Lifecycle timing chosen by the composition root. There is intentionally no
/// ambient or library default.
#[derive(Clone, Copy, Debug)]
pub struct LifecyclePolicy {
    pub idle_after: Duration,
    pub worker_slots: usize,
}

/// Work coalesced behind an entry's capacity-one runnable marker.
#[derive(Debug, Default)]
pub struct ReconcileWork {
    pub batch: Batch,
}

/// The effectful half of an entry lifecycle.
pub trait EntryOps: Send + Sync + 'static {
    type Attachment: Send + 'static;

    fn attach(&self, name: &VaultName) -> Result<Self::Attachment, JobFailure>;
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
    fn detach(&self, name: &VaultName, attachment: Self::Attachment);
}

/// A lifecycle job's semantic failure class.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JobFailure {
    Environmental(String),
    WatcherTerminal(String),
    LostMaintainership,
    MaintainerContended(String),
}

/// The immediate answer to client demand. Warming never blocks the caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Demand {
    State(TrustState),
    MaintainerContended(String),
    DuplicateRoot(AliasConflict),
    UnknownVault,
}

#[derive(Debug)]
pub enum HostError {
    NoWorkerSlots,
    WorkerStopped,
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoWorkerSlots => f.write_str("the host requires at least one worker slot"),
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
    last_demand: Instant,
    demand_leases: usize,
    safety_pins: usize,
    epoch: u64,
}

enum Job {
    Attach(VaultName, u64),
    Recover(VaultName, u64),
    Reconcile(VaultName, u64),
    Detach(VaultName, u64),
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
                            last_demand: now,
                            demand_leases: 0,
                            safety_pins: 0,
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
            let shared = Arc::clone(&shared);
            let receiver = Arc::clone(&receiver);
            workers.push(thread::spawn(move || {
                loop {
                    let job = receiver.lock().expect("worker receiver poisoned").recv();
                    match job {
                        Ok(job) => run_job(&shared, job),
                        Err(_) => break,
                    }
                }
            }));
        }
        Ok(Self {
            shared,
            _workers: workers,
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
    pub fn demand(&self, name: &VaultName) -> Result<Demand, HostError> {
        let Some(entry) = self.shared.entries.get(name) else {
            return Ok(Demand::UnknownVault);
        };
        if let Some(conflict) = self
            .shared
            .registry
            .recheck(name)
            .map_err(|_| HostError::WorkerStopped)?
        {
            return Ok(Demand::DuplicateRoot(conflict));
        }
        let mut state = entry.gate.lock().expect("entry gate poisoned");
        state.last_demand = Instant::now();
        state.demand_leases += 1;
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
            self.shared
                .jobs
                .send(job)
                .map_err(|_| HostError::WorkerStopped)?;
        }
        let answer = Demand::State(state.trust.clone());
        state.demand_leases -= 1;
        Ok(answer)
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
        for (name, entry) in &self.shared.entries {
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            if state.attachment.is_some()
                && state.demand_leases == 0
                && state.safety_pins == 0
                && now.saturating_duration_since(state.last_demand) >= self.shared.idle_after
                && !state.runnable
            {
                state.runnable = true;
                state.epoch += 1;
                let epoch = state.epoch;
                self.shared
                    .jobs
                    .send(Job::Detach(name.clone(), epoch))
                    .map_err(|_| HostError::WorkerStopped)?;
            }
        }
        Ok(())
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
                    state.trust = TrustState::Ready;
                }
                Err(JobFailure::MaintainerContended(_)) => state.trust = TrustState::Unattached,
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
            match result {
                Ok(()) => {
                    state.attachment = Some(attachment);
                    state.trust = TrustState::Ready;
                }
                Err(JobFailure::LostMaintainership) => {
                    state.pending = Batch::default();
                    state.trust = TrustState::Unattached;
                    drop(state);
                    shared.ops.detach(&name, attachment);
                }
                Err(_) => {
                    state.attachment = Some(attachment);
                    state.trust = TrustState::untrusted(UntrustedReason::environmental_refusal());
                }
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
                    if state.pending.is_empty() {
                        state.runnable = false;
                        state.trust = TrustState::Ready;
                        break;
                    }
                }
                Err(JobFailure::LostMaintainership) => {
                    shared.ops.detach(&name, attachment);
                    state.pending = Batch::default();
                    state.runnable = false;
                    state.trust = TrustState::Unattached;
                    break;
                }
                Err(_) => {
                    state.attachment = Some(attachment);
                    state.runnable = false;
                    state.trust = TrustState::untrusted(UntrustedReason::environmental_refusal());
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
    }

    impl EntryOps for Arc<FakeOps> {
        type Attachment = ();

        fn attach(&self, _: &VaultName) -> Result<(), JobFailure> {
            self.attaches.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn reconcile(&self, _: &VaultName, _: &mut (), _: ReconcileWork) -> Result<(), JobFailure> {
            Ok(())
        }

        fn recover(&self, _: &VaultName, _: &mut ()) -> Result<(), JobFailure> {
            self.recovers.fetch_add(1, Ordering::SeqCst);
            Ok(())
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
}
