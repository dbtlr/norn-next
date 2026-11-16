//! The apply seam's lifecycle half: the queue of admitted applies an entry
//! keeps beside its claim, the progress record each apply keeps, and the
//! handle its caller waits on.
//!
//! **An apply is a request-driven job whose reply rides in the job**, the way
//! an explicit reload's does. Admission puts the plan at the back of the
//! entry's queue with the reply and the progress record beside it, and the
//! apply job that takes the entry's claim takes the queue's head. What the job
//! runs is in `lifecycle.rs` beside every other job; what is here is the
//! vocabulary it runs with and the one place a caller is answered from when the
//! job could not answer it.
//!
//! **The progress record is what answers an apply its job did not.** The job
//! sets the resolved plan in it once planning finishes and marks it publishing
//! before the first publication, both through [`ApplyProgress`], which the ops
//! are handed. An apply whose worker unwound is answered from it by the
//! unwind's cleanup, once that has published over the entry: not applied
//! before the mark, with the cause the entry then publishes and the plan where
//! planning had finished; unknown after it, with the plan. A reply dropped
//! with no answer at all — a queue dropped with its host — is answered the
//! same way by [`PendingApply::wait`], with the cause the entry publishes when
//! the caller asks.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Instant;

use norn_fs::Batch;
use norn_store::StoreReading;

use crate::request_tally::RequestTally;
use norn_wire::{
    ApplyReport, ErrorDetail, ErrorEnvelope, PlanDocument, ResolvedPlan, VaultAnswer, VaultName,
};

/// What an apply answers its caller with: the report, inside the answer a
/// read's report crosses in, or the code it ended in.
pub(crate) type ApplyAnswer = Result<VaultAnswer<ApplyReport>, ErrorEnvelope>;

/// Where an admitted apply's caller waits for its answer.
pub(super) type ApplyReply = mpsc::SyncSender<ApplyAnswer>;

/// How far one apply got, read by its caller where the reply was dropped
/// without an answer.
///
/// The job the apply runs in holds one and hands it to
/// [`EntryOps::apply`](super::EntryOps::apply), which records the two moments
/// the caller's answer turns on: the resolved plan, once planning finishes,
/// and publication, just before the first target is published, through the
/// leg's check that it still stands.
#[derive(Clone, Default)]
pub struct ApplyProgress {
    record: Arc<Mutex<ProgressRecord>>,
}

#[derive(Default)]
struct ProgressRecord {
    plan: Option<ResolvedPlan>,
    publishing: bool,
    /// The caller answered the apply from this record and stopped waiting
    /// ([`PendingApply::abandon`]) before publication began: the job's check
    /// before publication refuses to begin once this is set.
    abandoned: bool,
    /// What the apply's job cost, recorded on the worker that ran it before
    /// the answer is sent, and added to the waiting caller's request tally
    /// once it is answered.
    tally: RequestTally,
}

impl ApplyProgress {
    /// Record `plan` as the plan this apply resolved: every answer given from
    /// here carries it.
    pub fn planned(&self, plan: &ResolvedPlan) {
        self.lock().plan = Some(plan.clone());
    }

    /// Record that publication is about to begin, unless the caller has
    /// abandoned the apply: an apply dropped from here may have landed some
    /// of its targets. Answers whether publication may begin. Recorded
    /// through
    /// [`ProgressReporter::begin_publishing`](super::ProgressReporter::begin_publishing),
    /// which asks first whether the apply's leg still stands.
    ///
    /// **One hold of the record's lock reads the abandoned mark and sets the
    /// publishing mark**, as [`PendingApply::abandon`] reads the publishing
    /// mark and sets the abandoned one in one hold: of the two, whichever
    /// takes the lock first decides, so an apply answered not run never
    /// begins publishing afterwards, and one that began is never answered
    /// not run.
    pub(super) fn begin_publishing(&self) -> bool {
        let mut record = self.lock();
        if record.abandoned {
            return false;
        }
        record.publishing = true;
        true
    }

    /// Whether publication began.
    pub(super) fn began_publishing(&self) -> bool {
        self.lock().publishing
    }

    /// Add `tally` to what this apply's job cost.
    pub(crate) fn tally(&self, tally: RequestTally) {
        let mut record = self.lock();
        record.tally = record.tally.plus(tally);
    }

    /// Hand what the job cost to the request waiting on this thread, once.
    pub(crate) fn hand_tally_to_this_thread(&self) {
        let tally = std::mem::take(&mut self.lock().tally);
        crate::request_tally::add(tally);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ProgressRecord> {
        // A record is two plain writes; one a panicking writer left half
        // made is still the record of how far it got.
        self.record.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The answer an abandoned apply is given, read from this record in the
    /// one hold that marks it abandoned: an answer that already arrived on
    /// `answer`; else unknown, with the resolved plan, once publication
    /// began; else not run, for `cause`, with the plan where planning had
    /// finished — and the mark set, so the job stands down at its check
    /// before publication.
    fn abandon(&self, answer: &mpsc::Receiver<ApplyAnswer>, cause: ErrorEnvelope) -> ApplyAnswer {
        let mut record = self.lock();
        if let Ok(answered) = answer.try_recv() {
            return answered;
        }
        match (&record.plan, record.publishing) {
            (Some(plan), true) => Err(ErrorEnvelope::new(
                "the host stopped while the apply was publishing, so some of its targets may \
                 have landed; send the plan again to finish it",
                ErrorDetail::apply_outcome_unknown(plan.clone()),
            )),
            (plan, _) => {
                let plan = plan.clone();
                record.abandoned = true;
                Err(not_run(cause, plan))
            }
        }
    }

    /// The answer a dropped reply is given: unknown once publication began,
    /// and not applied, for `cause`, before it.
    pub(super) fn unanswered(&self, cause: impl FnOnce() -> ErrorEnvelope) -> ErrorEnvelope {
        let record = self.lock();
        match (&record.plan, record.publishing) {
            (Some(plan), true) => ErrorEnvelope::new(
                "the apply was dropped after its publication began, so some of its targets may \
                 have landed; send the plan again to finish it",
                ErrorDetail::apply_outcome_unknown(plan.clone()),
            ),
            (plan, _) => {
                let plan = plan.clone();
                drop(record);
                not_run(cause(), plan)
            }
        }
    }
}

impl fmt::Debug for ApplyProgress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let record = self.lock();
        formatter
            .debug_struct("ApplyProgress")
            .field("planned", &record.plan.is_some())
            .field("publishing", &record.publishing)
            .field("abandoned", &record.abandoned)
            .finish()
    }
}

/// The `host/apply-not-run` answer for `cause`, carrying `plan` where
/// planning had finished.
pub(crate) fn not_run(cause: ErrorEnvelope, plan: Option<ResolvedPlan>) -> ErrorEnvelope {
    ErrorEnvelope::new(
        "the apply was not run, and no document was written",
        ErrorDetail::apply_not_run(cause, plan),
    )
}

/// What an apply's work came to, as [`EntryOps::apply`](super::EntryOps::apply)
/// answers it.
#[derive(Debug)]
pub struct ApplyEnd {
    /// The answer the apply ended in, or that it stood down before
    /// publication.
    pub answer: ApplyEnding,
    /// What the entry derives because the changeset did not commit: the
    /// paths the apply touched, which the entry heals from what they hold.
    /// `None` where the changeset committed or nothing landed.
    pub heal: Option<Batch>,
}

/// How an apply's work ended: with an answer of its own, or standing down.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // One per apply, moved once: an answer is the common ending, and boxing it would allocate for the rarer stand-down's sake.
pub enum ApplyEnding {
    /// The report, with where the store stood when the apply took its one
    /// snapshot — the reading the answer is given under — or the code the
    /// apply ended in.
    Answered(Result<(StoreReading, ApplyReport), ErrorEnvelope>),
    /// The apply's leg no longer stood when publication was about to begin,
    /// so every shadow was removed and nothing was published. The cause is
    /// the teardown's, which the apply job reads off the entry and answers
    /// with, beside the plan the apply's progress recorded.
    StoodDown,
    /// The store refused the apply's one snapshot as damaged, in the store's
    /// words, before anything was planned. The apply job publishes the
    /// damage with the rebuild it owes, as it publishes damage a read carried
    /// to its claim, and answers the apply not applied with it.
    Damaged(String),
}

impl ApplyEnd {
    /// An apply that ended in `answer`, owing no heal.
    pub fn answered(answer: Result<(StoreReading, ApplyReport), ErrorEnvelope>) -> Self {
        ApplyEnd {
            answer: ApplyEnding::Answered(answer),
            heal: None,
        }
    }

    /// An apply whose one snapshot met damage, described by `detail`.
    pub fn damaged(detail: impl Into<String>) -> Self {
        ApplyEnd {
            answer: ApplyEnding::Damaged(detail.into()),
            heal: None,
        }
    }

    /// An apply that stood down before publication, owing no heal.
    pub fn stood_down() -> Self {
        ApplyEnd {
            answer: ApplyEnding::StoodDown,
            heal: None,
        }
    }
}

/// One admitted apply, waiting in its entry's queue or running.
pub(super) struct QueuedApply {
    /// The plan the caller sent.
    pub(super) plan: PlanDocument,
    /// Where its caller waits.
    pub(super) reply: ApplyReply,
    /// How far it got.
    pub(super) progress: ApplyProgress,
    /// The recovery demand its admission raised, given back with its demand.
    pub(super) recovery_demand: Option<u64>,
}

/// The apply a job is running, as the entry holds it: a second sender to
/// its caller and its progress, so an unwound job's apply is answered where
/// the unwind is published.
pub(super) struct RunningApply {
    reply: ApplyReply,
    progress: ApplyProgress,
}

impl RunningApply {
    /// The entry's hold on `apply`, which a job is about to run.
    pub(super) fn of(apply: &QueuedApply) -> Self {
        RunningApply {
            reply: apply.reply.clone(),
            progress: apply.progress.clone(),
        }
    }

    /// Answer the apply its job left unanswered, from its progress: not
    /// applied, for `cause`, before publication began, and unknown after. A
    /// caller that stopped waiting is not an error.
    pub(super) fn answer_unanswered(self, cause: ErrorEnvelope) {
        let _ = self.reply.send(Err(self.progress.unanswered(|| cause)));
    }
}

/// The applies an entry admitted and has not yet run, first in, first out.
///
/// **Each one is demand on the entry** while it waits: its admission
/// recorded a demand lease, which is given back when the job running it
/// takes it off the queue, or when it is answered unrun, so no idle detach
/// begins over a queued apply and a release that ends with one queued re-arms
/// the attach it owes.
///
/// The queue knows its vault's name, because it is answered from the one
/// place every publication over the entry passes — the end of a gate hold —
/// which has the entry's state and nothing else.
pub(super) struct ApplyQueue {
    vault: VaultName,
    queued: VecDeque<QueuedApply>,
}

impl ApplyQueue {
    /// An empty queue for the vault `vault`.
    pub(super) fn for_vault(vault: VaultName) -> Self {
        ApplyQueue {
            vault,
            queued: VecDeque::new(),
        }
    }

    /// The vault this queue's applies were admitted against.
    pub(super) fn vault(&self) -> &VaultName {
        &self.vault
    }

    pub(super) fn push(&mut self, apply: QueuedApply) {
        self.queued.push_back(apply);
    }

    pub(super) fn take_head(&mut self) -> Option<QueuedApply> {
        self.queued.pop_front()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.queued.is_empty()
    }

    /// Take every queued apply off the queue, in the order they were
    /// admitted.
    pub(super) fn take_all(&mut self) -> VecDeque<QueuedApply> {
        std::mem::take(&mut self.queued)
    }
}

impl QueuedApply {
    /// Answer this apply not applied, for `cause`: it never ran, so no
    /// document was written and no plan was resolved. A caller that stopped
    /// waiting is not an error.
    pub(super) fn answer_unrun(self, cause: ErrorEnvelope) {
        let _ = self.reply.send(Err(not_run(cause, None)));
    }
}

/// The handle an `apply` caller holds: the answer, or the admitted apply it
/// waits on.
///
/// **Dropping it is how a caller stops waiting.** The apply is the entry's
/// from admission on: the job running it ignores a reply nobody waits for and
/// finishes.
pub struct PendingApply {
    waiting: Waiting,
}

enum Waiting {
    /// A preview, answered at once.
    Answered(Box<ApplyAnswer>),
    /// An admitted apply.
    Queued {
        answer: mpsc::Receiver<ApplyAnswer>,
        progress: ApplyProgress,
        /// The cause an apply dropped before publication is answered with:
        /// where its entry stands, read once the reply is gone.
        cause: Box<dyn FnOnce() -> ErrorEnvelope + Send>,
    },
}

impl PendingApply {
    /// A handle holding `answer` already.
    pub(crate) fn answered(answer: ApplyAnswer) -> Self {
        PendingApply {
            waiting: Waiting::Answered(Box::new(answer)),
        }
    }

    /// A handle waiting on `answer` for an apply whose progress is
    /// `progress`, answered with `cause` where the reply is dropped before
    /// publication began.
    pub(super) fn queued(
        answer: mpsc::Receiver<ApplyAnswer>,
        progress: ApplyProgress,
        cause: Box<dyn FnOnce() -> ErrorEnvelope + Send>,
    ) -> Self {
        PendingApply {
            waiting: Waiting::Queued {
                answer,
                progress,
                cause,
            },
        }
    }

    /// Wait for the apply's answer.
    ///
    /// **There is no host-level bound**: an apply runs as long as its plan
    /// takes, and a timeout a client wants is the serving layer's. A reply
    /// dropped with no answer sent on it is answered from the apply's
    /// progress record.
    pub fn wait(self) -> ApplyAnswer {
        match self.waiting {
            Waiting::Answered(answer) => *answer,
            Waiting::Queued {
                answer,
                progress,
                cause,
            } => {
                let answered = answer
                    .recv()
                    .unwrap_or_else(|_| Err(progress.unanswered(cause)));
                // The job recorded its tally before the answer was sent, so
                // it is whole by now; a dropped reply hands what it reached.
                progress.hand_tally_to_this_thread();
                answered
            }
        }
    }
}

impl PendingApply {
    /// Wait for the apply's answer until `deadline`: the answer, or this
    /// handle back where the deadline passed first, to wait again or to
    /// [`abandon`](Self::abandon).
    ///
    /// The serving layer's bounded drain waits in short slices of this,
    /// reading its own deadline between them; a dropped reply is answered
    /// from the progress record, as [`wait`](Self::wait) answers it.
    pub fn wait_until(self, deadline: Instant) -> Result<ApplyAnswer, PendingApply> {
        let Waiting::Queued {
            answer,
            progress,
            cause,
        } = self.waiting
        else {
            return Ok(self.wait());
        };
        let left = deadline.saturating_duration_since(Instant::now());
        let answered = match answer.recv_timeout(left) {
            Ok(answered) => answered,
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(progress.unanswered(cause)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err(PendingApply::queued(answer, progress, cause));
            }
        };
        progress.hand_tally_to_this_thread();
        Ok(answered)
    }

    /// **Stop waiting, and answer from what the apply's record holds**: an
    /// answer that already arrived; else `host/apply-outcome-unknown` with
    /// the resolved plan once publication began; else `host/apply-not-run`
    /// carrying `cause` — the caller's reason for not waiting, such as the
    /// serving layer's `serve/draining` — and the resolved plan where
    /// planning had finished, no plan where it had not (a queued apply, or
    /// one still planning).
    ///
    /// The record is read and marked abandoned in one hold of its lock, and
    /// the job's check before publication reads the mark in one hold of the
    /// same lock, so an apply answered not run here never begins publishing:
    /// its job stands down at that check, removing every shadow it staged.
    /// An answer the job sends later is discarded. This is a read of what the
    /// host already records, not a host shutdown: the apply's job runs on
    /// to its check, and an apply already publishing finishes.
    pub fn abandon(self, cause: ErrorEnvelope) -> ApplyAnswer {
        match self.waiting {
            Waiting::Answered(answer) => *answer,
            Waiting::Queued {
                answer, progress, ..
            } => {
                let answered = progress.abandon(&answer, cause);
                progress.hand_tally_to_this_thread();
                answered
            }
        }
    }
}

#[cfg(test)]
impl PendingApply {
    /// Block until the job has sent this apply's reply, and keep it unread
    /// where [`PendingApply::abandon`] reads it first: a test that abandons
    /// an apply already answered synchronises on the send, which the job
    /// makes after it has given its claim back.
    pub(crate) fn await_reply(&mut self) {
        if let Waiting::Queued { answer, .. } = &mut self.waiting {
            let reply = answer.recv().expect("the job sends its reply");
            let (resend, kept) = mpsc::channel();
            resend.send(reply).expect("the receiver is held here");
            *answer = kept;
        }
    }
}

impl fmt::Debug for PendingApply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.waiting {
            Waiting::Answered(answer) => formatter
                .debug_tuple("PendingApply::Answered")
                .field(answer)
                .finish(),
            Waiting::Queued { progress, .. } => formatter
                .debug_tuple("PendingApply::Queued")
                .field(progress)
                .finish(),
        }
    }
}
