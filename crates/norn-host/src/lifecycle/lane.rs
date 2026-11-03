//! The background lane: the host's threads for background work that needs no
//! vault claim.
//!
//! **The lane is kept apart from the shared worker pool**, so long background
//! work never takes a slot an attach, a reconcile or an apply needs. It runs
//! background units — an engine's initialization and its feed and reconcile
//! pages today — which hold no claim and no attachment, so they run
//! alongside their vault's reconciles and applies and share only CPU with
//! them. The lane's thread count, `LifecyclePolicy::background_threads`, is
//! the host-wide background budget and caps that competition.
//!
//! **The lane serves derived stores in background turns.** The background
//! scheduler fills the lane's coalescing set with every store that owes lane
//! work and wakes a thread. A thread takes one store from the front and runs
//! one turn for it: units until the background turn budget is spent, checked
//! between units, always at least one. A store still owed when its turn ends
//! goes to the back, so a store with a large backlog takes one turn while
//! every other owed store takes one, and at most one turn runs for a store at
//! a time.
//!
//! The set is keyed by vault name. Within one host a name names exactly one
//! derived store — its derived directory is the name's — so the key is the
//! derived store's, as the read budget's is; two registrations over one root
//! are two names and two stores.
//!
//! **A turn feeds only while its entry's read stance is serve or settle.** It
//! reads the stance under the entry's gate before each unit and lets the gate
//! go before the unit runs, so a unit never holds the gate. A stance that moves
//! to refuse after that read is met at the next unit boundary; the unit in
//! flight is the accepted window, and it is one unit: the retirement rules of
//! the engine slots keep that unit from committing over a store that is
//! discarded.
//!
//! The set's lock and the reading's lock are leaves: no thread holds either
//! across a take of an entry gate or across a unit.

use std::collections::{BTreeSet, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant};

use norn_wire::VaultName;

use super::{EntryOps, ReadStance, Shared};

/// What one background unit of a derived store came to, as the ops answer it
/// to the lane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackgroundUnit {
    /// Nothing is owed: no unit ran.
    NothingOwed,
    /// One unit ran; `owed` says whether the store still owes another.
    Ran { owed: bool },
    /// One unit ran and was refused by its environment — an open failure, a
    /// busy database, descriptor exhaustion. It is not a verdict: the work
    /// stays owed, and the turn ends so the refusal is not repeated at once.
    /// The lane asks the scheduler to retry the store after
    /// [`BACKGROUND_RETRY_AFTER`], so the next ask that finds it owed starts
    /// it again although no wake comes.
    Refused,
    /// One unit ran and its read met damage in the store lifetime `epoch`. The
    /// lane hands the verdict to the entry's damage judgment, which schedules
    /// the rebuild where the entry still stands on that lifetime.
    StoreDamaged { epoch: String, detail: String },
}

/// How long a store the lane left owed waits before the scheduler asks for it
/// again where no wake comes first: after a unit its environment refused, and
/// after a turn that unwound. Long enough that a refusal or an unwind that
/// repeats costs a lane thread one unit a second, short enough that owed work
/// never waits on an unrelated wake or a maintenance deadline an hour away.
pub(crate) const BACKGROUND_RETRY_AFTER: Duration = Duration::from_secs(1);

/// What the lane's turns ran, counted since the host began: the turns, the
/// units they ran, and the longest turn's clock beside the most units one
/// turn ran.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LaneReading {
    /// Background turns that ran at least one unit.
    pub turns: u64,
    /// Units those turns ran.
    pub units: u64,
    /// The most units one turn ran.
    pub most_units_in_a_turn: u32,
    /// The longest one turn took.
    pub longest_turn: Duration,
}

/// The coalescing set of owed stores, and which of them a turn is running.
#[derive(Default)]
struct LaneSet {
    /// Owed stores in the order their turns run.
    queue: VecDeque<VaultName>,
    /// The stores in `queue`, so an owed store is queued once.
    queued: BTreeSet<VaultName>,
    /// Stores a turn is running now: at most one turn per store.
    running: BTreeSet<VaultName>,
    /// Running stores found owed again while their turn ran, queued when it
    /// ends whatever it reports.
    again: BTreeSet<VaultName>,
    stopping: bool,
}

/// The shared half of the background lane: the set the scheduler fills, and
/// what the lane's threads wait on.
#[derive(Default)]
pub(super) struct Lane {
    set: Mutex<LaneSet>,
    ready: Condvar,
    reading: Mutex<LaneReading>,
}

fn tolerant<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Lane {
    /// Say that `name`'s store owes lane work: queue it at the back of the
    /// set, once, and wake a thread. A store whose turn is running is queued
    /// again when that turn ends.
    pub(super) fn enqueue(&self, name: &VaultName) {
        let mut set = tolerant(&self.set);
        if set.stopping {
            return;
        }
        if set.running.contains(name) {
            set.again.insert(name.clone());
            return;
        }
        if set.queued.insert(name.clone()) {
            set.queue.push_back(name.clone());
            drop(set);
            self.ready.notify_one();
        }
    }

    /// Take the next owed store and mark its turn running, waiting for one,
    /// or `None` where the lane is stopping.
    fn next(&self) -> Option<VaultName> {
        let mut set = tolerant(&self.set);
        loop {
            if set.stopping {
                return None;
            }
            if let Some(name) = set.queue.pop_front() {
                set.queued.remove(&name);
                set.running.insert(name.clone());
                return Some(name);
            }
            set = self.ready.wait(set).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// End `name`'s turn, queueing the store at the back where it is still
    /// `owed` or was found owed while the turn ran.
    fn finish(&self, name: &VaultName, owed: bool) {
        let mut set = tolerant(&self.set);
        set.running.remove(name);
        let again = set.again.remove(name);
        drop(set);
        if owed || again {
            self.enqueue(name);
        }
    }

    /// Start no turn and no unit from here on, and wake every thread so it
    /// ends once the unit it has in flight returns.
    pub(super) fn stop(&self) {
        tolerant(&self.set).stopping = true;
        self.ready.notify_all();
    }

    fn stopping(&self) -> bool {
        tolerant(&self.set).stopping
    }

    fn record(&self, units: u32, took: Duration) {
        if units == 0 {
            return;
        }
        let mut reading = tolerant(&self.reading);
        reading.turns += 1;
        reading.units += u64::from(units);
        reading.most_units_in_a_turn = reading.most_units_in_a_turn.max(units);
        reading.longest_turn = reading.longest_turn.max(took);
    }

    pub(super) fn reading(&self) -> LaneReading {
        *tolerant(&self.reading)
    }
}

/// Start one lane thread for the host `shared` is. It ends where the lane is
/// stopped or the host is gone.
///
/// **Each turn is caught**, so an unwind out of a unit ends neither the thread
/// nor any later turn. The store whose turn unwound is queued again by the
/// scheduler's retry after [`BACKGROUND_RETRY_AFTER`], where it is still owed,
/// rather than at once, so a unit that unwinds every time it runs does not
/// hold a lane thread.
pub(super) fn spawn<O: EntryOps>(
    shared: Weak<Shared<O>>,
    lane: Arc<Lane>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while let Some(name) = lane.next() {
            let Some(host) = shared.upgrade() else {
                lane.finish(&name, false);
                break;
            };
            let owed = std::panic::catch_unwind(AssertUnwindSafe(|| turn(&host, &name)))
                .unwrap_or_else(|_| {
                    host.scheduler
                        .retry_at(Instant::now() + BACKGROUND_RETRY_AFTER);
                    false
                });
            drop(host);
            lane.finish(&name, owed);
        }
    })
}

/// One background turn for `name`'s store: units until the background turn
/// budget is spent, checked between units, at least one, while the entry's
/// stance feeds and the lane is not stopping. Answers whether the store still
/// owes work.
fn turn<O: EntryOps>(shared: &Arc<Shared<O>>, name: &VaultName) -> bool {
    let began = Instant::now();
    let mut owed = true;
    let mut units = 0;
    super::background::run_units_until(
        shared.background_turn_budget,
        &mut units,
        || began.elapsed(),
        || -> Result<bool, std::convert::Infallible> {
            if !owed || shared.lane.stopping() {
                return Ok(false);
            }
            if !feeds(shared, name) {
                owed = false;
                return Ok(false);
            }
            match shared.ops.background_unit(name) {
                BackgroundUnit::NothingOwed => {
                    owed = false;
                    Ok(false)
                }
                BackgroundUnit::Ran { owed: more } => {
                    owed = more;
                    Ok(true)
                }
                BackgroundUnit::Refused => {
                    owed = false;
                    shared
                        .scheduler
                        .retry_at(Instant::now() + BACKGROUND_RETRY_AFTER);
                    Ok(true)
                }
                BackgroundUnit::StoreDamaged { epoch, detail } => {
                    owed = false;
                    super::judge_damage_a_background_read_met(shared, name, &epoch, detail);
                    Ok(true)
                }
            }
        },
    )
    .unwrap_or_else(|never| match never {});
    shared.lane.record(units, began.elapsed());
    owed && !shared.lane.stopping()
}

/// Whether `name`'s entry is attached and its read stance is serve or
/// settle, read under its gate and let go before any unit runs. An entry the
/// host no longer serves, or whose gate a panic poisoned, feeds nothing.
fn feeds<O: EntryOps>(shared: &Arc<Shared<O>>, name: &VaultName) -> bool {
    let Some(entry) = shared.entries.get(name) else {
        return false;
    };
    if entry.gate.is_poisoned() {
        return false;
    }
    let state = entry.gate.lock().expect("entry gate poisoned");
    state.coverage.accounted_for() && state.read_stance() != ReadStance::Refuse
}
