//! The hard deadlines: the start-bound watchdog and the shutdown margin's
//! end, each an `_exit` at a moment, with no I/O before it.
//!
//! **One timer thread holds the armed moment.** It is its own thread, never
//! the I/O thread or a pool's, so nothing a stalled runtime, a host call or a
//! stderr pipe nobody reads is doing can keep it from firing; and it writes
//! nothing before it exits, since a write could block on that same pipe. What
//! the process was waiting on was logged when it was entered.
//!
//! Declared: a process stopped by SIGSTOP holds every wall-clock bound in
//! abeyance, this one included.

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Instant;

/// The exit armed now, if any: when, and with what status.
type Armed = Option<(Instant, u8)>;

/// The timer thread's armed moment.
pub(crate) struct HardExit {
    armed: Mutex<Armed>,
    changed: Condvar,
    /// What firing does: `_exit` in a host; a record in this crate's tests.
    fire: Box<dyn Fn(u8) + Send + Sync>,
}

impl HardExit {
    /// The timer of a host process, whose firing is `_exit`.
    pub(crate) fn start() -> std::io::Result<Arc<HardExit>> {
        HardExit::with(Box::new(|status| crate::sys::exit_now(status)))
    }

    /// A timer whose firing is `fire`, on a thread of its own.
    pub(crate) fn with(fire: Box<dyn Fn(u8) + Send + Sync>) -> std::io::Result<Arc<HardExit>> {
        let exit = Arc::new(HardExit {
            armed: Mutex::new(None),
            changed: Condvar::new(),
            fire,
        });
        let timer = Arc::clone(&exit);
        std::thread::Builder::new()
            .name("norn-exit".into())
            .spawn(move || timer.run())?;
        Ok(exit)
    }

    /// A timer that never fires and has no thread, for the cases that build a
    /// host's serving state without being a host process.
    #[cfg(test)]
    pub(crate) fn inert() -> Arc<HardExit> {
        Arc::new(HardExit {
            armed: Mutex::new(None),
            changed: Condvar::new(),
            fire: Box::new(|_| {}),
        })
    }

    /// Exit with `status` at `at`, unless an earlier exit is armed already.
    pub(crate) fn arm(&self, at: Instant, status: u8) {
        let mut armed = self.armed();
        if armed.is_none_or(|(earlier, _)| at < earlier) {
            *armed = Some((at, status));
            self.changed.notify_all();
        }
    }

    /// Exit with `status` now, from the timer thread.
    pub(crate) fn now(&self, status: u8) {
        *self.armed() = Some((Instant::now(), status));
        self.changed.notify_all();
    }

    /// Stand the armed exit down: the watchdog, once the host listens.
    pub(crate) fn disarm(&self) {
        *self.armed() = None;
        self.changed.notify_all();
    }

    /// What is armed, for this crate's tests.
    #[cfg(test)]
    pub(crate) fn armed_at(&self) -> Armed {
        *self.armed()
    }

    fn armed(&self) -> std::sync::MutexGuard<'_, Armed> {
        self.armed.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn run(&self) {
        let mut armed = self.armed();
        loop {
            match *armed {
                None => {
                    armed = self
                        .changed
                        .wait(armed)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                Some((at, status)) => {
                    let now = Instant::now();
                    if now >= at {
                        // A host's firing does not return. A test's records
                        // it, and the timer stands down.
                        *armed = None;
                        (self.fire)(status);
                        continue;
                    }
                    armed = self
                        .changed
                        .wait_timeout(armed, at - now)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    fn recorded() -> (Arc<HardExit>, mpsc::Receiver<(Instant, u8)>) {
        let (fired, firings) = mpsc::channel();
        let fired = Mutex::new(fired);
        let exit = HardExit::with(Box::new(move |status| {
            let _ = fired.lock().unwrap().send((Instant::now(), status));
        }))
        .unwrap();
        (exit, firings)
    }

    #[test]
    fn an_armed_exit_fires_at_its_moment_with_its_status() {
        let (exit, firings) = recorded();
        let at = Instant::now() + Duration::from_millis(30);
        exit.arm(at, 1);
        let (fired, status) = firings.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(fired >= at);
        assert_eq!(status, 1);
    }

    #[test]
    fn a_disarmed_exit_does_not_fire_and_the_earlier_of_two_wins() {
        let (exit, firings) = recorded();
        // A distant moment, so the timer cannot fire before the disarm: the
        // disarm clears it, and a later moment armed after it stands, where
        // the earlier would otherwise have won.
        let distant = Instant::now() + Duration::from_secs(60);
        exit.arm(distant, 1);
        exit.disarm();
        assert_eq!(exit.armed_at(), None);
        let later = distant + Duration::from_secs(60);
        exit.arm(later, 1);
        assert_eq!(exit.armed_at(), Some((later, 1)));
        exit.disarm();
        assert!(firings.recv_timeout(Duration::from_millis(100)).is_err());

        exit.arm(Instant::now() + Duration::from_secs(60), 1);
        exit.arm(Instant::now() + Duration::from_millis(10), 0);
        exit.arm(Instant::now() + Duration::from_secs(30), 1);
        let (_, status) = firings.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(status, 0);
    }

    #[test]
    fn an_exit_now_fires_at_once() {
        let (exit, firings) = recorded();
        exit.arm(Instant::now() + Duration::from_secs(60), 0);
        exit.now(1);
        let (_, status) = firings.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(status, 1);
    }
}
