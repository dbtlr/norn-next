//! The entry gate: the lock over one entry's lifecycle state, the count of
//! every time it has been taken, and the signal a waiter outside it is woken
//! by.
//!
//! **Every hold of an entry's state begins in [`EntryGate::lock`].** The mutex
//! is private to this module, so no lock site elsewhere can take the state
//! without moving the count, and the count moves while the taker holds the
//! gate. A holder that reads [`EntryGate::times_taken`] twice without giving
//! the gate back reads the same number both times; a holder that gave it back
//! and took it again between the two readings reads a difference of at least
//! one, its own retake. That difference is how a read attests that the
//! statement it establishes on ran under one continuous hold.
//!
//! **Every hold ends in [`GateHold`]'s drop, and that is where the signal
//! moves.** A hold reads the state's [`Stanced::stance`] as it is taken and
//! again as it ends, and where the two differ it moves the gate's signal and
//! wakes every waiter on it. The signal therefore moves exactly when a hold
//! changed the stance, whatever that hold wrote: a publication that changes
//! only counters inside one stance wakes nobody. The lock order is the gate,
//! then the signal. A holder of the gate may take the signal; a waiter on the
//! signal holds nothing else, so no holder of the signal waits for the gate.

use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, LockResult, Mutex, MutexGuard, PoisonError};

/// The part of a gated state a waiter outside the gate waits on.
///
/// [`Stanced::stance`] is read at both ends of every hold, and
/// [`Stanced::end_hold`] runs at the end of every hold before the second
/// reading: it is the one place a fact about the history of the state's
/// holds, rather than about one instant of it, is recorded.
pub(super) trait Stanced {
    /// What a waiter is woken by a change of.
    type Stance: Copy + Eq;

    /// The stance the state stands at.
    fn stance(&self) -> Self::Stance;

    /// Record what this hold left, as the hold ends.
    fn end_hold(&mut self);
}

/// The lock over one entry's state, counting each time it is taken.
pub(super) struct EntryGate<T> {
    state: Mutex<T>,
    /// Times [`EntryGate::lock`] has handed out the state. Moved only by a
    /// caller that holds the gate, so a reading under the gate is stable for
    /// as long as that hold lasts.
    taken: AtomicU64,
    /// The stance's generation: moved by every hold that ended at a stance
    /// other than the one it began at. Taken under the gate or by a waiter
    /// holding nothing else.
    signal: Mutex<u64>,
    /// Notified with every move of the signal.
    moved: Condvar,
}

/// One hold of an entry gate: the state, reached through `Deref`, and the
/// stance the hold began at.
///
/// **Its drop is where the stance's signal moves.** The guard is the last
/// field to go, so the signal moves while the gate is still held, and a
/// waiter that read the generation under the gate and let the gate go cannot
/// miss a change made after it let go.
pub(super) struct GateHold<'g, T: Stanced> {
    signal: &'g Mutex<u64>,
    moved: &'g Condvar,
    opened: T::Stance,
    guard: MutexGuard<'g, T>,
}

impl<T: Stanced> Deref for GateHold<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T: Stanced> DerefMut for GateHold<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl<T: Stanced> Drop for GateHold<'_, T> {
    /// End the hold, and move the signal where the stance changed.
    ///
    /// It runs on an unwinding thread as on any other, so it reads the signal
    /// through a poison rather than panicking a second time: the signal is a
    /// counter, and a counter an unwind left behind still counts.
    fn drop(&mut self) {
        self.guard.end_hold();
        if self.guard.stance() != self.opened {
            let mut generation = self.signal.lock().unwrap_or_else(PoisonError::into_inner);
            *generation = generation.wrapping_add(1);
            drop(generation);
            self.moved.notify_all();
        }
    }
}

impl<T> EntryGate<T> {
    pub(super) fn new(state: T) -> Self {
        EntryGate {
            state: Mutex::new(state),
            taken: AtomicU64::new(0),
            signal: Mutex::new(0),
            moved: Condvar::new(),
        }
    }

    /// How many times the gate has been taken, over the entry's life.
    ///
    /// **Read it under the gate.** The mutex orders every move of the count
    /// before the hold that reads it, so two readings inside one hold are
    /// equal and a retake between them is a difference.
    pub(super) fn times_taken(&self) -> u64 {
        self.taken.load(Ordering::Relaxed)
    }

    /// Clear a poisoned gate, for a case that poisoned it on purpose.
    #[cfg(test)]
    pub(super) fn clear_poison(&self) {
        self.state.clear_poison();
    }
}

impl<T: Stanced> EntryGate<T> {
    /// Take the gate, waiting for it, and count the take.
    ///
    /// The count moves after the mutex is held, so every move of it happens
    /// inside a hold; a poisoned gate is still a gate taken, and is counted.
    pub(super) fn lock(&self) -> LockResult<GateHold<'_, T>> {
        let locked = self.state.lock();
        self.taken.fetch_add(1, Ordering::Relaxed);
        match locked {
            Ok(guard) => Ok(self.hold(guard)),
            Err(poisoned) => Err(PoisonError::new(self.hold(poisoned.into_inner()))),
        }
    }

    /// Take the gate from a drop, waiting for it, and count the take.
    ///
    /// **On an unwinding thread it reads through a poisoned gate**, because a
    /// second panic there aborts the process; on any other thread it panics on
    /// the poison as every other take of the gate does. Nothing recovers a
    /// poisoned gate, so what a drop writes through the poison is read by no
    /// later holder: reading through it is what keeps the drop from panicking,
    /// and nothing more.
    pub(super) fn lock_in_a_drop(&self) -> GateHold<'_, T> {
        if std::thread::panicking() {
            self.lock().unwrap_or_else(PoisonError::into_inner)
        } else {
            self.lock().expect("entry gate poisoned")
        }
    }

    /// Take the gate where it is free, counting the take the way
    /// [`EntryGate::lock`] does. A case's probe of whether a hold stands.
    #[cfg(test)]
    pub(super) fn try_lock(&self) -> std::sync::TryLockResult<GateHold<'_, T>> {
        use std::sync::TryLockError;
        let attempt = self.state.try_lock();
        if !matches!(attempt, Err(TryLockError::WouldBlock)) {
            self.taken.fetch_add(1, Ordering::Relaxed);
        }
        match attempt {
            Ok(guard) => Ok(self.hold(guard)),
            Err(TryLockError::Poisoned(poisoned)) => Err(TryLockError::Poisoned(PoisonError::new(
                self.hold(poisoned.into_inner()),
            ))),
            Err(TryLockError::WouldBlock) => Err(TryLockError::WouldBlock),
        }
    }

    /// A hold over `guard`, opened at the stance the state stands at now.
    fn hold<'g>(&'g self, guard: MutexGuard<'g, T>) -> GateHold<'g, T> {
        GateHold {
            signal: &self.signal,
            moved: &self.moved,
            opened: guard.stance(),
            guard,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A state whose stance is a number a case sets, and which counts the
    /// holds that ended over it.
    #[derive(Default)]
    struct Dial {
        stance: u8,
        holds_ended: u64,
    }

    impl Stanced for Dial {
        type Stance = u8;

        fn stance(&self) -> u8 {
            self.stance
        }

        fn end_hold(&mut self) {
            self.holds_ended += 1;
        }
    }

    /// **A continuous hold reads one count; a retake reads a difference.**
    /// The count is the gate's, moved by the take itself, so two readings
    /// inside one hold agree, and a hold given back and taken again between
    /// them differs by the retake. The control is the first half: a count
    /// that moved on reading would differ inside one hold too.
    #[test]
    fn a_retake_between_two_readings_is_a_difference_and_a_continuous_hold_is_none() {
        let gate = EntryGate::new(Dial::default());
        let held = gate.lock().expect("a fresh gate");
        let opening = gate.times_taken();
        assert_eq!(
            gate.times_taken(),
            opening,
            "a continuous hold read two different counts"
        );
        drop(held);
        let _retaken = gate.lock().expect("a fresh gate");
        assert_eq!(
            gate.times_taken() - opening,
            1,
            "a hold given back and taken again read no retake"
        );
    }

    /// **The signal moves with the stance and with nothing else.** A hold
    /// that writes the state without changing its stance leaves the
    /// generation where it was; a hold that ends at another stance moves it
    /// once. Every hold ends through the state's own record of it.
    #[test]
    fn a_hold_moves_the_signal_only_where_it_changed_the_stance() {
        let gate = EntryGate::new(Dial::default());
        let seen = *gate.signal.lock().expect("a fresh signal");
        gate.lock().expect("a fresh gate").stance = 0;
        assert_eq!(
            *gate.signal.lock().expect("a fresh signal"),
            seen,
            "a hold that left the stance where it was moved the signal"
        );
        gate.lock().expect("a fresh gate").stance = 1;
        assert_eq!(
            *gate.signal.lock().expect("a fresh signal"),
            seen.wrapping_add(1),
            "a hold that changed the stance did not move the signal once"
        );
        assert_eq!(gate.lock().expect("a fresh gate").holds_ended, 2);
    }
}
