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
//! Precision belongs to the condition; slack belongs to the budget. **A
//! harness deadline is a hang detector, never a performance bar.** It answers
//! whether the state ever converged, and nothing about how quickly. A suite
//! that wants the settle time asserts it itself, as its own measurement,
//! under the lane rules its evidence kind belongs to. A budget set near the
//! time convergence usually takes makes machine speed a test result.
//!
//! So the condition is written exactly — the precise set of paths, the exact
//! row — and the budget is written loose.
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
//! **The bound observes; it does not preempt.** A probe that begins inside
//! the work bound runs to completion, and a condition it observes to hold is
//! a success however late that lands.
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
//! failure payload is what this module offers in its place.

use std::fmt;
use std::time::{Duration, Instant};

/// The first gap between probes, and the longest one.
///
/// Cadence is granularity, not a bound: it decides how soon after the
/// condition becomes true the wait notices, and it never extends a wait past
/// the work bound, because the last gap is clamped to what is left of it. The
/// bounds a wait obeys are the two in its [`Budget`] and nothing else.
const FIRST_POLL_WAIT: Duration = Duration::from_millis(1);
const LONGEST_POLL_WAIT: Duration = Duration::from_millis(50);

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
    let mut poll_wait = FIRST_POLL_WAIT;
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
            // The bound observes and does not preempt: a condition seen to
            // hold is the answer, however long the evaluation that saw it
            // took and however far past the work bound it lands.
            Observed::Met(value) => return Ok(value),
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
        std::thread::sleep(poll_wait.min(left));
        poll_wait = (poll_wait * 2).min(LONGEST_POLL_WAIT);
    }
}

#[cfg(test)]
mod tests {
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
        let long = Duration::from_millis(250);

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
            failures[0].elapsed < failures[1].elapsed,
            "both waits ran the same length ({:?}, {:?}) whatever they declared, so one bound is \
             governing both",
            failures[0].elapsed,
            failures[1].elapsed
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
