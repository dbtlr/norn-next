//! What a running host is asked to stop by, from inside the process.
//!
//! **A stopped worker stops the host.** Outside a drain, a host whose worker
//! pool has gone can no longer serve, so rather than answer every later
//! request as worth asking again, the route that met it logs that the worker
//! stopped and that the host is exiting to be restarted, and triggers the
//! stop, which exits 1. The trigger is one-shot: the first request to meet
//! the stopped worker logs and triggers, and the rest only answer.
//!
//! **The seam the bounded drain takes over.** Today the serving loop meets the
//! trigger with the plain stop every signal takes, and the drain that answers
//! what is still in flight is a later task: that task routes
//! [`Stopper::requested`] through it, and nothing here changes.

use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

use crate::log::log;

/// The one-shot stop trigger a host's routes share with its serving loop.
#[derive(Debug, Default)]
pub(crate) struct Stopper {
    triggered: AtomicBool,
    requested: Notify,
}

impl Stopper {
    /// The host's worker stopped: log it once, and ask the serving loop to
    /// stop the process with exit 1. Answers whether this call was the one
    /// that triggered it.
    pub(crate) fn worker_stopped(&self, detail: &str) -> bool {
        if self.triggered.swap(true, Ordering::SeqCst) {
            return false;
        }
        log(format_args!(
            "the host's worker stopped ({detail}); exiting so the service manager or the next \
             client can start the host again"
        ));
        // A permit is kept when the loop is not yet waiting, so the trigger
        // is never lost.
        self.requested.notify_one();
        true
    }

    /// Whether the stop was triggered.
    #[cfg(test)]
    pub(crate) fn triggered(&self) -> bool {
        self.triggered.load(Ordering::SeqCst)
    }

    /// Completes once the stop is triggered.
    pub(crate) async fn requested(&self) {
        self.requested.notified().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first trigger is the one that logs and wakes the loop, and a
    /// trigger made before the loop waits is not lost.
    #[tokio::test(flavor = "current_thread")]
    async fn the_trigger_fires_once_and_is_kept_until_the_loop_waits() {
        let stopper = Stopper::default();
        assert!(stopper.worker_stopped("the host worker pool stopped"));
        assert!(!stopper.worker_stopped("the host worker pool stopped"));
        assert!(stopper.triggered());
        tokio::time::timeout(std::time::Duration::from_secs(1), stopper.requested())
            .await
            .expect("a trigger made before the wait wakes it");
    }
}
