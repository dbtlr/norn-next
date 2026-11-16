//! The channel-wide bounds every host process of every channel keeps.
//!
//! **They are compile-time constants, never settings.** A start waiting on a
//! held lock and the host it waits on may have read different settings, so a
//! bound either side could change would let one break the other's wait. The
//! one setting among them, `drain_bound`, is clamped here to its cap, so no
//! value any process read can stretch a stop past the release bound.
//!
//! **The release bound's inequality.** A waiter may first find the lock held
//! a moment after another start took it. That holder may take the whole start
//! bound to listen, close its listener between two of the waiter's polls (a
//! poll is at most two dial bounds, the hello's and the drain's, plus the poll
//! interval), and then take the largest drain and the margin to exit. So the
//! release bound must exceed the start bound + the drain cap + the margin +
//! two dial bounds + the poll interval: 10 + 20 + 5 + 2 + 0.1 = 37.1 s. 40 s is
//! that plus slack, and a test holds the inequality over these constants, so a
//! change to any term re-checks it.

use std::time::Duration;

/// The longest `drain_bound` a host keeps, whatever its settings say: listener
/// close to the last in-flight request finishing.
pub(crate) const DRAIN_CAP: Duration = Duration::from_secs(20);

/// `drain_bound`'s end to process exit: answering what is still running,
/// flushing, exiting. Its end is the hard exit.
pub(crate) const SHUTDOWN_MARGIN: Duration = Duration::from_secs(5);

/// Taking `serve.lock` to listening: the mint, the token file lock's wait,
/// the registry read, the host's construction, the probe and the bind.
pub(crate) const START_BOUND: Duration = Duration::from_secs(10);

/// One connect plus one hello while a start waits on a held lock.
pub(crate) const DIAL_BOUND: Duration = Duration::from_secs(1);

/// The drain request a takeover sends on a verified connection: a bound of
/// its own, beside the hello's.
pub(crate) const DRAIN_DIAL_BOUND: Duration = Duration::from_secs(1);

/// Between the end of one held-lock poll and the next.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How often a start tries `serve.lock` alone while a drain it sent may
/// still be outstanding, so it usually takes a freed lock ahead of a
/// launched start polling at [`POLL_INTERVAL`].
pub(crate) const LOCK_TRY_INTERVAL: Duration = Duration::from_millis(10);

/// Listener close to `serve.lock` coming free, for any stopping host; and the
/// longest a start waits on a held lock with no verified answer, from its
/// last deadline reset.
pub(crate) const RELEASE_BOUND: Duration = Duration::from_secs(40);

/// The whole held-lock wait of one start, explicit or launched, however many
/// deadline resets: twice the release bound.
pub(crate) const TAKEOVER_CAP: Duration = Duration::from_secs(80);

/// How often a serving host compares its lock file's name with the
/// descriptor it holds.
pub(crate) const LOCK_RECHECK: Duration = Duration::from_secs(5);

/// The longest slice an apply's wait takes before it reads the drain's
/// deadline again.
pub(crate) const APPLY_WAIT_SLICE: Duration = Duration::from_millis(100);

/// The drain bound a host keeps: its setting, clamped to [`DRAIN_CAP`].
pub(crate) fn drain_bound(setting: Duration) -> Duration {
    setting.min(DRAIN_CAP)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The release bound's inequality**, over the constants themselves: a
    /// waiter outlasts a holder that took the lock just before it looked, then
    /// listened at the end of its start bound, closed its listener between
    /// two of the waiter's polls and took the largest drain and the margin.
    #[test]
    fn the_release_bound_exceeds_every_holder_a_waiter_can_meet() {
        let longest_holder = START_BOUND
            + DRAIN_CAP
            + SHUTDOWN_MARGIN
            + DIAL_BOUND
            + DRAIN_DIAL_BOUND
            + POLL_INTERVAL;
        assert_eq!(longest_holder, Duration::from_millis(37_100));
        assert!(longest_holder < RELEASE_BOUND, "{longest_holder:?}");
        assert_eq!(TAKEOVER_CAP, RELEASE_BOUND * 2);
    }

    /// No setting stretches a stop past the cap the inequality is over.
    #[test]
    fn a_drain_bound_past_the_cap_is_held_to_it() {
        assert_eq!(drain_bound(Duration::from_secs(60)), DRAIN_CAP);
        assert_eq!(drain_bound(Duration::from_secs(3)), Duration::from_secs(3));
    }

    /// The lock tries a takeover makes while its drain is outstanding come
    /// several times within one poll, or they would not win the freed lock
    /// ahead of a launched start polling at the poll interval.
    #[test]
    fn a_takeover_tries_the_lock_several_times_a_poll() {
        assert!(LOCK_TRY_INTERVAL * 5 <= POLL_INTERVAL);
    }
}
