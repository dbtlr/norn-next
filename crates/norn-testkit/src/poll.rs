//! The cadence a bounded wait polls at.
//!
//! Bounded waits in this crate ask, wait, and ask again: [`crate::process`]
//! asks about process lifecycle and [`crate::wait`] asks whether a condition
//! holds. The subjects differ; the cadence is one policy, and it
//! lives here once. The first question is asked before any wait, so a subject
//! that is already there is answered immediately. The gap then doubles to a
//! ceiling, so a wait that lasts costs a bounded number of questions rather
//! than one per millisecond.
//!
//! Cadence is granularity, not a bound. It decides how late a wait notices
//! what it was waiting for — by at most one gap — and it never extends a wait
//! past the bound the call site declared, because every gap is clamped to what
//! is left of that bound.

use std::time::Duration;

/// The gap before the second question.
pub(crate) const FIRST_GAP: Duration = Duration::from_millis(1);

/// The widest gap between two questions, however long the wait runs.
pub(crate) const LONGEST_GAP: Duration = Duration::from_millis(50);

/// Wait out `gap`, or `left` if that is shorter, and return the gap to use
/// after it.
///
/// Clamping to `left` is what keeps cadence out of the bound: the last gap
/// ends at the bound itself, which is where the caller asks its final
/// question.
pub(crate) fn sleep_gap(gap: Duration, left: Duration) -> Duration {
    std::thread::sleep(gap.min(left));
    (gap * 2).min(LONGEST_GAP)
}
