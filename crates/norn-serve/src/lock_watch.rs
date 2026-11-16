//! The holder's recheck of its channel lock's name.
//!
//! **Every 5 s a serving host compares the lock file's name with the
//! descriptor it holds now.** flock locks belong to an open file description,
//! so the comparison is against the *current* handle: once a re-take adopts a
//! new one, the old is closed and never compared again, or the holder would
//! be refused by its own new lock.
//!
//! - **The name is the handle's:** nothing to do.
//! - **The name differs** (someone deleted or replaced `serve.lock`): the
//!   holder tries the name. **Taking it**, it adopts the new handle, closes
//!   the old and keeps serving: deleting the lock alone never stops a host.
//! - **Another start holds the new name:** the holder tries again each tick.
//!   A start that took the new name while this host still holds the port fails
//!   its probe within its start bound and exits, freeing the name. Held past
//!   one start bound plus one tick, the holder concludes a second host serves
//!   the channel on another endpoint, logs it, and drains, exiting 1.
//! - **A recheck or a re-take that errors** (`EACCES`, `EIO`, a link at the
//!   name) logs one warning and tries again next tick; it never drains.
//!
//! Declared: deleting `serve.lock` is outside the contract. A person who
//! deletes it and starts a second host on another port gets two hosts for at
//! most one start bound plus two ticks and the old host's drain and margin
//! (10 + 10 + 25 = 45 s); and a restart loop of same-port starts that each
//! hold the new name briefly can look like one long holder, so the holder
//! drains though no second host serves.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use norn_config::{ChannelLock, ConfigDirs};

use crate::bounds::{LOCK_RECHECK, START_BOUND};
use crate::lifecycle::{Lifecycle, StopReason};
use crate::log::log;

/// What one recheck saw.
#[derive(Debug)]
pub(crate) enum Seen {
    /// The name is the held handle's.
    Holds,
    /// The name differed, and the holder took the new name.
    Retook,
    /// The name differed, and another start holds the new one.
    HeldByAnother,
    /// The recheck or the re-take refused.
    Failed(String),
}

/// What the holder does after a recheck.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Verdict {
    Keep,
    /// Log one warning, and try again next tick.
    Warn(String),
    /// Another start held the new name past the grace: drain.
    Drain,
}

/// How long another start may hold the re-made name before the holder
/// drains: one start bound plus one tick.
pub(crate) const GRACE: Duration = START_BOUND.saturating_add(LOCK_RECHECK);

/// The recheck's memory between ticks: since when another start has held the
/// re-made name.
#[derive(Debug, Default)]
pub(crate) struct Recheck {
    held_since: Option<Instant>,
}

impl Recheck {
    /// Judge one recheck's sight at `now`.
    pub(crate) fn judge(&mut self, now: Instant, seen: Seen) -> Verdict {
        match seen {
            Seen::Holds | Seen::Retook => {
                self.held_since = None;
                Verdict::Keep
            }
            Seen::Failed(error) => Verdict::Warn(error),
            Seen::HeldByAnother => {
                let since = *self.held_since.get_or_insert(now);
                if now.duration_since(since) >= GRACE {
                    Verdict::Drain
                } else {
                    Verdict::Keep
                }
            }
        }
    }
}

/// Look at the name once, re-taking it where it differs.
fn look(dirs: &ConfigDirs, lock: &Mutex<ChannelLock>) -> Seen {
    let mut held = lock.lock().unwrap_or_else(PoisonError::into_inner);
    match held.matches_name() {
        Ok(true) => Seen::Holds,
        Err(error) => Seen::Failed(format!("rechecking the channel lock failed: {error}")),
        Ok(false) => match ChannelLock::try_take(dirs) {
            // The new handle becomes the current one, and the old closes.
            Ok(Some(new)) => {
                drop(std::mem::replace(&mut *held, new));
                Seen::Retook
            }
            Ok(None) => Seen::HeldByAnother,
            Err(error) => Seen::Failed(format!("re-taking the channel lock failed: {error}")),
        },
    }
}

/// Recheck the lock every tick until a drain begins, on a thread of its own.
/// The lock stays in `lock`, held by the serving entry until the process
/// exits.
pub(crate) fn start(
    dirs: ConfigDirs,
    lock: Arc<Mutex<ChannelLock>>,
    lifecycle: Arc<Lifecycle>,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("norn-lock-watch".into())
        .spawn(move || {
            let mut recheck = Recheck::default();
            while !lifecycle.drain.has_begun() {
                std::thread::sleep(LOCK_RECHECK);
                let seen = look(&dirs, &lock);
                if matches!(seen, Seen::Retook) {
                    log(format_args!(
                        "the channel lock file in {} was removed; this host took it again and \
                         keeps serving",
                        dirs.config_dir().display()
                    ));
                }
                match recheck.judge(Instant::now(), seen) {
                    Verdict::Keep => {}
                    Verdict::Warn(error) => log(format_args!("warning: {error}")),
                    Verdict::Drain => {
                        log(format_args!(
                            "another norn host took this channel after its lock file in {} was \
                             removed; this host stops so that one host serves the channel",
                            dirs.config_dir().display()
                        ));
                        lifecycle.begin_drain(StopReason::LockTaken);
                        return;
                    }
                }
            }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use norn_testkit::scratch::Scratch;

    use super::*;

    /// Another start holding the re-made name drains the holder only once it
    /// has held it past one start bound plus one tick; a re-take or a name
    /// that matches again in between forgets it.
    #[test]
    fn another_holder_drains_only_past_the_grace() {
        let start = Instant::now();
        let mut recheck = Recheck::default();
        assert_eq!(recheck.judge(start, Seen::HeldByAnother), Verdict::Keep);
        assert_eq!(
            recheck.judge(start + LOCK_RECHECK, Seen::HeldByAnother),
            Verdict::Keep
        );
        assert_eq!(
            recheck.judge(start + GRACE, Seen::HeldByAnother),
            Verdict::Drain
        );

        let mut recheck = Recheck::default();
        recheck.judge(start, Seen::HeldByAnother);
        assert_eq!(
            recheck.judge(start + LOCK_RECHECK, Seen::Retook),
            Verdict::Keep
        );
        assert_eq!(
            recheck.judge(start + GRACE, Seen::HeldByAnother),
            Verdict::Keep,
            "a re-take restarts the grace"
        );
    }

    #[test]
    fn a_failed_recheck_warns_and_never_drains() {
        let mut recheck = Recheck::default();
        let start = Instant::now();
        for tick in 0..10 {
            assert!(matches!(
                recheck.judge(start + LOCK_RECHECK * tick, Seen::Failed("EIO".into())),
                Verdict::Warn(_)
            ));
        }
    }

    #[test]
    fn the_grace_is_one_start_bound_and_one_tick() {
        assert_eq!(GRACE, Duration::from_secs(15));
    }

    /// The look itself: a removed name is re-taken, the new handle adopted
    /// and the old one closed, so the next look matches; a name another
    /// handle holds is reported held.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: removing the lock file as a person would.
    fn a_removed_name_is_retaken_and_the_new_handle_is_the_one_compared() {
        let scratch = Scratch::new("norn-serve-lock-watch");
        std::fs::create_dir_all(scratch.join("config")).expect("a config base");
        let dirs = ConfigDirs::new(scratch.join("config"), scratch.join("data")).expect("bases");
        let lock = Mutex::new(ChannelLock::try_take(&dirs).unwrap().expect("a free lock"));
        assert!(matches!(look(&dirs, &lock), Seen::Holds));

        let path = dirs.config_dir().join("serve.lock");
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(look(&dirs, &lock), Seen::Retook));
        assert!(matches!(look(&dirs, &lock), Seen::Holds));
        assert!(
            ChannelLock::try_take(&dirs).unwrap().is_none(),
            "the re-taken name is held"
        );

        std::fs::remove_file(&path).unwrap();
        let other = ChannelLock::try_take(&dirs)
            .unwrap()
            .expect("the re-made name");
        assert!(matches!(look(&dirs, &lock), Seen::HeldByAnother));
        drop(other);
        assert!(matches!(look(&dirs, &lock), Seen::Retook));
    }
}
