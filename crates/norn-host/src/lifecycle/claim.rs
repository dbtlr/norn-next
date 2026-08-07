//! The hold an entry keeps on itself.
//!
//! The epoch, the scheduling gate, the leg running against the entry, the queue
//! slot and the coverage the entry's work runs against are one another's
//! invariants, and the fields carrying them are private to this module: every
//! move any of them makes is a method here, so a caller can neither write a
//! state no move produces nor read one out of the shape it is stored in.

use super::Job;

/// An entry's coverage, and who holds it.
///
/// Coverage is the attachment a vault is served from: the watcher, the store
/// and the maintainer lock. One holder has it at a time — the entry itself, or
/// a leg running against the entry — and this says which, so custody is read
/// rather than inferred from an empty attachment beside a registered leg.
///
/// A leg is registered against the entry from the instant it begins and holds
/// the coverage only from the instant it takes it, so a leg standing over
/// parked coverage is a state of its own rather than a contradiction.
///
/// The custody below is reached through the moves alone, and every move names
/// the leg making it: a leg takes what the entry holds and none other, parks
/// back what it took, and ends the coverage it took alone.
pub(super) struct Coverage<A>(Custody<A>);

/// Who holds an entry's coverage.
enum Custody<A> {
    /// The entry has none.
    None,
    /// The entry holds its own coverage, and any work that runs against the
    /// entry takes it from here.
    Parked(A),
    /// The leg at this epoch holds the entry's coverage. What becomes of it —
    /// parked back, handed to a release, or given to
    /// [`EntryOps::detach`](super::EntryOps::detach) — is that leg's to say
    /// until it ends.
    ///
    /// A leg here is whatever the entry handed its coverage to at that epoch:
    /// the claim registered in [`Claim::leg`], or a release the entry began
    /// itself, which registers no claim. One of them holds it at a time —
    /// [`Coverage::take`] takes from [`Custody::Parked`] alone, so coverage
    /// already out with a leg is taken by no other, whatever epoch that other
    /// stands at.
    OnLeg(u64),
}

impl<A> Coverage<A> {
    /// An entry accounting for no coverage: none in its own hand, and none out
    /// with a leg.
    pub(super) fn none() -> Self {
        Self(Custody::None)
    }

    /// Whether the entry itself holds its coverage. Work runs against coverage
    /// in the entry's own hand alone: coverage a leg holds is that leg's until
    /// it ends, and an entry with none is one an attach is what serves.
    pub(super) fn in_hand(&self) -> bool {
        matches!(self.0, Custody::Parked(_))
    }

    /// Take the coverage the entry holds, for the leg at this epoch. An entry
    /// holding none gives none, and coverage already out with a leg is taken by
    /// no other.
    pub(super) fn take(&mut self, leg: u64) -> Option<A> {
        match std::mem::replace(&mut self.0, Custody::OnLeg(leg)) {
            Custody::Parked(coverage) => Some(coverage),
            held => {
                self.0 = held;
                None
            }
        }
    }

    /// Take the coverage the entry holds and give it up: the caller returns it
    /// to [`EntryOps::detach`](super::EntryOps::detach), and the entry accounts
    /// for none from here.
    ///
    /// Coverage out with a leg is not the caller's to give up, and the record
    /// of the leg holding it stands: that leg is what ends it.
    pub(super) fn give_up(&mut self) -> Option<A> {
        match std::mem::replace(&mut self.0, Custody::None) {
            Custody::Parked(coverage) => Some(coverage),
            held => {
                self.0 = held;
                None
            }
        }
    }

    /// Park the coverage this leg took back in the entry, which holds it from
    /// here.
    ///
    /// A leg parks back what it took: coverage another leg holds is that leg's
    /// until it ends, and parking over it would leave the entry holding
    /// coverage with a leg still out with the entry's own.
    pub(super) fn park_by(&mut self, leg: u64, coverage: A) {
        debug_assert!(
            matches!(self.0, Custody::OnLeg(held) if held == leg)
                || matches!(self.0, Custody::None),
            "coverage parked by a leg that is not what holds it"
        );
        self.0 = Custody::Parked(coverage);
    }

    /// Put coverage an attach installed into an entry that holds none. What an
    /// attach installs is coverage the entry never had, so this takes nothing
    /// out of any leg's hands.
    pub(super) fn install(&mut self, coverage: A) {
        debug_assert!(
            matches!(self.0, Custody::None),
            "coverage installed over coverage the entry accounts for"
        );
        self.0 = Custody::Parked(coverage);
    }

    /// The coverage this leg took has gone where the leg sent it, so the entry
    /// holds none. Coverage the leg parked back, or that another leg has since
    /// taken, is untouched: what a leg gave back is not the leg's to end.
    pub(super) fn released_by(&mut self, leg: u64) {
        if matches!(self.0, Custody::OnLeg(held) if held == leg) {
            self.0 = Custody::None;
        }
    }
}

/// What holds an entry's scheduling gate.
///
/// The gate is what every producer of work tests before it schedules against an
/// entry: an entry whose gate is taken already owes the work whatever holds it
/// is doing. A job scheduled against the entry holds the gate itself, so a
/// marker can never stand over an open gate — that is a job no dispatcher tick
/// would ever reach.
enum Gate {
    /// Nothing holds the entry: a claim may take it, and work may be scheduled
    /// against it.
    Open,
    /// A claim holds the entry with no job standing behind it, and says which
    /// claim it is.
    Held(Holder),
    /// The next job the entry owes, waiting on a dispatcher tick to send it.
    Scheduled(Job),
}

/// A claim running against an entry.
///
/// A watcher poll and a job leg both take the entry and both give it back, and
/// they end at different sites: a poll ends where its own tick ends it, a job
/// leg where the epilogue that dispatched it does. The kind is carried here so
/// neither ends the other, at the epoch they may both stand at.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Leg {
    /// A watcher poll, at the epoch the entry stood at when it took it.
    Poll(u64),
    /// A job leg, at the epoch its job carries.
    Job(u64),
}

impl Leg {
    /// The epoch the leg was taken at.
    fn epoch(self) -> u64 {
        match self {
            Self::Poll(epoch) | Self::Job(epoch) => epoch,
        }
    }
}

/// What holds an entry's gate where no job is scheduled against it.
///
/// A claim writes this from the leg it has registered — [`Claim::holder`] is
/// where every write of it comes from — so the gate names the leg running
/// against the entry, and names a job sent into the channel where none is.
#[derive(Clone, Copy)]
enum Holder {
    /// The leg running against the entry.
    Running(Leg),
    /// A job the entry has handed to the job channel, which the queue slot
    /// names and no leg has begun.
    Sent,
}

impl From<Leg> for Holder {
    fn from(leg: Leg) -> Self {
        Self::Running(leg)
    }
}

/// The one job an entry is waiting on in the job channel.
///
/// The slot names its occupant, so only that job's own arrival gives it back:
/// an arrival at a superseded epoch answers for itself alone, and a slot freed
/// under the job that replaced it is that job sent into the channel a second
/// time. Every move the slot makes names the job making it, so a job can neither
/// free a slot it does not hold nor read one it does.
#[derive(Default)]
struct Slot(Option<u64>);

impl Slot {
    /// Whether a job is waiting in the slot.
    fn taken(&self) -> bool {
        self.0.is_some()
    }

    /// The job waiting in the slot, where one is waiting in it. Nothing the
    /// entry does turns on which job that is — every move the slot makes names
    /// the job making it — so this reads the slot for tests alone.
    #[cfg(test)]
    fn job(&self) -> Option<u64> {
        self.0
    }

    /// Whether this job is the one waiting in the slot.
    fn holds(&self, job: u64) -> bool {
        self.0 == Some(job)
    }

    /// Take the slot for this job, out from under whatever holds it. The caller
    /// sends this job under the same lock, so a slot it takes over names work
    /// the entry has moved on from: that job's arrival answers for itself alone,
    /// and the slot it no longer holds is one it cannot free.
    fn take(&mut self, job: u64) {
        self.0 = Some(job);
    }

    /// Give back the slot a send took, where this job is what holds it. A slot
    /// standing for a job no channel holds is an entry every later dispatch
    /// refuses; a slot standing for another job is that job's own occupancy, and
    /// taking it away is what sends that job twice.
    fn free(&mut self, job: u64) {
        if self.holds(job) {
            self.0 = None;
        }
    }

    /// Stop waiting on the job holding the slot. What that job's arrival finds
    /// is an entry that has moved past it, and the work that replaced it is not
    /// held behind a job with nothing left to do.
    fn abandon(&mut self) {
        self.0 = None;
    }
}

/// An entry's claim on itself.
///
/// The epoch is what every leg carries and rechecks: an entry that has moved on
/// from a leg's epoch has superseded the work that leg was scheduled for, and
/// the leg answers for itself alone from there. The gate, the running leg and
/// the queue slot are all named by epochs raised under it.
pub(super) struct Claim {
    epoch: u64,
    gate: Gate,
    /// The leg running against the entry, where one is running.
    ///
    /// The leg outlives the gate it took: a leg that gives the gate back before
    /// it ends is still the leg running, and the work it is about to hand on is
    /// dispatched by it alone. [`Claim::take_slot_for_marked`] is what that
    /// outliving is for — a dispatcher tick never sends a job the running leg
    /// is about to send itself.
    leg: Option<Leg>,
    slot: Slot,
}

impl Default for Claim {
    fn default() -> Self {
        Self {
            epoch: 0,
            gate: Gate::Open,
            leg: None,
            slot: Slot::default(),
        }
    }
}

impl Claim {
    pub(super) fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Whether the entry still stands at the epoch a leg carries.
    pub(super) fn stands_at(&self, epoch: u64) -> bool {
        self.epoch == epoch
    }

    /// Move the entry on from the work it stands at. Every leg carrying an
    /// earlier epoch answers for itself alone from here.
    pub(super) fn supersede(&mut self) -> u64 {
        self.epoch += 1;
        self.epoch
    }

    /// Whether anything holds the gate: a claim under way, or a job scheduled
    /// against the entry.
    pub(super) fn is_held(&self) -> bool {
        !matches!(self.gate, Gate::Open)
    }

    /// The job the entry has scheduled and not yet handed to the channel.
    pub(super) fn marker(&self) -> Option<&Job> {
        match &self.gate {
            Gate::Scheduled(job) => Some(job),
            Gate::Open | Gate::Held(_) => None,
        }
    }

    /// Take the gate for the work the entry has just taken on, leaving a job
    /// already scheduled behind it holding the gate in its own right.
    ///
    /// The leg running against the entry is what holds the gate, and where none
    /// is running the job the entry is sending holds it: this is reached from
    /// [`Claim::hand_on`] alone, and what a hand-on sends is in the channel
    /// before the entry's lock goes back.
    fn hold(&mut self) {
        match self.gate {
            // A marker holds the gate in its own right.
            Gate::Scheduled(_) => {}
            // The leg running against the entry keeps the gate across the move
            // it is making.
            Gate::Held(Holder::Running(leg)) if self.leg == Some(leg) => {}
            Gate::Held(_) | Gate::Open => self.gate = Gate::Held(self.holder()),
        }
    }

    /// What holds the gate where this claim is what takes it: the leg
    /// registered against the entry, and where none is, the job the entry is
    /// sending into the channel.
    fn holder(&self) -> Holder {
        self.leg.map_or(Holder::Sent, Holder::from)
    }

    /// Schedule the entry's next job: the entry moves on to it, and its marker
    /// holds the gate until a dispatcher tick sends it.
    pub(super) fn schedule(&mut self, next: impl FnOnce(u64) -> Job) -> Job {
        let job = next(self.supersede());
        self.gate = Gate::Scheduled(job.clone());
        job
    }

    /// Move the entry on to the next job its running leg sends itself. The leg
    /// holds the gate across the move, so nothing claims the entry between the
    /// work it moves on from and the work it moves on to, and the job it hands
    /// on gets no marker of its own: the leg sends it, and a marker beside a
    /// job already sent is one a dispatcher tick sends a second time.
    ///
    /// A marker left standing from before the move keeps the gate — it names
    /// work raised under the epoch this move supersedes, and [`hand_off`] drops
    /// only a marker naming the job going into the channel.
    ///
    /// [`hand_off`]: Claim::hand_off
    pub(super) fn hand_on(&mut self, next: impl FnOnce(u64) -> Job) -> Job {
        let job = next(self.supersede());
        self.hold();
        job
    }

    /// Put the gate back, and the job scheduled against the entry with it: work
    /// nothing is left holding the entry for is work no dispatch reaches.
    pub(super) fn open(&mut self) {
        self.gate = Gate::Open;
    }

    /// Put back a claim on the gate, leaving a job scheduled against the entry
    /// holding it. The gate stands for a claim and for scheduled work alike, so
    /// a claim gives it back only where nothing is scheduled.
    pub(super) fn release(&mut self) {
        if !matches!(self.gate, Gate::Scheduled(_)) {
            self.gate = Gate::Open;
        }
    }

    /// Give up the job scheduled against the entry, leaving the gate to the leg
    /// running against it and open where none is running.
    pub(super) fn drop_marker(&mut self) {
        if matches!(self.gate, Gate::Scheduled(_)) {
            self.gate = self.leg.map_or(Gate::Open, |leg| Gate::Held(leg.into()));
        }
    }

    /// Record the job a claim took the entry from under, where the entry has
    /// nothing else scheduled. The marker holds the gate, so the job the claim
    /// did not take away is one a later tick still reaches.
    ///
    /// A job already scheduled against the entry is the newer of the two and
    /// keeps the gate: the claim is putting back work it interrupted, not work
    /// the entry has since moved on to.
    pub(super) fn restore(&mut self, job: Job) {
        if !matches!(self.gate, Gate::Scheduled(_)) {
            self.gate = Gate::Scheduled(job);
        }
    }

    /// Schedule a job against the entry as it stands, without moving it on.
    ///
    /// Whatever holds the gate gives way to this job, a marker included: the
    /// caller is naming the work the entry owes at the epoch it stands at, and
    /// a marker it displaces is work raised under an epoch the entry has left.
    pub(super) fn mark(&mut self, job: Job) {
        self.gate = Gate::Scheduled(job);
    }

    /// Move the entry on from work an invalidation supersedes. The job it had
    /// scheduled and the slot that job was waiting in go with the epoch they
    /// were raised under; whether the gate itself goes back is the caller's,
    /// because a leg still running is holding it.
    pub(super) fn invalidate(&mut self) {
        self.supersede();
        self.abandon_slot();
        self.drop_marker();
    }

    /// Stop waiting on the job holding the queue slot.
    fn abandon_slot(&mut self) {
        self.slot.abandon();
    }

    /// The leg running against the entry, where one is running.
    pub(super) fn leg(&self) -> Option<Leg> {
        self.leg
    }

    /// Take the entry for a watcher poll: the poll holds the gate and is the leg
    /// running against the entry until its own tick ends it.
    pub(super) fn begin_poll(&mut self, epoch: u64) {
        let leg = Leg::Poll(epoch);
        self.leg = Some(leg);
        self.gate = Gate::Held(leg.into());
    }

    /// Take the entry for the job leg a worker has begun. The marker the job
    /// stood behind goes with the start: the job is off the channel, and a
    /// marker beside a job already running is one a dispatcher tick sends a
    /// second time.
    pub(super) fn begin_job_leg(&mut self, epoch: u64) {
        let leg = Leg::Job(epoch);
        self.leg = Some(leg);
        self.gate = Gate::Held(leg.into());
    }

    /// End the claim a watcher poll holds on the entry, where the poll is still
    /// the leg running. A job leg standing at the same epoch is another claim,
    /// and a poll ends its own alone.
    ///
    /// The gate stands for a claim on the entry and for work scheduled against
    /// it alike, so a claim gives the gate back only where nothing is scheduled:
    /// a marker standing holds the gate in its own right.
    pub(super) fn end_poll(&mut self, epoch: u64) {
        if self.leg == Some(Leg::Poll(epoch)) {
            self.leg = None;
        }
        self.release();
    }

    /// End a job leg's hold on the entry, where it is still the leg running. A
    /// leg that handed the entry on is no longer the one running, and what runs
    /// then is that job's own leg.
    pub(super) fn end_job_leg(&mut self, epoch: u64) -> bool {
        if self.leg == Some(Leg::Job(epoch)) {
            self.leg = None;
            true
        } else {
            false
        }
    }

    /// End whichever leg is running against the entry at this epoch. The release
    /// every teardown reaches is the one completion a poll claim and a job leg
    /// come to alike, and it ends the leg it was handed the resources by.
    pub(super) fn end_leg(&mut self, epoch: u64) {
        if self.leg.map(Leg::epoch) == Some(epoch) {
            self.leg = None;
        }
    }

    /// The job waiting in the entry's queue slot, where a job is waiting in it.
    #[cfg(test)]
    pub(super) fn slot(&self) -> Option<u64> {
        self.slot.job()
    }

    /// Take the queue slot for this job, out from under whatever holds it.
    pub(super) fn take_slot(&mut self, epoch: u64) {
        self.slot.take(epoch);
    }

    /// Give back the queue slot a send took, where the entry still holds it for
    /// that send.
    pub(super) fn free_slot(&mut self, epoch: u64) {
        self.slot.free(epoch);
    }

    /// Take the queue slot for the job the entry has scheduled, where no job
    /// holds the slot and no leg is running. The slot is taken under the same
    /// lock that reads the marker, so the job in the channel and the slot naming
    /// it are installed together.
    pub(super) fn take_slot_for_marked(&mut self) -> Option<Job> {
        if self.slot.taken() || self.leg().is_some() {
            return None;
        }
        let job = self.marker()?.clone();
        self.take_slot(job.epoch());
        Some(job)
    }

    /// Stand the entry at an epoch of the caller's choosing. Tests reach states
    /// a race window opens and a deterministic run cannot.
    #[cfg(test)]
    pub(super) fn stand_at(&mut self, epoch: u64) {
        self.epoch = epoch;
    }

    /// Hand the entry the next job a job leg owes it. The leg's claim ends where
    /// the job enters the channel, and the job takes the queue slot under the
    /// same lock, so no producer takes it for newer work in the instant between.
    ///
    /// The job itself holds the gate from here. The marker it stood behind goes
    /// with the send — a marker beside a job already sent is one a dispatcher
    /// tick sends a second time, under the same epoch — while a marker at
    /// another epoch is newer work, and keeps the gate for the tick that sends
    /// it.
    pub(super) fn hand_off(&mut self, leg: u64, job: &Job) {
        self.end_job_leg(leg);
        let holder = self.holder();
        match self.gate {
            Gate::Held(_) => self.gate = Gate::Held(holder),
            Gate::Scheduled(ref marker) if marker.epoch() == job.epoch() => {
                self.gate = Gate::Held(holder);
            }
            Gate::Open | Gate::Scheduled(_) => {}
        }
        self.take_slot(job.epoch());
    }
}
