//! Where the host process stands — starting, serving or draining — and the
//! one drain every stop runs.
//!
//! **Every stop is one drain.** SIGTERM, SIGINT, SIGHUP (the signal thread),
//! an accepted drain request (the drain route), an auto-launched host's idle
//! exit, a removed lock name another start took (the lock watch) and a
//! stopped worker (the stop trigger) each call [`Lifecycle::begin_drain`].
//! The first call to arrive while the host serves begins it; any later one is
//! a no-op. Beginning it, in order:
//!
//! 1. sets the stage to draining, so a hello on an open connection reports
//!    `draining` from now on;
//! 2. sets the drain's deadline, `drain_bound` from now;
//! 3. arms the hard exit at that deadline plus the shutdown margin, with the
//!    status the reason exits with — the timer's own thread, so a stalled
//!    I/O thread cannot keep it from firing;
//! 4. sets the admission's draining flag and sweeps every waiting claim to
//!    `refused`;
//! 5. wakes the serving loop, which closes the listener and waits out the
//!    in-flight requests.
//!
//! The listener closes after the stage is set, and the hard exit is armed
//! before it closes, so a host frees its lock within `drain_bound` + the
//! margin of closing its listener, and a waiter that saw `draining` knows
//! the lock comes free within the release bound of when it looked.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::{Notify, watch};

use crate::admission::Admission;
use crate::bounds::SHUTDOWN_MARGIN;
use crate::drain::Drain;
use crate::exit::HardExit;
use crate::log::log;

/// The status a start that refused, or a host that stopped abnormally, exits
/// with.
pub(crate) const REFUSED: u8 = 1;

/// Where the process stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Stage {
    /// Not yet listening: a signal ends the start at once, exit 1.
    Starting,
    /// Listening: a stop begins the drain.
    Serving,
    /// The drain has begun.
    Draining,
}

/// What began a drain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StopReason {
    Terminate,
    Interrupt,
    Hangup,
    /// A verified explicit start asked this auto-launched host to retire.
    DrainRequest,
    /// An auto-launched host saw no activity for `auto_exit_after`.
    Idle,
    /// Another start held the lock name, re-made after this host's was
    /// removed, past one start bound.
    LockTaken,
    /// The host's worker stopped, so it can no longer serve.
    WorkerStopped,
}

impl StopReason {
    /// The status the process exits with after this drain: 0 for a drain a
    /// signal, a drain request or the idle exit began; 1 for a removed lock
    /// name or a stopped worker, so a service manager restarts it.
    pub(crate) fn status(self) -> u8 {
        match self {
            StopReason::Terminate
            | StopReason::Interrupt
            | StopReason::Hangup
            | StopReason::DrainRequest
            | StopReason::Idle => 0,
            StopReason::LockTaken | StopReason::WorkerStopped => REFUSED,
        }
    }
}

impl fmt::Display for StopReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            StopReason::Terminate => "SIGTERM",
            StopReason::Interrupt => "SIGINT",
            StopReason::Hangup => "SIGHUP",
            StopReason::DrainRequest => "a drain request",
            StopReason::Idle => "the idle exit",
            StopReason::LockTaken => "another start took the channel lock",
            StopReason::WorkerStopped => "the host's worker stopped",
        })
    }
}

/// What a call to [`Lifecycle::begin_drain`] did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Began {
    /// This call began the drain.
    Drain,
    /// A drain had already begun.
    AlreadyDraining,
    /// The host is not yet listening; nothing began.
    NotListening,
}

/// The host process's stage, shared by every thread that can stop it.
pub(crate) struct Lifecycle {
    stage: Mutex<(Stage, Option<StopReason>)>,
    pub(crate) drain: Arc<Drain>,
    admission: Arc<Admission>,
    exits: Arc<HardExit>,
    /// Set once a drain's beginning has made all five of its steps, so a
    /// reader that sees it sees the deadline and the reason too.
    begun: AtomicBool,
    /// Woken when a drain begins.
    stopping: Notify,
    listener_closed: watch::Sender<bool>,
}

impl Lifecycle {
    pub(crate) fn new(drain: Arc<Drain>, admission: Arc<Admission>, exits: Arc<HardExit>) -> Self {
        Lifecycle {
            stage: Mutex::new((Stage::Starting, None)),
            drain,
            admission,
            exits,
            begun: AtomicBool::new(false),
            stopping: Notify::new(),
            listener_closed: watch::Sender::new(false),
        }
    }

    fn stage_mut(&self) -> std::sync::MutexGuard<'_, (Stage, Option<StopReason>)> {
        self.stage.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn stage(&self) -> Stage {
        self.stage_mut().0
    }

    /// The reason the drain began, once it has.
    pub(crate) fn reason(&self) -> Option<StopReason> {
        self.stage_mut().1
    }

    /// The admission whose flag and sweep the drain sets.
    pub(crate) fn admission(&self) -> Arc<Admission> {
        Arc::clone(&self.admission)
    }

    /// The hard exits this process arms.
    pub(crate) fn exits(&self) -> &HardExit {
        &self.exits
    }

    /// The host listens: from now on a stop drains, and an auto-launched
    /// host's idle interval starts, not at the drain's construction, which
    /// the held-lock wait and the host's own construction come after.
    /// Answers false where a stop already ended the start.
    pub(crate) fn listening(&self) -> bool {
        let mut stage = self.stage_mut();
        if stage.0 != Stage::Starting {
            return false;
        }
        stage.0 = Stage::Serving;
        self.drain.touch();
        true
    }

    /// Run `while_starting` under the stage's lock if the host is not yet
    /// listening, so the start cannot begin listening meanwhile: the signal
    /// thread's exit before listening. Answers whether it ran.
    pub(crate) fn if_starting(&self, while_starting: impl FnOnce()) -> bool {
        let stage = self.stage_mut();
        if stage.0 != Stage::Starting {
            return false;
        }
        while_starting();
        drop(stage);
        true
    }

    /// **Begin the drain**, for `reason`, where the host serves and no drain
    /// has begun (the module doc's five steps).
    pub(crate) fn begin_drain(&self, reason: StopReason) -> Began {
        {
            let mut stage = self.stage_mut();
            match stage.0 {
                Stage::Starting => return Began::NotListening,
                Stage::Draining => return Began::AlreadyDraining,
                Stage::Serving => *stage = (Stage::Draining, Some(reason)),
            }
        }
        let deadline = self.drain.begin();
        self.exits.arm(deadline + SHUTDOWN_MARGIN, reason.status());
        self.admission.begin_drain();
        log(format_args!("draining: {reason}"));
        self.begun.store(true, Ordering::SeqCst);
        self.stopping.notify_one();
        Began::Drain
    }

    /// Completes once a drain has begun, all five of its steps made: the
    /// stage alone turns `draining` before the deadline is set, so it is not
    /// what this reads. A permit is kept, so a drain begun before the serving
    /// loop waits is not lost.
    pub(crate) async fn stopping(&self) {
        if self.begun.load(Ordering::SeqCst) {
            return;
        }
        self.stopping.notified().await;
    }

    /// The serving loop closed the listener.
    pub(crate) fn listener_closed(&self) {
        self.listener_closed.send_replace(true);
    }

    /// Completes once the listener is closed.
    pub(crate) async fn until_listener_closed(&self) {
        let mut closed = self.listener_closed.subscribe();
        let _ = closed.wait_for(|closed| *closed).await;
    }

    /// When the drain's hard exit fires, once a drain has begun.
    #[cfg(test)]
    pub(crate) fn exit_at(&self) -> Option<(std::time::Instant, u8)> {
        self.exits.armed_at()
    }
}

/// A lifecycle for this crate's cases: serving, with an inert hard exit.
#[cfg(test)]
pub(crate) fn serving_for_tests(drain: Arc<Drain>, admission: Arc<Admission>) -> Arc<Lifecycle> {
    let lifecycle = Arc::new(Lifecycle::new(drain, admission, HardExit::inert()));
    assert!(lifecycle.listening());
    lifecycle
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn lifecycle() -> (Arc<Lifecycle>, Arc<Admission>) {
        let admission = Arc::new(Admission::default());
        let lifecycle = Arc::new(Lifecycle::new(
            Arc::new(Drain::new(Duration::from_secs(3))),
            Arc::clone(&admission),
            HardExit::inert(),
        ));
        (lifecycle, admission)
    }

    /// Before the host listens nothing drains; once it does, the first stop
    /// begins the drain — the stage, the deadline, the hard exit at the
    /// deadline plus the margin with the reason's status, the admission's
    /// flag — and every later one is a no-op.
    #[test]
    fn the_first_stop_while_serving_begins_the_one_drain() {
        let (lifecycle, admission) = lifecycle();
        assert_eq!(
            lifecycle.begin_drain(StopReason::Terminate),
            Began::NotListening
        );
        assert!(!lifecycle.drain.has_begun());
        let before = std::time::Instant::now();
        assert!(lifecycle.listening());
        assert!(
            lifecycle.drain.idle_since().expect("idle") >= before,
            "listening starts the idle interval"
        );
        assert!(!lifecycle.listening(), "listening is entered once");

        assert_eq!(
            lifecycle.begin_drain(StopReason::WorkerStopped),
            Began::Drain
        );
        assert_eq!(lifecycle.stage(), Stage::Draining);
        assert!(admission.draining());
        let deadline = lifecycle.drain.deadline().expect("a deadline");
        assert_eq!(
            lifecycle.exit_at(),
            Some((deadline + SHUTDOWN_MARGIN, REFUSED))
        );
        assert_eq!(
            lifecycle.begin_drain(StopReason::Terminate),
            Began::AlreadyDraining
        );
        assert_eq!(lifecycle.reason(), Some(StopReason::WorkerStopped));
    }

    #[test]
    fn a_signal_a_drain_request_or_the_idle_exit_exits_0_and_the_rest_1() {
        for reason in [
            StopReason::Terminate,
            StopReason::Interrupt,
            StopReason::Hangup,
            StopReason::DrainRequest,
            StopReason::Idle,
        ] {
            assert_eq!(reason.status(), 0, "{reason}");
        }
        for reason in [StopReason::LockTaken, StopReason::WorkerStopped] {
            assert_eq!(reason.status(), 1, "{reason}");
        }
    }

    /// **The serving loop is not woken by the stage alone.** A stop on the
    /// signal thread turns the stage `draining` before it sets the deadline
    /// the loop reads once woken, so the loop waits for the whole beginning.
    #[test]
    fn a_stage_turned_draining_before_its_deadline_wakes_no_one() {
        let (lifecycle, _) = lifecycle();
        lifecycle.listening();
        // Where a stop on another thread stands between its first two steps.
        *lifecycle.stage_mut() = (Stage::Draining, Some(StopReason::Terminate));
        let mut stopping = std::pin::pin!(lifecycle.stopping());
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(
            stopping.as_mut().poll(&mut context).is_pending(),
            "the loop woke before the drain had a deadline"
        );
        *lifecycle.stage_mut() = (Stage::Serving, None);
        lifecycle.begin_drain(StopReason::Terminate);
        assert!(lifecycle.drain.deadline().is_some());
        assert!(stopping.as_mut().poll(&mut context).is_ready());
    }

    /// A drain begun before anyone waits is still seen.
    #[tokio::test(flavor = "current_thread")]
    async fn a_drain_begun_before_the_loop_waits_is_not_lost() {
        let (lifecycle, _) = lifecycle();
        lifecycle.listening();
        lifecycle.begin_drain(StopReason::Idle);
        tokio::time::timeout(Duration::from_secs(1), lifecycle.stopping())
            .await
            .expect("the drain is seen");
    }
}
