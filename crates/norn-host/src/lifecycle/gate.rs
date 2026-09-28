//! The entry gate: the lock over one entry's lifecycle state, the count of
//! every time it has been taken, and the signal a waiter outside it is woken
//! by.
//!
//! **Every hold of an entry's state begins in a take of this module's** —
//! [`EntryGate::lock`], or its bounded form [`EntryGate::lock_until`] — and
//! every take counts itself. The mutex is private to this module, so no lock
//! site elsewhere can take the state without moving the count, and the count
//! moves while the taker holds the gate. A holder that reads
//! [`EntryGate::times_taken`] twice without giving the gate back reads the
//! same number both times; a holder that gave it back and took it again
//! between the two readings reads a difference of at least one, its own
//! retake. That difference is how a read attests that the statement it
//! establishes on ran under one continuous hold.
//!
//! **Every hold ends in [`GateHold`]'s drop, and that is where the signal
//! moves.** A hold reads the state's [`Stanced::stance`] as it is taken and
//! again as it ends, and where the two differ it moves the gate's signal and
//! wakes every waiter on it. The signal therefore moves exactly when a hold
//! changed the stance, whatever that hold wrote: a publication that changes
//! only counters inside one stance wakes nobody. The lock order is the gate,
//! then the signal. A holder of the gate may take the signal; a waiter on the
//! signal holds nothing else, so no holder of the signal waits for the gate.
//!
//! **A take can be bounded, and work can be left for the next hold.**
//! [`EntryGate::lock_until`] waits for the gate no later than a deadline:
//! every hold's end, once the gate is back, wakes the takers waiting that way,
//! and a taker waiting that way holds nothing but the lock that counts it, so
//! it never waits on the gate while holding it. A caller that owes the state
//! a write and must not wait for the gate — a demand lease going back from its
//! drop, whatever thread drops it — leaves it with
//! [`EntryGate::run_under_the_next_hold`], and the next take of the gate runs
//! it before its taker reads the state.

use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, LockResult, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::Instant;

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
    /// The takers waiting for the gate by a deadline, and how many holds have
    /// ended while any of them waited.
    release: Mutex<Released>,
    /// Notified as a hold ends, once the gate is back, where a taker waits by
    /// a deadline.
    freed: Condvar,
    /// Writes a caller that could not wait for the gate left for the next
    /// hold, run by that hold before its taker reads the state.
    deferred: Mutex<Vec<Deferred<T>>>,
    /// Whether `deferred` holds work, so a take that finds none reads it
    /// without taking that lock.
    has_deferred: AtomicBool,
    /// A case's hook, run once by the next read acquisition that lets this
    /// gate go, in the instant after the guard goes back: the window a change
    /// published there has to wake the read from.
    #[cfg(test)]
    let_go_hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

/// A write left for the next hold of a gate.
type Deferred<T> = Box<dyn FnOnce(&mut T) + Send>;

/// The takers waiting for a gate by a deadline, and the holds that ended
/// while any did.
#[derive(Default)]
struct Released {
    waiting: usize,
    ended: u64,
}

/// One hold of an entry gate: the state, reached through `Deref`, and the
/// stance the hold began at.
///
/// **Its drop is where the stance's signal moves.** The guard goes after the
/// drop's own body, so the signal moves while the gate is still held, and a
/// waiter that read the generation under the gate and let the gate go cannot
/// miss a change made after it let go. The takers waiting by a deadline are
/// woken by the field after the guard, once the gate is back.
pub(super) struct GateHold<'g, T: Stanced> {
    signal: &'g Mutex<u64>,
    moved: &'g Condvar,
    opened: T::Stance,
    guard: MutexGuard<'g, T>,
    /// Declared after the guard, so it drops once the gate is back. Nothing
    /// reads it: its drop is its whole work.
    _freed: FreedOnDrop<'g>,
}

/// What wakes the takers waiting for a gate by a deadline, as a hold's last
/// field to drop.
struct FreedOnDrop<'g> {
    release: &'g Mutex<Released>,
    freed: &'g Condvar,
}

impl Drop for FreedOnDrop<'_> {
    /// Wake every taker waiting by a deadline, where one waits.
    ///
    /// It takes the lock that counts those takers, and a taker counts itself
    /// under that lock before it tries the gate, so a hold that ends after a
    /// taker found the gate taken finds that taker counted and wakes it. It
    /// reads through a poison, because it runs on an unwinding thread too.
    fn drop(&mut self) {
        let mut released = self.release.lock().unwrap_or_else(PoisonError::into_inner);
        if released.waiting > 0 {
            released.ended = released.ended.wrapping_add(1);
            drop(released);
            self.freed.notify_all();
        }
    }
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
            release: Mutex::new(Released::default()),
            freed: Condvar::new(),
            deferred: Mutex::new(Vec::new()),
            has_deferred: AtomicBool::new(false),
            #[cfg(test)]
            let_go_hook: Mutex::new(None),
        }
    }

    /// Run `hook` once, in the instant after the next read acquisition lets
    /// this gate go.
    #[cfg(test)]
    pub(super) fn when_a_read_lets_go(&self, hook: impl FnOnce() + Send + 'static) {
        *self.let_go_hook.lock().expect("let-go hook poisoned") = Some(Box::new(hook));
    }

    /// Run the hook a case set, where it set one. The caller holds no hold of
    /// this gate.
    #[cfg(test)]
    pub(super) fn run_the_let_go_hook(&self) {
        let hook = self
            .let_go_hook
            .lock()
            .expect("let-go hook poisoned")
            .take();
        if let Some(hook) = hook {
            hook();
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

    /// The stance's generation as it stands.
    ///
    /// **Read it under the gate**, and pass it to
    /// [`EntryGate::wait_for_the_stance_to_move`] once the gate is given back:
    /// every hold that changes the stance moves the generation before it
    /// gives the gate back, so a reading taken under the gate is behind every
    /// change made after that hold.
    pub(super) fn stance_generation(&self) -> u64 {
        *self.signal.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Wait until the stance's generation moves past `seen`, or until
    /// `deadline`, and answer whether it moved.
    ///
    /// **The caller holds no hold of this gate.** The wait holds the signal
    /// alone, which no holder of the signal waits on the gate while holding,
    /// so a holder of the gate that moves the signal is never waiting on this
    /// wait. A deadline already past answers at once.
    pub(super) fn wait_for_the_stance_to_move(&self, seen: u64, deadline: Instant) -> bool {
        let generation = self.signal.lock().unwrap_or_else(PoisonError::into_inner);
        let remaining = deadline.saturating_duration_since(Instant::now());
        let (generation, _) = self
            .moved
            .wait_timeout_while(generation, remaining, |generation| *generation == seen)
            .unwrap_or_else(PoisonError::into_inner);
        *generation != seen
    }

    /// Whether a hold unwound while it stood, poisoning the gate.
    ///
    /// Asked without taking the gate, so it counts no take and runs none of
    /// the writes left for the next hold. A gate poisoned after this answers
    /// is still poisoned at the take that follows, and that take answers the
    /// poison itself.
    pub(super) fn is_poisoned(&self) -> bool {
        self.state.is_poisoned()
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

    /// Take the gate, waiting for it no later than `deadline`, and count the
    /// take; or, where the deadline passes first, take nothing and answer
    /// nothing.
    ///
    /// The gate is tried once more whatever the clock says, so a caller that
    /// arrives at its deadline takes a free gate rather than being turned
    /// away from it. **The wait holds no hold of the gate**: it holds the
    /// lock that counts the takers waiting by a deadline, and tries the gate
    /// without blocking under it, and a hold's end wakes it once the gate is
    /// back.
    pub(super) fn lock_until(&self, deadline: Instant) -> Option<LockResult<GateHold<'_, T>>> {
        let mut released = self.release.lock().unwrap_or_else(PoisonError::into_inner);
        released.waiting += 1;
        let taken = loop {
            match self.state.try_lock() {
                Ok(guard) => break Some(Ok(guard)),
                Err(TryLockError::Poisoned(poisoned)) => break Some(Err(poisoned.into_inner())),
                Err(TryLockError::WouldBlock) => {}
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break None;
            }
            let seen = released.ended;
            released = self
                .freed
                .wait_timeout_while(released, remaining, |released| released.ended == seen)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        };
        released.waiting -= 1;
        drop(released);
        let taken = taken?;
        self.taken.fetch_add(1, Ordering::Relaxed);
        Some(match taken {
            Ok(guard) => Ok(self.hold(guard)),
            Err(guard) => Err(PoisonError::new(self.hold(guard))),
        })
    }

    /// Leave `work` for the next hold of the gate, which runs it before its
    /// taker reads the state; and run it now, under a hold of its own, where
    /// the gate is free.
    ///
    /// **It never waits for the gate**, so a caller can make a write it owes
    /// from anywhere: from a drop, from a thread a deadline turned away from
    /// the gate, or from a thread that holds the gate itself. The write lands
    /// with whichever hold comes next rather than holding that caller until
    /// the gate is free, and every reader of the state is a later hold, so no
    /// reader misses it. A poisoned gate is read through, as a drop must.
    pub(super) fn run_under_the_next_hold(&self, work: impl FnOnce(&mut T) + Send + 'static) {
        {
            let mut deferred = self.deferred.lock().unwrap_or_else(PoisonError::into_inner);
            deferred.push(Box::new(work));
            self.has_deferred.store(true, Ordering::Release);
        }
        let free = match self.state.try_lock() {
            Ok(guard) => Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        };
        if let Some(guard) = free {
            self.taken.fetch_add(1, Ordering::Relaxed);
            drop(self.hold(guard));
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

    /// A hold over `guard`, opened at the stance the state stands at now, with
    /// the writes left for it run.
    ///
    /// **Every take of the gate comes through here**, so a write left by
    /// [`EntryGate::run_under_the_next_hold`] lands before any taker reads
    /// the state. It runs inside the hold, so a stance it changes moves the
    /// signal where the hold ends, as any other write does.
    fn hold<'g>(&'g self, guard: MutexGuard<'g, T>) -> GateHold<'g, T> {
        let mut hold = GateHold {
            signal: &self.signal,
            moved: &self.moved,
            opened: guard.stance(),
            guard,
            _freed: FreedOnDrop {
                release: &self.release,
                freed: &self.freed,
            },
        };
        if self.has_deferred.load(Ordering::Acquire) {
            let work = {
                let mut deferred = self.deferred.lock().unwrap_or_else(PoisonError::into_inner);
                self.has_deferred.store(false, Ordering::Release);
                std::mem::take(&mut *deferred)
            };
            for write in work {
                write(&mut *hold);
            }
        }
        hold
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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

    /// **A hold that unwinds still moves the signal.** The hold changes the
    /// stance and then unwinds; its end runs on the unwinding thread as on
    /// any other, so a waiter is woken by a change an unwind left behind.
    #[test]
    fn a_hold_that_unwinds_after_changing_the_stance_moves_the_signal() {
        let gate = EntryGate::new(Dial::default());
        let seen = gate.stance_generation();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut held = gate.lock().expect("a fresh gate");
            held.stance = 1;
            panic!("the hold unwinds after changing the stance");
        }));
        assert!(unwound.is_err(), "the hold did not unwind");
        assert_eq!(
            gate.stance_generation(),
            seen.wrapping_add(1),
            "a hold that unwound after changing the stance did not move the signal"
        );
    }

    /// **The signal moves with the stance and with nothing else.** A hold
    /// that writes the state without changing its stance leaves the
    /// generation where it was; a hold that ends at another stance moves it
    /// once. Every hold ends through the state's own record of it.
    #[test]
    fn a_hold_moves_the_signal_only_where_it_changed_the_stance() {
        let gate = EntryGate::new(Dial::default());
        let seen = gate.stance_generation();
        gate.lock().expect("a fresh gate").stance = 0;
        assert_eq!(
            gate.stance_generation(),
            seen,
            "a hold that left the stance where it was moved the signal"
        );
        gate.lock().expect("a fresh gate").stance = 1;
        assert_eq!(
            gate.stance_generation(),
            seen.wrapping_add(1),
            "a hold that changed the stance did not move the signal once"
        );
        assert_eq!(gate.lock().expect("a fresh gate").holds_ended, 2);
    }

    /// **A bounded take ends at its deadline, and is woken by a hold's end
    /// before it.** A take whose deadline passes while the gate is held
    /// answers nothing; a take with a minute left is woken once the holder
    /// gives the gate back, and takes it.
    #[test]
    fn a_bounded_take_ends_at_its_deadline_and_is_woken_by_a_holds_end() {
        let gate = EntryGate::new(Dial::default());
        let held = gate.lock().expect("a fresh gate");
        std::thread::scope(|scope| {
            assert!(
                scope
                    .spawn(|| gate
                        .lock_until(Instant::now() + Duration::from_millis(10))
                        .is_none())
                    .join()
                    .expect("the bounded take"),
                "a take bounded by a passed deadline took a held gate"
            );
            let taker = scope.spawn(|| {
                let started = Instant::now();
                let taken = gate
                    .lock_until(Instant::now() + Duration::from_secs(60))
                    .is_some();
                (taken, started.elapsed())
            });
            // The taker is given time to be waiting before the hold ends, so
            // the end is what wakes it.
            std::thread::sleep(Duration::from_millis(100));
            drop(held);
            let (taken, waited) = taker.join().expect("the bounded take");
            assert!(taken, "a bounded take found no gate once the hold ended");
            assert!(
                waited < Duration::from_secs(30),
                "the bounded take waited out its deadline rather than being woken"
            );
        });
    }

    /// **A write left for the next hold lands before that hold's taker reads
    /// the state**, and a write left over a free gate lands at once.
    #[test]
    fn a_write_left_for_the_next_hold_lands_before_its_taker_reads() {
        let gate = EntryGate::new(Dial::default());
        let held = gate.lock().expect("a fresh gate");
        gate.run_under_the_next_hold(|dial| dial.stance = 3);
        assert_eq!(
            held.stance, 0,
            "a write left for the next hold ran under this one"
        );
        drop(held);
        assert_eq!(gate.lock().expect("a fresh gate").stance, 3);
        // Two holds have ended; a write over a free gate ends a third of its
        // own, which the reading hold below counts before its own end.
        gate.run_under_the_next_hold(|dial| dial.stance = 4);
        let reading = gate.lock().expect("a fresh gate");
        assert_eq!(reading.stance, 4);
        assert_eq!(
            reading.holds_ended, 3,
            "a write over a free gate ran under no hold of its own"
        );
    }

    /// **A waiter is woken by the change and not by the deadline.** The
    /// deadline here is a minute out, so a wait that answers at all inside
    /// the case was woken; the control is the wait that met no change, which
    /// answers at its own deadline with nothing moved.
    #[test]
    fn a_waiter_outside_the_gate_is_woken_by_a_change_of_stance() {
        let gate = EntryGate::new(Dial::default());
        let seen = gate.stance_generation();
        assert!(
            !gate.wait_for_the_stance_to_move(seen, Instant::now() + Duration::from_millis(10)),
            "a wait that met no change answered that the stance moved"
        );
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                gate.wait_for_the_stance_to_move(seen, Instant::now() + Duration::from_secs(60))
            });
            // The waiter is given time to be waiting before the change, so the
            // change is what wakes it rather than a generation already moved.
            std::thread::sleep(Duration::from_millis(100));
            let started = Instant::now();
            gate.lock().expect("a fresh gate").stance = 1;
            assert!(
                waiter.join().expect("the waiter"),
                "the waiter answered without the move"
            );
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "the waiter waited out its deadline rather than being woken"
            );
        });
    }
}
