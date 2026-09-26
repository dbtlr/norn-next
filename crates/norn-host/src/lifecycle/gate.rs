//! The entry gate: the lock over one entry's lifecycle state, and the count of
//! every time it has been taken.
//!
//! **Every hold of an entry's state begins in [`EntryGate::lock`].** The mutex
//! is private to this module, so no lock site elsewhere can take the state
//! without moving the count, and the count moves while the taker holds the
//! gate. A holder that reads [`EntryGate::times_taken`] twice without giving
//! the gate back reads the same number both times; a holder that gave it back
//! and took it again between the two readings reads a difference of at least
//! one, its own retake. That difference is how a read attests that the
//! statement it establishes on ran under one continuous hold.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LockResult, Mutex, MutexGuard};

/// The lock over one entry's state, counting each time it is taken.
pub(super) struct EntryGate<T> {
    state: Mutex<T>,
    /// Times [`EntryGate::lock`] has handed out the state. Moved only by a
    /// caller that holds the gate, so a reading under the gate is stable for
    /// as long as that hold lasts.
    taken: AtomicU64,
}

impl<T> EntryGate<T> {
    pub(super) fn new(state: T) -> Self {
        EntryGate {
            state: Mutex::new(state),
            taken: AtomicU64::new(0),
        }
    }

    /// Take the gate, waiting for it, and count the take.
    ///
    /// The count moves after the mutex is held, so every move of it happens
    /// inside a hold; a poisoned gate is still a gate taken, and is counted.
    pub(super) fn lock(&self) -> LockResult<MutexGuard<'_, T>> {
        let locked = self.state.lock();
        self.taken.fetch_add(1, Ordering::Relaxed);
        locked
    }

    /// How many times the gate has been taken, over the entry's life.
    ///
    /// **Read it under the gate.** The mutex orders every move of the count
    /// before the hold that reads it, so two readings inside one hold are
    /// equal and a retake between them is a difference.
    pub(super) fn times_taken(&self) -> u64 {
        self.taken.load(Ordering::Relaxed)
    }

    /// Take the gate where it is free, counting the take the way
    /// [`EntryGate::lock`] does. A case's probe of whether a hold stands.
    #[cfg(test)]
    pub(super) fn try_lock(&self) -> std::sync::TryLockResult<MutexGuard<'_, T>> {
        let attempt = self.state.try_lock();
        if !matches!(attempt, Err(std::sync::TryLockError::WouldBlock)) {
            self.taken.fetch_add(1, Ordering::Relaxed);
        }
        attempt
    }

    /// Clear a poisoned gate, for a case that poisoned it on purpose.
    #[cfg(test)]
    pub(super) fn clear_poison(&self) {
        self.state.clear_poison();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A continuous hold reads one count; a retake reads a difference.**
    /// The count is the gate's, moved by the take itself, so two readings
    /// inside one hold agree, and a hold given back and taken again between
    /// them differs by the retake. The control is the first half: a count
    /// that moved on reading would differ inside one hold too.
    #[test]
    fn a_retake_between_two_readings_is_a_difference_and_a_continuous_hold_is_none() {
        let gate = EntryGate::new(());
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
}
