//! The signal thread: SIGTERM, SIGINT and SIGHUP, received by `sigwait` on a
//! thread of their own.
//!
//! **The mask comes first.** [`block`] runs as the first thing the host
//! process does, on the main thread, before any other thread exists, so every
//! thread spawned after it — the runtime, the log and timer threads, the
//! pools, the host's workers — inherits the mask, and a handled signal is
//! never delivered to its default action. What remains is the moment from
//! exec to that first statement, which no code in the process can reach: a
//! launcher that must close it starts the process with the signals already
//! blocked, a mask the process keeps.
//!
//! **What each signal does:**
//!
//! - Before the host listens, any of them ends the start at once: the line
//!   naming it is given a bounded chance to be written, then `_exit(1)`, which
//!   frees the channel lock if the start held it.
//! - Once it listens, each begins the drain (`lifecycle.rs`). SIGHUP — a
//!   closed terminal — is handled only when the process did not inherit it as
//!   ignored, so `nohup norn serve` keeps ignoring it: an ignored SIGHUP is
//!   neither blocked nor waited for, and stays ignored.
//! - A SIGTERM or SIGINT received after an earlier SIGTERM or SIGINT, whatever
//!   began the drain, ends the process at once with the timer thread's
//!   `_exit(1)`: a person who presses Ctrl-C twice gets the process back, and
//!   what was still running is answered by nothing. The first one during a
//!   drain a drain request or the idle exit began is not a second. A further
//!   SIGHUP is never a second: one terminal close can deliver SIGHUP twice.
//!
//! The thread never waits on the I/O thread, so a stalled runtime cannot keep
//! a drain or an exit from firing.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::lifecycle::{Lifecycle, REFUSED, StopReason};
use crate::log::{flush_within, log};
use crate::sys::{self, SignalSet, c_int};

/// The longest the line naming a signal before listening waits to be written.
const LAST_LINE_WAIT: Duration = Duration::from_millis(100);

/// The signals this process waits for, blocked in the main thread.
pub(crate) struct Mask {
    set: SignalSet,
}

/// Block SIGTERM, SIGINT and — unless it was inherited as ignored — SIGHUP in
/// the calling thread. Call it before any thread is spawned.
pub(crate) fn block() -> std::io::Result<Mask> {
    let mut signals = vec![libc::SIGTERM, libc::SIGINT];
    if !sys::inherited_as_ignored(libc::SIGHUP) {
        signals.push(libc::SIGHUP);
    }
    let set = SignalSet::of(&signals);
    sys::block(&set)?;
    Ok(Mask { set })
}

/// The lifecycle the signal thread acts on, once the start has built it.
/// Until it is set, every handled signal ends the start.
pub(crate) type Target = Arc<OnceLock<Arc<Lifecycle>>>;

/// Start the signal thread over `mask`.
pub(crate) fn start(mask: Mask, target: Target) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("norn-signals".into())
        .spawn(move || receive(&mask.set, &target))?;
    Ok(())
}

fn receive(set: &SignalSet, target: &Target) {
    let mut terminated = false;
    loop {
        let Ok(signal) = sys::wait(set) else {
            // A wait the kernel refuses has no retry that would differ; the
            // signals stay blocked and pending, and the process carries on
            // without them.
            log(format_args!(
                "warning: waiting for signals failed; SIGTERM, SIGINT and SIGHUP are not handled"
            ));
            return;
        };
        let Some(reason) = reason(signal) else {
            continue;
        };
        let Some(lifecycle) = target.get() else {
            exit_before_listening(reason);
        };
        if lifecycle.if_starting(|| exit_before_listening(reason)) {
            continue;
        }
        if matches!(reason, StopReason::Terminate | StopReason::Interrupt) {
            if terminated {
                lifecycle.exits().now(REFUSED);
                continue;
            }
            terminated = true;
        }
        lifecycle.begin_drain(reason);
    }
}

fn reason(signal: c_int) -> Option<StopReason> {
    match signal {
        libc::SIGTERM => Some(StopReason::Terminate),
        libc::SIGINT => Some(StopReason::Interrupt),
        libc::SIGHUP => Some(StopReason::Hangup),
        _ => None,
    }
}

/// End a start a signal arrived in before it listened: exit 1.
fn exit_before_listening(reason: StopReason) -> ! {
    log(format_args!(
        "refused: {reason} arrived before the host was listening"
    ));
    flush_within(LAST_LINE_WAIT);
    sys::exit_now(REFUSED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_handled_signal_names_its_stop() {
        assert_eq!(reason(libc::SIGTERM), Some(StopReason::Terminate));
        assert_eq!(reason(libc::SIGINT), Some(StopReason::Interrupt));
        assert_eq!(reason(libc::SIGHUP), Some(StopReason::Hangup));
        assert_eq!(reason(libc::SIGUSR1), None);
    }
}
