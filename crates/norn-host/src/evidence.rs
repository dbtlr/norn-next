//! What the host's jobs spent and did, kept rather than discarded.
//!
//! A lifecycle job answers with a state — ready, warming, untrusted — and that
//! answer says nothing about how the job got there. Two derivations of one vault
//! that reach the same rows can still differ in what they read, how many
//! changesets they landed, and how many rungs of the ladder they climbed, and
//! those differences are the whole subject of a suite that compares them. This
//! module is where they are readable.
//!
//! # It is evidence, not a counter of derivation
//!
//! `norn-store`'s [derivation counters] are a closed vocabulary with a rule
//! behind it: each one counts derivation, and a request that only reads moves
//! none of them. Nothing here belongs in that vocabulary — a read that opens a
//! file, a job that ran a recovery, a changeset that landed — so nothing here is
//! spelled as one. These are cumulative facts about a host's own work, read by
//! subtracting two readings.
//!
//! [derivation counters]: norn_store::DerivationCounters
//!
//! # What a job's account holds, exactly
//!
//! A lifecycle job runs on one worker thread from the entry point to its
//! return. The entry point opens a [window] over that thread's filesystem reads
//! and holds it for the length of the job, so what is folded in at the end is
//! what the thread read **while the job ran**: reads made before the window
//! opened are not this job's and never reach it, and reads made when no window
//! stands reach no account at all. One window stands per thread, so nothing
//! else can take the reading a running job is standing on.
//!
//! The changeset tallies are scoped the same way and by the same guard: the job
//! code that applies a changeset records what the store told it on its own
//! thread, the guard empties that tally as it is made, and the entry point
//! folds in what stands when the job leaves.
//!
//! [window]: norn_fs::reads::ReadWindow
//!
//! # What is not here
//!
//! **Snapshot reads.** `norn_store::SnapshotReader` is an uninhabited type: no
//! value of it exists, [`crate::EntryOps`] mints none, and no read is answered
//! from one. There is nothing to observe, so there is no field for it here — an
//! always-zero count would read as a snapshot surface that is quiet rather than
//! as one that is not built.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use norn_fs::reads::ReadWindow;
use norn_store::IncrementOutcome;

/// One host's cumulative account of what its jobs spent and did.
///
/// Every field is a running total. A caller that wants what one job cost takes a
/// [reading](JobEvidence::read) before and after it and subtracts.
#[derive(Debug, Default)]
pub struct JobEvidence {
    document_opens: AtomicU64,
    stats: AtomicU64,
    walk_dirents: AtomicU64,
    changesets_applied: AtomicU64,
    documents_upserted: AtomicU64,
    documents_deleted: AtomicU64,
    tombstones_recorded: AtomicU64,
    findings_discarded: AtomicU64,
    recoveries_run: AtomicU64,
    rebuilds_run: AtomicU64,
}

/// One reading of a host's account.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EvidenceReading {
    /// Files opened for their content, over every job.
    pub document_opens: u64,
    /// Names stated, however the stat was spelled.
    pub stats: u64,
    /// Directory entries taken off a directory stream.
    pub walk_dirents: u64,
    /// Changesets that landed. A changeset is the unit of atomicity, so this is
    /// how many times a job committed something.
    pub changesets_applied: u64,
    pub documents_upserted: u64,
    pub documents_deleted: u64,
    pub tombstones_recorded: u64,
    /// Findings the changesets discarded, on both maintenance axes.
    pub findings_discarded: u64,
    /// Recovery rungs run: how many times a job re-established coverage over an
    /// attachment that still held its resources.
    pub recoveries_run: u64,
    /// Rung-3 rebuilds run: how many times a job discarded damaged derived state
    /// and built it from the vault again.
    pub rebuilds_run: u64,
}

impl EvidenceReading {
    /// What happened between an earlier reading and this one.
    ///
    /// Every field is cumulative and never decreases, so the difference is what
    /// the work between the two readings spent and did.
    pub fn since(self, earlier: EvidenceReading) -> EvidenceReading {
        EvidenceReading {
            document_opens: self.document_opens - earlier.document_opens,
            stats: self.stats - earlier.stats,
            walk_dirents: self.walk_dirents - earlier.walk_dirents,
            changesets_applied: self.changesets_applied - earlier.changesets_applied,
            documents_upserted: self.documents_upserted - earlier.documents_upserted,
            documents_deleted: self.documents_deleted - earlier.documents_deleted,
            tombstones_recorded: self.tombstones_recorded - earlier.tombstones_recorded,
            findings_discarded: self.findings_discarded - earlier.findings_discarded,
            recoveries_run: self.recoveries_run - earlier.recoveries_run,
            rebuilds_run: self.rebuilds_run - earlier.rebuilds_run,
        }
    }
}

impl JobEvidence {
    /// This host's account as it stands.
    pub fn read(&self) -> EvidenceReading {
        let get = |field: &AtomicU64| field.load(Ordering::Relaxed);
        EvidenceReading {
            document_opens: get(&self.document_opens),
            stats: get(&self.stats),
            walk_dirents: get(&self.walk_dirents),
            changesets_applied: get(&self.changesets_applied),
            documents_upserted: get(&self.documents_upserted),
            documents_deleted: get(&self.documents_deleted),
            tombstones_recorded: get(&self.tombstones_recorded),
            findings_discarded: get(&self.findings_discarded),
            recoveries_run: get(&self.recoveries_run),
            rebuilds_run: get(&self.rebuilds_run),
        }
    }

    pub(crate) fn count_recovery(&self) {
        self.recoveries_run.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn count_rebuild(&self) {
        self.rebuilds_run.fetch_add(1, Ordering::Relaxed);
    }

    /// Add what one job's window reported, and what that job's changesets did,
    /// to the account.
    ///
    /// The window is consumed by its own report and the changeset tally is
    /// emptied as it is read, so no reading is folded twice and nothing a job
    /// spent is left behind for the next one.
    fn absorb(&self, window: ReadWindow) {
        let reads = window.finish();
        self.document_opens
            .fetch_add(reads.document_opens, Ordering::Relaxed);
        self.stats.fetch_add(reads.stats, Ordering::Relaxed);
        self.walk_dirents
            .fetch_add(reads.walk_dirents, Ordering::Relaxed);

        let changesets = take_changeset_tally();
        self.changesets_applied
            .fetch_add(changesets.applied, Ordering::Relaxed);
        self.documents_upserted
            .fetch_add(changesets.documents_upserted, Ordering::Relaxed);
        self.documents_deleted
            .fetch_add(changesets.documents_deleted, Ordering::Relaxed);
        self.tombstones_recorded
            .fetch_add(changesets.tombstones_recorded, Ordering::Relaxed);
        self.findings_discarded
            .fetch_add(changesets.findings_discarded, Ordering::Relaxed);
    }

    /// Open a job's window, and fold what it reports into the account when the
    /// guard is dropped, whichever way the job leaves.
    pub(crate) fn attributing(self: &Arc<Self>) -> Attribution {
        // Both tallies start where the guard does. The changeset tally has no
        // window type of its own because nothing outside this crate writes it:
        // emptying it here is the same statement the read window makes for
        // itself.
        let window = ReadWindow::open();
        let _ = take_changeset_tally();
        Attribution {
            account: Arc::clone(self),
            window: Some(window),
        }
    }
}

/// A job's attribution window: what this thread spends while it stands belongs
/// to the account it was made from, and nothing else does.
pub(crate) struct Attribution {
    account: Arc<JobEvidence>,
    /// The window this job's reads are counted in. It is taken out of the
    /// option to be consumed by the fold, which is the only place it is taken:
    /// a window stands from the guard's making to the guard's drop.
    window: Option<ReadWindow>,
}

impl Drop for Attribution {
    fn drop(&mut self) {
        let window = self
            .window
            .take()
            .expect("a job's read window stands until the job's guard is dropped");
        self.account.absorb(window);
    }
}

/// What the changesets this thread applied did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ChangesetTally {
    applied: u64,
    documents_upserted: u64,
    documents_deleted: u64,
    tombstones_recorded: u64,
    findings_discarded: u64,
}

thread_local! {
    static CHANGESETS: Cell<ChangesetTally> = const { Cell::new(ChangesetTally {
        applied: 0,
        documents_upserted: 0,
        documents_deleted: 0,
        tombstones_recorded: 0,
        findings_discarded: 0,
    }) };
}

/// Record what one applied changeset did.
///
/// The store answers every increment with an outcome, and this is where that
/// answer stops being dropped on the floor: the job that applied the changeset
/// records it on its own thread, and the entry point that job runs under folds
/// the thread's tally into the host's account.
pub(crate) fn count_changeset(outcome: &IncrementOutcome) {
    CHANGESETS.with(|cell| {
        let mut tally = cell.get();
        tally.applied += 1;
        tally.documents_upserted += outcome.documents_upserted;
        tally.documents_deleted += outcome.documents_deleted;
        tally.tombstones_recorded += outcome.tombstones_recorded;
        tally.findings_discarded += outcome.invalidated.findings_discarded;
        cell.set(tally);
    });
}

fn take_changeset_tally() -> ChangesetTally {
    CHANGESETS.with(|cell| cell.replace(ChangesetTally::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome() -> IncrementOutcome {
        IncrementOutcome {
            generation: Some(1),
            documents_upserted: 2,
            documents_deleted: 1,
            tombstones_recorded: 1,
            affected_classes: Default::default(),
            invalidated: norn_store::Invalidation {
                findings_discarded: 3,
            },
        }
    }

    #[test]
    fn a_changeset_a_job_applied_reaches_the_account_when_the_job_ends() {
        let evidence = Arc::new(JobEvidence::default());
        {
            let _job = evidence.attributing();
            count_changeset(&outcome());
            count_changeset(&outcome());
            assert_eq!(
                evidence.read(),
                EvidenceReading::default(),
                "the account moves where the job ends, not while it runs"
            );
        }
        let read = evidence.read();
        assert_eq!(read.changesets_applied, 2);
        assert_eq!(read.documents_upserted, 4);
        assert_eq!(read.documents_deleted, 2);
        assert_eq!(read.tombstones_recorded, 2);
        assert_eq!(read.findings_discarded, 6);
    }

    /// A tally folded once is not folded again, and work done between two jobs
    /// belongs to neither: a second job over the same thread reports its own.
    #[test]
    fn a_second_job_over_one_thread_reports_only_its_own() {
        let evidence = Arc::new(JobEvidence::default());
        drop(evidence.attributing());
        let before = evidence.read();
        count_changeset(&outcome());
        {
            let _job = evidence.attributing();
            count_changeset(&outcome());
        }
        assert_eq!(evidence.read().since(before).changesets_applied, 1);
    }

    #[test]
    fn a_rung_names_itself() {
        let evidence = Arc::new(JobEvidence::default());
        evidence.count_recovery();
        evidence.count_rebuild();
        evidence.count_rebuild();
        let read = evidence.read();
        assert_eq!((read.recoveries_run, read.rebuilds_run), (1, 2));
    }
}
