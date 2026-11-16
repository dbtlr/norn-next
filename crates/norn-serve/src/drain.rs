//! What every part of a serving host reads about its drain: whether it has
//! begun, its deadline, and which requests are still in flight.
//!
//! **A request is in flight until its answer is flushed**, not until the
//! route has it: the drain waits for the answer to reach the socket, then
//! half-closes the connection so the answer is not lost to a reset at exit
//! (`connection.rs`). A request counts from the moment its head reaches the
//! route; its answer's flush, or its connection's end, takes it off.
//!
//! **The drain deadline is process-wide and set once**, when the drain
//! begins: `drain_bound` from then. Each request still running at it is
//! answered by kind (`request.rs`, `wire.rs`): an apply from its abandonment,
//! a synchronous mutation `serve/outcome-unknown`, and a read not at all — its
//! connection closes with the process. A read so cut off is counted apart, so
//! the drain does not wait for an answer that will never come.
//!
//! **Activity, for the idle exit**, is the last moment a connection was
//! accepted, a hello answered or a request taken off the in-flight count;
//! while any request is in flight the host is not idle.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::{Notify, watch};

/// The drain's shared state.
#[derive(Debug)]
pub(crate) struct Drain {
    /// `drain_bound`, already held to its cap.
    bound: Duration,
    deadline: OnceLock<Instant>,
    began: watch::Sender<bool>,
    /// Requests in flight: taken up by a route and not yet flushed.
    requests: AtomicUsize,
    /// Of those, reads still running past the deadline, which are never
    /// answered.
    cut_off: AtomicUsize,
    /// Woken whenever either count falls.
    settled: Notify,
    last_activity: Mutex<Instant>,
}

impl Drain {
    /// A drain not yet begun, whose bound is `bound`.
    pub(crate) fn new(bound: Duration) -> Drain {
        Drain {
            bound,
            deadline: OnceLock::new(),
            began: watch::Sender::new(false),
            requests: AtomicUsize::new(0),
            cut_off: AtomicUsize::new(0),
            settled: Notify::new(),
            last_activity: Mutex::new(Instant::now()),
        }
    }

    /// Begin the drain, once: set its deadline and wake whoever waits for
    /// it. Answers the deadline.
    pub(crate) fn begin(&self) -> Instant {
        let deadline = *self.deadline.get_or_init(|| Instant::now() + self.bound);
        self.began.send_replace(true);
        deadline
    }

    pub(crate) fn has_begun(&self) -> bool {
        self.deadline.get().is_some()
    }

    /// The drain's deadline, once it has begun.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.deadline.get().copied()
    }

    /// Completes once the drain has begun.
    pub(crate) async fn begun(&self) {
        let mut began = self.began.subscribe();
        // The sender lives as long as `self`, so the wait ends only by the
        // flag; an error is a sender gone, which it is not.
        let _ = began.wait_for(|began| *began).await;
    }

    /// Completes once the drain has begun and its deadline has passed.
    pub(crate) async fn bound_passed(&self) {
        self.begun().await;
        if let Some(deadline) = self.deadline() {
            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
        }
    }

    /// A request was taken up.
    pub(crate) fn request_started(&self) {
        self.requests.fetch_add(1, Ordering::SeqCst);
    }

    /// A request's answer was flushed, or its connection ended.
    pub(crate) fn request_ended(&self) {
        self.touch();
        self.requests.fetch_sub(1, Ordering::SeqCst);
        self.settled.notify_waiters();
    }

    /// A read still running at the deadline, which the drain does not wait
    /// for while the returned mark is held.
    pub(crate) fn cut_off(&self) -> CutOff<'_> {
        self.cut_off.fetch_add(1, Ordering::SeqCst);
        self.settled.notify_waiters();
        CutOff(self)
    }

    /// Whether every request in flight is answered and flushed, cut-off reads
    /// aside.
    pub(crate) fn settled(&self) -> bool {
        self.requests.load(Ordering::SeqCst) <= self.cut_off.load(Ordering::SeqCst)
    }

    /// Wait until [`settled`](Self::settled) holds or `until` passes;
    /// answers whether it holds.
    pub(crate) async fn settle_by(&self, until: Instant) -> bool {
        let until = tokio::time::Instant::from_std(until);
        loop {
            let woken = self.settled.notified();
            if self.settled() {
                return true;
            }
            tokio::select! {
                () = woken => {}
                () = tokio::time::sleep_until(until) => return self.settled(),
            }
        }
    }

    /// Something a client did: a connection accepted, a hello answered.
    pub(crate) fn touch(&self) {
        *self
            .last_activity
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Instant::now();
    }

    /// When the host last saw activity, or `None` while a request is in
    /// flight.
    pub(crate) fn idle_since(&self) -> Option<Instant> {
        if self.requests.load(Ordering::SeqCst) > 0 {
            return None;
        }
        Some(
            *self
                .last_activity
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Requests in flight now.
    #[cfg(test)]
    pub(crate) fn in_flight(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

/// One read cut off at the deadline. A read that answers after all takes its
/// mark back, and is then waited for as any other request.
#[derive(Debug)]
pub(crate) struct CutOff<'a>(&'a Drain);

impl Drop for CutOff<'_> {
    fn drop(&mut self) {
        self.0.cut_off.fetch_sub(1, Ordering::SeqCst);
        self.0.settled.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deadline_is_set_once_at_the_bound_from_the_first_begin() {
        let drain = Drain::new(Duration::from_secs(3));
        assert!(!drain.has_begun());
        assert_eq!(drain.deadline(), None);
        let before = Instant::now();
        let deadline = drain.begin();
        assert!(deadline >= before + Duration::from_secs(3));
        assert_eq!(drain.begin(), deadline, "a second begin moves nothing");
        assert!(drain.has_begun());
    }

    /// The drain waits for every request but a cut-off read, and a cut-off
    /// read that answers after all is waited for again.
    #[tokio::test(flavor = "current_thread")]
    async fn a_cut_off_read_is_not_waited_for_until_it_answers() {
        let drain = Drain::new(Duration::from_secs(1));
        drain.request_started();
        drain.request_started();
        assert!(!drain.settled());
        drain.request_ended();
        let cut = drain.cut_off();
        assert!(drain.settled());
        assert!(
            drain
                .settle_by(Instant::now() + Duration::from_millis(10))
                .await
        );
        drop(cut);
        assert!(!drain.settled(), "an answered read is in flight again");
        drain.request_ended();
        assert!(drain.settled());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn settling_ends_at_its_bound_when_a_request_stays() {
        let drain = Drain::new(Duration::from_secs(1));
        drain.request_started();
        let until = Instant::now() + Duration::from_millis(30);
        assert!(!drain.settle_by(until).await);
        assert!(Instant::now() >= until);
    }

    /// A host with a request in flight is not idle, and the request's end is
    /// activity.
    #[test]
    fn a_request_in_flight_is_activity_and_its_end_restarts_the_idle_clock() {
        let drain = Drain::new(Duration::from_secs(1));
        let idle = drain.idle_since().expect("idle with nothing in flight");
        drain.request_started();
        assert_eq!(drain.idle_since(), None);
        drain.request_ended();
        assert!(drain.idle_since().expect("idle again") >= idle);
    }
}
