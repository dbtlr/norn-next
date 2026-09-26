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
//! No window scopes the reader mint a leg's publication runs. It is counted at
//! the act, as a rung or a watcher poll is: the lifecycle mints the handle over
//! the coverage a leg hands back in the gate hold that publishes the leg's
//! outcome, after the entry point has returned, and what the mint ran is added
//! to this account where the mint returns.
//!
//! [window]: norn_fs::reads::ReadWindow
//!
//! # The read account is beside it, and is not the same subject
//!
//! [`ReadEvidence`] keeps what this host's **reads** cost, and it is separate
//! for the reason this account is separate from a derivation counter: a read
//! is not a job. It runs on the caller's thread rather than on a worker, it
//! opens no attribution window, and what is asked of it is what it did while
//! it held the entry gate rather than what it read off the filesystem. Folding
//! the two would make a job's reading move when a client read a vault.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use norn_fs::reads::ReadWindow;
use norn_store::IncrementOutcome;

/// One host's cumulative account of what its jobs spent and did.
///
/// Every field is a running total. A caller that wants what one job cost takes a
/// reading (`JobEvidence::read`) before and after it and subtracts.
#[derive(Debug, Default)]
pub struct JobEvidence {
    document_opens: AtomicU64,
    stats: AtomicU64,
    walk_dirents: AtomicU64,
    documents_derived: AtomicU64,
    changesets_applied: AtomicU64,
    documents_upserted: AtomicU64,
    documents_deleted: AtomicU64,
    tombstones_recorded: AtomicU64,
    findings_discarded: AtomicU64,
    recoveries_run: AtomicU64,
    rebuilds_run: AtomicU64,
    watcher_polls: AtomicU64,
    watcher_rescans_reported: AtomicU64,
    mint_statements_under_the_gate: AtomicU64,
}

/// One reading of a host's account.
///
/// **The account is written on every build and read on this one.** Counting
/// what a job spent is how the answer stops being discarded, and it happens
/// whatever features are on; taking a reading is a harness act, so the reader
/// is behind `induced-failure` with the rest of the harness-reachable surface.
#[cfg(any(feature = "induced-failure", test))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EvidenceReading {
    /// Files opened for their content, over every job.
    pub document_opens: u64,
    /// Names stated, however the stat was spelled.
    pub stats: u64,
    /// Directory entries taken off a directory stream.
    pub walk_dirents: u64,
    /// Vault documents whose bytes a job handed to derivation: one per
    /// document read and derived, whatever the derivation concluded — facts,
    /// a quarantine, or a block it could not read.
    ///
    /// **The one site a vault document's bytes reach derivation** is where
    /// this is tallied. Every job that derives runs under an attribution
    /// window and folds its tally into the account when it ends, so what a
    /// stretch between two readings moved is what the jobs that ended inside
    /// it derived: zero where none of them derived a document from the vault,
    /// and nonzero where one did, whichever job it was. A job still running
    /// when the stretch closes reaches a later reading.
    pub documents_derived: u64,
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
    /// Watcher polls taken over an attachment, however each one answered.
    ///
    /// One per poll of an entry's attachment — a pass of the dispatcher's
    /// watcher scan, or one of the bounded drains a heal-bearing leg takes on
    /// its way out — counted where the poll reaches the attachment rather
    /// than where it likes the answer:
    /// a pass that drains nothing, one that drains facts, and one that reports
    /// the subscription's terminal failure all move it by one. So this is the
    /// positive fact behind a claim about *ticks* — a state that stood still
    /// over a stretch of polls is only that where the polls happened, and a
    /// dispatcher that stopped taking them leaves the same still state behind
    /// with this reading at zero.
    pub watcher_polls: u64,
    /// Polls that drained facts carrying a backend rescan.
    ///
    /// A watcher reports a lost path set as a rescan naming no path, and an
    /// entry holding coverage and owing no rung publishes
    /// [`WatcherOverflow`](norn_wire::UntrustedReason::WatcherOverflow) for
    /// exactly those facts. So this counts the times an attachment was told its
    /// account of the vault is unreliable until something rereads it — the
    /// overflow itself, which is otherwise readable only as a trust state that
    /// stands for the length of the reconcile clearing it.
    pub watcher_rescans_reported: u64,
    /// Statements the job legs' reader mints ran against a database while the
    /// entry gate was held.
    ///
    /// A leg that installs, swaps or parks coverage mints the handle that
    /// coverage serves reads from in the gate hold that publishes it, and the
    /// mint reads the database: the journal-mode read of the read-only open,
    /// and the store-epoch read that binds the connection to its file. Every
    /// other holder of that entry waits behind those statements, so this is
    /// what the legs' publications held the gate for. A read's own mint is not
    /// here: it is the read account's, and neither account moves for the
    /// other's work.
    ///
    /// **A mint that refused counts what it ran before it refused**, because
    /// the gate was held for those statements too. A leg that minted nothing,
    /// because it installed no coverage or parked coverage over a handle that
    /// was already standing, adds nothing.
    pub mint_statements_under_the_gate: u64,
}

#[cfg(any(feature = "induced-failure", test))]
impl EvidenceReading {
    /// What happened between an earlier reading and this one.
    ///
    /// Every field is cumulative and never decreases, so the difference over
    /// two readings of **one** account is what the work between them spent and
    /// did. Two readings of two different accounts are not such a pair, and the
    /// subtraction floors at zero rather than wrapping: a caller that crossed
    /// accounts reads zeroes instead of a field near `u64::MAX`.
    ///
    /// The lane that subtracts two readings is the churn suite's cost bars —
    /// what one act cost, over a host that has already done other work. A suite
    /// that reads a host from its first job onwards reads the account directly.
    pub fn since(self, earlier: EvidenceReading) -> EvidenceReading {
        EvidenceReading {
            document_opens: self.document_opens.saturating_sub(earlier.document_opens),
            stats: self.stats.saturating_sub(earlier.stats),
            walk_dirents: self.walk_dirents.saturating_sub(earlier.walk_dirents),
            documents_derived: self
                .documents_derived
                .saturating_sub(earlier.documents_derived),
            changesets_applied: self
                .changesets_applied
                .saturating_sub(earlier.changesets_applied),
            documents_upserted: self
                .documents_upserted
                .saturating_sub(earlier.documents_upserted),
            documents_deleted: self
                .documents_deleted
                .saturating_sub(earlier.documents_deleted),
            tombstones_recorded: self
                .tombstones_recorded
                .saturating_sub(earlier.tombstones_recorded),
            findings_discarded: self
                .findings_discarded
                .saturating_sub(earlier.findings_discarded),
            recoveries_run: self.recoveries_run.saturating_sub(earlier.recoveries_run),
            rebuilds_run: self.rebuilds_run.saturating_sub(earlier.rebuilds_run),
            watcher_polls: self.watcher_polls.saturating_sub(earlier.watcher_polls),
            watcher_rescans_reported: self
                .watcher_rescans_reported
                .saturating_sub(earlier.watcher_rescans_reported),
            mint_statements_under_the_gate: self
                .mint_statements_under_the_gate
                .saturating_sub(earlier.mint_statements_under_the_gate),
        }
    }
}

impl JobEvidence {
    /// This host's account as it stands.
    #[cfg(any(feature = "induced-failure", test))]
    pub fn read(&self) -> EvidenceReading {
        let get = |field: &AtomicU64| field.load(Ordering::Relaxed);
        EvidenceReading {
            document_opens: get(&self.document_opens),
            stats: get(&self.stats),
            walk_dirents: get(&self.walk_dirents),
            documents_derived: get(&self.documents_derived),
            changesets_applied: get(&self.changesets_applied),
            documents_upserted: get(&self.documents_upserted),
            documents_deleted: get(&self.documents_deleted),
            tombstones_recorded: get(&self.tombstones_recorded),
            findings_discarded: get(&self.findings_discarded),
            recoveries_run: get(&self.recoveries_run),
            rebuilds_run: get(&self.rebuilds_run),
            watcher_polls: get(&self.watcher_polls),
            watcher_rescans_reported: get(&self.watcher_rescans_reported),
            mint_statements_under_the_gate: get(&self.mint_statements_under_the_gate),
        }
    }

    pub(crate) fn count_recovery(&self) {
        self.recoveries_run.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn count_rebuild(&self) {
        self.rebuilds_run.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn count_watcher_poll(&self) {
        self.watcher_polls.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn count_watcher_rescan(&self) {
        self.watcher_rescans_reported
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record what one leg's reader mint ran under the entry gate.
    ///
    /// **This is added where the mint returns, and no window folds it.** The
    /// mint runs in the gate hold that publishes a leg's outcome, after the
    /// ops' entry point has returned and its window has been folded, so a
    /// thread tally written there would be emptied by the next window's
    /// opening rather than reach this account. The caller holds the count the
    /// mint answered with, so it is added directly, the way a rung and a
    /// watcher poll are counted at the act.
    pub(crate) fn count_mint_under_the_gate(&self, statements: u64) {
        self.mint_statements_under_the_gate
            .fetch_add(statements, Ordering::Relaxed);
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

        self.documents_derived
            .fetch_add(take_documents_derived(), Ordering::Relaxed);

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
        let _ = take_documents_derived();
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
        // This drop runs while a failed job unwinds, and a panic here would
        // abort the process rather than let that unwind finish. The window is
        // taken by whether it is there, so the fold is the same act it always
        // was and the guard has nothing left to panic about.
        if let Some(window) = self.window.take() {
            self.account.absorb(window);
        }
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

thread_local! {
    static DOCUMENTS_DERIVED: Cell<u64> = const { Cell::new(0) };
}

/// Record that one vault document's bytes were handed to derivation.
///
/// Tallied on the thread that derived it and folded into the account by the
/// job that thread runs, as a changeset is.
pub(crate) fn count_document_derived() {
    DOCUMENTS_DERIVED.with(|cell| cell.set(cell.get() + 1));
}

fn take_documents_derived() -> u64 {
    DOCUMENTS_DERIVED.with(|cell| cell.replace(0))
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
                typed_values_discarded: 0,
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

    /// A document derived inside a job reaches the account when the job ends,
    /// and one derived between two jobs belongs to neither.
    #[test]
    fn a_document_a_job_derived_reaches_the_account_when_the_job_ends() {
        let evidence = Arc::new(JobEvidence::default());
        count_document_derived();
        {
            let _job = evidence.attributing();
            count_document_derived();
            count_document_derived();
            assert_eq!(evidence.read().documents_derived, 0);
        }
        count_document_derived();
        drop(evidence.attributing());
        assert_eq!(evidence.read().documents_derived, 2);
    }

    #[test]
    fn a_rung_names_itself() {
        let evidence = Arc::new(JobEvidence::default());
        evidence.count_recovery();
        evidence.count_rebuild();
        evidence.count_rebuild();
        evidence.count_watcher_poll();
        evidence.count_watcher_poll();
        evidence.count_watcher_poll();
        evidence.count_watcher_rescan();
        let read = evidence.read();
        assert_eq!(
            (
                read.recoveries_run,
                read.rebuilds_run,
                read.watcher_polls,
                read.watcher_rescans_reported
            ),
            (1, 2, 3, 1)
        );
    }

    /// A leg's mint reaches the account where it is counted, with no window
    /// standing, and the fold of a later window neither carries it again nor
    /// empties it.
    #[test]
    fn a_legs_mint_reaches_the_account_where_it_is_counted() {
        let evidence = Arc::new(JobEvidence::default());
        evidence.count_mint_under_the_gate(2);
        assert_eq!(evidence.read().mint_statements_under_the_gate, 2);
        drop(evidence.attributing());
        assert_eq!(evidence.read().mint_statements_under_the_gate, 2);
    }
}

/// What a host's reads have cost, kept rather than discarded.
///
/// **Not the job account, and not a derivation counter.** A job's account says
/// what a lifecycle job spent; a derivation counter says what one request
/// derived, and a read derives nothing by construction. What this counts is
/// the read path's own shape: how many reads were served, what each ran while
/// it held the entry gate, and what concurrent reads of one entry paid for
/// sharing the one connection that entry holds.
///
/// **What an acquisition runs under the gate is three readings, not one.** The
/// establishing statement of a read that was served is exactly one, and the
/// bar on gate-held query work is read off that; the repair a read runs when
/// it finds the handle slot empty is under the same gate and is not query
/// work; and an establishment that refused ran its statement under the gate
/// and served nothing. They are counted apart so each reading says what it
/// asserts and none of them has to stand for another, and the widest reading
/// holds what one acquisition ran across all three so a ceiling over a single
/// read is one number rather than a sum of maxima.
///
/// **Every act is counted where the acquisition pays for it, never where the
/// read leaves**, so what an acquisition did is in the account whichever way it
/// left. The mint is counted where the mint returns, the establishment where
/// the establishment returns, and the wait for the entry's connection where
/// that wait begins — each of them before the branch that decides how the read
/// leaves, so the refusals are accounted exactly as the answers are. A wait
/// that begins cannot be abandoned: it returns only with the connection, so a
/// wait counted where it begins is a wait that ends. No path out of an
/// acquisition runs a statement under the gate, or waits out another read, and
/// reports nothing.
///
/// Every field is a running total for the host's whole life. Two of them are
/// maxima rather than sums, which is why a window over this account carries
/// neither: see [`ReadsSince`].
#[derive(Debug, Default)]
pub(crate) struct ReadEvidence {
    reads_served: AtomicU64,
    statements_under_the_gate: AtomicU64,
    mint_statements_under_the_gate: AtomicU64,
    refused_establishment_statements_under_the_gate: AtomicU64,
    widest_statements_under_the_gate: AtomicU64,
    reader_waits: AtomicU64,
    widest_reader_wait: AtomicU64,
}

/// One reading of a host's read account, over the whole of its life.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReadReading {
    /// Reads that took a hold, over every entry.
    pub reads_served: u64,
    /// Snapshot-establishing statements run while the entry gate was held.
    ///
    /// **One per read served**, so this moves with `reads_served`, and any
    /// other number is a read that ran query work under the lock every other
    /// holder of that entry waits behind. That claim is what this field is
    /// for, so it holds the establishment alone: the repair below runs under
    /// the same gate and is not query work, and folding the two would leave
    /// the exactly-one reading unable to say which of them moved it.
    pub statements_under_the_gate: u64,
    /// Statements the read path's own mints ran against a database while the
    /// entry gate was held.
    ///
    /// A read that meets a serving entry with an empty handle slot mints that
    /// handle again under its gate hold, before it establishes anything, and
    /// the mint reads the database: the journal-mode read of the read-only
    /// open, and the store-epoch read that binds the connection to its file.
    /// This is the repair's own cost, so **zero is a host whose reads all
    /// found a handle standing** and any other number is read-traffic healing.
    ///
    /// It counts what a mint ran whichever way that mint ended, and whichever
    /// way the read that paid for it left: a mint that refused held the gate
    /// for the statements it ran before it refused.
    pub mint_statements_under_the_gate: u64,
    /// Statements run under the entry gate by establishments that refused.
    ///
    /// An establishment that met a busy database opened its transaction, ran
    /// the statement that refused it and rolled back, all under the gate, and
    /// served no read. Those statements are kept here rather than in
    /// `statements_under_the_gate` so that reading stays exactly the reads it
    /// served; what is kept here makes no such claim, because an attempt that
    /// refused before its transaction opened ran none.
    pub refused_establishment_statements_under_the_gate: u64,
    /// The most statements any one acquisition ran under the gate — **its
    /// mint's and its establishment's together** — which is the value a
    /// per-read ceiling is stated against.
    ///
    /// An acquisition contributes what it ran, whether or not it was served:
    /// one for an acquisition that found a handle standing and established;
    /// its mint's statements and then that one where it healed first; its
    /// mint's alone where the mint refused; and its mint's plus the refused
    /// establishment's where the establishment is what refused.
    pub widest_statements_under_the_gate: u64,
    /// Acquisitions that gave the entry gate back and waited for the one
    /// connection their entry holds. Nonzero is reader contention, measured
    /// rather than assumed.
    ///
    /// **Counted where the wait begins, whichever way the acquisition
    /// leaves.** The count moves while the acquisition is still waiting, so a
    /// reading taken during contention already names every acquisition that
    /// found the connection taken. A wait that begins returns only with the
    /// connection, so each one counted here also ends. An acquisition that
    /// waited and was then refused — because its entry stopped serving, or
    /// because its handle was replaced while it waited — paid the whole of that
    /// wait, so it is one of these and is not among `reads_served`. Those are the paths contention is most likely to be
    /// interesting on, and a reading that held only served reads would
    /// under-report exactly there.
    ///
    /// It is a wait for the reader's connection and not for a gate: no
    /// acquisition waits for that connection while it holds the entry gate,
    /// and nothing here counts a wait for the gate itself.
    pub reader_waits: u64,
    /// The most times any one acquisition waited for its entry's connection.
    ///
    /// **One, or none.** An acquisition that finds the connection taken waits
    /// for it once and holds it from there, so the hold that establishes
    /// cannot contend again and a refused one gives the connection back rather
    /// than waiting a second time. A reading above one is a round this
    /// acquisition does not have.
    pub widest_reader_wait: u64,
}

/// What happened between an earlier reading of a host's read account and a
/// later one.
///
/// **A maximum is not a difference**, so a window carries none. The widest
/// read of a window cannot be computed from two readings of a running maximum:
/// the earlier reading may already hold the widest read the host ever made,
/// and subtracting or carrying it forward would both report a number about
/// another window. A bar on a maximum reads it off [`ReadReading`], whose
/// window is the whole of the host's life.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReadsSince {
    /// Reads that took a hold in this window, over every entry.
    pub reads_served: u64,
    /// Snapshot-establishing statements run while the entry gate was held, in
    /// this window.
    pub statements_under_the_gate: u64,
    /// Statements this window's read-path mints ran against a database while
    /// the entry gate was held.
    pub mint_statements_under_the_gate: u64,
    /// Statements this window's refused establishments ran while the entry
    /// gate was held.
    pub refused_establishment_statements_under_the_gate: u64,
    /// Acquisitions in this window that gave the entry gate back and waited
    /// for the one connection their entry holds, served and refused alike.
    pub reader_waits: u64,
}

impl ReadReading {
    /// What happened between an earlier reading and this one.
    pub fn since(self, earlier: ReadReading) -> ReadsSince {
        ReadsSince {
            reads_served: self.reads_served.saturating_sub(earlier.reads_served),
            statements_under_the_gate: self
                .statements_under_the_gate
                .saturating_sub(earlier.statements_under_the_gate),
            mint_statements_under_the_gate: self
                .mint_statements_under_the_gate
                .saturating_sub(earlier.mint_statements_under_the_gate),
            refused_establishment_statements_under_the_gate: self
                .refused_establishment_statements_under_the_gate
                .saturating_sub(earlier.refused_establishment_statements_under_the_gate),
            reader_waits: self.reader_waits.saturating_sub(earlier.reader_waits),
        }
    }
}

impl ReadEvidence {
    /// This host's read account as it stands.
    pub(crate) fn read(&self) -> ReadReading {
        let get = |field: &AtomicU64| field.load(Ordering::Relaxed);
        ReadReading {
            reads_served: get(&self.reads_served),
            statements_under_the_gate: get(&self.statements_under_the_gate),
            mint_statements_under_the_gate: get(&self.mint_statements_under_the_gate),
            refused_establishment_statements_under_the_gate: get(
                &self.refused_establishment_statements_under_the_gate
            ),
            widest_statements_under_the_gate: get(&self.widest_statements_under_the_gate),
            reader_waits: get(&self.reader_waits),
            widest_reader_wait: get(&self.widest_reader_wait),
        }
    }

    /// Record what one read's mint ran under the entry gate.
    ///
    /// **This is called where the mint returns and not where the read leaves**,
    /// because the read has already paid for those statements by then: a read
    /// that is refused after its mint refused ran them under the gate exactly
    /// as a read that went on to establish did. A read that minted nothing
    /// reports zero here and moves nothing.
    pub(crate) fn count_mint_under_the_gate(&self, statements: u64) {
        self.mint_statements_under_the_gate
            .fetch_add(statements, Ordering::Relaxed);
        // The widest is per acquisition, and this acquisition has run its
        // mint's statements and no establishing statement yet. A read that
        // goes on to establish widens it again below; a read that is refused
        // from here leaves this as what it ran.
        self.widest_statements_under_the_gate
            .fetch_max(statements, Ordering::Relaxed);
    }

    /// Record what one acquisition's establishment ran under the entry gate.
    ///
    /// **This is called where the establishment returns and not where the read
    /// leaves**, for the reason [`ReadEvidence::count_mint_under_the_gate`] is:
    /// the statement ran under the gate whichever answer came back, and the
    /// refusal path out is a path that already paid for it. Which of the two
    /// readings it lands in is the answer, because `statements_under_the_gate`
    /// claims to be the reads it served.
    ///
    /// The mint's statements are passed in again, already counted by the mint,
    /// because the widest reading is per acquisition: what one acquisition ran
    /// under the gate is its mint's statements and its establishment's, and a
    /// ceiling read off two separate maxima would be a sum of two different
    /// acquisitions.
    pub(crate) fn count_establishment_under_the_gate<S>(
        &self,
        establishment: &crate::Establishment<S>,
        mint_statements: u64,
    ) {
        let landing = if establishment.established.is_ok() {
            &self.statements_under_the_gate
        } else {
            &self.refused_establishment_statements_under_the_gate
        };
        landing.fetch_add(establishment.statements, Ordering::Relaxed);
        self.widest_statements_under_the_gate.fetch_max(
            mint_statements.saturating_add(establishment.statements),
            Ordering::Relaxed,
        );
    }

    /// Record that one acquisition waited for its entry's connection.
    ///
    /// **This is called where the wait begins and not where the read leaves**:
    /// the acquisition has given the entry gate back and found the connection
    /// taken, and it waits out another read whichever answer it goes on to get.
    /// The wait returns only with the connection, so every wait counted here
    /// ends, and the re-validation that decides the answer runs after both.
    /// Counting it here lets a reading taken while reads contend name the
    /// acquisitions that are waiting, before any of them is let through.
    ///
    /// The wait is the host's own count rather than a number the establishment
    /// reports: the acquisition is what waited, and the establishment it
    /// eventually ran waited for nothing. The widest is a structural reading
    /// rather than a sum — an acquisition waits once and then holds the
    /// connection — so one is the only value above zero this can produce.
    pub(crate) fn count_reader_wait(&self) {
        self.reader_waits.fetch_add(1, Ordering::Relaxed);
        self.widest_reader_wait.fetch_max(1, Ordering::Relaxed);
    }

    /// Record that one read was served.
    pub(crate) fn count_read(&self) {
        self.reads_served.fetch_add(1, Ordering::Relaxed);
    }
}
