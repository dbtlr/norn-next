//! The background scheduler, and the budget a maintenance leg runs under.
//!
//! **Claim-bound background work starts at a wake or a deadline, and no poll
//! tick starts it.** The scheduler is one thread per host. It sleeps until a
//! wake arrives or the earliest deadline it was handed comes, and then asks
//! every entry, under that entry's gate, whether a claim-bound slice is due.
//! It does no background work itself: where a slice is due it schedules a
//! maintenance leg under the claim's scheduled marker, or marks the entry as
//! owing maintenance where the leg cannot be scheduled now.

use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant};

use super::{Entry, EntryOps, Job, ReadStance, Shared, dispatch_pending};

/// What the scheduler has received and done, counted since the host began.
///
/// A wake is counted as it is sent, whether or not the scheduler was already
/// woken, and a deadline as the scheduler reaches it with no wake standing.
/// Each turn of the scheduler is one ask, so `asks` never exceeds
/// `wakes + deadlines`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SchedulerReading {
    /// Wakes sent to the scheduler.
    pub wakes: u64,
    /// Deadlines the scheduler reached with no wake standing.
    pub deadlines: u64,
    /// Asks the scheduler ran: each one reads every entry once.
    pub asks: u64,
}

/// The scheduler's leaf lock: the flags a wake sets, the deadline the last
/// ask left, and the counts. No thread holds it across a take of an entry
/// gate.
#[derive(Default)]
struct Signals {
    woken: bool,
    /// The earliest deadline any entry contributed at the last ask.
    earliest: Option<Instant>,
    /// The earliest retry the lane asked for since the last ask began: a
    /// store whose unit was refused or whose turn unwound, owed and with no
    /// wake coming. Kept apart from `earliest`, which each ask overwrites, so
    /// a retry asked for during an ask is not lost to it.
    retry: Option<Instant>,
    stopping: bool,
    reading: SchedulerReading,
}

/// The shared half of the background scheduler: what a wake is sent to, and
/// what the scheduler thread waits on.
#[derive(Default)]
pub(super) struct Scheduler {
    signals: Mutex<Signals>,
    ready: Condvar,
    #[cfg(test)]
    before_sleep: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Scheduler {
    fn signals(&self) -> std::sync::MutexGuard<'_, Signals> {
        self.signals.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Say that claim-bound work may be owed. The wake is contentless: the
    /// owed flags and due clocks the next ask reads say what. A wake sent while
    /// the scheduler is busy is kept, because it sets a flag rather than
    /// meeting a waiter.
    pub(super) fn wake(&self) {
        let mut signals = self.signals();
        signals.woken = true;
        signals.reading.wakes += 1;
        drop(signals);
        self.ready.notify_all();
    }

    /// Ask again at `at` where nothing asks sooner: the lane's retry of a
    /// store it left owed with no wake coming.
    pub(super) fn retry_at(&self, at: Instant) {
        let mut signals = self.signals();
        signals.retry = Some(signals.retry.map_or(at, |earlier| earlier.min(at)));
        drop(signals);
        self.ready.notify_all();
    }

    /// Stop the scheduler thread at its next turn.
    pub(super) fn stop(&self) {
        self.signals().stopping = true;
        self.ready.notify_all();
    }

    /// Wait for the next wake or the earliest deadline, and answer that the
    /// scheduler should ask, or `None` where it is stopping.
    pub(super) fn next_turn(&self) -> Option<()> {
        let mut signals = self.signals();
        loop {
            if signals.stopping {
                return None;
            }
            // Every ask asks every entry, a store the lane asked to retry
            // among them, so an ask that begins answers the retry too.
            if signals.woken {
                signals.woken = false;
                signals.earliest = None;
                signals.retry = None;
                signals.reading.asks += 1;
                return Some(());
            }
            let now = Instant::now();
            let next = match (signals.earliest, signals.retry) {
                (Some(earliest), Some(retry)) => Some(earliest.min(retry)),
                (earliest, retry) => earliest.or(retry),
            };
            match next {
                Some(at) if at <= now => {
                    signals.earliest = None;
                    signals.retry = None;
                    signals.reading.deadlines += 1;
                    signals.reading.asks += 1;
                    return Some(());
                }
                Some(at) => {
                    signals = self
                        .ready
                        .wait_timeout(signals, at - now)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
                None => {
                    signals = self
                        .ready
                        .wait(signals)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
        }
    }

    /// Record the earliest deadline the ask that just ended found, which is
    /// what the scheduler sleeps until where no wake arrives first.
    #[cfg(test)]
    pub(super) fn run_before_next_sleep(&self, hook: impl FnOnce() + Send + 'static) {
        *self.before_sleep.lock().unwrap() = Some(Box::new(hook));
    }

    pub(super) fn sleep_until(&self, earliest: Option<Instant>) {
        #[cfg(test)]
        {
            let hook = self.before_sleep.lock().unwrap().take();
            if let Some(hook) = hook {
                hook();
            }
        }
        self.signals().earliest = earliest;
    }

    pub(super) fn reading(&self) -> SchedulerReading {
        self.signals().reading
    }
}

/// Start the scheduler thread for the host `shared` is, waiting on
/// `scheduler`. It ends where the scheduler is stopped or the host is gone.
///
/// **Each turn is caught**, so an unwind out of one ends neither the thread
/// nor any later turn: nothing respawns a scheduler, as nothing respawns a
/// dispatcher. A turn that unwinds whole leaves no deadline, and the next
/// wake asks every entry again.
pub(super) fn spawn<O: EntryOps>(
    shared: Weak<Shared<O>>,
    scheduler: Arc<Scheduler>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while scheduler.next_turn().is_some() {
            let Some(shared) = shared.upgrade() else {
                break;
            };
            let earliest =
                std::panic::catch_unwind(AssertUnwindSafe(|| ask(&shared))).unwrap_or(None);
            drop(shared);
            scheduler.sleep_until(earliest);
        }
    })
}

/// One ask: read every entry's owed flags and due clocks, schedule or mark
/// where a claim-bound slice is due, and answer the earliest deadline an
/// entry contributes.
///
/// An unwind out of one entry's ask or out of the dispatch of the leg it
/// scheduled is caught there, so the entries after it are asked in this same
/// turn. An entry whose gate is poisoned — that unwind's own among them — is
/// passed by, as the dispatcher tick passes it by, so it costs no later turn
/// anything; a gate poisoned between the check and the take unwinds that one
/// entry's ask, and the next turn passes the entry by.
fn ask<O: EntryOps>(shared: &Arc<Shared<O>>) -> Option<Instant> {
    #[cfg(test)]
    if shared
        .panic_in_scheduler_ask
        .swap(false, std::sync::atomic::Ordering::SeqCst)
    {
        panic!("the scheduler's ask unwound before asking any entry");
    }
    let mut earliest: Option<Instant> = None;
    for entry in shared.entries.snapshot() {
        if entry.gate.is_poisoned() {
            continue;
        }
        let Ok(deadline) = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let (deadline, scheduled) = ask_entry(shared, &entry);
            if scheduled {
                let _ = dispatch_pending(shared, &entry);
            }
            deadline
        })) else {
            continue;
        };
        earliest = earlier(earliest, deadline);
    }
    earliest
}

/// Ask one entry, under its gate, whether a claim-bound slice is due, and
/// answer the deadline it contributes and whether a maintenance leg was
/// scheduled for it.
///
/// - An entry accounting for no coverage has no clocks running: it is
///   unmarked and contributes nothing.
/// - Where no slice is due the entry is unmarked and contributes its next
///   deadline.
/// - Where a slice is due the entry is marked and contributes no deadline,
///   so a clock past due cannot wake the scheduler again and again for as
///   long as the claim stays held. The leg is scheduled under the claim's
///   scheduled marker where the coverage is in hand, no release is in
///   flight, the claim is free, no teardown is due and no heal rung is owed,
///   as a watcher poll scheduled it before. The mark stands either way, so
///   the move that next opens the gate — the leg's own release among them —
///   wakes the scheduler, which asks again and finds the entry's next
///   deadline.
fn ask_entry<O: EntryOps>(
    shared: &Arc<Shared<O>>,
    entry: &Arc<Entry<O::Attachment>>,
) -> (Option<Instant>, bool) {
    let mut state = entry.gate.lock().expect("entry gate poisoned");
    let state = &mut *state;
    if !state.coverage.accounted_for() {
        state.claim.clear_maintenance_mark();
        state.background_due = None;
        return (None, false);
    }
    // Lane work needs no claim and no coverage in hand: it is owed wherever
    // the entry is attached and its stance feeds. A lane clock still to come
    // is a deadline; one already past is owed work, which the lane's turn
    // hands the clock's next reading back from, and over a stance that
    // refuses it waits for the wake the stance's return sends.
    // One reading of the clock, taken before the owed check: a lane clock at
    // or before it is owed at that later check too, since the clock only moves
    // on, and one after it is kept as a deadline. Two readings would drop a
    // clock that came due between them, neither owed nor a deadline.
    let now = Instant::now();
    if state.read_stance() != ReadStance::Refuse && shared.ops.background_owed(entry.name()) {
        shared.lane.enqueue(entry.name());
    }
    let lane_deadline = shared
        .ops
        .background_deadline(entry.name())
        .filter(|at| *at > now);
    if let Some(attachment) = state.coverage.held() {
        state.background_due = shared.ops.maintenance_deadline(attachment);
    }
    let due = shared.ops.maintenance_owed(entry.name())
        || state.background_due.is_some_and(|at| at <= Instant::now());
    if !due {
        state.claim.clear_maintenance_mark();
        return (earlier(state.background_due, lane_deadline), false);
    }
    state.claim.mark_owing_maintenance();
    let schedulable = state.coverage.in_hand()
        && !state.detach_in_flight
        && !state.claim.is_held()
        && !state.detach_due
        && !state.owes_a_rung();
    if schedulable {
        let name = entry.name().clone();
        state.claim.schedule(|epoch| Job::Maintenance(name, epoch));
    }
    (lane_deadline, schedulable)
}

/// The earlier of two deadlines, where either is set.
pub(super) fn earlier(one: Option<Instant>, other: Option<Instant>) -> Option<Instant> {
    match (one, other) {
        (Some(one), Some(other)) => Some(one.min(other)),
        (one, other) => one.or(other),
    }
}

/// The handle an entry sends its wakes through: one per host, cloned into
/// every entry's claim.
#[derive(Clone)]
pub(super) struct BackgroundWake(Arc<Scheduler>);

impl BackgroundWake {
    pub(super) fn to(scheduler: &Arc<Scheduler>) -> Self {
        Self(Arc::clone(scheduler))
    }

    /// A handle no scheduler thread listens on, for an entry a case builds
    /// outside a host.
    #[cfg(all(test, feature = "induced-failure"))]
    pub(super) fn unheard() -> Self {
        Self(Arc::new(Scheduler::default()))
    }

    pub(super) fn send(&self) {
        self.0.wake();
    }
}

/// The account of the maintenance legs a host ran: the legs, the units they
/// ran, and the longest leg's clock beside the most units one leg ran.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MaintenanceReading {
    /// Maintenance legs that ran their units.
    pub legs: u64,
    /// Units those legs ran.
    pub units: u64,
    /// The most units one leg ran.
    pub most_units_in_a_leg: u32,
    /// The longest one leg's units took.
    pub longest_leg: Duration,
}

impl MaintenanceReading {
    /// Record one leg's turn.
    pub(super) fn record(&mut self, turn: &MaintenanceTurn) {
        self.legs += 1;
        self.units += u64::from(turn.units);
        self.most_units_in_a_leg = self.most_units_in_a_leg.max(turn.units);
        self.longest_leg = self.longest_leg.max(turn.began.elapsed());
    }
}

/// One maintenance leg's budget: the time it may take, and the units it has
/// run, which is the logical counter its clock is paired with.
///
/// A leg checks the budget only between units and always runs at least one,
/// so it ends within its budget plus one unit, and a budget too small for any
/// unit runs exactly one unit per leg. A unit is the smallest step that is not
/// interrupted, and keeps its own work bounds.
pub struct MaintenanceTurn {
    budget: Duration,
    began: Instant,
    units: u32,
}

impl MaintenanceTurn {
    pub(crate) fn new(budget: Duration) -> Self {
        Self {
            budget,
            began: Instant::now(),
            units: 0,
        }
    }

    /// Run `unit` until it answers that it ran nothing, or the budget is spent
    /// at a unit boundary after the first unit. `unit` answers whether it ran
    /// a unit; an error ends the turn with that error.
    pub fn run_units<E>(&mut self, unit: impl FnMut() -> Result<bool, E>) -> Result<(), E> {
        let began = self.began;
        run_units_until(self.budget, &mut self.units, || began.elapsed(), unit)
    }

    /// The units this leg has run.
    pub fn units(&self) -> u32 {
        self.units
    }
}

/// [`MaintenanceTurn::run_units`] over a clock the caller reads, so a case can
/// state the boundary without a wall clock. A background turn on the lane runs
/// its units through it too, under the same budget.
pub(super) fn run_units_until<E>(
    budget: Duration,
    units: &mut u32,
    elapsed: impl Fn() -> Duration,
    mut unit: impl FnMut() -> Result<bool, E>,
) -> Result<(), E> {
    loop {
        if *units > 0 && elapsed() >= budget {
            return Ok(());
        }
        if !unit()? {
            return Ok(());
        }
        *units += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// Units of 10 ms each on a clock the units themselves move, with `owed`
    /// units available: answers how many ran and the time the turn took.
    fn turn(budget_ms: u64, owed: u32) -> (u32, Duration) {
        let clock = Cell::new(Duration::ZERO);
        let mut units = 0;
        let mut left = owed;
        run_units_until::<()>(
            Duration::from_millis(budget_ms),
            &mut units,
            || clock.get(),
            || {
                if left == 0 {
                    return Ok(false);
                }
                left -= 1;
                clock.set(clock.get() + Duration::from_millis(10));
                Ok(true)
            },
        )
        .unwrap();
        (units, clock.get())
    }

    #[test]
    fn a_turn_ends_at_the_first_unit_boundary_past_its_budget() {
        let (units, took) = turn(25, 100);
        assert_eq!(
            units, 3,
            "the turn stopped short of its budget or ran past it"
        );
        assert!(
            took <= Duration::from_millis(25 + 10),
            "the turn took {took:?}, more than its budget plus one unit"
        );
    }

    #[test]
    fn a_budget_too_small_for_any_unit_runs_exactly_one() {
        assert_eq!(turn(0, 100).0, 1);
        assert_eq!(turn(5, 100).0, 1);
    }

    #[test]
    fn a_turn_ends_where_no_unit_is_owed() {
        assert_eq!(turn(1_000, 4).0, 4);
        assert_eq!(turn(1_000, 0).0, 0);
    }

    #[test]
    fn a_unit_error_ends_the_turn_with_that_error() {
        let mut units = 0;
        let mut calls = 0;
        let ended = run_units_until(
            Duration::from_secs(1),
            &mut units,
            || Duration::ZERO,
            || {
                calls += 1;
                if calls == 2 { Err("refused") } else { Ok(true) }
            },
        );
        assert_eq!(ended, Err("refused"));
        assert_eq!((units, calls), (1, 2));
    }
}
