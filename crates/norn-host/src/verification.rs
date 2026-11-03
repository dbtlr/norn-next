//! Each attached store's periodic verification, run on the background lane.
//!
//! **The whole periodic verification is one background unit.** It runs
//! through read-only connections of its own: the read-only checks over one
//! snapshot of the store, then a `VACUUM INTO` that writes a copy of the store
//! into its derived directory, and the full-text check — the one check that
//! writes — on the copy ([`norn_store::VerificationSource`]). It holds no
//! claim, no attachment, no entry gate and never the store's writer, so a
//! write never waits for it and watcher batches keep draining while it reads
//! every page.
//!
//! **One slot per derived store, bound to one store lifetime at a time.** An
//! attach binds the store it opened, and a rebuild binds the store it built.
//! The slot keeps the verification's clock, which is a lane deadline: due one
//! [`crate::production::STORE_VERIFICATION_INTERVAL`] after the lifetime is
//! bound and after each verification that reached the end of its lifetime's
//! store or a verdict about it.
//!
//! **A store lifetime retires before its store goes.** A rebuild's discard, a
//! detach, an unwound leg's give-back and host shutdown each retire it in one
//! order: under the slot's lock the lifetime is unbound and a verification in
//! flight over it is stopped — its mark set, then its statement interrupted —
//! and only once that verification has returned does the caller go on to
//! discard the store or give back the maintainer lock. A stopped verification
//! returns within one progress-handler interval or one document's recompute,
//! and reaches no verdict.
//!
//! **A verdict counts only for the lifetime it read.** The entry judges a
//! damage verdict only while this slot is still bound to the lifetime the
//! verification read ([`StoreVerifications::is_current`]), which a rebuild's
//! discard has already ended, so a verdict over a replaced or retired store
//! schedules nothing. A refused verification is no verdict at all: it
//! schedules nothing and is next due at its normal interval.
//!
//! The slots' map lock and each slot's lock are leaves: no thread holds either
//! across a take of an entry gate or across a verification.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

#[cfg(test)]
use norn_store::VerificationPoint;
use norn_store::{Verification, VerificationSource, VerificationStop};
use norn_wire::VaultName;

use crate::BackgroundUnit;

/// What the lane's verifications came to, counted since the host began.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VerificationReading {
    /// Verifications that ran, whatever they came to.
    pub runs: u64,
    /// Verifications that found the store sound.
    pub sound: u64,
    /// Damage verdicts handed to the entry's judgment: each was over the
    /// lifetime the slot was still bound to when the verification returned.
    pub damaged: u64,
    /// Damage verdicts over a lifetime that had retired by the time the
    /// verification returned, which schedule nothing.
    pub stale: u64,
    /// Verifications the environment refused, which are no verdict.
    pub refused: u64,
    /// Verifications a retirement stopped before they reached a verdict.
    pub interrupted: u64,
}

/// One derived store's verification: its bound lifetime, its clock and the
/// verification in flight.
#[derive(Default)]
struct Slot {
    state: Mutex<SlotState>,
    /// Signalled when a verification in flight returns, which a retirement
    /// waits for.
    returned: Condvar,
}

#[derive(Default)]
struct SlotState {
    /// The store lifetime the slot verifies, or `None` once it retired.
    bound: Option<Arc<VerificationSource>>,
    /// When the bound lifetime is next due a verification.
    due: Option<Instant>,
    /// The stop of the verification in flight, while one is.
    running: Option<VerificationStop>,
}

/// The periodic verifications of every derived store the host has attached,
/// keyed by vault name as the lane's set is.
pub(crate) struct StoreVerifications {
    slots: Mutex<BTreeMap<VaultName, Arc<Slot>>>,
    interval: Duration,
    reading: Mutex<VerificationReading>,
    /// Runs at each point of a verification at which a stop is met, with the
    /// verification's stop, so a case can hold one in flight.
    #[cfg(test)]
    inside: Mutex<Option<InsideVerification>>,
}

/// A case's hook into every later verification: see
/// [`StoreVerifications::run_inside_verifications`].
#[cfg(test)]
type InsideVerification = Arc<dyn Fn(VerificationPoint, &VerificationStop) + Send + Sync>;

fn tolerant<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(PoisonError::into_inner)
}

impl StoreVerifications {
    pub(crate) fn new(interval: Duration) -> Self {
        Self {
            slots: Mutex::new(BTreeMap::new()),
            interval,
            reading: Mutex::new(VerificationReading::default()),
            #[cfg(test)]
            inside: Mutex::new(None),
        }
    }

    fn slot(&self, vault: &VaultName) -> Option<Arc<Slot>> {
        tolerant(&self.slots).get(vault).cloned()
    }

    /// Bind `vault`'s slot to the store lifetime `source` was minted from,
    /// due one interval from now, and clear a copy a crash left beside the
    /// store. Opens no database.
    ///
    /// A lifetime still bound is retired first, so a slot verifies one
    /// lifetime at a time; the attach and the rebuild that bind have each
    /// retired the lifetime before theirs already, and this finds none. A
    /// rebuild's discard has removed the copy already; an attach over a store
    /// it opened as it stood meets a crash's copy only here. A leftover copy
    /// the environment will not let go is left for the next verification,
    /// which clears the copy's place before it writes.
    pub(crate) fn bind(&self, vault: &VaultName, source: VerificationSource) {
        let _ = source.discard_copy();
        let slot = Arc::clone(
            tolerant(&self.slots)
                .entry(vault.clone())
                .or_insert_with(|| Arc::new(Slot::default())),
        );
        let mut state = Self::retired(&slot);
        state.bound = Some(Arc::new(source));
        state.due = Some(Instant::now() + self.interval);
    }

    /// Retire the store lifetime `vault`'s slot is bound to, ahead of a
    /// rebuild's discard of its store: a verification in flight over it is
    /// stopped and has returned when this does.
    pub(crate) fn retire_store(&self, vault: &VaultName) {
        if let Some(slot) = self.slot(vault) {
            drop(Self::retired(&slot));
        }
    }

    /// Retire `vault`'s lifetime and forget its slot: the give-back of a
    /// detach and of an unwound leg, before the maintainer lock goes.
    pub(crate) fn detach(&self, vault: &VaultName) {
        let slot = tolerant(&self.slots).remove(vault);
        if let Some(slot) = slot {
            drop(Self::retired(&slot));
        }
    }

    /// Retire every lifetime, at host shutdown: each verification in flight
    /// is stopped rather than waited out.
    pub(crate) fn retire_all(&self) {
        let slots: Vec<Arc<Slot>> = tolerant(&self.slots).values().cloned().collect();
        for slot in slots {
            drop(Self::retired(&slot));
        }
    }

    /// Unbind `slot`'s lifetime and stop a verification in flight over it,
    /// then wait for that verification to return, and answer the slot's
    /// state locked.
    fn retired(slot: &Slot) -> MutexGuard<'_, SlotState> {
        let mut state = tolerant(&slot.state);
        state.bound = None;
        state.due = None;
        if let Some(stop) = &state.running {
            stop.stop();
        }
        while state.running.is_some() {
            state = slot
                .returned
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        state
    }

    /// Whether `vault`'s slot is still bound to the lifetime `epoch` names:
    /// the test a damage verdict passes before the entry judges it.
    pub(crate) fn is_current(&self, vault: &VaultName, epoch: &str) -> bool {
        self.slot(vault).is_some_and(|slot| {
            tolerant(&slot.state)
                .bound
                .as_ref()
                .is_some_and(|bound| bound.epoch() == epoch)
        })
    }

    /// When `vault`'s bound lifetime is next due a verification, or `None`
    /// where it is bound to none or one is in flight. Cheap and nonblocking.
    pub(crate) fn deadline(&self, vault: &VaultName) -> Option<Instant> {
        let slot = self.slot(vault)?;
        let state = tolerant(&slot.state);
        if state.running.is_some() || state.bound.is_none() {
            return None;
        }
        state.due
    }

    /// Whether `vault`'s verification is due now.
    pub(crate) fn owed(&self, vault: &VaultName) -> bool {
        self.deadline(vault)
            .is_some_and(|due| due <= Instant::now())
    }

    /// Run `vault`'s verification as one background unit where it is due, or
    /// answer `None` where it is not.
    ///
    /// The verification runs holding no lock: the slot records its stop so a
    /// retirement can reach it, and the verdict is taken against the
    /// lifetime the slot is bound to when it returns. A verification that
    /// reached the end of a lifetime still bound moves the clock on one
    /// interval, unless it found that lifetime damaged; one whose lifetime
    /// retired leaves the clock to the next bind.
    ///
    /// **A damage verdict leaves the verification owed**, as a feed page that
    /// meets damage settles nothing and stays owed: the clock comes due again
    /// after the lane's retry backoff. A verdict the entry takes schedules the
    /// rebuild, whose discard retires this lifetime and its clock with it, so
    /// the verification runs again only where the entry set the verdict
    /// aside — its stance refused while the verdict was in flight, as under
    /// an overflow's reconcile, which keeps this store — and then only once
    /// the stance feeds again, when it meets the same damage and the entry
    /// takes it. Moved on a whole interval, that damage would stand an hour.
    /// A verdict the entry carries to the end of a claim held under a stance
    /// that still feeds may be met once more per backoff until that claim
    /// ends; the entry keeps the verdict it already carries.
    pub(crate) fn unit(&self, vault: &VaultName) -> Option<BackgroundUnit> {
        let slot = self.slot(vault)?;
        let source = {
            let mut state = tolerant(&slot.state);
            let due = state.due.is_some_and(|due| due <= Instant::now());
            if state.running.is_some() || !due {
                return None;
            }
            let source = Arc::clone(state.bound.as_ref()?);
            state.running = Some(source.stop_handle());
            source
        };
        let verdict = {
            // The stop is taken back on every way out, an unwind included,
            // so a retirement waiting for it is never left waiting.
            let returned = Returned(&slot);
            let verdict = self.verify(&source);
            drop(returned);
            verdict
        };
        let current = {
            let mut state = tolerant(&slot.state);
            let current = state
                .bound
                .as_ref()
                .is_some_and(|bound| Arc::ptr_eq(bound, &source));
            if current {
                let damaged =
                    matches!(&verdict, Verification::Failed(error) if error.damage().is_some());
                let next = if damaged {
                    crate::lifecycle::BACKGROUND_RETRY_AFTER
                } else {
                    self.interval
                };
                state.due = Some(Instant::now() + next);
            }
            current
        };
        let mut reading = tolerant(&self.reading);
        reading.runs += 1;
        Some(match verdict {
            Verification::Sound => {
                reading.sound += 1;
                BackgroundUnit::Ran { owed: false }
            }
            Verification::Interrupted => {
                reading.interrupted += 1;
                BackgroundUnit::Ran { owed: false }
            }
            Verification::Failed(error) if error.damage().is_some() => {
                if current {
                    reading.damaged += 1;
                    BackgroundUnit::StoreDamaged {
                        epoch: source.epoch().to_string(),
                        detail: crate::refusal::store_refusal_told(&error),
                    }
                } else {
                    reading.stale += 1;
                    BackgroundUnit::Ran { owed: false }
                }
            }
            Verification::Failed(_) => {
                reading.refused += 1;
                BackgroundUnit::Ran { owed: false }
            }
        })
    }

    fn verify(&self, source: &VerificationSource) -> Verification {
        #[cfg(test)]
        {
            let inside = tolerant(&self.inside).clone();
            if let Some(inside) = inside {
                let stop = source.stop_handle();
                return source.verify(|at| inside(at, &stop));
            }
        }
        source.verify(|_| {})
    }

    pub(crate) fn reading(&self) -> VerificationReading {
        *tolerant(&self.reading)
    }

    /// Run `inside` at each point of every later verification at which a stop
    /// is met.
    #[cfg(test)]
    pub(crate) fn run_inside_verifications(
        &self,
        inside: impl Fn(VerificationPoint, &VerificationStop) + Send + Sync + 'static,
    ) {
        *tolerant(&self.inside) = Some(Arc::new(inside));
    }

    /// Bring `vault`'s verification due now.
    #[cfg(test)]
    pub(crate) fn bring_due(&self, vault: &VaultName) {
        self.bring_due_at(vault, Instant::now());
    }

    /// Bring `vault`'s verification due at `at`.
    #[cfg(test)]
    pub(crate) fn bring_due_at(&self, vault: &VaultName, at: Instant) {
        if let Some(slot) = self.slot(vault) {
            tolerant(&slot.state).due = Some(at);
        }
    }
}

/// Takes a verification's stop back out of its slot and wakes a retirement
/// waiting for it, on every way out of the verification.
struct Returned<'a>(&'a Slot);

impl Drop for Returned<'_> {
    fn drop(&mut self) {
        tolerant(&self.0.state).running = None;
        self.0.returned.notify_all();
    }
}
