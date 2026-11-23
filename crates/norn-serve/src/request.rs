//! One wire request's life once its body has arrived: its claim, its vault's
//! mutation turn, its pool, its dispatches and the readiness retry between
//! them, and the one answer it ends in.
//!
//! **Every wait ends at one deadline: the first dispatch + `ready_wait`.** The
//! wait for a vault's mutation turn, for a pool thread and the readiness
//! retry's pauses all share it, and each is an async wait on the I/O runtime,
//! never a sleep on a blocking thread. Past it the request answers as its
//! claim's `expired` row says: the last `host/entry-not-ready` an attempt
//! answered while the entry was warming, or `serve/busy` where none did.
//!
//! **The readiness retry is the route's, not the host's.** It re-dispatches
//! only a `host/entry-not-ready` whose detail is `Warming` — the entry is
//! building, healing or bringing its coverage back, so the host ran nothing —
//! after 50 ms, doubling to a 1 s cap. `Unattached` and every other answer
//! pass through at once, and nothing is dispatched after an answer.
//!
//! **At the drain's bound each request still running is answered by its
//! kind** ([`AtBound`]), each answer the drain's `started → answered`: an
//! apply answers from its abandonment, which its own pool thread makes within
//! one wait slice (`wire.rs`); a synchronous mutation is answered
//! `serve/outcome-unknown` by the route, and the answer its host call gives
//! later is discarded; a read is not answered — its connection closes with
//! the process — and is marked cut off, so the drain does not wait for it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use hyper::body::Bytes;
use norn_wire::{ErrorDetail, ErrorEnvelope, NotReady, VaultName, Verb};
use tokio::sync::futures::Notified;
use tokio::sync::oneshot;
use tokio::time::Instant;

use crate::admission::{Admission, Admitted, Claim, ClaimState, InFlight, Semaphores, Turn};
use crate::drain::Drain;
use crate::pool::Pool;

/// The readiness retry's first pause.
pub(crate) const FIRST_PAUSE: Duration = Duration::from_millis(50);

/// The longest pause the readiness retry doubles to.
pub(crate) const PAUSE_CAP: Duration = Duration::from_secs(1);

/// What one dispatch to the host answered.
#[derive(Debug)]
pub(crate) enum Dispatched {
    /// The verb's report, encoded as JSON.
    Report(Vec<u8>),
    /// The host refused, with its envelope.
    Refused(ErrorEnvelope),
    /// The body did not parse as the verb's params; nothing was called.
    BadRequest(String),
    /// The host can no longer serve: its worker stopped. Whether the call
    /// took effect is not known.
    HostStopped(String),
    /// An answer a per-request dispatcher already composed for its own
    /// protocol. The wire route's dispatcher never
    /// answers it.
    Composed(Composed),
}

/// An answer composed on the pool thread, and the length of the verb report it
/// carries, where it carries one: what `serve.report_bytes` adds.
#[derive(Debug)]
pub(crate) struct Composed {
    pub(crate) answer: norn_mcp::Answer,
    pub(crate) report_bytes: Option<usize>,
}

/// What answers a dispatch: the route table over a host in production, a
/// scripted one in this crate's tests.
pub(crate) trait Dispatch: Send + Sync + 'static {
    /// Parse `body` as `verb`'s params, call the host, and encode its answer.
    fn dispatch(&self, verb: Verb, body: &[u8]) -> Dispatched;
}

/// Which pool a request runs on, and for a mutation the vault whose turn it
/// takes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Lane {
    Read,
    Mutation(VaultName),
}

/// What the drain does with a request still running at its bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AtBound {
    /// A read: its connection closes with the process.
    CloseWithProcess,
    /// An apply holding a `PendingApply`: its pool thread abandons it and
    /// answers from the record.
    Abandon,
    /// A synchronous mutation, which holds nothing the route can abandon:
    /// answered `serve/outcome-unknown`.
    OutcomeUnknown,
}

/// How a request ended, for the route to answer.
#[derive(Debug)]
pub(crate) enum Ended {
    /// The last attempt's answer.
    Dispatched(Dispatched),
    /// Nothing ran: the drain began while the request waited.
    Draining,
    /// Nothing ran: the deadline passed while the request waited, and no
    /// attempt answered `Warming`.
    Busy,
    /// Nothing ran past this answer: the deadline passed after an attempt
    /// answered `Warming`, and this is that answer.
    StillWarming(ErrorEnvelope),
    /// The call was taken up and its answer never came: the host call
    /// unwound. Whether it took effect is not known.
    Lost,
    /// A synchronous mutation still running at the drain's bound: whether it
    /// took effect is not known.
    StillRunningAtBound,
}

/// The route's cumulative counts, read by the counters route.
#[derive(Debug, Default)]
pub(crate) struct WireCounters {
    /// Host calls made: each pool closure that won `waiting → started`.
    pub(crate) dispatches: AtomicU64,
    /// Of those, the re-dispatches the readiness retry made.
    pub(crate) ready_retries: AtomicU64,
    /// Entries of a connection list a request walked. The route keeps no
    /// list of connections and walks none, so this stays zero; it is the
    /// counter a change that adds such a walk moves, beside the host's
    /// `host.request.registrations_visited`.
    #[cfg_attr(not(feature = "counters"), allow(dead_code))]
    pub(crate) connections_visited: AtomicU64,
    /// Bytes of every wire answer body sent, reports and refusals alike.
    pub(crate) body_bytes_encoded: AtomicU64,
    /// Bytes of the verb reports among them, as the route table encoded them.
    pub(crate) report_bytes: AtomicU64,
}

/// What every wire request of one host is served by.
pub(crate) struct Wire {
    pub(crate) admission: Arc<Admission>,
    pub(crate) semaphores: Arc<Semaphores>,
    pub(crate) reads: Pool,
    pub(crate) mutations: Pool,
    /// The route table over this process's host: what the wire route
    /// dispatches through, and what `/mcp`'s per-request wrapper calls.
    pub(crate) dispatcher: Arc<dyn Dispatch>,
    pub(crate) ready_wait: Duration,
    pub(crate) counters: Arc<WireCounters>,
    pub(crate) drain: Arc<Drain>,
}

/// One request's run, from admission to its one answer, and when its first
/// dispatch was made.
pub(crate) struct Run {
    pub(crate) ended: Ended,
    /// When the route first dispatched it, which is where its deadline and
    /// its host time are measured from.
    #[cfg_attr(not(feature = "counters"), allow(dead_code))]
    pub(crate) dispatched_at: Instant,
}

/// What one request holds while any of its work could still run: its verb
/// and body, its in-flight slot, its claim's registration with the drain's
/// sweep, and for a mutation its vault's turn.
///
/// **The request and each job queued for it share it, and it is given back
/// when the last of them lets go.** A request whose client disconnected is
/// dropped by hyper mid-wait, and leaves its share with a job that is queued
/// or running: the vault's turn, the in-flight slot and the sweep's view of
/// the claim then last until that job's host call has returned, or until a
/// thread has taken the job up and it ran nothing. A queued job's body stays
/// counted under the in-flight bound with it.
struct Held {
    verb: Verb,
    body: Bytes,
    dispatcher: Arc<dyn Dispatch>,
    admitted: Admitted,
    _slot: InFlight,
    _turn: Option<Turn>,
}

/// Ends the claim of a request that goes unanswered. hyper drops a request's
/// future when its client disconnects, and a claim still `waiting` then moves
/// to `refused`, the end in which nothing ran, so a job queued for it calls
/// nothing. A claim already `started` is left to its job, which holds the
/// request's [`Held`] until its host call returns. Either way no one is
/// answered. A request that ended answered finds its claim already ended, and
/// this moves nothing.
struct Unanswered {
    claim: Arc<Claim>,
    /// Where this crate's tests read that a request went unanswered.
    #[cfg(test)]
    admission: Arc<Admission>,
}

impl Drop for Unanswered {
    fn drop(&mut self) {
        let refused = self.claim.shift(ClaimState::Waiting, ClaimState::Refused);
        #[cfg(test)]
        if refused || self.claim.state() == ClaimState::Started {
            self.admission.note_unanswered();
        }
        #[cfg(not(test))]
        let _ = refused;
    }
}

impl Wire {
    /// Serve one request whose body has arrived whole, under the in-flight
    /// `slot` it was admitted with; `at_bound` says how the drain answers it
    /// if it is still running at the drain's bound. Each of its dispatches is
    /// made through `dispatcher`: the route table itself for the wire route,
    /// or a wrapper around it that `/mcp` composes per request.
    pub(crate) async fn serve(
        &self,
        verb: Verb,
        lane: Lane,
        at_bound: AtBound,
        body: Bytes,
        slot: InFlight,
        dispatcher: Arc<dyn Dispatch>,
    ) -> Run {
        let dispatched_at = Instant::now();
        let deadline = dispatched_at + self.ready_wait;
        let ended = self
            .run(verb, lane, at_bound, body, slot, dispatcher, deadline)
            .await;
        Run {
            ended,
            dispatched_at,
        }
    }

    #[allow(clippy::too_many_arguments)] // One request's parts, each its own.
    async fn run(
        &self,
        verb: Verb,
        lane: Lane,
        at_bound: AtBound,
        body: Bytes,
        slot: InFlight,
        dispatcher: Arc<dyn Dispatch>,
        deadline: Instant,
    ) -> Ended {
        let admitted = self.admission.admit();
        let claim = Arc::clone(&admitted.claim);
        let _unanswered = Unanswered {
            claim: Arc::clone(&claim),
            #[cfg(test)]
            admission: Arc::clone(&self.admission),
        };
        let (pool, mut turn) = match &lane {
            Lane::Read => (&self.reads, None),
            Lane::Mutation(vault) => (&self.mutations, Some(self.semaphores.of(vault))),
        };
        if let Some(turn) = turn.as_mut() {
            // The turn is waited for under the same deadline and drain as
            // every other wait; whatever wakes it, the re-check decides, and
            // the request runs only once it holds its vault's turn. The
            // claim entered `waiting` at admission, so the first pass is
            // that entry's re-read.
            loop {
                // Made before the flag is read, so a sweep that finishes
                // between the read and the wait still wakes it.
                let swept = self.admission.swept();
                if let Some(ended) = self.recheck(&claim, deadline, None) {
                    return ended;
                }
                tokio::select! {
                    biased;
                    () = turn.take() => break,
                    () = swept => {}
                    () = tokio::time::sleep_until(deadline) => {}
                }
            }
        }
        let held = Arc::new(Held {
            verb,
            body,
            dispatcher,
            admitted,
            _slot: slot,
            _turn: turn,
        });
        let mut last_warming: Option<ErrorEnvelope> = None;
        let mut pause = FIRST_PAUSE;
        let mut attempts = 0u64;
        loop {
            // Every dispatch is preceded by the re-check: the claim is
            // `waiting` here, by admission or by the reset after `Warming`.
            // The sweep's wake is made first, before the flag is read.
            let swept = self.admission.swept();
            if let Some(ended) = self.recheck(&claim, deadline, last_warming.as_ref()) {
                return ended;
            }
            let answer = match self
                .attempt(pool, &held, swept, deadline, attempts > 0, at_bound)
                .await
            {
                Attempt::Answered(answer) => answer,
                Attempt::Expired => return expired(last_warming.as_ref()),
                Attempt::Ended(ended) => return ended,
            };
            attempts += 1;
            match answer {
                Dispatched::Refused(envelope) if is_warming(&envelope) => {
                    // The host ran nothing: reset, or end, by the same order
                    // of reads every entry into `waiting` makes.
                    if self.admission.draining() {
                        claim.shift(ClaimState::Started, ClaimState::Refused);
                        return Ended::Draining;
                    }
                    if Instant::now() >= deadline {
                        claim.shift(ClaimState::Started, ClaimState::Expired);
                        return Ended::StillWarming(envelope);
                    }
                    let swept = self.admission.swept();
                    claim.shift(ClaimState::Started, ClaimState::Waiting);
                    last_warming = Some(envelope);
                    if let Some(ended) = self.recheck(&claim, deadline, last_warming.as_ref()) {
                        return ended;
                    }
                    let wake = (Instant::now() + pause).min(deadline);
                    tokio::select! {
                        biased;
                        () = swept => {}
                        () = tokio::time::sleep_until(wake) => {}
                    }
                    pause = (pause * 2).min(PAUSE_CAP);
                }
                answer => {
                    claim.shift(ClaimState::Started, ClaimState::Answered);
                    return Ended::Dispatched(answer);
                }
            }
        }
    }

    /// The re-read every entry into `waiting` is followed by: the draining
    /// flag, then the deadline. Answers how the request ends where either
    /// ended it — by this route's own move, or by the sweep's or the timer's
    /// that won it first — and nothing where the claim is still `waiting`.
    fn recheck(
        &self,
        claim: &Claim,
        deadline: Instant,
        last_warming: Option<&ErrorEnvelope>,
    ) -> Option<Ended> {
        if self.admission.draining() {
            claim.shift(ClaimState::Waiting, ClaimState::Refused);
        }
        if Instant::now() >= deadline {
            claim.shift(ClaimState::Waiting, ClaimState::Expired);
        }
        match claim.state() {
            ClaimState::Waiting => None,
            ClaimState::Refused => Some(Ended::Draining),
            ClaimState::Expired => Some(expired(last_warming)),
            // Only the pool closure moves a waiting claim to `started`, and
            // it is not yet queued when this runs.
            ClaimState::Started | ClaimState::Answered => None,
        }
    }

    /// One dispatch: queue the call, and wait for its answer, the deadline,
    /// the drain's sweep or the drain's bound, whichever ends the wait first.
    /// `swept` was made before the re-check that preceded this dispatch read
    /// the flag.
    async fn attempt(
        &self,
        pool: &Pool,
        held: &Arc<Held>,
        swept: Notified<'_>,
        deadline: Instant,
        retry: bool,
        at_bound: AtBound,
    ) -> Attempt {
        let claim = &held.admitted.claim;
        let (answer, answered) = oneshot::channel();
        {
            let held = Arc::clone(held);
            let counters = Arc::clone(&self.counters);
            let deadline = deadline.into_std();
            pool.submit(Box::new(move || {
                // A job taken up past its deadline expires its claim itself:
                // the timer that would have can fire late, and the request
                // then answers as the `expired` row says. Only one of this
                // and the timer wins `waiting → expired`, and either way
                // nothing runs.
                if std::time::Instant::now() >= deadline {
                    held.admitted
                        .claim
                        .shift(ClaimState::Waiting, ClaimState::Expired);
                }
                // A claim the sweep, the timer, this job's own deadline check
                // or its request's going moved first has been answered
                // already, or has no one to answer, and its call runs
                // nothing.
                if !held
                    .admitted
                    .claim
                    .shift(ClaimState::Waiting, ClaimState::Started)
                {
                    drop(held);
                    drop(answer);
                    return;
                }
                counters.dispatches.fetch_add(1, Ordering::Relaxed);
                if retry {
                    counters.ready_retries.fetch_add(1, Ordering::Relaxed);
                }
                let dispatched = held.dispatcher.dispatch(held.verb, &held.body);
                // The host call has returned: this job's share goes before
                // the answer, so a request that has its answer holds the
                // only share left.
                drop(held);
                let _ = answer.send(dispatched);
            }));
        }
        let mut answered = answered;
        tokio::pin!(swept);
        let mut timed = true;
        let mut bound_met = false;
        // A read cut off at the drain's bound, which the drain does not wait
        // for while it is held; given back if the read answers after all.
        let mut cut_off = None;
        loop {
            tokio::select! {
                biased;
                result = &mut answered => {
                    drop(cut_off.take());
                    return match result {
                        Ok(dispatched) => Attempt::Answered(dispatched),
                        // The call was taken up, or the claim is no longer
                        // waiting, and no answer came.
                        Err(_) => match claim.state() {
                            ClaimState::Refused => Attempt::Ended(Ended::Draining),
                            ClaimState::Expired => Attempt::Expired,
                            _ => Attempt::Ended(Ended::Lost),
                        },
                    };
                }
                () = &mut swept => {
                    if claim.state() == ClaimState::Refused {
                        return Attempt::Ended(Ended::Draining);
                    }
                    // In flight: the sweep passes a started call by. A fresh
                    // wait would only wake for a second drain, which there is
                    // not, so this arm is spent, and the wait goes on for the
                    // answer or the drain's bound.
                    swept.set(self.admission.swept());
                }
                () = self.drain.bound_passed(), if !bound_met => {
                    bound_met = true;
                    match at_bound {
                        // The drain's `started → answered`: the host's later
                        // answer goes to a receiver no one reads, and the
                        // job's share of the request's hold lasts until that
                        // call returns.
                        AtBound::OutcomeUnknown => {
                            if claim.shift(ClaimState::Started, ClaimState::Answered) {
                                return Attempt::Ended(Ended::StillRunningAtBound);
                            }
                        }
                        AtBound::CloseWithProcess => cut_off = Some(self.drain.cut_off()),
                        // The pool thread answers from the abandonment within
                        // one wait slice.
                        AtBound::Abandon => {}
                    }
                }
                () = tokio::time::sleep_until(deadline), if timed => {
                    timed = false;
                    if claim.shift(ClaimState::Waiting, ClaimState::Expired) {
                        return Attempt::Expired;
                    }
                    if claim.state() == ClaimState::Refused {
                        return Attempt::Ended(Ended::Draining);
                    }
                    // Started before the deadline: in flight, and answered
                    // as usual.
                }
            }
        }
    }
}

enum Attempt {
    Answered(Dispatched),
    /// The deadline passed while the call waited for a thread, and it runs
    /// nothing: the request answers as its claim's `expired` row says.
    Expired,
    Ended(Ended),
}

/// A request whose deadline passed before anything ran: the last warming
/// refusal where an attempt met one, `serve/busy` otherwise.
fn expired(last_warming: Option<&ErrorEnvelope>) -> Ended {
    match last_warming {
        Some(envelope) => Ended::StillWarming(envelope.clone()),
        None => Ended::Busy,
    }
}

/// Whether `envelope` is a `host/entry-not-ready` the readiness retry asks
/// again: the entry is warming, in any phase. `Unattached` is not.
pub(crate) fn is_warming(envelope: &ErrorEnvelope) -> bool {
    matches!(
        envelope.detail(),
        ErrorDetail::EntryNotReady {
            state: NotReady::Warming { .. },
            ..
        }
    )
}

/// The most re-dispatches the readiness retry can make within `ready_wait`:
/// the number of pauses — 50, 100, 200, 400, 800 ms, then 1 s each — whose
/// running sum stays at or below it. Each attempt itself takes time too, so
/// a request makes at most this many, and the counter bar holds every
/// workload's `serve.ready_retries` to it, computing it by this formula.
#[cfg(test)]
pub(crate) fn most_redispatches(ready_wait: Duration) -> u64 {
    let mut pause = FIRST_PAUSE;
    let mut waited = Duration::ZERO;
    let mut count = 0;
    while waited + pause <= ready_wait {
        waited += pause;
        count += 1;
        pause = (pause * 2).min(PAUSE_CAP);
    }
    count
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    use norn_wire::{WarmingPhase, WarmingPhase::Healing};

    use super::*;
    use crate::pool::Pool;

    /// A host that answers each dispatch from a script, and counts them.
    struct Scripted {
        answers: Mutex<VecDeque<Dispatched>>,
        calls: AtomicUsize,
        /// Holds every call until released, for the cases that need a call
        /// to be in flight.
        hold: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    impl Scripted {
        fn new(answers: impl IntoIterator<Item = Dispatched>) -> Arc<Scripted> {
            Arc::new(Scripted {
                answers: Mutex::new(answers.into_iter().collect()),
                calls: AtomicUsize::new(0),
                hold: Mutex::new(None),
            })
        }
    }

    impl Dispatch for Scripted {
        fn dispatch(&self, _: Verb, _: &[u8]) -> Dispatched {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(hold) = self.hold.lock().unwrap().as_ref() {
                let _ = hold.recv();
            }
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Dispatched::Report(b"{}".to_vec()))
        }
    }

    fn warming(phase: WarmingPhase) -> Dispatched {
        Dispatched::Refused(ErrorEnvelope::new(
            "warming",
            ErrorDetail::entry_not_ready(NotReady::warming(phase, 0, None)),
        ))
    }

    fn unattached() -> Dispatched {
        Dispatched::Refused(ErrorEnvelope::new(
            "unattached",
            ErrorDetail::entry_not_ready(NotReady::unattached()),
        ))
    }

    fn wire(dispatcher: Arc<dyn Dispatch>, ready_wait: Duration, read_threads: usize) -> Wire {
        Wire {
            admission: Arc::new(Admission::default()),
            semaphores: Arc::new(Semaphores::default()),
            reads: Pool::new("test-read", read_threads).unwrap(),
            mutations: Pool::new("test-mutation", 2).unwrap(),
            dispatcher,
            ready_wait,
            counters: Arc::new(WireCounters::default()),
            drain: Arc::new(Drain::new(Duration::from_secs(10))),
        }
    }

    fn body() -> Bytes {
        Bytes::from_static(b"{}")
    }

    /// Serve `verb` on `lane` with an empty body, under a slot of its own:
    /// a read closes with the process at the drain's bound, and a mutation
    /// is an apply.
    async fn ask(wire: &Wire, verb: Verb, lane: Lane) -> Run {
        let at_bound = match lane {
            Lane::Read => AtBound::CloseWithProcess,
            Lane::Mutation(_) => AtBound::Abandon,
        };
        ask_at(wire, verb, lane, at_bound).await
    }

    /// Serve `verb` on `lane` as [`ask`] does, answered as `at_bound` says
    /// at the drain's bound.
    async fn ask_at(wire: &Wire, verb: Verb, lane: Lane, at_bound: AtBound) -> Run {
        let slot = wire.admission.enter().expect("a slot under the bound");
        wire.serve(
            verb,
            lane,
            at_bound,
            body(),
            slot,
            Arc::clone(&wire.dispatcher),
        )
        .await
    }

    fn counts(wire: &Wire) -> (u64, u64) {
        (
            wire.counters.dispatches.load(Ordering::SeqCst),
            wire.counters.ready_retries.load(Ordering::SeqCst),
        )
    }

    #[test]
    fn the_retry_bound_follows_the_pause_schedule() {
        assert_eq!(most_redispatches(Duration::from_millis(49)), 0);
        assert_eq!(most_redispatches(Duration::from_millis(50)), 1);
        assert_eq!(most_redispatches(Duration::from_millis(1550)), 5);
        assert_eq!(most_redispatches(Duration::from_millis(2550)), 6);
        assert_eq!(most_redispatches(Duration::from_secs(30)), 33);
    }

    /// A ready entry is one dispatch and no retry.
    #[tokio::test(flavor = "current_thread")]
    async fn a_ready_answer_is_one_dispatch() {
        let host = Scripted::new([Dispatched::Report(b"{\"ok\":1}".to_vec())]);
        let wire = wire(host.clone(), Duration::from_secs(5), 2);
        let run = ask(&wire, Verb::Find, Lane::Read).await;
        assert!(matches!(
            run.ended,
            Ended::Dispatched(Dispatched::Report(_))
        ));
        assert_eq!(counts(&wire), (1, 0));
    }

    /// A warming entry is asked again until it answers, at the paced
    /// schedule, and nothing is dispatched after the answer.
    #[tokio::test(flavor = "current_thread")]
    async fn a_warming_entry_is_retried_until_it_answers() {
        let host = Scripted::new([
            warming(WarmingPhase::InstallingCoverage),
            warming(Healing),
            warming(WarmingPhase::ReleasingCoverage),
            Dispatched::Report(b"{}".to_vec()),
        ]);
        let wire = wire(host.clone(), Duration::from_secs(5), 2);
        let started = Instant::now();
        let run = ask(&wire, Verb::Get, Lane::Read).await;
        assert!(matches!(
            run.ended,
            Ended::Dispatched(Dispatched::Report(_))
        ));
        assert_eq!(counts(&wire), (4, 3));
        // 50 + 100 + 200 ms of pauses.
        assert!(started.elapsed() >= Duration::from_millis(350));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            host.calls.load(Ordering::SeqCst),
            4,
            "no dispatch after an answer"
        );
    }

    /// `Unattached`, and every other refusal, passes through at once.
    #[tokio::test(flavor = "current_thread")]
    async fn a_refusal_that_is_not_warming_passes_through_at_once() {
        for answer in [
            unattached(),
            Dispatched::Refused(ErrorEnvelope::new(
                "parked",
                ErrorDetail::entry_held(VaultName::new("notes").unwrap()),
            )),
        ] {
            let host = Scripted::new([answer]);
            let wire = wire(host.clone(), Duration::from_secs(5), 2);
            let started = Instant::now();
            let run = ask(&wire, Verb::Get, Lane::Read).await;
            assert!(matches!(
                run.ended,
                Ended::Dispatched(Dispatched::Refused(_))
            ));
            assert_eq!(counts(&wire), (1, 0));
            assert!(started.elapsed() < FIRST_PAUSE);
        }
    }

    /// Past the deadline a warming entry's last refusal is the answer, and
    /// the retries stay within the bound the schedule gives.
    #[tokio::test(flavor = "current_thread")]
    async fn past_the_deadline_the_last_warming_refusal_is_the_answer() {
        let host = Scripted::new((0..100).map(|_| warming(Healing)));
        let ready_wait = Duration::from_millis(400);
        let wire = wire(host.clone(), ready_wait, 2);
        let run = ask(&wire, Verb::Get, Lane::Read).await;
        let Ended::StillWarming(envelope) = run.ended else {
            panic!("a warming entry past the deadline answered {:?}", run.ended);
        };
        assert!(is_warming(&envelope));
        let (dispatches, retries) = counts(&wire);
        assert_eq!(dispatches, retries + 1);
        assert!(retries <= most_redispatches(ready_wait), "{retries}");
        assert!(run.dispatched_at.elapsed() < ready_wait + Duration::from_millis(200));
    }

    /// A request that waited out its deadline for a pool thread is answered
    /// `serve/busy`, and its call never runs.
    #[tokio::test(flavor = "current_thread")]
    async fn a_request_that_waits_out_its_deadline_for_a_thread_is_busy_and_never_runs() {
        let host = Scripted::new([]);
        let (release, held) = std::sync::mpsc::channel();
        *host.hold.lock().unwrap() = Some(held);
        let wire = Arc::new(wire(host.clone(), Duration::from_millis(200), 1));
        let first = {
            let wire = Arc::clone(&wire);
            tokio::spawn(async move { ask(&wire, Verb::Find, Lane::Read).await })
        };
        // The one thread is held by the first call.
        while host.calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let second = ask(&wire, Verb::Find, Lane::Read).await;
        assert!(matches!(second.ended, Ended::Busy), "{:?}", second.ended);
        release.send(()).unwrap();
        let first = first.await.unwrap();
        assert!(
            matches!(first.ended, Ended::Dispatched(_)),
            "a started call is answered"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            host.calls.load(Ordering::SeqCst),
            1,
            "the expired call ran nothing"
        );
        assert_eq!(counts(&wire).0, 1);
    }

    /// **A queued call that a thread takes up after its deadline runs
    /// nothing**, even when the deadline's timer has not yet fired: the job
    /// itself moves the claim to `expired`, and the request is answered
    /// `serve/busy`. The runtime thread is blocked past the deadline so the
    /// timer cannot fire first.
    #[tokio::test(flavor = "current_thread")]
    async fn a_queued_call_taken_up_after_its_deadline_runs_nothing() {
        let host = Scripted::new([]);
        let (release, held) = std::sync::mpsc::channel();
        *host.hold.lock().unwrap() = Some(held);
        let ready_wait = Duration::from_millis(100);
        let wire = Arc::new(wire(host.clone(), ready_wait, 1));
        let first = {
            let wire = Arc::clone(&wire);
            tokio::spawn(async move { ask(&wire, Verb::Find, Lane::Read).await })
        };
        while host.calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Poll the second request once: its call is queued behind the held
        // thread, and its wait is pending.
        let second = ask(&wire, Verb::Find, Lane::Read);
        tokio::pin!(second);
        tokio::select! {
            biased;
            run = &mut second => panic!("answered before its call was taken up: {:?}", run.ended),
            () = std::future::ready(()) => {}
        }
        // Past the deadline with the runtime blocked, so no timer has run.
        std::thread::sleep(ready_wait + Duration::from_millis(50));
        // A second release, so a call that wrongly runs is not held.
        release.send(()).unwrap();
        release.send(()).unwrap();
        // The pool is first in, first out: once this job runs, the second
        // request's job has been taken up and has returned.
        let (ran, taken) = std::sync::mpsc::channel();
        wire.reads.submit(Box::new(move || ran.send(()).unwrap()));
        taken
            .recv_timeout(Duration::from_secs(10))
            .expect("the pool ran the queued jobs");
        assert_eq!(
            host.calls.load(Ordering::SeqCst),
            1,
            "a call taken up past its deadline ran"
        );
        let run = second.await;
        assert!(matches!(run.ended, Ended::Busy), "{:?}", run.ended);
        assert!(matches!(first.await.unwrap().ended, Ended::Dispatched(_)));
        assert_eq!(counts(&wire).0, 1);
    }

    /// The drain's sweep refuses a request waiting for its thread, and the
    /// call never runs; a request already started is in flight and answered.
    #[tokio::test(flavor = "current_thread")]
    async fn the_sweep_refuses_a_waiting_request_and_passes_a_started_one() {
        let host = Scripted::new([]);
        let (release, held) = std::sync::mpsc::channel();
        *host.hold.lock().unwrap() = Some(held);
        let wire = Arc::new(wire(host.clone(), Duration::from_secs(10), 1));
        let started = {
            let wire = Arc::clone(&wire);
            tokio::spawn(async move { ask(&wire, Verb::Find, Lane::Read).await })
        };
        while host.calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let waiting = {
            let wire = Arc::clone(&wire);
            tokio::spawn(async move { ask(&wire, Verb::Find, Lane::Read).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(wire.admission.begin_drain(), 1);
        let waiting = waiting.await.unwrap();
        assert!(
            matches!(waiting.ended, Ended::Draining),
            "{:?}",
            waiting.ended
        );
        release.send(()).unwrap();
        let started = started.await.unwrap();
        assert!(
            matches!(started.ended, Ended::Dispatched(_)),
            "{:?}",
            started.ended
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(host.calls.load(Ordering::SeqCst), 1);
    }

    /// A request in its readiness pause when the drain begins is refused,
    /// with no re-dispatch.
    #[tokio::test(flavor = "current_thread")]
    async fn a_request_in_its_readiness_pause_is_refused_by_the_drain() {
        let host = Scripted::new((0..100).map(|_| warming(Healing)));
        let wire = Arc::new(wire(host.clone(), Duration::from_secs(10), 2));
        let pausing = {
            let wire = Arc::clone(&wire);
            tokio::spawn(async move { ask(&wire, Verb::Get, Lane::Read).await })
        };
        while host.calls.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        wire.admission.begin_drain();
        let ended = pausing.await.unwrap().ended;
        assert!(matches!(ended, Ended::Draining), "{ended:?}");
        let calls = host.calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            host.calls.load(Ordering::SeqCst),
            calls,
            "no re-dispatch after the drain"
        );
    }

    /// A request admitted after the drain began is refused by its own
    /// re-check and never dispatched.
    #[tokio::test(flavor = "current_thread")]
    async fn a_request_admitted_after_the_drain_is_refused_by_its_own_recheck() {
        let host = Scripted::new([]);
        let wire = wire(host.clone(), Duration::from_secs(10), 2);
        wire.admission.begin_drain();
        let run = ask(&wire, Verb::Find, Lane::Read).await;
        assert!(matches!(run.ended, Ended::Draining));
        assert_eq!(host.calls.load(Ordering::SeqCst), 0);
    }

    /// One vault's second mutation waits for the first's turn without
    /// holding a mutation thread, so another vault's mutation runs beside it;
    /// past its deadline the waiter is busy and never runs.
    #[tokio::test(flavor = "current_thread")]
    async fn one_vaults_mutations_hold_one_thread_and_another_vault_runs_beside() {
        let host = Scripted::new([]);
        let (release, held) = std::sync::mpsc::channel();
        *host.hold.lock().unwrap() = Some(held);
        let wire = Arc::new(wire(host.clone(), Duration::from_millis(300), 2));
        let notes = VaultName::new("notes").unwrap();
        let first = {
            let (wire, notes) = (Arc::clone(&wire), notes.clone());
            tokio::spawn(async move { ask(&wire, Verb::Set, Lane::Mutation(notes)).await })
        };
        while host.calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let second = {
            let wire = Arc::clone(&wire);
            tokio::spawn(async move { ask(&wire, Verb::Set, Lane::Mutation(notes)).await })
        };
        let other = {
            let wire = Arc::clone(&wire);
            let other = VaultName::new("other").unwrap();
            tokio::spawn(async move { ask(&wire, Verb::Set, Lane::Mutation(other)).await })
        };
        while host.calls.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let second = second.await.unwrap();
        assert!(matches!(second.ended, Ended::Busy), "{:?}", second.ended);
        release.send(()).unwrap();
        release.send(()).unwrap();
        assert!(matches!(first.await.unwrap().ended, Ended::Dispatched(_)));
        assert!(matches!(other.await.unwrap().ended, Ended::Dispatched(_)));
        assert_eq!(host.calls.load(Ordering::SeqCst), 2);
        assert_eq!(wire.semaphores.held(), 0);
    }

    /// `verb` on `lane`, served on its own task, as a connection's route
    /// serves it: aborting the task is what hyper does to a request whose
    /// client disconnects.
    fn spawned(wire: &Arc<Wire>, verb: Verb, lane: Lane) -> tokio::task::JoinHandle<Run> {
        let wire = Arc::clone(wire);
        tokio::spawn(async move { ask(&wire, verb, lane).await })
    }

    /// Abort a request's task and wait until its future has been dropped.
    async fn disconnect(request: tokio::task::JoinHandle<Run>) {
        request.abort();
        let _ = request.await;
    }

    /// Wait until `condition` holds, or fail after five seconds.
    async fn until(what: &str, condition: impl Fn() -> bool) {
        let by = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < by, "never: {what}");
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    /// Wait until a one-thread `pool` has taken up every job queued before
    /// now: it takes them in order, so a job queued now runs after them.
    async fn taken_up(pool: &Pool) {
        let (done, taken) = oneshot::channel();
        pool.submit(Box::new(move || {
            let _ = done.send(());
        }));
        taken.await.expect("the pool takes the job up");
    }

    /// **A request whose client goes while its call is queued runs nothing.**
    /// Its job stays queued behind the call holding the one read thread; once
    /// the pool takes it up, it finds the claim ended and calls nothing.
    #[tokio::test(flavor = "current_thread")]
    async fn a_request_that_goes_while_queued_runs_nothing() {
        let host = Scripted::new([]);
        let wire = Arc::new(wire(host.clone(), Duration::from_secs(10), 1));
        // Made after the wire, so a failing case closes the hold before the
        // pools' drop joins a thread a call holds.
        let (release, held) = std::sync::mpsc::channel();
        *host.hold.lock().unwrap() = Some(held);
        let first = spawned(&wire, Verb::Find, Lane::Read);
        until("the first call runs", || {
            host.calls.load(Ordering::SeqCst) == 1
        })
        .await;
        let gone = spawned(&wire, Verb::Find, Lane::Read);
        // A read is admitted and its job queued in one poll, with no await
        // between, so a second registered claim is a queued job.
        until("the second request is queued", || {
            wire.admission.registered() == 2
        })
        .await;
        disconnect(gone).await;
        release.send(()).unwrap();
        // Closed, so a call that should not run cannot block the pool.
        drop(release);
        assert!(matches!(first.await.unwrap().ended, Ended::Dispatched(_)));
        taken_up(&wire.reads).await;
        assert_eq!(host.calls.load(Ordering::SeqCst), 1, "the gone request ran");
        assert_eq!(counts(&wire).0, 1);
        assert_eq!(wire.admission.registered(), 0);
    }

    /// **A drain after a client went starts nothing.** The gone request's job
    /// is still queued when the drain sweeps, and nothing runs after it.
    #[tokio::test(flavor = "current_thread")]
    async fn a_drain_after_a_disconnect_starts_nothing() {
        let host = Scripted::new([]);
        let wire = Arc::new(wire(host.clone(), Duration::from_secs(10), 1));
        // Made after the wire, so a failing case closes the hold before the
        // pools' drop joins a thread a call holds.
        let (release, held) = std::sync::mpsc::channel();
        *host.hold.lock().unwrap() = Some(held);
        let first = spawned(&wire, Verb::Find, Lane::Read);
        until("the first call runs", || {
            host.calls.load(Ordering::SeqCst) == 1
        })
        .await;
        let gone = spawned(&wire, Verb::Find, Lane::Read);
        until("the second request is queued", || {
            wire.admission.registered() == 2
        })
        .await;
        disconnect(gone).await;
        wire.admission.begin_drain();
        release.send(()).unwrap();
        drop(release);
        assert!(matches!(first.await.unwrap().ended, Ended::Dispatched(_)));
        taken_up(&wire.reads).await;
        assert_eq!(
            host.calls.load(Ordering::SeqCst),
            1,
            "an attempt started after the drain's sweep finished"
        );
    }

    /// **The plain stop runs no queued host call.** It drops every
    /// connection's request, then the pools, whose threads take up what is
    /// still queued; a gone request's job among it calls nothing.
    #[tokio::test(flavor = "current_thread")]
    async fn the_plain_stop_runs_no_queued_call() {
        let host = Scripted::new([]);
        let wire = Arc::new(wire(host.clone(), Duration::from_secs(10), 1));
        // Made after the wire, so a failing case closes the hold before the
        // pools' drop joins a thread a call holds.
        let (release, held) = std::sync::mpsc::channel();
        *host.hold.lock().unwrap() = Some(held);
        let first = spawned(&wire, Verb::Find, Lane::Read);
        until("the first call runs", || {
            host.calls.load(Ordering::SeqCst) == 1
        })
        .await;
        let gone = spawned(&wire, Verb::Find, Lane::Read);
        until("the second request is queued", || {
            wire.admission.registered() == 2
        })
        .await;
        disconnect(gone).await;
        disconnect(first).await;
        release.send(()).unwrap();
        drop(release);
        let wire = Arc::try_unwrap(wire).unwrap_or_else(|_| panic!("the requests have gone"));
        // The pools' drop joins their threads, each taking up the queue.
        drop(wire);
        assert_eq!(host.calls.load(Ordering::SeqCst), 1);
    }

    /// **A mutation whose client goes while it runs keeps its vault's turn
    /// until the host call returns**, so a second mutation of the vault does
    /// not run beside it: here it waits out its deadline and runs nothing.
    #[tokio::test(flavor = "current_thread")]
    async fn a_mutation_whose_client_goes_while_it_runs_keeps_its_vaults_turn() {
        let host = Scripted::new([]);
        let wire = Arc::new(wire(host.clone(), Duration::from_millis(300), 2));
        // Made after the wire, so a failing case closes the hold before the
        // pools' drop joins a thread a call holds.
        let (release, held) = std::sync::mpsc::channel();
        *host.hold.lock().unwrap() = Some(held);
        let notes = VaultName::new("notes").unwrap();
        let first = spawned(&wire, Verb::Init, Lane::Mutation(notes.clone()));
        until("the first mutation runs", || {
            host.calls.load(Ordering::SeqCst) == 1
        })
        .await;
        disconnect(first).await;
        let second = tokio::time::timeout(
            Duration::from_secs(3),
            ask(&wire, Verb::Init, Lane::Mutation(notes.clone())),
        )
        .await
        .expect("a second mutation of the vault ran beside the first");
        assert!(matches!(second.ended, Ended::Busy), "{:?}", second.ended);
        assert_eq!(host.calls.load(Ordering::SeqCst), 1);
        release.send(()).unwrap();
        drop(release);
        until("the turn is given back", || wire.semaphores.held() == 0).await;
        let third = ask(&wire, Verb::Init, Lane::Mutation(notes)).await;
        assert!(
            matches!(third.ended, Ended::Dispatched(_)),
            "{:?}",
            third.ended
        );
        assert_eq!(host.calls.load(Ordering::SeqCst), 2);
    }

    /// **A read whose client goes while it runs stays registered for the
    /// sweep until the host call returns.**
    #[tokio::test(flavor = "current_thread")]
    async fn a_read_whose_client_goes_while_it_runs_is_held_until_its_call_returns() {
        let host = Scripted::new([]);
        let wire = Arc::new(wire(host.clone(), Duration::from_secs(10), 2));
        // Made after the wire, so a failing case closes the hold before the
        // pools' drop joins a thread a call holds.
        let (release, held) = std::sync::mpsc::channel();
        *host.hold.lock().unwrap() = Some(held);
        let first = spawned(&wire, Verb::Find, Lane::Read);
        until("the read runs", || host.calls.load(Ordering::SeqCst) == 1).await;
        disconnect(first).await;
        assert_eq!(wire.admission.registered(), 1, "the running read let go");
        release.send(()).unwrap();
        drop(release);
        until("the read lets go once its call returns", || {
            wire.admission.registered() == 0
        })
        .await;
        assert_eq!(host.calls.load(Ordering::SeqCst), 1);
    }

    /// A host that answers `get` warming and holds `find` until released.
    struct WarmingGet {
        finds: AtomicUsize,
        gets: AtomicUsize,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl Dispatch for WarmingGet {
        fn dispatch(&self, verb: Verb, _: &[u8]) -> Dispatched {
            if verb == Verb::Get {
                self.gets.fetch_add(1, Ordering::SeqCst);
                return warming(Healing);
            }
            self.finds.fetch_add(1, Ordering::SeqCst);
            let _ = self.release.lock().unwrap().recv();
            Dispatched::Report(b"{}".to_vec())
        }
    }

    /// **An expiry in the pool queue after a warming attempt answers that
    /// warming refusal**, as the claim's `expired` row says, not
    /// `serve/busy`: the retry waits for the one read thread a `find` holds,
    /// past the deadline.
    #[tokio::test(flavor = "current_thread")]
    async fn an_expiry_in_the_queue_after_a_warming_attempt_answers_the_warming_refusal() {
        let (release, held) = std::sync::mpsc::channel();
        let host = Arc::new(WarmingGet {
            finds: AtomicUsize::new(0),
            gets: AtomicUsize::new(0),
            release: Mutex::new(held),
        });
        let wire = Arc::new(wire(host.clone(), Duration::from_millis(400), 1));
        let get = spawned(&wire, Verb::Get, Lane::Read);
        until("the get answers warming", || {
            host.gets.load(Ordering::SeqCst) >= 1
        })
        .await;
        let find = spawned(&wire, Verb::Find, Lane::Read);
        until("the find holds the thread", || {
            host.finds.load(Ordering::SeqCst) == 1
        })
        .await;
        let ended = get.await.unwrap().ended;
        drop(release);
        let Ended::StillWarming(envelope) = ended else {
            panic!("an expiry after a warming attempt answered {ended:?}");
        };
        assert!(is_warming(&envelope));
        assert!(matches!(find.await.unwrap().ended, Ended::Dispatched(_)));
        taken_up(&wire.reads).await;
        assert_eq!(
            host.gets.load(Ordering::SeqCst),
            1,
            "the expired re-dispatch ran"
        );
    }

    fn draining_wire(dispatcher: Arc<dyn Dispatch>, bound: Duration) -> Wire {
        Wire {
            drain: Arc::new(Drain::new(bound)),
            ..wire(dispatcher, Duration::from_secs(10), 2)
        }
    }

    /// **A synchronous mutation still running at the drain's bound is
    /// answered there**, by the drain's `started → answered`, and the answer
    /// its host call gives later is discarded. Its vault's turn and its
    /// registration with the sweep last until that call returns.
    #[tokio::test(flavor = "current_thread")]
    async fn a_synchronous_mutation_running_at_the_bound_is_answered_and_its_late_answer_dropped() {
        let host = Scripted::new([]);
        let wire = Arc::new(draining_wire(host.clone(), Duration::from_millis(50)));
        // Made after the wire, so a failing case closes the hold before the
        // pools' drop joins a thread a call holds.
        let (release, held) = std::sync::mpsc::channel();
        *host.hold.lock().unwrap() = Some(held);
        let notes = VaultName::new("notes").unwrap();
        let running = {
            let wire = Arc::clone(&wire);
            tokio::spawn(async move {
                ask_at(
                    &wire,
                    Verb::VaultRegister,
                    Lane::Mutation(notes),
                    AtBound::OutcomeUnknown,
                )
                .await
            })
        };
        until("the mutation runs", || {
            host.calls.load(Ordering::SeqCst) == 1
        })
        .await;
        wire.admission.begin_drain();
        let deadline = wire.drain.begin();
        let ended = running.await.unwrap().ended;
        assert!(matches!(ended, Ended::StillRunningAtBound), "{ended:?}");
        assert!(std::time::Instant::now() >= deadline);
        assert_eq!(wire.semaphores.held(), 1, "the turn went with the answer");
        assert_eq!(wire.admission.registered(), 1);
        release.send(()).unwrap();
        drop(release);
        until("the call returns and lets go", || {
            wire.semaphores.held() == 0 && wire.admission.registered() == 0
        })
        .await;
        assert_eq!(host.calls.load(Ordering::SeqCst), 1);
    }

    /// **A read still running at the bound is not answered**: it is cut off,
    /// so the drain does not wait for it, and if it answers after all it is
    /// answered and waited for again.
    #[tokio::test(flavor = "current_thread")]
    async fn a_read_running_at_the_bound_is_cut_off_and_not_answered() {
        let host = Scripted::new([]);
        let wire = Arc::new(draining_wire(host.clone(), Duration::from_millis(30)));
        // Made after the wire, so a failing case closes the hold before the
        // pools' drop joins a thread a call holds.
        let (release, held) = std::sync::mpsc::channel();
        *host.hold.lock().unwrap() = Some(held);
        // The connection's count, which the route's cut-off offsets.
        wire.drain.request_started();
        let running = spawned(&wire, Verb::Find, Lane::Read);
        until("the read runs", || host.calls.load(Ordering::SeqCst) == 1).await;
        wire.admission.begin_drain();
        let deadline = wire.drain.begin();
        assert!(
            wire.drain
                .settle_by(deadline + Duration::from_secs(5))
                .await,
            "the drain waited for a read cut off at its bound"
        );
        assert!(!running.is_finished(), "a cut-off read was answered");
        release.send(()).unwrap();
        let ended = running.await.unwrap().ended;
        assert!(matches!(ended, Ended::Dispatched(_)), "{ended:?}");
        assert!(
            !wire.drain.settled(),
            "an answered read is waited for until it flushes"
        );
    }

    /// An apply at the bound is answered by its own pool thread's
    /// abandonment, so the route only waits for it.
    #[tokio::test(flavor = "current_thread")]
    async fn an_apply_at_the_bound_waits_for_its_pool_threads_answer() {
        let host = Scripted::new([]);
        let wire = Arc::new(draining_wire(host.clone(), Duration::from_millis(20)));
        // Made after the wire, so a failing case closes the hold before the
        // pools' drop joins a thread a call holds.
        let (release, held) = std::sync::mpsc::channel();
        *host.hold.lock().unwrap() = Some(held);
        let notes = VaultName::new("notes").unwrap();
        let running = spawned(&wire, Verb::Set, Lane::Mutation(notes));
        until("the apply runs", || host.calls.load(Ordering::SeqCst) == 1).await;
        wire.admission.begin_drain();
        let deadline = wire.drain.begin();
        tokio::time::sleep_until(
            tokio::time::Instant::from_std(deadline) + Duration::from_millis(30),
        )
        .await;
        assert!(!running.is_finished());
        release.send(()).unwrap();
        assert!(matches!(running.await.unwrap().ended, Ended::Dispatched(_)));
    }
}
