//! Waiting for a condition to hold, under a bound the call site declared.
//!
//! [`wait_until`] polls a condition until it holds or its budget runs out.
//! It is the sibling of the wait in [`crate::process`]: that one bounds a
//! child process's run, and this one bounds an in-process wait for state to
//! converge.
//!
//! # The budget is always declared
//!
//! There is no ambient default. [`Budget`] has no `Default`, and no constant
//! here supplies one: every wait names its bound at the call site, or takes
//! it from a [`Convergence`] the suite declared and threaded in. A crate-wide
//! default is the shape this forbids — one number governing waits nobody
//! wrote it for, which no call site can tune and every call site inherits.
//!
//! # Condition-precise, deadline-generous
//!
//! Precision belongs to the condition; slack belongs to both bounds. **A
//! bound here answers whether the state ever converged, and nothing about how
//! quickly.** A suite that wants the settle time asserts it itself, as its own
//! measurement, under the lane rules its evidence kind belongs to. A work
//! bound set near the time convergence usually takes makes machine speed a
//! test result.
//!
//! The probe bound is sized the same way, and it is the easier of the two to
//! write too tight, because it is a bar on a single sample: one evaluation
//! that meets a cold cache or a collection pause fails the whole wait as
//! [`FailureKind::ProbeOverran`], however much work bound was left. So size it
//! for the slowest evaluation the probe could plausibly make, not the typical
//! one. What it is there to catch is a probe whose cost is structurally wrong
//! — a scan where a lookup was meant — and that reads as overrun at any
//! generous bound.
//!
//! So the condition is written exactly — the precise set of paths, the exact
//! row — and both bounds are written loose.
//!
//! # The work bound is not the probe bound
//!
//! [`Budget`] carries two bounds over two different things. The **work
//! bound** is how long the condition may take to become true. The **probe
//! bound** is how long one evaluation of that condition may take. They are
//! separate because a probe that is itself slow otherwise consumes the work
//! bound and reports as a condition that never converged, which is a wrong
//! diagnosis of a real defect: [`FailureKind::ProbeOverran`] names the slow
//! probe instead.
//!
//! **The bounds observe; they do not preempt.** A probe that begins inside
//! the work bound runs to completion, and a condition it observes to hold is
//! a success however late that lands — including an evaluation that passed the
//! probe bound while it ran, because a subject that converged failing its wait
//! is a false negative no budget can settle. Both bounds are read between
//! evaluations, on an evaluation that has already returned.
//!
//! # A probe returns, and that part is the call site's
//!
//! Observing rather than preempting is what leaves this constraint with the
//! caller. [`wait_until`] runs the probe on the calling thread, so a probe
//! that never returns — a lock it never gets, a read on a pipe nothing writes
//! — hangs the wait past both of its bounds, which are only ever read after an
//! evaluation returns. That is the outcome the rest of this module rules out,
//! and here it is an accepted limitation rather than a case handled: the run
//! ends on the runner's own timeout, naming neither a bound nor a state. So a
//! probe takes a reading and returns. It does not wait inside itself, and
//! anything it calls that could block carries a bound of its own.
//!
//! # A wait that brackets other waits outlives them
//!
//! Some waits are held open on purpose. A case that blocks a subject at a gate
//! — a fake that parks its job thread until the case says go — is waiting
//! inside the subject while the case runs a sequence of waits of its own
//! outside it. Those two waits are not peers: **the held-open wait's budget
//! strictly dominates the sum of the outer waits it brackets.**
//!
//! **No nested wait shares a flat constant with a sequence it must outlive.**
//! A suite with one number for every wait puts the hold and the sequence on
//! equal budgets, and the hold then lets go partway through the sequence it was
//! opened to bracket. The subject resumes under a case that is still asserting
//! the subject is parked, and the failure lands somewhere else entirely — on
//! the next assertion, on a later case, or nowhere on a machine that happened
//! to be quick.
//!
//! [`Budget::dominating`] is how the two are related: the held-open wait
//! derives its budget from the same budget the outer waits obey, over the
//! count of them it brackets. Deriving is the point — a second independent
//! constant is the same defect with two numbers to keep in step.
//!
//! # An outcome is the condition; the batches are what a wait pumps
//!
//! [`absorb_until`] is [`wait_until`] for a subject that only makes progress
//! when something is taken off it and applied. **The unit a subject reports in
//! is not the unit an outcome arrives in**: a source is free to split one
//! change across as many reports as it likes and in any order, so a wait that
//! takes one report, applies it, and asserts the outcome after the wait is
//! asserting that the split did not happen. When it does happen the case fails
//! against a state the next report settles, which is a statement about
//! granularity wearing the costume of a defect in the subject.
//!
//! So the outcome is the condition and the reports are what the wait pumps.
//! [`Absorbed`] is what the pump has taken, for the conditions that ask about
//! the reports rather than about the subject, and it renders into every pending
//! observation so a wait that expires says what had arrived by then.
//!
//! # A failure says what it saw
//!
//! [`Observed::Pending`] carries the state that was seen, so a
//! [`WaitFailure`] names the bound it passed and the last observation
//! together. A bare timeout — a failure reporting only that time ran out — is
//! unrepresentable here, because the type does not let a condition report
//! "not yet" without saying what "not yet" looked like.
//!
//! # Unbounded waiting is not offered
//!
//! There is no wait that runs on while the condition keeps making progress.
//! A progress heartbeat trades a wrong deadline for a starvation nobody
//! diagnoses: the run ends when the runner's own timeout kills it, and that
//! failure names neither a bound nor a state. A hard bound carrying a rich
//! failure payload is what this module offers in its place, for every probe
//! that returns.

use std::collections::BTreeSet;
use std::fmt;
use std::time::{Duration, Instant};

use crate::poll;

/// What a failure reports for a pending observation that named no state.
///
/// [`Observed::Pending`] carries a state so that no failure is a bare
/// timeout. An empty string is that shape smuggled back in — a diagnostic
/// ending in a colon and nothing — so it is replaced by a phrase that at least
/// says which side failed to report.
const UNREPORTED_STATE: &str = "(no state reported)";

/// The two bounds a wait obeys.
///
/// # No ambient default
///
/// `Budget` has no `Default`. A budget is written at the call site or derived
/// from a [`Convergence`], so the number a wait obeys is one somebody chose
/// for that wait.
///
/// ```
/// use std::time::Duration;
///
/// use norn_testkit::wait::Budget;
///
/// let declared = Budget::new(Duration::from_secs(2), Duration::from_millis(200));
/// assert_eq!(declared.work(), Duration::from_secs(2));
/// assert_eq!(declared.probe(), Duration::from_millis(200));
/// ```
///
/// The default this type does not have, spelled out so that gaining one
/// fails. The example above pins the same path, so a rename breaks that one
/// rather than leaving this one passing for the wrong reason.
///
/// ```compile_fail
/// use norn_testkit::wait::Budget;
///
/// let ambient = Budget::default();
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Budget {
    work: Duration,
    probe: Duration,
}

impl Budget {
    /// A budget of `work` for the condition to become true, and `probe` for
    /// any single evaluation of it.
    pub const fn new(work: Duration, probe: Duration) -> Self {
        Budget { work, probe }
    }

    /// How long the condition has to become true.
    pub const fn work(&self) -> Duration {
        self.work
    }

    /// How long one evaluation of the condition may take.
    pub const fn probe(&self) -> Duration {
        self.probe
    }

    /// A budget that strictly dominates `waits` waits on this one.
    ///
    /// A wait held open across a sequence of other waits takes its bound from
    /// here, so the two are one number apart rather than two numbers to keep
    /// in step. `waits` of them run for at most `waits` times this work bound,
    /// and the derived bound is one whole wait wider than that — margin enough
    /// for what happens between the waits, and strict, so the hold is never
    /// the thing that expires first.
    ///
    /// The probe bound carries over unchanged: what a held-open wait brackets
    /// says how long it may wait, and nothing about how expensive one look at
    /// its own gate is.
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use norn_testkit::wait::Budget;
    ///
    /// let outer = Budget::new(Duration::from_secs(10), Duration::from_millis(250));
    /// let held_open = outer.dominating(2);
    /// assert!(held_open.work() > outer.work() * 2);
    /// assert_eq!(held_open.probe(), outer.probe());
    /// ```
    ///
    /// The multiplication saturates, so an absurd count yields an absurd bound
    /// rather than a wrapped small one — a hold that expires at once is
    /// exactly the defect this exists to rule out.
    pub fn dominating(&self, waits: u32) -> Budget {
        Budget::new(
            self.work.saturating_mul(waits.saturating_add(1)),
            self.probe,
        )
    }
}

/// A suite's convergence budget, stated as a floor plus an allowance per
/// change.
///
/// Convergence budgets derive from the changed set. A flat constant is wrong
/// in both directions at once: sized for a large churn it hides a hang on a
/// small one, and sized for a small churn it fails on a large one for no
/// reason but volume. So a suite declares the floor, the per-change
/// allowance, and the probe bound once, and each call site derives its budget
/// from the changes it actually made.
///
/// ```
/// use std::time::Duration;
///
/// use norn_testkit::wait::Convergence;
///
/// let settling = Convergence::new(
///     Duration::from_secs(2),
///     Duration::from_millis(20),
///     Duration::from_millis(200),
/// );
/// assert_eq!(settling.budget_for(0).work(), Duration::from_secs(2));
/// assert_eq!(settling.budget_for(50).work(), Duration::from_secs(3));
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Convergence {
    floor: Duration,
    per_change: Duration,
    probe: Duration,
}

impl Convergence {
    /// A budget of at least `floor`, growing by `per_change` for every change
    /// waited on, with `probe` bounding one evaluation of the condition.
    pub const fn new(floor: Duration, per_change: Duration, probe: Duration) -> Self {
        Convergence {
            floor,
            per_change,
            probe,
        }
    }

    /// The budget for converging on `changes` changes.
    ///
    /// Both the multiplication and the addition saturate, so a caller that
    /// passes an absurd count gets an absurd budget rather than a wrapped
    /// small one — a wrapped budget would turn a wide churn into a wait that
    /// fails immediately and blames the condition.
    pub fn budget_for(&self, changes: usize) -> Budget {
        let changes = u32::try_from(changes).unwrap_or(u32::MAX);
        Budget::new(
            self.floor
                .saturating_add(self.per_change.saturating_mul(changes)),
            self.probe,
        )
    }

    /// The budget for a changed set of no changes.
    pub const fn floor(&self) -> Duration {
        self.floor
    }

    /// What each change adds to the budget.
    pub const fn per_change(&self) -> Duration {
        self.per_change
    }

    /// How long one evaluation of the condition may take.
    pub const fn probe(&self) -> Duration {
        self.probe
    }
}

/// What one evaluation of the condition saw.
///
/// The `Pending` arm carries the observed state rather than a bare "no",
/// which is what makes every [`WaitFailure`] diagnostic: the last state the
/// condition reported is the state the failure names.
pub enum Observed<T> {
    /// The condition holds, and this is what the wait returns.
    Met(T),
    /// The condition does not hold yet, described as what was seen.
    Pending(String),
}

impl<T> Observed<T> {
    /// The condition does not hold yet, described as what was seen.
    ///
    /// A state worth reading is the caller's part. An empty description is the
    /// one thing the wait fixes up: a failure reports it as
    /// `(no state reported)`, so the diagnostic says a state was missing
    /// instead of trailing off after its colon.
    pub fn pending(state: impl Into<String>) -> Self {
        Observed::Pending(state.into())
    }
}

/// Which bound a wait passed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureKind {
    /// The work bound elapsed with the condition never observed to hold.
    Elapsed,
    /// One evaluation of the condition passed the probe bound, and the wait
    /// stopped there rather than spending the rest of the work bound on more
    /// evaluations that cost the same.
    ///
    /// One evaluation is the whole sample: a single slow one ends the wait,
    /// whatever the ones around it cost and whatever work bound is left. That
    /// is why the probe bound is sized for the slowest plausible evaluation
    /// rather than the typical one — this failure reads as "the probe is
    /// structurally too expensive", and a bound tight enough to be tripped by
    /// one unlucky evaluation makes it say that about a probe that is fine.
    ProbeOverran {
        /// How long that evaluation took.
        took: Duration,
    },
}

/// A wait that ended without observing its condition.
///
/// It names the bound it passed and the last state it observed. Both are
/// always present: the bound comes from the [`Budget`] the call site
/// declared, and the state from the last [`Observed::Pending`].
#[derive(Clone, Eq, PartialEq)]
pub struct WaitFailure {
    /// What was being waited for.
    pub what: String,
    /// Which bound was passed.
    pub kind: FailureKind,
    /// The budget the call site declared.
    pub budget: Budget,
    /// How long the wait ran.
    pub elapsed: Duration,
    /// How many times the condition was evaluated.
    pub probes: usize,
    /// The state the last evaluation reported.
    pub last_state: String,
}

impl fmt::Display for WaitFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let WaitFailure {
            what,
            kind,
            budget,
            elapsed,
            probes,
            last_state,
        } = self;
        match kind {
            FailureKind::Elapsed => write!(
                f,
                "waiting for {what} passed its {:?} work bound after {elapsed:?} and {probes} \
                 probes; last observed: {last_state}",
                budget.work()
            ),
            FailureKind::ProbeOverran { took } => write!(
                f,
                "waiting for {what} stopped at probe {probes}, which took {took:?} and passed its \
                 {:?} probe bound {elapsed:?} into a {:?} work bound; last observed: {last_state}",
                budget.probe(),
                budget.work()
            ),
        }
    }
}

/// The diagnostic, so that unwrapping a failed wait prints it.
impl fmt::Debug for WaitFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for WaitFailure {}

/// Poll `probe` until the condition holds, or `budget` runs out.
///
/// `what` names the condition, and appears in the failure. The probe reports
/// [`Observed::Met`] with whatever the caller wants back, or
/// [`Observed::Pending`] with the state it saw.
///
/// The condition is evaluated at least once, whatever the budget, and once
/// more at the work bound itself — so a condition that becomes true in the
/// last gap between probes is observed rather than missed. A probe that
/// begins inside the work bound is never abandoned: if it reports
/// [`Observed::Met`], the wait succeeded.
///
/// ```
/// use std::time::Duration;
///
/// use norn_testkit::wait::{Budget, Observed, wait_until};
///
/// let mut seen = 0;
/// let count = wait_until(
///     "the third look",
///     Budget::new(Duration::from_secs(5), Duration::from_millis(500)),
///     || {
///         seen += 1;
///         if seen == 3 {
///             Observed::Met(seen)
///         } else {
///             Observed::pending(format!("{seen} looks so far"))
///         }
///     },
/// )
/// .expect("the condition holds well inside the bound");
/// assert_eq!(count, 3);
/// ```
pub fn wait_until<T>(
    what: &str,
    budget: Budget,
    mut probe: impl FnMut() -> Observed<T>,
) -> Result<T, WaitFailure> {
    let started = Instant::now();
    let mut poll_wait = poll::FIRST_GAP;
    let mut probes = 0usize;
    // Written by the first pending observation, and read only after one has
    // happened: every failure path here is downstream of a probe that
    // reported a state.
    let mut last_state: String;

    loop {
        let probe_started = Instant::now();
        let observed = probe();
        let took = probe_started.elapsed();
        probes += 1;

        match observed {
            // The bounds observe and do not preempt: a condition seen to hold
            // is the answer, however long the evaluation that saw it took and
            // however far past either bound it lands. Both bounds are read
            // below, on an evaluation that reported pending.
            Observed::Met(value) => return Ok(value),
            Observed::Pending(state) if state.is_empty() => {
                last_state = UNREPORTED_STATE.to_string();
            }
            Observed::Pending(state) => last_state = state,
        }

        let failure = |kind| WaitFailure {
            what: what.to_string(),
            kind,
            budget,
            elapsed: started.elapsed(),
            probes,
            last_state: last_state.clone(),
        };

        if took > budget.probe() {
            return Err(failure(FailureKind::ProbeOverran { took }));
        }
        let left = budget.work().saturating_sub(started.elapsed());
        if left.is_zero() {
            return Err(failure(FailureKind::Elapsed));
        }
        poll_wait = poll::sleep_gap(poll_wait, left);
    }
}

/// Everything an [`absorb_until`] pump has taken and applied so far.
///
/// Two things are recorded, and both are read. The **count** is what a
/// condition asks when the arrival itself is the outcome — a case that wants
/// one more report and does not care what it said. The **facts** are what the
/// reports said about themselves, noted by the caller because only the caller
/// knows the type it took: a condition asks about one by name, and every one
/// of them renders into a pending observation.
///
/// Facts are a set, so a fact ten reports carry is one line in a failure rather
/// than ten, and a fact is a whole rendering rather than a field, because what
/// a report carries belongs to the crate that defined it and not to this one.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Absorbed {
    batches: usize,
    facts: BTreeSet<String>,
}

impl Absorbed {
    /// How many reports the pump has taken and applied.
    pub fn batches(&self) -> usize {
        self.batches
    }

    /// Note what a report said about itself.
    ///
    /// The pump counts the reports, because taking them is its job; what one
    /// said is the caller's to render, because the type is the caller's.
    pub fn note(&mut self, fact: impl Into<String>) {
        self.facts.insert(fact.into());
    }

    /// Whether any report so far carried `fact`.
    pub fn saw(&self, fact: &str) -> bool {
        self.facts.contains(fact)
    }
}

impl fmt::Display for Absorbed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} absorbed, carrying {:?}", self.batches, self.facts)
    }
}

/// Take reports off `subject` and apply them until `condition` holds.
///
/// Every look asks the condition, takes at most one report, applies it, and
/// asks again. **A condition that already holds ends the wait without taking
/// anything**, so a case's next absorption still has the reports its own change
/// produced.
///
/// The three closures divide by what they know. `take` is the only one that
/// sees the report's type, so it is what notes onto [`Absorbed`] whatever that
/// report said about itself. `apply` puts the report into the subject. The
/// condition reads the subject and the facts together, and it is the only thing
/// that ends the wait.
///
/// `budget`'s probe bound covers one whole take-and-apply rather than one look
/// at a state, because that is what a probe here does. A caller sizing it from
/// a wait that only reads state gets [`FailureKind::ProbeOverran`] blaming a
/// probe for the work this wait exists to drive.
///
/// A failure panics with the wait's own diagnostic: an absorption that never
/// reached its outcome has nothing to hand back but the reports it saw, and
/// [`Absorbed`] is in the diagnostic already.
pub fn absorb_until<S, B>(
    what: &str,
    budget: Budget,
    subject: &mut S,
    mut take: impl FnMut(&mut S, &mut Absorbed) -> Option<B>,
    mut apply: impl FnMut(&mut S, B),
    mut condition: impl FnMut(&mut S, &Absorbed) -> Observed<()>,
) {
    let mut absorbed = Absorbed::default();
    wait_until(what, budget, || {
        if let Observed::Met(()) = condition(subject, &absorbed) {
            return Observed::Met(());
        }
        if let Some(batch) = take(subject, &mut absorbed) {
            absorbed.batches += 1;
            apply(subject, batch);
        }
        match condition(subject, &absorbed) {
            Observed::Met(()) => Observed::Met(()),
            Observed::Pending(state) => Observed::pending(format!("{state}; {absorbed}")),
        }
    })
    .unwrap_or_else(|failure| panic!("{failure}"));
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    /// Budgets small enough to keep the suite quick, and loose enough that
    /// they are hang detectors rather than measurements of this machine.
    const WORK: Duration = Duration::from_millis(50);
    const PROBE: Duration = Duration::from_secs(5);

    fn budget() -> Budget {
        Budget::new(WORK, PROBE)
    }

    #[test]
    fn a_condition_that_already_holds_is_observed_at_once() {
        let value = wait_until("a settled state", budget(), || Observed::Met("ready"))
            .expect("a condition that holds on the first look");
        assert_eq!(value, "ready");
    }

    #[test]
    fn a_condition_that_becomes_true_inside_the_bound_is_observed() {
        let mut looks = 0;
        let value = wait_until("a converging state", budget(), || {
            looks += 1;
            if looks < 3 {
                Observed::pending(format!("{looks} of 3 looks"))
            } else {
                Observed::Met(looks)
            }
        })
        .expect("a condition that holds on the third look");
        assert_eq!(value, 3);
    }

    /// **The bar.** A condition that never holds ends the wait with a failure
    /// that names the bound it passed and the last state it saw.
    ///
    /// The forbidden shape is on both axes. A wait that polls for as long as
    /// the run lasts never returns here at all, and the suite dies by the
    /// runner's timeout with nothing to read. A wait that returns a bare
    /// timeout returns, and says only that time ran out — which names neither
    /// the bound that was in force nor the state the subject was actually in,
    /// so the two live failures it covers, "the budget was too tight" and
    /// "the subject is stuck", are indistinguishable.
    #[test]
    fn a_condition_that_never_holds_fails_naming_its_bound_and_the_last_state() {
        let mut looks = 0;
        let failure = wait_until("the dirty set to hold every changed path", budget(), || {
            looks += 1;
            Observed::<()>::pending(format!(
                "3 of 5 paths present, missing [d.md, e.md] ({looks})"
            ))
        })
        .expect_err("a condition that is never true");

        assert_eq!(failure.kind, FailureKind::Elapsed);
        assert_eq!(
            failure.budget.work(),
            WORK,
            "the failure carries a bound the call site did not declare"
        );
        assert!(
            failure.elapsed >= WORK,
            "the wait gave up after {:?}, inside its own {WORK:?} bound",
            failure.elapsed
        );
        assert!(
            failure.probes > 1,
            "the condition was evaluated {} times, so the wait did not poll",
            failure.probes
        );
        assert_eq!(
            failure.last_state,
            format!("3 of 5 paths present, missing [d.md, e.md] ({looks})")
        );

        let rendered = failure.to_string();
        for expected in [
            "the dirty set to hold every changed path",
            &format!("{WORK:?}"),
            "3 of 5 paths present, missing [d.md, e.md]",
        ] {
            assert!(
                rendered.contains(expected),
                "the failure does not name {expected:?}: {rendered}"
            );
        }
    }

    /// **The bar against an ambient bound.** Two waits declaring two bounds
    /// obey the two bounds they declared.
    ///
    /// A default that governs regardless of what a call site asks for is the
    /// forbidden shape, and it is invisible to any single wait: one call site
    /// reading back the number it passed proves nothing, because a universal
    /// bound and a declared bound agree whenever they happen to be equal.
    /// Two call sites disagreeing about their bound is what separates them.
    #[test]
    fn each_wait_obeys_the_bound_its_own_call_site_declared() {
        let short = Duration::from_millis(10);
        let long = Duration::from_millis(400);
        // A ceiling between the two declared bounds. Each wait is judged
        // against its own bound rather than against the other's measurement:
        // comparing two elapsed times makes a descheduled short wait look like
        // a governing default, and the discrimination survives without it,
        // because a single bound governing both waits either holds the short
        // one past this ceiling or lets the long one return under its own
        // bound.
        let between = Duration::from_millis(200);

        let mut failures = Vec::new();
        for bound in [short, long] {
            failures.push(
                wait_until(
                    "a state that never settles",
                    Budget::new(bound, PROBE),
                    || Observed::<()>::pending("nothing yet"),
                )
                .expect_err("a condition that is never true"),
            );
        }

        assert_eq!(failures[0].budget.work(), short);
        assert_eq!(failures[1].budget.work(), long);
        assert!(
            failures[0].elapsed >= short && failures[1].elapsed >= long,
            "a wait returned inside its own bound: {:?} against {short:?}, {:?} against {long:?}",
            failures[0].elapsed,
            failures[1].elapsed
        );
        assert!(
            failures[0].elapsed < between,
            "the wait declaring {short:?} ran {:?}, past the {between:?} ceiling between the two \
             declared bounds, so a bound it did not declare is governing it",
            failures[0].elapsed
        );
    }

    /// **The bar separating the two bounds.** A probe that is itself slow is
    /// reported as a slow probe, not as a condition that never held.
    ///
    /// One bound over both is the forbidden shape: under it a probe costing
    /// more than the whole work bound produces the same failure as a subject
    /// that never converges, and the reader chases the subject instead of the
    /// probe.
    #[test]
    fn a_slow_probe_is_named_as_a_slow_probe_rather_than_as_a_stuck_subject() {
        let probe_bound = Duration::from_millis(5);
        let slow = Duration::from_millis(60);
        let failure = wait_until(
            "a state read through a slow probe",
            Budget::new(Duration::from_secs(30), probe_bound),
            || {
                std::thread::sleep(slow);
                Observed::<()>::pending("read one row")
            },
        )
        .expect_err("a probe that passes its own bound");

        let FailureKind::ProbeOverran { took } = failure.kind else {
            panic!("a slow probe was reported as {}", failure);
        };
        assert!(
            took >= slow,
            "the reported probe time {took:?} is under what the probe slept"
        );
        assert_eq!(failure.budget.probe(), probe_bound);
        assert_eq!(failure.last_state, "read one row");

        let rendered = failure.to_string();
        assert!(
            rendered.contains(&format!("{probe_bound:?}")) && rendered.contains("probe bound"),
            "the failure does not name the probe bound it passed: {rendered}"
        );
    }

    /// **The bar on preemption.** A probe that begins inside the work bound
    /// runs to completion, and the condition it observes is honored.
    ///
    /// The forbidden shape is a work bound that abandons an evaluation in
    /// flight, or that discards its answer for arriving late. Under it a
    /// subject that converged fails its wait, which is a false negative no
    /// amount of budget can settle.
    #[test]
    fn a_condition_observed_by_a_probe_that_began_inside_the_bound_is_honored() {
        let value = wait_until(
            "a state seen by a probe that outlives the work bound",
            Budget::new(Duration::from_millis(1), Duration::from_secs(30)),
            || {
                std::thread::sleep(Duration::from_millis(40));
                Observed::Met("settled")
            },
        )
        .expect("a condition observed by a probe that started inside the bound");
        assert_eq!(value, "settled");
    }

    /// **The bar on preemption, at the other bound.** An evaluation that
    /// passes the probe bound and observes the condition anyway is honored.
    ///
    /// This is where "the bounds observe, they do not preempt" is decided,
    /// because it is the only case in which the probe bound has an answer to
    /// discard: the sibling case above passes the work bound instead, and a
    /// wait that weighed the probe bound against a met condition would still
    /// pass it. The forbidden shape is exactly that weighing — under it a
    /// probe a millisecond over its bound turns an observed convergence into a
    /// [`FailureKind::ProbeOverran`], and the suite fails a subject that is
    /// where it should be. The bound is read on the next pending observation
    /// instead, which the slow-probe case above pins.
    #[test]
    fn a_condition_observed_by_a_probe_that_passed_the_probe_bound_is_honored() {
        let probe_bound = Duration::from_millis(5);
        let slow = Duration::from_millis(60);
        let value = wait_until(
            "a state read through a probe slower than its own bound",
            // Work enough that only the probe bound is passed, so a failure
            // here could only be the probe bound discarding the answer.
            Budget::new(Duration::from_secs(30), probe_bound),
            || {
                std::thread::sleep(slow);
                Observed::Met("settled")
            },
        )
        .expect("a condition observed by an evaluation that passed the probe bound");
        assert_eq!(value, "settled");
    }

    /// The look taken at the work bound is a real look: a condition that
    /// becomes true in the last gap between probes is observed there.
    ///
    /// A wait that reports elapsed the moment it wakes from that gap is the
    /// forbidden shape, and it fails a subject that converged inside the
    /// budget it was given — with a diagnostic naming a state that was already
    /// stale when it was recorded.
    #[test]
    fn a_condition_that_becomes_true_in_the_last_gap_is_observed_at_the_bound() {
        let work = Duration::from_millis(60);
        // Read from before the wait starts, so the condition turns true a hair
        // ahead of the wait's own bound: the look that sees it is the one
        // taken after the last gap, which is clamped to the bound itself.
        let started = Instant::now();
        let value = wait_until(
            "a state that settles as its bound expires",
            Budget::new(work, PROBE),
            || {
                let waited = started.elapsed();
                if waited >= work {
                    Observed::Met("settled")
                } else {
                    Observed::pending(format!("{waited:?} into a {work:?} bound"))
                }
            },
        )
        .expect("a condition true at the work bound is seen by the look taken there");
        assert_eq!(value, "settled");
    }

    /// A pending observation that describes nothing still fails with something
    /// to read. An empty description renders as a bound, a colon, and nothing
    /// — the bare timeout [`Observed::Pending`] exists to forbid, reached from
    /// the caller's side rather than the type's.
    #[test]
    fn a_pending_observation_that_describes_nothing_still_names_a_state() {
        let failure = wait_until(
            "a state nothing described",
            Budget::new(Duration::ZERO, PROBE),
            || Observed::<()>::pending(""),
        )
        .expect_err("a condition that is never true");

        assert_eq!(failure.last_state, UNREPORTED_STATE);
        let rendered = failure.to_string();
        assert!(
            rendered.ends_with(UNREPORTED_STATE),
            "the failure trails off after its colon: {rendered:?}"
        );
    }

    /// A zero work bound is still a bound, and the condition is still asked.
    /// A wait that skips the evaluation reports a state nothing observed.
    #[test]
    fn a_zero_work_bound_still_evaluates_the_condition_once() {
        let mut looks = 0;
        let failure = wait_until(
            "a state under no budget at all",
            Budget::new(Duration::ZERO, PROBE),
            || {
                looks += 1;
                Observed::<()>::pending("empty")
            },
        )
        .expect_err("a condition that is never true");
        assert_eq!(looks, 1);
        assert_eq!(failure.probes, 1);
        assert_eq!(failure.last_state, "empty");
    }

    /// **The bar on budget composition.** A held-open wait's budget strictly
    /// dominates the sum of the outer waits it brackets.
    ///
    /// The forbidden shape is the flat constant shared between them: under it
    /// a hold and the sequence it brackets expire together, so the hold lets
    /// go partway through and the subject resumes under a case still asserting
    /// it is parked. The comparison here is against the whole sequence, not
    /// against one of its waits, because a hold that merely outlives a single
    /// outer wait still lets go inside a sequence of two.
    #[test]
    fn a_held_open_budget_outlives_every_sequence_of_outer_waits_it_brackets() {
        let outer = Budget::new(Duration::from_secs(10), Duration::from_millis(250));

        for bracketed in [0u32, 1, 2, 7] {
            let held_open = outer.dominating(bracketed);
            let sequence = outer.work() * bracketed;
            assert!(
                held_open.work() > sequence,
                "a hold bracketing {bracketed} waits gets {:?}, which does not outlive the \
                 {sequence:?} those waits may take",
                held_open.work()
            );
            assert_eq!(
                held_open.probe(),
                outer.probe(),
                "the derived bound changed how long one look at the gate may take"
            );
        }
    }

    /// A count nothing could bracket still yields a hold wider than the wait it
    /// derives from. A wrapped one would be narrower, and the hold would be the
    /// first thing to expire.
    #[test]
    fn a_held_open_budget_saturates_rather_than_wrapping() {
        let outer = Budget::new(Duration::from_secs(3_600), Duration::from_millis(250));
        assert!(outer.dominating(u32::MAX).work() > outer.work());
    }

    /// **The bar on convergence budgets.** The budget grows with the changed
    /// set, from a floor.
    ///
    /// A flat constant is the forbidden shape: the same number that lets a
    /// thousand-file churn settle lets a one-file change hang for as long
    /// before anyone hears about it.
    #[test]
    fn a_convergence_budget_grows_with_the_changed_set() {
        let settling = Convergence::new(
            Duration::from_secs(2),
            Duration::from_millis(20),
            Duration::from_millis(200),
        );

        assert_eq!(settling.budget_for(0).work(), Duration::from_secs(2));
        assert_eq!(settling.budget_for(1).work(), Duration::from_millis(2_020));
        assert_eq!(settling.budget_for(100).work(), Duration::from_secs(4));
        assert!(
            settling.budget_for(1_000).work() > settling.budget_for(10).work(),
            "a wider changed set bought no more budget"
        );
        assert_eq!(
            settling.budget_for(1_000).probe(),
            settling.probe(),
            "the probe bound is the suite's, whatever the changed set"
        );
    }

    /// A count nothing could really change still yields a budget larger than
    /// the floor. A wrapped one would be smaller, and a wide churn would fail
    /// faster than a narrow one.
    #[test]
    fn a_convergence_budget_saturates_rather_than_wrapping() {
        let settling = Convergence::new(
            Duration::from_secs(1),
            Duration::from_secs(3_600),
            Duration::from_millis(100),
        );
        let absurd = settling.budget_for(usize::MAX).work();
        assert!(
            absurd > settling.floor(),
            "a changed set of usize::MAX produced {absurd:?}, at or under the {:?} floor",
            settling.floor()
        );
        assert!(absurd > settling.budget_for(1_000).work());

        // The count that separates the guard from a truncating cast. usize::MAX
        // cannot: its low 32 bits are u32::MAX, which is what the guard yields
        // anyway. This one truncates to zero, and a truncating conversion
        // returns the bare floor.
        #[cfg(target_pointer_width = "64")]
        {
            let past_u32 = settling.budget_for(1usize << 32).work();
            assert!(
                past_u32 > settling.floor(),
                "a changed set of 2^32 produced {past_u32:?}, at or under the {:?} floor",
                settling.floor()
            );
        }
    }

    /// One report a case chose to make, rather than one a source happened to
    /// settle: the paths it invalidated, and nothing else.
    struct DrivenBatch(Vec<&'static str>);

    /// A subject whose reports are chosen rather than observed: a queue of
    /// settled reports, the paths that really exist, and the paths applying
    /// those reports has left stored.
    ///
    /// Applying a report means reading each invalidated path and keeping the
    /// ones that exist, which is what a scoped reconcile does to a store.
    #[derive(Default)]
    struct DrivenBatches {
        queue: VecDeque<DrivenBatch>,
        tree: BTreeSet<&'static str>,
        stored: BTreeSet<&'static str>,
    }

    impl DrivenBatches {
        fn take(&mut self, absorbed: &mut Absorbed) -> Option<DrivenBatch> {
            let batch = self.queue.pop_front()?;
            for path in &batch.0 {
                absorbed.note(*path);
            }
            Some(batch)
        }

        fn apply(&mut self, batch: DrivenBatch) {
            for path in batch.0 {
                if self.tree.contains(path) {
                    self.stored.insert(path);
                } else {
                    self.stored.remove(path);
                }
            }
        }
    }

    fn absorbing_budget() -> Budget {
        Budget::new(Duration::from_secs(5), PROBE)
    }

    /// **The bar the absorbing idiom stands on.** One change reported across
    /// two reports is one outcome, whichever half arrives first.
    ///
    /// The forbidden shape is the wait that ends at the first report and leaves
    /// the outcome to a bare assertion after it. Under it each split direction
    /// fails on the half that had not arrived — the destination missing when
    /// the source's removal came first, the source still stored when the
    /// destination came first — and both read as an apply that dropped a path
    /// rather than as a report that had not finished arriving.
    #[test]
    fn a_split_rename_is_absorbed_whichever_half_arrives_first() {
        for (label, order) in [
            ("destination first", ["renamed/b.md", "source/b.md"]),
            ("source first", ["source/b.md", "renamed/b.md"]),
        ] {
            let mut driven = DrivenBatches {
                queue: order.iter().map(|root| DrivenBatch(vec![root])).collect(),
                tree: ["renamed/b.md"].into_iter().collect(),
                stored: ["source/b.md"].into_iter().collect(),
            };
            absorb_until(
                &format!("both halves of a rename split {label}"),
                absorbing_budget(),
                &mut driven,
                DrivenBatches::take,
                DrivenBatches::apply,
                |driven, _| {
                    if !driven.stored.contains("source/b.md")
                        && driven.stored.contains("renamed/b.md")
                    {
                        Observed::Met(())
                    } else {
                        Observed::pending(format!("stored: {:?}", driven.stored))
                    }
                },
            );
            assert_eq!(
                driven.stored,
                ["renamed/b.md"].into_iter().collect(),
                "the {label} split settled on the wrong store"
            );
            assert!(
                driven.queue.is_empty(),
                "the {label} split left a report unabsorbed"
            );
        }
    }

    /// An absorption whose outcome already holds takes nothing, so a case's
    /// next absorption still has the reports its own change produced.
    #[test]
    fn an_outcome_that_already_holds_absorbs_nothing() {
        let mut driven = DrivenBatches {
            queue: [DrivenBatch(vec!["later.md"])].into_iter().collect(),
            tree: ["later.md"].into_iter().collect(),
            stored: ["already.md"].into_iter().collect(),
        };
        absorb_until(
            "an outcome the store already carries",
            absorbing_budget(),
            &mut driven,
            DrivenBatches::take,
            DrivenBatches::apply,
            |driven, _| {
                if driven.stored.contains("already.md") {
                    Observed::Met(())
                } else {
                    Observed::pending("already.md is not stored")
                }
            },
        );
        assert_eq!(driven.queue.len(), 1, "a settled report was consumed");
    }

    /// A condition that asks about the reports rather than the subject reads
    /// what the pump absorbed: how many arrived, and what each said.
    #[test]
    fn a_condition_reads_the_count_and_the_facts_the_reports_carried() {
        let mut driven = DrivenBatches {
            queue: [DrivenBatch(vec!["a.md"]), DrivenBatch(vec!["b.md"])]
                .into_iter()
                .collect(),
            tree: ["a.md", "b.md"].into_iter().collect(),
            stored: BTreeSet::new(),
        };
        absorb_until(
            "both reports to arrive, the second naming b.md",
            absorbing_budget(),
            &mut driven,
            DrivenBatches::take,
            DrivenBatches::apply,
            |_, absorbed| {
                if absorbed.batches() == 2 && absorbed.saw("b.md") {
                    Observed::Met(())
                } else {
                    Observed::pending(absorbed.to_string())
                }
            },
        );
        assert_eq!(driven.stored, ["a.md", "b.md"].into_iter().collect());
    }

    /// An absorption that never reaches its outcome says what did arrive, so a
    /// failure separates "nothing was reported" from "the reports did not add
    /// up to the outcome".
    #[test]
    #[should_panic(expected = "carrying {\"a.md\"}")]
    fn an_absorption_that_expires_names_the_reports_it_took() {
        let mut driven = DrivenBatches {
            queue: [DrivenBatch(vec!["a.md"])].into_iter().collect(),
            tree: ["a.md"].into_iter().collect(),
            stored: BTreeSet::new(),
        };
        absorb_until(
            "an outcome no report carries",
            Budget::new(WORK, PROBE),
            &mut driven,
            DrivenBatches::take,
            DrivenBatches::apply,
            |driven, _| {
                if driven.stored.contains("never.md") {
                    Observed::Met(())
                } else {
                    Observed::pending("never.md is not stored")
                }
            },
        );
    }

    /// Unwrapping a failed wait prints the diagnostic rather than a field
    /// dump, because the panic message is what a reader gets.
    #[test]
    fn the_failure_debugs_as_its_diagnostic() {
        let failure = wait_until(
            "a state that never settles",
            Budget::new(Duration::ZERO, PROBE),
            || Observed::<()>::pending("nothing yet"),
        )
        .expect_err("a condition that is never true");
        assert_eq!(format!("{failure:?}"), failure.to_string());
    }
}
