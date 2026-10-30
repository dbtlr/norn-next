//! Build authority retains only bounded proof, never watcher resources.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct BuildProof {
    invalid: AtomicBool,
    accepting: AtomicBool,
    recording: AtomicBool,
    facts: Mutex<BuildFacts>,
    waker: Mutex<Option<std::task::Waker>>,
}

impl BuildProof {
    pub(super) fn new() -> Self {
        Self {
            invalid: AtomicBool::new(false),
            accepting: AtomicBool::new(true),
            recording: AtomicBool::new(false),
            facts: Mutex::new(BuildFacts::default()),
            waker: Mutex::new(None),
        }
    }

    pub(super) fn record(&self, root: &NormalizedPath, kind: EventKind) {
        if !self.recording.load(Ordering::Relaxed) {
            return;
        }
        let beneath = !matches!(
            kind,
            EventKind::Create(CreateKind::File)
                | EventKind::Remove(notify::event::RemoveKind::File)
                | EventKind::Modify(ModifyKind::Data(_))
        );
        let mut facts = self.facts.lock().unwrap_or_else(PoisonError::into_inner);
        if !self.recording.load(Ordering::Relaxed) {
            return;
        }
        if facts
            .control
            .iter()
            .any(|control| control.starts_with(root))
        {
            drop(facts);
            self.withdraw(BuildWithdrawal::Control);
            return;
        }
        if !facts.roots.contains_key(root) && facts.roots.len() == BUILD_FACT_CAP {
            drop(facts);
            self.withdraw(BuildWithdrawal::Capacity);
            return;
        }
        facts.sequence += 1;
        let sequence = facts.sequence;
        let record = facts.roots.entry(root.clone()).or_insert(Record {
            reported: sequence,
            underived_from: Some(sequence),
            beneath,
        });
        record.reported = sequence;
        record.beneath |= beneath;
        if let Some(observer) = &facts.observer {
            observer.recorded(1);
        }
    }

    fn notify(&self) {
        // Release notification bookkeeping before invoking the small host wake.
        let waker = self
            .waker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    pub(super) fn invalidate(&self) {
        self.withdraw(BuildWithdrawal::Coverage);
    }

    pub(super) fn withdraw(&self, cause: BuildWithdrawal) {
        if !self.invalid.swap(true, Ordering::SeqCst) {
            let mut facts = self.facts.lock().unwrap_or_else(PoisonError::into_inner);
            facts.withdrawal = Some(cause);
            if let Some(observer) = &facts.observer {
                observer.withdrawn(cause);
            }
            drop(facts);
            self.notify();
        }
    }

    pub(super) fn close_admission(&self) {
        if self.accepting.swap(false, Ordering::SeqCst) {
            self.notify();
        }
    }
}

/// Why bounded construction authority was withdrawn.
#[derive(Clone, Copy)]
pub enum BuildWithdrawal {
    /// Root, backend, rescan, or schema coverage no longer supports it.
    Coverage,
    /// An accepted root reaches a control file.
    Control,
    /// The build exhausted its distinct-root bound.
    Capacity,
}

/// Atomic-only host evidence sink. Calls may hold watcher State or an entry gate;
/// implementations must never take those locks, perform I/O, or wake a waiter.
pub trait BuildObserver: Send + Sync {
    /// Count accepted root reports.
    fn recorded(&self, count: u64);
    /// Count a construction's first full withdrawal.
    fn withdrawn(&self, cause: BuildWithdrawal);
}

/// Maximum distinct accepted roots retained by one construction.
pub const BUILD_FACT_CAP: usize = 1024;

/// Bounded acceptance history, retained until the construction retires.
#[derive(Default)]
struct BuildFacts {
    sequence: u64,
    control: Vec<NormalizedPath>,
    observer: Option<Arc<dyn BuildObserver>>,
    withdrawal: Option<BuildWithdrawal>,
    roots: std::collections::BTreeMap<NormalizedPath, Record>,
}

struct Record {
    reported: u64,
    underived_from: Option<u64>,
    beneath: bool,
}

/// The build's watcher authority, captured before enumeration.
///
/// Every from-zero build records bounded accepted roots. Readers compare their
/// requirements with those roots; coverage loss withdraws the authority in full.
///
/// Root checks perform filesystem work and must run outside all locks. Admission
/// and held checks read bounded bookkeeping only: entry gates may call them.
/// Watcher callbacks may wake the stance signal but never acquire an entry gate. This handle
/// owns no subscription, watcher, coalescer, delivery slot or batch maps.
#[derive(Clone)]
pub struct BuildAuthority {
    proof: Arc<BuildProof>,
    control: Arc<(Mutex<SubscriptionState>, Condvar)>,
    state: Weak<Mutex<State>>,
    root: RootAnchor,
}

impl BuildAuthority {
    /// Bind host evidence, including reports accepted before binding.
    pub fn observe(&self, observer: Arc<dyn BuildObserver>) {
        let mut facts = self
            .proof
            .facts
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        observer.recorded(facts.sequence);
        if let Some(cause) = facts.withdrawal {
            observer.withdrawn(cause);
        }
        facts.observer = Some(observer);
    }

    /// Stop recording when construction closes. Established reads retain history.
    pub fn retire(&self) {
        let _facts = self
            .proof
            .facts
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.proof.recording.store(false, Ordering::Relaxed);
    }

    /// Inspect bounded acceptance history under one bookkeeping lock.
    /// Callers may hold the entry gate; callbacks must never acquire watcher State.
    pub fn visit_roots<T>(
        &self,
        visit: impl FnOnce(
            u64,
            &mut dyn Iterator<Item = (&NormalizedPath, u64, Option<u64>, bool)>,
        ) -> T,
    ) -> T {
        let facts = self
            .proof
            .facts
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut roots = facts
            .roots
            .iter()
            .map(|(path, record)| (path, record.reported, record.underived_from, record.beneath));
        visit(facts.sequence, &mut roots)
    }

    /// The latest accepted root report in this construction.
    pub fn sequence(&self) -> u64 {
        self.visit_roots(|sequence, _| sequence)
    }

    /// Record any known source contradiction permanently, for all derived sources.
    pub fn invalidate(&self) {
        self.proof.invalidate();
    }

    /// Check the registered root outside entry gates and watcher locks.
    /// Restoration never revives this build's authority.
    pub fn check_root(&self) -> bool {
        if let Err(error) = self.root.check() {
            self.invalidate();
            if let Some(state) = self.state.upgrade() {
                record_terminal(&state, &self.control, error);
            } else {
                publish_control(&self.control, SubscriptionState::Terminal(error));
            }
            return false;
        }
        self.held_is_current()
    }

    /// Cheap new-acquisition check. Lock order is entry gate before bookkeeping.
    pub fn admits(&self) -> bool {
        self.proof.accepting.load(Ordering::SeqCst)
            && self.held_is_current()
            && matches!(
                *self
                    .control
                    .0
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner),
                SubscriptionState::Live
            )
    }

    /// Established immutable reads survive administrative unsubscription. Known
    /// accepted facts and source/root/backend contradictions still withdraw them.
    pub fn held_is_current(&self) -> bool {
        if self.proof.invalid.load(Ordering::SeqCst) {
            return false;
        }
        if matches!(
            *self
                .control
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
            SubscriptionState::Terminal(_)
        ) {
            self.invalidate();
            return false;
        }
        true
    }

    /// Register the one stance-signal wake for this build. Re-registration
    /// replaces it; storage never grows with requests. An already withdrawn or
    /// administratively closed authority wakes immediately, including when it
    /// races registration. No bridge thread or polling is needed.
    ///
    /// The adapter must retain only the stance signal, never an entry, gate,
    /// subscription or coverage. Its wake may run under watcher bookkeeping or
    /// the registering entry gate: it must never acquire an entry gate or watcher
    /// bookkeeping, perform I/O, wait for work, or call back into this API.
    /// Signal bookkeeping must be released before the host takes an entry gate
    /// or calls authority checks. Thus both entry→signal and watcher→signal are
    /// allowed, and signal→entry/watcher is forbidden. The proof's notification
    /// mutex is always released before wake or replaced-waker destruction.
    pub fn register_waker(&self, waker: &std::task::Waker) {
        let mut slot = self
            .proof
            .waker
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let withdrawn = self.proof.invalid.load(Ordering::SeqCst)
            || !self.proof.accepting.load(Ordering::SeqCst);
        let replaced = if !withdrawn && !slot.as_ref().is_some_and(|old| old.will_wake(waker)) {
            slot.replace(waker.clone())
        } else {
            None
        };
        drop(slot);
        drop(replaced);
        if withdrawn {
            waker.wake_by_ref();
        }
    }
}

impl Subscription {
    /// Normalize a build control path under the coverage's already proven case behavior.
    pub fn normalize_build_control(&self, path: &Path) -> Result<NormalizedPath, crate::PathError> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .normalizer
            .normalize(path)
    }

    /// Capture fresh authority under live synchronized coverage, before enumeration.
    /// Replaces and withdraws the previous build's authority, waking its waiter.
    /// Prior facts do not prevent capture; terminal coverage loss does.
    pub fn capture_build_authority(&self, control: &[NormalizedPath]) -> Option<BuildAuthority> {
        self.check_root_anchor().ok()?;
        let mut state = self.state.lock().ok()?;
        if state.terminal.is_some()
            || state.terminal_taken
            || state.unsubscribed
            || state.coalescer_died
        {
            return None;
        }
        state.build_authority.invalidate();
        state
            .build_authority
            .recording
            .store(false, Ordering::Relaxed);
        state.build_authority = Arc::new(BuildProof::new());
        state
            .build_authority
            .facts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .control = control.to_vec();
        state
            .build_authority
            .recording
            .store(true, Ordering::Relaxed);
        let observation = BuildAuthority {
            proof: Arc::clone(&state.build_authority),
            control: Arc::clone(&self.control),
            state: Arc::downgrade(&self.state),
            root: self.root_anchor.clone(),
        };
        observation.admits().then_some(observation)
    }
}
