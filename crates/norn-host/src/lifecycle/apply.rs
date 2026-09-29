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

use norn_fs::Batch;
use norn_store::StoreReading;
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
}

impl ApplyProgress {
    /// Record `plan` as the plan this apply resolved: every answer given from
    /// here carries it.
    pub fn planned(&self, plan: &ResolvedPlan) {
        self.lock().plan = Some(plan.clone());
    }

    /// Record that publication is about to begin: an apply dropped from here
    /// may have landed some of its targets. Recorded through
    /// [`ProgressReporter::begin_publishing`](super::ProgressReporter::begin_publishing),
    /// which asks first whether the apply's leg still stands.
    pub(super) fn publishing(&self) {
        self.lock().publishing = true;
    }

    /// Whether publication began.
    pub(super) fn began_publishing(&self) -> bool {
        self.lock().publishing
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ProgressRecord> {
        // A record is two plain writes; one a panicking writer left half
        // made is still the record of how far it got.
        self.record.lock().unwrap_or_else(PoisonError::into_inner)
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
}

impl ApplyEnd {
    /// An apply that ended in `answer`, owing no heal.
    pub fn answered(answer: Result<(StoreReading, ApplyReport), ErrorEnvelope>) -> Self {
        ApplyEnd {
            answer: ApplyEnding::Answered(answer),
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
            } => answer
                .recv()
                .unwrap_or_else(|_| Err(progress.unanswered(cause))),
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
