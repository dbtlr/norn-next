//! Build authority retains only bounded proof, never watcher resources.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct BuildProof {
    invalid: AtomicBool,
    accepting: AtomicBool,
    waker: Mutex<Option<std::task::Waker>>,
}

impl BuildProof {
    pub(super) fn new() -> Self {
        Self {
            invalid: AtomicBool::new(false),
            accepting: AtomicBool::new(true),
            waker: Mutex::new(None),
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
        if !self.invalid.swap(true, Ordering::SeqCst) {
            self.notify();
        }
    }

    pub(super) fn close_admission(&self) {
        if self.accepting.swap(false, Ordering::SeqCst) {
            self.notify();
        }
    }
}

/// The build's watcher authority, captured before enumeration.
///
/// Every from-zero build captures its own authority. Host early admission consumes
/// it in NORN-468 PR C. Any accepted fact or coverage loss withdraws it in full;
/// NORN-469 narrows fact withdrawal to the affected paths.
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
    /// Capture fresh authority under live synchronized coverage, before enumeration.
    /// Replaces and withdraws the previous build's authority, waking its waiter.
    /// Prior facts do not prevent capture; terminal coverage loss does.
    pub fn capture_build_authority(&self) -> Option<BuildAuthority> {
        self.check_root_anchor().ok()?;
        let mut state = self.state.lock().ok()?;
        if state.terminal.is_some() || state.unsubscribed || state.coalescer_died {
            return None;
        }
        state.build_authority.invalidate();
        state.build_authority = Arc::new(BuildProof::new());
        let observation = BuildAuthority {
            proof: Arc::clone(&state.build_authority),
            control: Arc::clone(&self.control),
            state: Arc::downgrade(&self.state),
            root: self.root_anchor.clone(),
        };
        observation.admits().then_some(observation)
    }
}
