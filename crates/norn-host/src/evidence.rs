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
//! An apply job's snapshot is counted at the act the same way: what the one
//! snapshot it minted ran — its `where` matching and its link judgments — is
//! read off the snapshot's own counters where the job's planning and applying
//! are done with it, beside the mint that opened it.
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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use norn_config::schema::RuleWork;
use norn_fs::reads::ReadWindow;
use norn_store::{DerivationCounters, IncrementOutcome, SnapshotCounters};

/// One host's cumulative account of what its jobs spent and did.
///
/// Every field is a running total. A caller that wants what one job cost takes a
/// reading (`JobEvidence::read`) before and after it and subtracts.
#[derive(Debug, Default)]
pub struct JobEvidence {
    document_opens: AtomicU64,
    stats: AtomicU64,
    walk_dirents: AtomicU64,
    write_dirents: AtomicU64,
    target_reads: AtomicU64,
    shadow_reads: AtomicU64,
    documents_derived: AtomicU64,
    changesets_applied: AtomicU64,
    documents_upserted: AtomicU64,
    documents_deleted: AtomicU64,
    tombstones_recorded: AtomicU64,
    findings_discarded: AtomicU64,
    findings_written: AtomicU64,
    links_redecided: AtomicU64,
    link_health_keys_resolved: AtomicU64,
    link_health_candidates_read: AtomicU64,
    changeset_read_steps: AtomicU64,
    recoveries_run: AtomicU64,
    rebuilds_run: AtomicU64,
    watcher_polls: AtomicU64,
    watcher_rescans_reported: AtomicU64,
    mint_statements_under_the_gate: AtomicU64,
    apply_mint_statements: AtomicU64,
    apply_mints: AtomicU64,
    apply_snapshots_opened: AtomicU64,
    apply_statements: AtomicU64,
    apply_vm_steps: AtomicU64,
    apply_full_scan_steps: AtomicU64,
    /// What the jobs' rule judgments paid, summed: one total of several
    /// counts, folded whole under its lock when a job ends.
    rule_work: Mutex<RuleWork>,
    /// The file each counted read of every job read, in the order the jobs
    /// ended, where a [`norn_fs::reads::FileRecording`] was armed while they
    /// ran: nothing is kept while none is.
    #[cfg(feature = "induced-failure")]
    files_read: std::sync::Mutex<Vec<norn_fs::reads::FileRead>>,
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
    /// Reads of a file's content through the contained open, over every
    /// job: one per read, so a file read twice counts twice
    /// ([`norn_fs::reads::ReadTally::document_opens`]).
    pub document_opens: u64,
    /// Names stated, however the stat was spelled.
    pub stats: u64,
    /// Directory entries taken off a directory stream.
    pub walk_dirents: u64,
    /// Directory entries the write kernel reads to judge a target's spelling
    /// ([`norn_fs::reads::ReadTally::write_dirents`]), separate from the walk.
    pub write_dirents: u64,
    /// Vault files the write kernel read and hashed through its own open: a
    /// target judged by staging, by publication's verification and by a
    /// landing's confirmation, and a create's source copied into its shadow
    /// ([`norn_fs::reads::ReadTally::target_reads`]).
    pub target_reads: u64,
    /// Staged shadows the write kernel read and hashed to confirm before
    /// publishing them ([`norn_fs::reads::ReadTally::shadow_reads`]).
    pub shadow_reads: u64,
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
    /// Findings the jobs' increments wrote, the link-health findings their
    /// re-decision filed among them — an increment carrying findings alone
    /// and no changeset included, since its request writes them all the same.
    pub findings_written: u64,
    /// Links the increments re-decided the link health of, each once per
    /// increment however many ways it was reached
    /// ([`norn_store::DerivationCounters::links_redecided`]).
    pub links_redecided: u64,
    /// Keys the increments' re-decisions resolved.
    pub link_health_keys_resolved: u64,
    /// Candidates the increments' re-decisions read.
    pub link_health_candidates_read: u64,
    /// Virtual-machine steps the increments' multi-row reads took, as the
    /// store's request reads them ([`norn_store::Request::read_steps`]): the
    /// re-decision's reads of the classes and paths a changeset names among
    /// them, and whatever an increment carrying findings alone read. Harness
    /// evidence about execution cost, never a derivation counter.
    pub changeset_read_steps: u64,
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
    /// Statements the apply jobs' reader mints ran against a database, inside
    /// the entry's claim and outside its gate.
    ///
    /// An apply that matches a `where` target or reads the links its plan
    /// reaches mints a read handle of its own to read them on, and that mint
    /// reads the database as a leg's does. **A mint that refused counts what
    /// it ran before it refused**; an apply that reads neither mints nothing
    /// and adds nothing.
    pub apply_mint_statements: u64,
    /// Read handles the apply jobs minted, counted at each mint whichever
    /// way it ended: one for an apply that read the store, and none for one
    /// that did not.
    pub apply_mints: u64,
    /// Snapshots the apply jobs established on the read handles they
    /// minted: one for an apply that read the store, and none for one that
    /// did not.
    pub apply_snapshots_opened: u64,
    /// Statements the apply jobs ran on those snapshots — the one that
    /// establishes each snapshot, a `where` target's matching, the link
    /// judgments of planning and of the applier's check, and the fresh plan a
    /// refusal resolves — as each snapshot counted them
    /// ([`norn_store::SnapshotCounters::statements_executed`]). A snapshot
    /// opened is one statement here before it answers anything.
    pub apply_statements: u64,
    /// Virtual-machine steps those statements took.
    pub apply_vm_steps: u64,
    /// Steps those statements took walking a table or an index end to end.
    pub apply_full_scan_steps: u64,
    /// What judging the documents the jobs derived and the results their
    /// plans composed against the vault schema's field declarations and
    /// rules paid, and filling the rule defaults of the documents their
    /// plans create, as the judge and the defaults fixpoint tally it
    /// ([`RuleWork`]): the logical counts of rule work, which no statement
    /// counter sees because both run in this process and read no database.
    ///
    /// **Tallied where the work is done** — a document's bytes reaching
    /// derivation, beside [`EvidenceReading::documents_derived`], the
    /// applier's schema check, and a creation's defaults at planning — and
    /// folded when the job ends. Each reads the one document and the schema,
    /// so every count here is the sum of what each one paid.
    pub rule_work: RuleWork,
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
            write_dirents: self.write_dirents.saturating_sub(earlier.write_dirents),
            target_reads: self.target_reads.saturating_sub(earlier.target_reads),
            shadow_reads: self.shadow_reads.saturating_sub(earlier.shadow_reads),
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
            findings_written: self
                .findings_written
                .saturating_sub(earlier.findings_written),
            links_redecided: self.links_redecided.saturating_sub(earlier.links_redecided),
            link_health_keys_resolved: self
                .link_health_keys_resolved
                .saturating_sub(earlier.link_health_keys_resolved),
            link_health_candidates_read: self
                .link_health_candidates_read
                .saturating_sub(earlier.link_health_candidates_read),
            changeset_read_steps: self
                .changeset_read_steps
                .saturating_sub(earlier.changeset_read_steps),
            recoveries_run: self.recoveries_run.saturating_sub(earlier.recoveries_run),
            rebuilds_run: self.rebuilds_run.saturating_sub(earlier.rebuilds_run),
            watcher_polls: self.watcher_polls.saturating_sub(earlier.watcher_polls),
            watcher_rescans_reported: self
                .watcher_rescans_reported
                .saturating_sub(earlier.watcher_rescans_reported),
            mint_statements_under_the_gate: self
                .mint_statements_under_the_gate
                .saturating_sub(earlier.mint_statements_under_the_gate),
            apply_mint_statements: self
                .apply_mint_statements
                .saturating_sub(earlier.apply_mint_statements),
            apply_mints: self.apply_mints.saturating_sub(earlier.apply_mints),
            apply_snapshots_opened: self
                .apply_snapshots_opened
                .saturating_sub(earlier.apply_snapshots_opened),
            apply_statements: self
                .apply_statements
                .saturating_sub(earlier.apply_statements),
            apply_vm_steps: self.apply_vm_steps.saturating_sub(earlier.apply_vm_steps),
            apply_full_scan_steps: self
                .apply_full_scan_steps
                .saturating_sub(earlier.apply_full_scan_steps),
            rule_work: rule_work_since(self.rule_work, earlier.rule_work),
        }
    }
}

/// What `later`, a total of rule work, holds beyond `earlier`, count by
/// count, flooring at zero as [`EvidenceReading::since`] does.
#[cfg(any(feature = "induced-failure", test))]
fn rule_work_since(later: RuleWork, earlier: RuleWork) -> RuleWork {
    RuleWork {
        rules_evaluated: later
            .rules_evaluated
            .saturating_sub(earlier.rules_evaluated),
        selector_terms: later.selector_terms.saturating_sub(earlier.selector_terms),
        rules_selected: later.rules_selected.saturating_sub(earlier.rules_selected),
        constraint_entries: later
            .constraint_entries
            .saturating_sub(earlier.constraint_entries),
        declaration_bytes: later
            .declaration_bytes
            .saturating_sub(earlier.declaration_bytes),
        constraints_judged: later
            .constraints_judged
            .saturating_sub(earlier.constraints_judged),
        pattern_characters: later
            .pattern_characters
            .saturating_sub(earlier.pattern_characters),
        placement_walks: later
            .placement_walks
            .saturating_sub(earlier.placement_walks),
        placement_weight: later
            .placement_weight
            .saturating_sub(earlier.placement_weight),
        placement_verdicts_reused: later
            .placement_verdicts_reused
            .saturating_sub(earlier.placement_verdicts_reused),
        findings: later.findings.saturating_sub(earlier.findings),
        defaults_rounds: later
            .defaults_rounds
            .saturating_sub(earlier.defaults_rounds),
        defaults_filled: later
            .defaults_filled
            .saturating_sub(earlier.defaults_filled),
        captures_bound: later.captures_bound.saturating_sub(earlier.captures_bound),
    }
}

impl JobEvidence {
    /// The file each counted read of every job read while a
    /// [`norn_fs::reads::FileRecording`] stood, in the order the jobs ended.
    ///
    /// The log only grows, so what one act read is what follows the length a
    /// caller took before it. It is the reads' identity beside the counts
    /// [`JobEvidence::read`] holds — the same reads, counted the same way —
    /// and is kept on a build with `induced-failure` alone.
    #[cfg(feature = "induced-failure")]
    pub fn files_read(&self) -> Vec<norn_fs::reads::FileRead> {
        self.files_read
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// This host's account as it stands.
    #[cfg(any(feature = "induced-failure", test))]
    pub fn read(&self) -> EvidenceReading {
        let get = |field: &AtomicU64| field.load(Ordering::Relaxed);
        EvidenceReading {
            document_opens: get(&self.document_opens),
            stats: get(&self.stats),
            walk_dirents: get(&self.walk_dirents),
            write_dirents: get(&self.write_dirents),
            target_reads: get(&self.target_reads),
            shadow_reads: get(&self.shadow_reads),
            documents_derived: get(&self.documents_derived),
            changesets_applied: get(&self.changesets_applied),
            documents_upserted: get(&self.documents_upserted),
            documents_deleted: get(&self.documents_deleted),
            tombstones_recorded: get(&self.tombstones_recorded),
            findings_discarded: get(&self.findings_discarded),
            findings_written: get(&self.findings_written),
            links_redecided: get(&self.links_redecided),
            link_health_keys_resolved: get(&self.link_health_keys_resolved),
            link_health_candidates_read: get(&self.link_health_candidates_read),
            changeset_read_steps: get(&self.changeset_read_steps),
            recoveries_run: get(&self.recoveries_run),
            rebuilds_run: get(&self.rebuilds_run),
            watcher_polls: get(&self.watcher_polls),
            watcher_rescans_reported: get(&self.watcher_rescans_reported),
            mint_statements_under_the_gate: get(&self.mint_statements_under_the_gate),
            apply_mint_statements: get(&self.apply_mint_statements),
            apply_mints: get(&self.apply_mints),
            apply_snapshots_opened: get(&self.apply_snapshots_opened),
            apply_statements: get(&self.apply_statements),
            apply_vm_steps: get(&self.apply_vm_steps),
            apply_full_scan_steps: get(&self.apply_full_scan_steps),
            rule_work: *self
                .rule_work
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
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

    /// Record what one apply job's reader mint ran, counted at the act where
    /// the mint returns, as a leg's mint is.
    pub(crate) fn count_apply_mint(&self, statements: u64) {
        self.apply_mints.fetch_add(1, Ordering::Relaxed);
        self.apply_mint_statements
            .fetch_add(statements, Ordering::Relaxed);
    }

    /// Record what one apply job ran on the snapshots it established, counted
    /// at the act where the job's planning and applying are done with them,
    /// as its mint is.
    pub(crate) fn count_apply_snapshot(&self, work: SnapshotWork) {
        for (field, value) in [
            (&self.apply_snapshots_opened, work.snapshots_opened),
            (&self.apply_statements, work.statements),
            (&self.apply_vm_steps, work.vm_steps),
            (&self.apply_full_scan_steps, work.full_scan_steps),
        ] {
            field.fetch_add(value, Ordering::Relaxed);
        }
    }

    /// Add what one job's window reported, and what that job's changesets did,
    /// to the account.
    ///
    /// The window is consumed by its own report and the changeset tally is
    /// emptied as it is read, so no reading is folded twice and nothing a job
    /// spent is left behind for the next one.
    fn absorb(&self, window: ReadWindow) {
        #[cfg(feature = "induced-failure")]
        let reads = {
            let (reads, files) = window.finish_with_files();
            if !files.is_empty() {
                // Read through a poison: this runs while a failed job unwinds,
                // and a panic here would abort rather than let it finish.
                self.files_read
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .extend(files);
            }
            reads
        };
        #[cfg(not(feature = "induced-failure"))]
        let reads = window.finish();
        self.document_opens
            .fetch_add(reads.document_opens, Ordering::Relaxed);
        self.stats.fetch_add(reads.stats, Ordering::Relaxed);
        self.walk_dirents
            .fetch_add(reads.walk_dirents, Ordering::Relaxed);
        self.write_dirents
            .fetch_add(reads.write_dirents, Ordering::Relaxed);
        self.target_reads
            .fetch_add(reads.target_reads, Ordering::Relaxed);
        self.shadow_reads
            .fetch_add(reads.shadow_reads, Ordering::Relaxed);

        self.documents_derived
            .fetch_add(take_documents_derived(), Ordering::Relaxed);
        // Read through a poison for the reason the file log is: this runs
        // while a failed job unwinds.
        let judged = take_rule_work();
        let mut rule_work = self
            .rule_work
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *rule_work = rule_work.plus(judged);
        drop(rule_work);

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
        self.findings_written
            .fetch_add(changesets.findings_written, Ordering::Relaxed);
        self.links_redecided
            .fetch_add(changesets.links_redecided, Ordering::Relaxed);
        self.link_health_keys_resolved
            .fetch_add(changesets.link_health_keys_resolved, Ordering::Relaxed);
        self.link_health_candidates_read
            .fetch_add(changesets.link_health_candidates_read, Ordering::Relaxed);
        self.changeset_read_steps
            .fetch_add(changesets.read_steps, Ordering::Relaxed);
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
        let _ = take_rule_work();
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

/// What one apply job ran on the snapshots it established, summed over them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct SnapshotWork {
    snapshots_opened: u64,
    statements: u64,
    vm_steps: u64,
    full_scan_steps: u64,
}

impl SnapshotWork {
    /// No snapshot at all.
    pub(crate) const NONE: SnapshotWork = SnapshotWork {
        snapshots_opened: 0,
        statements: 0,
        vm_steps: 0,
        full_scan_steps: 0,
    };

    /// What one snapshot ran, read off its own counters.
    pub(crate) fn of(counters: SnapshotCounters) -> SnapshotWork {
        SnapshotWork {
            snapshots_opened: counters.snapshots_opened(),
            statements: counters.statements_executed(),
            vm_steps: counters.vm_steps(),
            full_scan_steps: counters.full_scan_steps(),
        }
    }

    /// This work and `other` together.
    #[must_use]
    pub(crate) fn plus(self, other: SnapshotWork) -> SnapshotWork {
        SnapshotWork {
            snapshots_opened: self.snapshots_opened + other.snapshots_opened,
            statements: self.statements + other.statements,
            vm_steps: self.vm_steps + other.vm_steps,
            full_scan_steps: self.full_scan_steps + other.full_scan_steps,
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
    findings_written: u64,
    links_redecided: u64,
    link_health_keys_resolved: u64,
    link_health_candidates_read: u64,
    read_steps: u64,
}

thread_local! {
    static CHANGESETS: Cell<ChangesetTally> = const { Cell::new(ChangesetTally {
        applied: 0,
        documents_upserted: 0,
        documents_deleted: 0,
        tombstones_recorded: 0,
        findings_discarded: 0,
        findings_written: 0,
        links_redecided: 0,
        link_health_keys_resolved: 0,
        link_health_candidates_read: 0,
        read_steps: 0,
    }) };
}

/// Record what one applied changeset did: the store's `outcome` for it.
///
/// The store answers every increment with an outcome, and this is where that
/// answer stops being dropped on the floor: the job that applied the changeset
/// records it on its own thread, and the entry point that job runs under folds
/// the thread's tally into the host's account. An increment carrying findings
/// alone applied no changeset and is not one; what its request did is
/// [`count_increment_work`]'s.
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

/// Record what one increment's request did, whether or not it carried a
/// changeset: the findings it wrote and the re-decision it ran, read off the
/// derivation `counters` the request kept, and the `read_steps` its multi-row
/// reads took.
///
/// Every increment a job runs is counted here, a flush carrying findings
/// alone among them: its request writes those findings and reads the store
/// like any other, so leaving it out would leave work the job did out of the
/// job's account.
pub(crate) fn count_increment_work(counters: &DerivationCounters, read_steps: u64) {
    CHANGESETS.with(|cell| {
        let mut tally = cell.get();
        tally.findings_written += counters.findings_written();
        tally.links_redecided += counters.links_redecided();
        tally.link_health_keys_resolved += counters.link_health_keys_resolved();
        tally.link_health_candidates_read += counters.link_health_candidates_read();
        tally.read_steps += read_steps;
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

thread_local! {
    static RULE_WORK: Cell<RuleWork> = const { Cell::new(RuleWork::NONE) };
}

/// Record what judging one document against the vault schema's field
/// declarations and rules paid.
///
/// Tallied on the thread that derived the document, beside
/// [`count_document_derived`], and folded into the account by the job that
/// thread runs, as a changeset is.
pub(crate) fn count_rule_work(work: RuleWork) {
    RULE_WORK.with(|cell| cell.set(cell.get().plus(work)));
}

fn take_rule_work() -> RuleWork {
    RULE_WORK.with(|cell| cell.replace(RuleWork::NONE))
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
            affected_paths: Default::default(),
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

    /// **Rule work a job judged reaches the account when the job ends**,
    /// count by count, and work judged between two jobs belongs to neither:
    /// the logical counts no statement counter sees are a job's account like
    /// its documents derived.
    #[test]
    fn the_rule_work_a_job_judged_reaches_the_account_when_the_job_ends() {
        let judged = RuleWork {
            rules_evaluated: 3,
            selector_terms: 5,
            rules_selected: 2,
            constraint_entries: 7,
            declaration_bytes: 17,
            constraints_judged: 4,
            pattern_characters: 11,
            placement_walks: 1,
            placement_weight: 13,
            placement_verdicts_reused: 1,
            findings: 2,
            defaults_rounds: 3,
            defaults_filled: 2,
            captures_bound: 1,
        };
        let evidence = Arc::new(JobEvidence::default());
        count_rule_work(judged);
        {
            let _job = evidence.attributing();
            count_rule_work(judged);
            count_rule_work(judged);
            assert_eq!(evidence.read().rule_work, RuleWork::NONE);
        }
        count_rule_work(judged);
        let before = evidence.read();
        drop(evidence.attributing());
        assert_eq!(before.rule_work, judged.plus(judged));
        assert_eq!(evidence.read().since(before).rule_work, RuleWork::NONE);
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

    /// A changeset's re-decision steps reach the account with the job that
    /// applied it, and an apply's snapshot work where it is counted, with no
    /// window standing, as a mint's does.
    #[test]
    fn a_changesets_steps_and_an_applys_snapshot_reach_the_account() {
        let evidence = Arc::new(JobEvidence::default());
        {
            let _job = evidence.attributing();
            count_changeset(&outcome());
            count_increment_work(&DerivationCounters::default(), 40);
        }
        evidence.count_apply_snapshot(SnapshotWork {
            snapshots_opened: 1,
            statements: 4,
            vm_steps: 300,
            full_scan_steps: 0,
        });
        let read = evidence.read();
        assert_eq!(read.changeset_read_steps, 40);
        assert_eq!(
            (
                read.apply_snapshots_opened,
                read.apply_statements,
                read.apply_vm_steps,
                read.apply_full_scan_steps
            ),
            (1, 4, 300, 0)
        );
    }

    /// Under an armed recording, the files a job's counted reads read reach
    /// the account with the job, and nothing is kept for a job that ran
    /// while none was armed.
    #[cfg(feature = "induced-failure")]
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the document the job reads.
    fn a_jobs_files_reach_the_account_only_while_recorded() {
        let scratch = norn_testkit::scratch::Scratch::new("evidence-files");
        let anchor = scratch.join("");
        std::fs::write(anchor.join("note.md"), "body").expect("a document");
        let evidence = Arc::new(JobEvidence::default());
        {
            let _job = evidence.attributing();
            norn_fs::read_and_hash(&anchor, std::path::Path::new("note.md")).expect("a read");
        }
        assert!(evidence.files_read().is_empty());
        {
            let _recording = norn_fs::reads::record_files();
            let _job = evidence.attributing();
            norn_fs::read_and_hash(&anchor, std::path::Path::new("note.md")).expect("a read");
        }
        assert_eq!(
            evidence.files_read(),
            vec![norn_fs::reads::FileRead {
                act: norn_fs::reads::ReadAct::Document,
                path: anchor.join("note.md"),
            }]
        );
        assert_eq!(evidence.read().document_opens, 2);
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
/// **What an establishment ran under the gate is the gate holder's reading of
/// SQLite's own count.** A handle's connection counts every statement SQLite
/// begins on it, on the thread that begins it. The acquisition reads its
/// thread's count and its entry gate's take count as each round takes the
/// gate, and both again as the establishing round gives the gate back; what a
/// round that let the gate go ran is carried into the next, and the wait
/// between them is outside both. The statement difference is what is counted,
/// and the establishment reports no count of its own; the take difference is
/// whether the establishing round's two readings bound one continuous hold,
/// because a gate given back and taken again between them is a retake the gate
/// itself counted. A statement moved outside the hold is outside the readings,
/// and one run while the gate was let go and taken back is a retake.
///
/// **What an acquisition runs under the gate is three readings, not one.** A
/// read that was served ran its snapshot's establishment,
/// [`norn_store::SNAPSHOT_ESTABLISHMENT_STATEMENTS`]: the deferred `BEGIN`,
/// which reads no row, and the establishing statement, the one read of the
/// database, and the bar on gate-held query work is read off that; the repair
/// a read runs when it finds the handle slot empty is under the same gate and
/// is not query work; and an establishment that refused ran what SQLite began
/// before it refused, and the rollback that ended it, under the gate and served
/// nothing. They are counted apart so each reading says what it
/// asserts and none of them has to stand for another, and the widest reading
/// holds what one acquisition ran across all three so a ceiling over a single
/// read is one number rather than a sum of maxima.
///
/// **Every act is counted where the acquisition pays for it, never where the
/// read leaves**, so what an acquisition did is in the account whichever way it
/// left. The mint is counted where the mint returns, the establishment where
/// the hold it ran under gives the gate back, and the wait for the entry's
/// connection and the settle wait where each begins, so the refusals are
/// accounted exactly as the answers are. A wait that begins cannot be
/// abandoned: a wait for the connection returns with the connection or at the
/// read's bound, and a settle wait returns woken or at the read's bound, so a
/// wait counted where it begins is a wait that ends. No path out of an
/// acquisition runs a statement under the gate, waits out another read, or
/// waits out a change, and reports nothing.
///
/// **The rounds of the gate an acquisition took are the one reading recorded
/// where it leaves**, because the count is whole only there. It is recorded
/// on every way out, served, refused before a wait or after one, and
/// unwound, so a refused acquisition's rounds are in the account as a served
/// one's are.
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
    gate_retakes_within_establishing_holds: AtomicU64,
    reader_waits: AtomicU64,
    gate_rounds_after_the_first: AtomicU64,
    widest_gate_rounds_after_the_first: AtomicU64,
    settle_waits: AtomicU64,
    settle_rounds: AtomicU64,
    settle_expiries: AtomicU64,
    preview_link_judgments: LinkJudgmentAccount,
    repair_batches: RepairBatchAccount,
    planning_holds: PlanningHoldAccount,
}

/// What one acquisition read off SQLite's count of its thread and off its
/// entry gate, between the readings its rounds of the gate opened on and the
/// one its establishing round gave the gate back with.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EstablishingHold {
    /// Statements SQLite began on the acquiring thread while the acquisition
    /// held the gate, over every round it took.
    pub(crate) statements: u64,
    /// Times the entry gate was taken between the establishing round's two
    /// readings. Zero for a hold that never let the gate go.
    pub(crate) gate_retakes: u64,
}

/// One reading of a host's read account, over the whole of its life.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReadReading {
    /// Reads that took a hold, over every entry.
    pub reads_served: u64,
    /// Statements SQLite ran on the served reads' connections while the entry
    /// gate was held, as the holder of that gate read them off SQLite's count
    /// of its thread on both sides of its hold.
    ///
    /// **[`norn_store::SNAPSHOT_ESTABLISHMENT_STATEMENTS`] per read served**,
    /// the deferred `BEGIN` and the establishing statement, so this moves with
    /// `reads_served`, and any other number is a read that ran other work
    /// under the lock every other holder of that entry waits behind. That
    /// claim is what this field is for, so it holds the establishment alone:
    /// the repair below runs under the same gate and is not query work, and
    /// folding the two would leave the per-read reading unable to say which of
    /// them moved it.
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
    /// Statements SQLite ran under the entry gate for establishments that
    /// refused.
    ///
    /// An establishment that met a busy database opened its transaction, ran
    /// the statement that refused it and rolled back, all under the gate, and
    /// served no read. What is counted is what SQLite began: the `BEGIN`, the
    /// statement that refused where SQLite began running it, and the
    /// `ROLLBACK`. Those statements are kept here rather than in
    /// `statements_under_the_gate` so that reading stays exactly the reads it
    /// served; what is kept here makes no per-attempt claim, because an
    /// attempt refused before SQLite began a statement ran none.
    pub refused_establishment_statements_under_the_gate: u64,
    /// The most statements any one acquisition ran under the gate — **its
    /// mint's and its establishment's together** — which is the value a
    /// per-read ceiling is stated against.
    ///
    /// An acquisition contributes what it ran, whether or not it was served:
    /// its establishment's statements for an acquisition that found a handle
    /// standing and established; its mint's statements and then those where it
    /// healed first; its mint's alone where the mint refused; and its mint's
    /// plus the refused establishment's where the establishment is what
    /// refused.
    pub widest_statements_under_the_gate: u64,
    /// Times an entry gate was taken again inside an establishing hold: between
    /// the reading an acquisition opened its establishing hold on and the one
    /// it gave the gate back with.
    ///
    /// **Zero.** An establishing hold is one continuous hold of the gate, so
    /// the gate's own take count reads the same at both ends of it. Any other
    /// number is an acquisition that let the gate go and took it back around
    /// what it counted, which is an establishment that may have run outside
    /// the lock however many statements the count across it reads. Served and
    /// refused establishments alike are in it.
    pub gate_retakes_within_establishing_holds: u64,
    /// Acquisitions that gave the entry gate back and waited for the one
    /// connection their entry holds. Nonzero is reader contention, measured
    /// rather than assumed.
    ///
    /// **Counted where the wait begins, whichever way the acquisition
    /// leaves.** The count moves while the acquisition is still waiting, so a
    /// reading taken during contention already names every acquisition that
    /// found the connection taken. A wait that begins returns only with the
    /// connection, so each one counted here also ends. An acquisition that
    /// waited and was then refused, because its entry stopped serving or
    /// because its handle was replaced while it waited, paid the whole of that
    /// wait, so it is one of these and is not among `reads_served`. Those are
    /// the paths contention is most likely to be interesting on, and a reading
    /// that held only served reads would under-report exactly there.
    ///
    /// It is a wait for the reader's connection and not for a gate: no
    /// acquisition waits for that connection while it holds the entry gate,
    /// and nothing here counts a wait for the gate itself.
    pub reader_waits: u64,
    /// Rounds of the entry gate acquisitions took after their first, summed
    /// over acquisitions.
    ///
    /// **One per wait for the connection, served or refused.** Every
    /// acquisition takes a first round of the gate. One that found the
    /// connection taken gives the gate back, waits, and takes another round,
    /// in which it reads afresh the stance its entry stands at, because the
    /// instant it read before is not the instant it answers under. That round
    /// is the cost of contention: a hold of the gate taken again. The stance
    /// read inside it is an in-memory read under that hold, so the round is
    /// what is counted, and a round is the one way an acquisition reads the
    /// stance again. Each acquisition counts its own rounds and records them
    /// once, where it leaves. So this equals [`ReadReading::reader_waits`],
    /// and a count below it is a contended acquisition that answered under
    /// the stance it read before it waited. The rounds after a settle wait
    /// are [`ReadReading::settle_rounds`], which is what keeps that
    /// equality whole.
    pub gate_rounds_after_the_first: u64,
    /// The most rounds of the entry gate any one acquisition took after its
    /// first.
    ///
    /// **One, or none, over an entry that neither settles nor replaces its
    /// handle while a read waits.** A contended acquisition holds the
    /// connection from the round after its wait, so that round cannot contend
    /// and ends in the establishment or a refusal. Only an entry that settled,
    /// or replaced the handle waited for, sends the acquisition round again:
    /// it gives the connection back, waits out the change or takes the new
    /// handle, and may contend once more. A reading above one over a vault at
    /// rest is an acquisition that took a round the read path does not have,
    /// and it is read per acquisition so that one taking two cannot hide
    /// beside one taking none.
    pub widest_gate_rounds_after_the_first: u64,
    /// Settle waits that acquisitions began over an entry taking in a change:
    /// the acquisition gave the entry gate back and waited on the entry's
    /// stance signal, for the entry to publish `Ready` or to derive the facts
    /// the read met.
    ///
    /// **Counted where each wait begins**, so a reading taken while a read
    /// waits already names it. One acquisition may wait more than once where
    /// the entry settles, serves and settles again before it retakes the gate.
    /// Nonzero is reads meeting a change, which the read-concurrency bars do
    /// not provoke and do not read.
    pub settle_waits: u64,
    /// Rounds of the entry gate acquisitions took after a settle wait,
    /// recorded where each acquisition leaves.
    ///
    /// **Counted apart from [`ReadReading::gate_rounds_after_the_first`]**, so
    /// that reading goes on equalling [`ReadReading::reader_waits`]: every
    /// wait for the connection is followed by one round, and every settle
    /// wait by one round here. So this equals `settle_waits` over any
    /// window in which no acquisition is still waiting.
    pub settle_rounds: u64,
    /// Acquisitions refused because the entry was still taking in its change
    /// when their settle bound ran out: each is a
    /// [`ReadRefusal::Unsettled`](crate::ReadRefusal::Unsettled).
    pub settle_expiries: u64,
    /// What the previews this host answered spent judging their plans' links
    /// on the store's resolution door, over every judgment each ran.
    pub preview_link_judgments: LinkJudgmentCost,
    /// What the batch reads of the repairs this host planned reported of
    /// themselves, over every repair.
    pub repair_batches: RepairBatchCost,
    /// What the read holds this host planned on ran on their snapshots, over
    /// every preview and repair.
    pub planning_holds: PlanningHoldCost,
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
    /// Times an entry gate was taken again inside this window's establishing
    /// holds.
    pub gate_retakes_within_establishing_holds: u64,
    /// Rounds of the entry gate this window's acquisitions took after their
    /// first, recorded where each acquisition left.
    pub gate_rounds_after_the_first: u64,
    /// Settle waits this window's acquisitions began.
    pub settle_waits: u64,
    /// Rounds of the entry gate this window's acquisitions took after a
    /// settle wait.
    pub settle_rounds: u64,
    /// Acquisitions this window refused past their settle bound.
    pub settle_expiries: u64,
    /// What this window's previews spent judging their plans' links.
    pub preview_link_judgments: LinkJudgmentCost,
    /// What this window's batch reads reported of themselves.
    pub repair_batches: RepairBatchCost,
    /// What this window's planning holds ran on their snapshots.
    pub planning_holds: PlanningHoldCost,
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
            gate_retakes_within_establishing_holds: self
                .gate_retakes_within_establishing_holds
                .saturating_sub(earlier.gate_retakes_within_establishing_holds),
            gate_rounds_after_the_first: self
                .gate_rounds_after_the_first
                .saturating_sub(earlier.gate_rounds_after_the_first),
            settle_waits: self.settle_waits.saturating_sub(earlier.settle_waits),
            settle_rounds: self.settle_rounds.saturating_sub(earlier.settle_rounds),
            settle_expiries: self.settle_expiries.saturating_sub(earlier.settle_expiries),
            preview_link_judgments: self
                .preview_link_judgments
                .since(earlier.preview_link_judgments),
            repair_batches: self.repair_batches.since(earlier.repair_batches),
            planning_holds: self.planning_holds.since(earlier.planning_holds),
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
            gate_retakes_within_establishing_holds: get(
                &self.gate_retakes_within_establishing_holds
            ),
            reader_waits: get(&self.reader_waits),
            gate_rounds_after_the_first: get(&self.gate_rounds_after_the_first),
            widest_gate_rounds_after_the_first: get(&self.widest_gate_rounds_after_the_first),
            settle_waits: get(&self.settle_waits),
            settle_rounds: get(&self.settle_rounds),
            settle_expiries: get(&self.settle_expiries),
            preview_link_judgments: self.preview_link_judgments.read(),
            repair_batches: self.repair_batches.read(),
            planning_holds: self.planning_holds.read(),
        }
    }

    /// Record what one preview spent judging its plan's links, where the
    /// preview answers, whatever it answered.
    pub(crate) fn count_preview_link_judgments(&self, cost: LinkJudgmentCost) {
        self.preview_link_judgments.add(cost);
    }

    /// Record what one repair's batch read reported of itself, whatever the
    /// read answered.
    pub(crate) fn count_repair_batch(&self, cost: RepairBatchCost) {
        self.repair_batches.add(cost);
    }

    /// Record what one planning hold ran on its snapshot, where the hold is
    /// given back, however the planning ended.
    pub(crate) fn count_planning_hold(&self, cost: PlanningHoldCost) {
        self.planning_holds.add(cost);
    }

    /// Record what one read's mint ran under the entry gate.
    ///
    /// **This is called where the mint returns and not where the read leaves**,
    /// because the read has already paid for those statements by then: a read
    /// that is refused after its mint refused ran them under the gate exactly
    /// as a read that went on to establish did. A read that minted nothing
    /// reports zero here and moves nothing.
    ///
    /// `acquisition_mints` is what every mint this acquisition ran has run so
    /// far, this one's included: an acquisition that waited and found its
    /// entry's slot empty again mints once more, and the widest reading is
    /// what one acquisition ran.
    pub(crate) fn count_mint_under_the_gate(&self, statements: u64, acquisition_mints: u64) {
        self.mint_statements_under_the_gate
            .fetch_add(statements, Ordering::Relaxed);
        // The widest is per acquisition, and this acquisition has run its
        // mints' statements and no establishing statement yet. A read that
        // goes on to establish widens it again below; a read that is refused
        // from here leaves this as what it ran.
        self.widest_statements_under_the_gate
            .fetch_max(acquisition_mints, Ordering::Relaxed);
    }

    /// Record what one served acquisition's establishing hold ran under the
    /// entry gate.
    ///
    /// **`hold` is the gate holder's own reading**, not a count the
    /// establishment reports: the acquisition read SQLite's count of its
    /// thread and its gate's take count as it took the gate, and again as it
    /// gave the gate back, and these are the differences. A statement SQLite
    /// runs on the read's connection under the gate is in its statements
    /// however it was composed, so the per-read bar read off
    /// `statements_under_the_gate` fails for one more than the establishment;
    /// an establishing statement run before or after that hold is not in them,
    /// and the bar fails for one fewer; one run while the gate was let go and
    /// taken back inside the hold is a retake, and the zero bar read off
    /// `gate_retakes_within_establishing_holds` fails for that.
    ///
    /// The mint's statements are passed in again, already counted by the mint,
    /// because the widest reading is per acquisition: what one acquisition ran
    /// under the gate is its mint's statements and its establishment's, and a
    /// ceiling read off two separate maxima would be a sum of two different
    /// acquisitions.
    pub(crate) fn count_establishment_under_the_gate(
        &self,
        hold: EstablishingHold,
        mint_statements: u64,
    ) {
        self.statements_under_the_gate
            .fetch_add(hold.statements, Ordering::Relaxed);
        self.count_the_hold(hold, mint_statements);
    }

    /// Record what one acquisition whose establishment refused ran under the
    /// entry gate, read the way [`ReadEvidence::count_establishment_under_the_gate`]
    /// reads it.
    ///
    /// **It is counted apart from the served reading** because
    /// `statements_under_the_gate` claims to be the reads it served, and this
    /// acquisition served none; it held the gate for what it ran all the same,
    /// so the refusal is a path that paid for it.
    pub(crate) fn count_refused_establishment_under_the_gate(
        &self,
        hold: EstablishingHold,
        mint_statements: u64,
    ) {
        self.refused_establishment_statements_under_the_gate
            .fetch_add(hold.statements, Ordering::Relaxed);
        self.count_the_hold(hold, mint_statements);
    }

    /// Count what served and refused establishing holds share: whether the
    /// hold stayed one hold, and the per-acquisition reading of what one
    /// acquisition ran under the gate, its mint's statements and its
    /// establishing hold's together.
    fn count_the_hold(&self, hold: EstablishingHold, mint_statements: u64) {
        self.gate_retakes_within_establishing_holds
            .fetch_add(hold.gate_retakes, Ordering::Relaxed);
        self.widest_statements_under_the_gate.fetch_max(
            mint_statements.saturating_add(hold.statements),
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
    /// eventually ran waited for nothing.
    pub(crate) fn count_reader_wait(&self) {
        self.reader_waits.fetch_add(1, Ordering::Relaxed);
    }

    /// Record the rounds of its entry gate one acquisition took after its
    /// first.
    ///
    /// **This is called once per acquisition, where it leaves**, with the
    /// acquisition's own count of its rounds, so the total is the rounds taken
    /// and the widest is what one acquisition took rather than a ratio over
    /// many: an acquisition taking two reads two there however many beside it
    /// took none.
    pub(crate) fn count_gate_rounds_after_the_first(&self, rounds: u64) {
        self.gate_rounds_after_the_first
            .fetch_add(rounds, Ordering::Relaxed);
        self.widest_gate_rounds_after_the_first
            .fetch_max(rounds, Ordering::Relaxed);
    }

    /// Record that one acquisition began a settle wait: a wait for its entry
    /// to publish `Ready` or to derive the facts the read met.
    ///
    /// **Called where the wait begins**, after the acquisition gave the entry
    /// gate back, so a reading taken while the read waits names it whatever
    /// answer it goes on to get.
    pub(crate) fn count_settle_wait(&self) {
        self.settle_waits.fetch_add(1, Ordering::Relaxed);
    }

    /// Record the rounds of its entry gate one acquisition took after its
    /// settle waits, once, where it leaves.
    pub(crate) fn count_settle_rounds(&self, rounds: u64) {
        self.settle_rounds.fetch_add(rounds, Ordering::Relaxed);
    }

    /// Record that one acquisition was refused past its settle bound.
    pub(crate) fn count_settle_expiry(&self) {
        self.settle_expiries.fetch_add(1, Ordering::Relaxed);
    }

    /// Record that one read was served.
    pub(crate) fn count_read(&self) {
        self.reads_served.fetch_add(1, Ordering::Relaxed);
    }
}

/// What judging a plan's links on the store's resolution door cost, summed
/// over every judgment one request ran: a link cascade's backlink pass and
/// its spelling probe, the planning's resolution change set, and the
/// applier's computation of it again.
///
/// **Read off the judgments the request really ran.** The plan's link index
/// adds each judgment's own report ([`norn_store::ResolutionWork`]) and the
/// difference its snapshot's counters moved by while the judgment ran, so a
/// bar over a planned move reads what its planning cost rather than a copy of
/// what that planning is thought to ask.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LinkJudgmentCost {
    /// Judgments run: each one call of the resolution door.
    pub judgments: u64,
    /// Links judged, each once per judgment however many ways it was reached.
    pub links_evaluated: u64,
    /// Key resolutions the judgments ran.
    pub keys_resolved: u64,
    /// Rows the key resolutions read.
    pub head_rows: u64,
    /// Statements the judgments ran on their snapshot.
    pub statements_executed: u64,
    /// Virtual-machine steps those statements took.
    pub vm_steps: u64,
    /// Steps those statements took walking a table or an index end to end.
    pub full_scan_steps: u64,
}

impl LinkJudgmentCost {
    /// No judgment at all.
    pub(crate) const NONE: LinkJudgmentCost = LinkJudgmentCost {
        judgments: 0,
        links_evaluated: 0,
        keys_resolved: 0,
        head_rows: 0,
        statements_executed: 0,
        vm_steps: 0,
        full_scan_steps: 0,
    };

    /// One judgment that did `work` while its snapshot's counters moved from
    /// `before` to `after`.
    pub(crate) fn of(
        work: &norn_store::ResolutionWork,
        before: norn_store::SnapshotCounters,
        after: norn_store::SnapshotCounters,
    ) -> LinkJudgmentCost {
        LinkJudgmentCost {
            judgments: 1,
            links_evaluated: work.links_evaluated,
            keys_resolved: work.keys_resolved,
            head_rows: work.head_rows,
            statements_executed: after
                .statements_executed()
                .saturating_sub(before.statements_executed()),
            vm_steps: after.vm_steps().saturating_sub(before.vm_steps()),
            full_scan_steps: after
                .full_scan_steps()
                .saturating_sub(before.full_scan_steps()),
        }
    }

    /// This cost and `other` together.
    #[must_use]
    pub(crate) fn plus(self, other: LinkJudgmentCost) -> LinkJudgmentCost {
        LinkJudgmentCost {
            judgments: self.judgments + other.judgments,
            links_evaluated: self.links_evaluated + other.links_evaluated,
            keys_resolved: self.keys_resolved + other.keys_resolved,
            head_rows: self.head_rows + other.head_rows,
            statements_executed: self.statements_executed + other.statements_executed,
            vm_steps: self.vm_steps + other.vm_steps,
            full_scan_steps: self.full_scan_steps + other.full_scan_steps,
        }
    }

    /// What was spent between an earlier running total and this one.
    #[must_use]
    pub fn since(self, earlier: LinkJudgmentCost) -> LinkJudgmentCost {
        LinkJudgmentCost {
            judgments: self.judgments.saturating_sub(earlier.judgments),
            links_evaluated: self.links_evaluated.saturating_sub(earlier.links_evaluated),
            keys_resolved: self.keys_resolved.saturating_sub(earlier.keys_resolved),
            head_rows: self.head_rows.saturating_sub(earlier.head_rows),
            statements_executed: self
                .statements_executed
                .saturating_sub(earlier.statements_executed),
            vm_steps: self.vm_steps.saturating_sub(earlier.vm_steps),
            full_scan_steps: self.full_scan_steps.saturating_sub(earlier.full_scan_steps),
        }
    }
}

/// A running total of [`LinkJudgmentCost`]s, added to from any thread.
#[derive(Debug, Default)]
struct LinkJudgmentAccount {
    judgments: AtomicU64,
    links_evaluated: AtomicU64,
    keys_resolved: AtomicU64,
    head_rows: AtomicU64,
    statements_executed: AtomicU64,
    vm_steps: AtomicU64,
    full_scan_steps: AtomicU64,
}

impl LinkJudgmentAccount {
    fn add(&self, cost: LinkJudgmentCost) {
        for (field, value) in [
            (&self.judgments, cost.judgments),
            (&self.links_evaluated, cost.links_evaluated),
            (&self.keys_resolved, cost.keys_resolved),
            (&self.head_rows, cost.head_rows),
            (&self.statements_executed, cost.statements_executed),
            (&self.vm_steps, cost.vm_steps),
            (&self.full_scan_steps, cost.full_scan_steps),
        ] {
            field.fetch_add(value, Ordering::Relaxed);
        }
    }

    fn read(&self) -> LinkJudgmentCost {
        let get = |field: &AtomicU64| field.load(Ordering::Relaxed);
        LinkJudgmentCost {
            judgments: get(&self.judgments),
            links_evaluated: get(&self.links_evaluated),
            keys_resolved: get(&self.keys_resolved),
            head_rows: get(&self.head_rows),
            statements_executed: get(&self.statements_executed),
            vm_steps: get(&self.vm_steps),
            full_scan_steps: get(&self.full_scan_steps),
        }
    }
}

/// What the batch reads of the repairs a host planned reported of
/// themselves, summed.
///
/// The snapshot work of a repair's read hold, the batch read's among it, is
/// the planning holds' ([`PlanningHoldCost`]).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RepairBatchCost {
    /// Batches read: one per repair that reached its read.
    pub batches: u64,
    /// The statements the batches report running.
    pub validate_statements: u64,
    /// The rows the batches report reading.
    pub validate_rows_read: u64,
    /// The steps the batches report taking through a loop no constraint
    /// bounds.
    pub validate_full_scan_steps: u64,
    /// The sorts the batches report running.
    pub validate_sorts: u64,
    /// The virtual-machine operations the batches report running.
    pub validate_vm_steps: u64,
}

impl RepairBatchCost {
    /// One batch read that reported `work`, none where it was refused.
    pub(crate) fn of(work: Option<&norn_store::ValidateWork>) -> RepairBatchCost {
        let work = work.copied().unwrap_or_default();
        RepairBatchCost {
            batches: 1,
            validate_statements: work.statements,
            validate_rows_read: work.rows_read,
            validate_full_scan_steps: work.full_scan_steps,
            validate_sorts: work.sorts,
            validate_vm_steps: work.vm_steps,
        }
    }

    /// What was spent between an earlier running total and this one.
    #[must_use]
    pub fn since(self, earlier: RepairBatchCost) -> RepairBatchCost {
        RepairBatchCost {
            batches: self.batches.saturating_sub(earlier.batches),
            validate_statements: self
                .validate_statements
                .saturating_sub(earlier.validate_statements),
            validate_rows_read: self
                .validate_rows_read
                .saturating_sub(earlier.validate_rows_read),
            validate_full_scan_steps: self
                .validate_full_scan_steps
                .saturating_sub(earlier.validate_full_scan_steps),
            validate_sorts: self.validate_sorts.saturating_sub(earlier.validate_sorts),
            validate_vm_steps: self
                .validate_vm_steps
                .saturating_sub(earlier.validate_vm_steps),
        }
    }
}

/// A running total of [`RepairBatchCost`]s, added to from any thread.
#[derive(Debug, Default)]
struct RepairBatchAccount {
    batches: AtomicU64,
    validate_statements: AtomicU64,
    validate_rows_read: AtomicU64,
    validate_full_scan_steps: AtomicU64,
    validate_sorts: AtomicU64,
    validate_vm_steps: AtomicU64,
}

impl RepairBatchAccount {
    fn add(&self, cost: RepairBatchCost) {
        for (field, value) in [
            (&self.batches, cost.batches),
            (&self.validate_statements, cost.validate_statements),
            (&self.validate_rows_read, cost.validate_rows_read),
            (
                &self.validate_full_scan_steps,
                cost.validate_full_scan_steps,
            ),
            (&self.validate_sorts, cost.validate_sorts),
            (&self.validate_vm_steps, cost.validate_vm_steps),
        ] {
            field.fetch_add(value, Ordering::Relaxed);
        }
    }

    fn read(&self) -> RepairBatchCost {
        let get = |field: &AtomicU64| field.load(Ordering::Relaxed);
        RepairBatchCost {
            batches: get(&self.batches),
            validate_statements: get(&self.validate_statements),
            validate_rows_read: get(&self.validate_rows_read),
            validate_full_scan_steps: get(&self.validate_full_scan_steps),
            validate_sorts: get(&self.validate_sorts),
            validate_vm_steps: get(&self.validate_vm_steps),
        }
    }
}

/// What the read holds a host planned on ran on their snapshots, summed: the
/// whole of each hold's snapshot counters when its planning is done, whoever
/// ran the statements, as an apply job's snapshots are counted
/// in the job's account.
///
/// **Counted where the hold is given back, not where a read is made.** A
/// read added anywhere inside a preview's or a repair's planning moves the
/// account, so a bar over a repair reads what the repair ran rather than what
/// its batch read alone is thought to. The establishing statement is among
/// the statements, and the link judgments the plan ran are among all three
/// counts, as well as in [`LinkJudgmentCost`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PlanningHoldCost {
    /// Holds planned on, one snapshot each.
    pub holds: u64,
    /// Statements the snapshots ran, as they counted them.
    pub statements_executed: u64,
    /// Virtual-machine steps those statements took.
    pub vm_steps: u64,
    /// Steps those statements took walking a table or an index end to end.
    pub full_scan_steps: u64,
}

impl PlanningHoldCost {
    /// One hold whose snapshot's counters read `counters`.
    pub(crate) fn of(counters: norn_store::SnapshotCounters) -> PlanningHoldCost {
        PlanningHoldCost {
            holds: 1,
            statements_executed: counters.statements_executed(),
            vm_steps: counters.vm_steps(),
            full_scan_steps: counters.full_scan_steps(),
        }
    }

    /// What was spent between an earlier running total and this one.
    #[must_use]
    pub fn since(self, earlier: PlanningHoldCost) -> PlanningHoldCost {
        PlanningHoldCost {
            holds: self.holds.saturating_sub(earlier.holds),
            statements_executed: self
                .statements_executed
                .saturating_sub(earlier.statements_executed),
            vm_steps: self.vm_steps.saturating_sub(earlier.vm_steps),
            full_scan_steps: self.full_scan_steps.saturating_sub(earlier.full_scan_steps),
        }
    }
}

/// A running total of [`PlanningHoldCost`]s, added to from any thread.
#[derive(Debug, Default)]
struct PlanningHoldAccount {
    holds: AtomicU64,
    statements_executed: AtomicU64,
    vm_steps: AtomicU64,
    full_scan_steps: AtomicU64,
}

impl PlanningHoldAccount {
    fn add(&self, cost: PlanningHoldCost) {
        for (field, value) in [
            (&self.holds, cost.holds),
            (&self.statements_executed, cost.statements_executed),
            (&self.vm_steps, cost.vm_steps),
            (&self.full_scan_steps, cost.full_scan_steps),
        ] {
            field.fetch_add(value, Ordering::Relaxed);
        }
    }

    fn read(&self) -> PlanningHoldCost {
        let get = |field: &AtomicU64| field.load(Ordering::Relaxed);
        PlanningHoldCost {
            holds: get(&self.holds),
            statements_executed: get(&self.statements_executed),
            vm_steps: get(&self.vm_steps),
            full_scan_steps: get(&self.full_scan_steps),
        }
    }
}
