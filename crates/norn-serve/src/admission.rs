//! What decides whether a wire request runs: the in-flight bound, each
//! request's claim, the draining flag and the sweep over waiting claims, and
//! the per-vault mutation semaphores.
//!
//! **A claim is one small atomic state machine per admitted request**, and
//! whoever makes a terminal transition sends the request's one answer; no one
//! else does:
//!
//! | From | To | By | When |
//! |---|---|---|---|
//! | (admission) | `waiting` | the route | admitted, before its first dispatch |
//! | `waiting` | `started` | the pool closure | just before it calls the host |
//! | `waiting` | `refused` | the drain's sweep, or the route's re-check | the drain has begun |
//! | `waiting` | `refused` | the request going unanswered | its client disconnected; no one is answered |
//! | `waiting` | `expired` | the deadline timer, or the route's re-check | the deadline passed |
//! | `started` | `waiting` | the route | the attempt answered `Warming`, no drain, deadline not passed |
//! | `started` | `refused` | the route | the attempt answered `Warming` and the drain has begun |
//! | `started` | `expired` | the route | the attempt answered `Warming` and the deadline passed |
//! | `started` | `answered` | the route | any other answer |
//!
//! A closure whose `waiting → started` fails runs nothing. **Every entry into
//! `waiting` is followed, before any dispatch, by a read of the draining flag
//! and then of the deadline**, so no claim enters `waiting` behind the sweep:
//! with sequentially consistent atomics a claim entering `waiting` is either
//! seen by the sweep or sees the flag, and once the sweep has finished no
//! attempt starts; and it is either expired by the timer, which fires after the
//! claim is `waiting`, or sees the deadline passed.
//!
//! **What the drain calls.** [`Admission::begin_drain`] sets the flag, sweeps
//! every waiting claim to `refused` and wakes the requests standing on them,
//! each of which answers `serve/draining`; a request arriving after it is
//! refused before admission. The drain (`lifecycle.rs`) calls it once, as it
//! begins.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use norn_wire::VaultName;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

/// Requests in flight on the wire route at once, dispatched or waiting; past
/// it a request is answered `serve/busy` at once. The hello and the drain
/// request are exempt.
pub(crate) const IN_FLIGHT_BOUND: usize = 64;

/// Where one request's claim stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ClaimState {
    Waiting = 0,
    Started = 1,
    Refused = 2,
    Expired = 3,
    Answered = 4,
}

impl ClaimState {
    fn from_u8(value: u8) -> ClaimState {
        match value {
            0 => ClaimState::Waiting,
            1 => ClaimState::Started,
            2 => ClaimState::Refused,
            3 => ClaimState::Expired,
            _ => ClaimState::Answered,
        }
    }
}

/// One admitted request's claim.
#[derive(Debug)]
pub(crate) struct Claim(AtomicU8);

impl Claim {
    /// A claim entering `waiting` at admission.
    fn waiting() -> Claim {
        Claim(AtomicU8::new(ClaimState::Waiting as u8))
    }

    /// Move the claim from `from` to `to`, answering whether this caller made
    /// the move: exactly one of any two callers racing for one transition
    /// wins it.
    pub(crate) fn shift(&self, from: ClaimState, to: ClaimState) -> bool {
        self.0
            .compare_exchange(from as u8, to as u8, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub(crate) fn state(&self) -> ClaimState {
        ClaimState::from_u8(self.0.load(Ordering::SeqCst))
    }
}

/// The process-wide admission state of the wire route.
#[derive(Debug, Default)]
pub(crate) struct Admission {
    in_flight: AtomicUsize,
    draining: AtomicBool,
    /// Woken once the drain's sweep has finished.
    drain_swept: Notify,
    /// Every claim admitted and not yet ended, for the drain's sweep. Keyed by
    /// an id so a request leaving removes its own entry in one step.
    claims: Mutex<HashMap<u64, Weak<Claim>>>,
    next_claim: AtomicU64,
    /// Requests that went unanswered with their claim not yet ended, for
    /// this crate's tests to see that a disconnect reached the route.
    #[cfg(test)]
    unanswered: AtomicUsize,
}

impl Admission {
    /// Take one in-flight slot, or nothing where all are taken.
    pub(crate) fn enter(self: &Arc<Self>) -> Option<InFlight> {
        let mut current = self.in_flight.load(Ordering::SeqCst);
        loop {
            if current >= IN_FLIGHT_BOUND {
                return None;
            }
            match self.in_flight.compare_exchange(
                current,
                current + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    return Some(InFlight {
                        admission: Arc::clone(self),
                    });
                }
                Err(seen) => current = seen,
            }
        }
    }

    /// Requests in flight now.
    #[cfg(test)]
    pub(crate) fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }

    /// Claims registered for the drain's sweep now.
    #[cfg(test)]
    pub(crate) fn registered(&self) -> usize {
        self.claims().len()
    }

    /// Note a request that went unanswered with its claim not yet ended.
    #[cfg(test)]
    pub(crate) fn note_unanswered(&self) {
        self.unanswered.fetch_add(1, Ordering::SeqCst);
    }

    /// Requests that went unanswered with their claim not yet ended.
    #[cfg(test)]
    pub(crate) fn unanswered(&self) -> usize {
        self.unanswered.load(Ordering::SeqCst)
    }

    /// Whether the drain has begun.
    pub(crate) fn draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    /// Admit a claim in `waiting`, registered for the drain's sweep until the
    /// returned guard is dropped.
    pub(crate) fn admit(self: &Arc<Self>) -> Admitted {
        let id = self.next_claim.fetch_add(1, Ordering::SeqCst);
        let claim = Arc::new(Claim::waiting());
        self.claims().insert(id, Arc::downgrade(&claim));
        Admitted {
            admission: Arc::clone(self),
            id,
            claim,
        }
    }

    /// A future that completes once the drain's sweep has finished. Made
    /// before the flag is read, so a sweep that finishes between the read and
    /// the wait still wakes it.
    pub(crate) fn swept(&self) -> tokio::sync::futures::Notified<'_> {
        self.drain_swept.notified()
    }

    /// **Begin the drain:** set the flag, then sweep every claim that is
    /// `waiting` when the sweep reaches it to `refused`, then wake the requests
    /// standing on them, each of which answers `serve/draining`. Answers how
    /// many claims the sweep refused.
    pub(crate) fn begin_drain(&self) -> usize {
        self.draining.store(true, Ordering::SeqCst);
        let claims: Vec<Arc<Claim>> = self.claims().values().filter_map(Weak::upgrade).collect();
        let refused = claims
            .iter()
            .filter(|claim| claim.shift(ClaimState::Waiting, ClaimState::Refused))
            .count();
        self.drain_swept.notify_waiters();
        refused
    }

    fn claims(&self) -> MutexGuard<'_, HashMap<u64, Weak<Claim>>> {
        // An insert or a remove; one a panic interrupted left the map whole.
        self.claims.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One in-flight slot, given back where it is dropped.
#[derive(Debug)]
pub(crate) struct InFlight {
    admission: Arc<Admission>,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.admission.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// An admitted request's claim, registered for the sweep while it is held.
#[derive(Debug)]
pub(crate) struct Admitted {
    admission: Arc<Admission>,
    id: u64,
    pub(crate) claim: Arc<Claim>,
}

impl Drop for Admitted {
    fn drop(&mut self) {
        self.admission.claims().remove(&self.id);
    }
}

/// One async semaphore per vault with a mutation in flight: a vault's
/// mutations take at most one mutation-pool thread at a time, and the rest
/// wait here holding none.
///
/// An entry lives while a permit or a waiter holds it, and the last one out
/// removes it, so the map holds the vaults with a mutation in flight and no
/// more; a request looks up its own vault's entry and walks no other.
#[derive(Debug, Default)]
pub(crate) struct Semaphores {
    vaults: Mutex<HashMap<VaultName, Arc<Semaphore>>>,
}

impl Semaphores {
    /// The semaphore `vault`'s mutations take turns on, held by the caller
    /// until it acquires or gives up.
    pub(crate) fn of(self: &Arc<Self>, vault: &VaultName) -> Turn {
        let semaphore = Arc::clone(
            self.vaults()
                .entry(vault.clone())
                .or_insert_with(|| Arc::new(Semaphore::new(1))),
        );
        Turn {
            semaphores: Arc::clone(self),
            vault: vault.clone(),
            semaphore: Some(semaphore),
            permit: None,
        }
    }

    fn vaults(&self) -> MutexGuard<'_, HashMap<VaultName, Arc<Semaphore>>> {
        self.vaults.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(crate) fn held(&self) -> usize {
        self.vaults().len()
    }
}

/// One mutation's place on its vault's semaphore: waiting for its turn, then
/// holding it until dropped.
#[derive(Debug)]
pub(crate) struct Turn {
    semaphores: Arc<Semaphores>,
    vault: VaultName,
    semaphore: Option<Arc<Semaphore>>,
    permit: Option<OwnedSemaphorePermit>,
}

impl Turn {
    /// Wait for this vault's turn. Cancel-safe: dropping the wait gives the
    /// place up.
    pub(crate) async fn take(&mut self) {
        if self.permit.is_some() {
            return;
        }
        let semaphore = Arc::clone(self.semaphore.as_ref().expect("a turn holds its semaphore"));
        // The semaphore is never closed, so the acquire only ever succeeds.
        if let Ok(permit) = semaphore.acquire_owned().await {
            self.permit = Some(permit);
        }
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        drop(self.permit.take());
        let semaphore = self.semaphore.take();
        let mut vaults = self.semaphores.vaults();
        // The map's own reference and this one: no other permit holder or
        // waiter stands on the vault, so its entry goes.
        if let Some(semaphore) = semaphore
            && Arc::strong_count(&semaphore) == 2
        {
            vaults.remove(&self.vault);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_in_flight_bound_refuses_the_slot_past_it_and_gives_each_back() {
        let admission = Arc::new(Admission::default());
        let held: Vec<InFlight> = (0..IN_FLIGHT_BOUND)
            .map(|_| admission.enter().expect("a slot under the bound"))
            .collect();
        assert!(admission.enter().is_none());
        drop(held);
        assert_eq!(admission.in_flight(), 0);
        assert!(admission.enter().is_some());
    }

    /// Exactly one of two racers wins each transition out of a state.
    #[test]
    fn one_claim_has_one_winner_per_transition() {
        let claim = Claim::waiting();
        assert!(claim.shift(ClaimState::Waiting, ClaimState::Started));
        assert!(!claim.shift(ClaimState::Waiting, ClaimState::Refused));
        assert!(claim.shift(ClaimState::Started, ClaimState::Waiting));
        assert!(claim.shift(ClaimState::Waiting, ClaimState::Expired));
        assert!(!claim.shift(ClaimState::Waiting, ClaimState::Started));
        assert_eq!(claim.state(), ClaimState::Expired);
    }

    /// The sweep refuses every waiting claim and only those: a started claim
    /// is in flight, and an ended request's claim is not walked.
    #[test]
    fn the_sweep_refuses_every_waiting_claim_and_no_started_one() {
        let admission = Arc::new(Admission::default());
        let waiting = admission.admit();
        let started = admission.admit();
        assert!(
            started
                .claim
                .shift(ClaimState::Waiting, ClaimState::Started)
        );
        let ended = admission.admit();
        drop(ended);
        assert!(!admission.draining());
        assert_eq!(admission.begin_drain(), 1);
        assert!(admission.draining());
        assert_eq!(waiting.claim.state(), ClaimState::Refused);
        assert_eq!(started.claim.state(), ClaimState::Started);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn one_vaults_mutations_take_turns_and_its_entry_goes_with_the_last() {
        let semaphores = Arc::new(Semaphores::default());
        let notes = VaultName::new("notes").unwrap();
        let other = VaultName::new("other").unwrap();
        let mut first = semaphores.of(&notes);
        first.take().await;
        let mut second = semaphores.of(&notes);
        let waited =
            tokio::time::timeout(std::time::Duration::from_millis(20), second.take()).await;
        assert!(waited.is_err(), "a second mutation of one vault waits");
        let mut elsewhere = semaphores.of(&other);
        elsewhere.take().await;
        assert_eq!(semaphores.held(), 2);
        drop(first);
        second.take().await;
        drop(second);
        drop(elsewhere);
        assert_eq!(semaphores.held(), 0);
    }
}
