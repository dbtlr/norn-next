//! The authoritative inventory of Layer 2 certification cases.
//!
//! A soak run is *qualifying* only if it ran the cases the layer requires. That
//! sentence needs a list, and a list living in prose is a list nothing checks:
//! a case deleted, renamed, or moved behind a feature nothing turns on leaves
//! the suites green and the layer uncertified. So the list is code, and a
//! reconciliation walks it against cargo's own answer about what compiled.
//!
//! # The inventory is target-independent
//!
//! An entry names an id, the suite it belongs to, the capability lane a run has
//! to schedule it in, what it states, and the test that carries it. It does not
//! name a runner, a platform image or a workflow step: which machine runs which
//! lane is the campaign's business and changes without the obligations changing.
//!
//! # The reconciliation runs in both directions
//!
//! Forward: every id resolves to exactly one test cargo compiled into the target
//! its reference names, under the feature the entry declares, and that test is
//! one a plain run of the target executes. A case deleted, renamed, left behind
//! a feature the lane does not turn on, or `#[ignore]`d fails here — each of
//! those leaves a passing run that certified less than it claimed.
//!
//! Backward: every test compiled into a **claimed** target is named by some
//! entry, matched on the entry's whole `file::fn` reference rather than on the
//! function name alone. A case added to the churn suite and not to the inventory
//! is a case the layer's own record does not know about, and a later reader
//! counting the inventory would undercount what the suites hold. The claim is
//! per target and deliberately partial: [`CLAIMED_TARGETS`] holds the
//! integration suites that *are* certification suites and nothing else. A
//! crate's library target is never claimed — it holds every unit test that crate
//! has, and asking the inventory to name them all would make it a copy of the
//! workspace.
//!
//! # What the reconciliation does not catch
//!
//! It is a name-level reconciliation over cargo's listing, plus a digest over
//! the table's own text. Three edits pass it, and a reader should not read a
//! green reconciliation as ruling them out:
//!
//! - **A carrier and its entry renamed in one diff.** The reference still
//!   resolves, so nothing fails. What surfaces the edit is the contract digest:
//!   the carrier string is one of the fields [`contract_digest`] absorbs, so the
//!   value a qualifying run is recorded under moves and the five-run count
//!   restarts.
//! - **A carrier gutted.** A test whose body lost its assertions compiles,
//!   lists, and passes. Nothing here reads a body; what a case asserts is the
//!   review's question and the [`Case::states`] text is what a review reads it
//!   against.
//! - **An id reused for a different obligation.** Ids are never reused by rule
//!   rather than by mechanism — nothing here holds an id against what an older
//!   ledger recorded it as, because a record is the campaign's and a build knows
//!   only its own table. The digest moves, which is what tells a reader to look.
//!
//! # The unreached table is data, not a silence
//!
//! [`UNREACHED_ARMS`] holds the trust-transition arms the contract names and no
//! test carries at the production path. The reconciliation does not read this
//! table at all: what is wrong there is an ownership question rather than a
//! broken reference, and an obligation nobody wrote down is the one that gets
//! lost. The certification suite's own unreached-arm case is what prints the
//! table under a run and holds each row to naming what it awaits, so the ruling
//! that assigns the work has the facts in front of it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use norn_fixtures::digest::{Sha256, hex};

use crate::regression::{TargetRef, TestIndex, TestRef};

/// Which of the layer's suites a case belongs to.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Suite {
    /// Convergence-to-equivalence under ordinary editing. Rung 1's suite.
    Churn,
    /// A condition injected at the production path stated over it. Rungs 2 and
    /// 3's suite, across the three crates that own the seams a rung meets.
    InducedFailure,
    /// One store answering for itself, and two derivations answering the same.
    Operational,
    /// A watcher failure withdrawing trust and taking its own recovery path.
    TrustTransition,
}

impl Suite {
    pub fn name(&self) -> &'static str {
        match self {
            Suite::Churn => "churn",
            Suite::InducedFailure => "induced-failure",
            Suite::Operational => "operational",
            Suite::TrustTransition => "trust-transition",
        }
    }
}

/// The capability a run needs to schedule a case, and what decides its outcome.
///
/// This is what a certification campaign reads to know a single machine cannot
/// certify the layer: two of these lanes require the case to be *run twice*, on
/// machines that answer differently.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Lane {
    /// A filesystem and nothing else. One required outcome everywhere.
    Any,
    /// The machine's one real platform watcher, held under a cross-process
    /// lease: the case attaches a production host, so it contends with every
    /// other binary the runner started rather than only with its own threads.
    RealWatcher,
    /// A real watcher, and the required outcome is the volume's own answer
    /// about case: two spellings are one place where the volume folds and two
    /// where it does not. **Covered by a run on a folding volume and a run on a
    /// case-sensitive one**; either alone leaves the other answer unasserted.
    RealWatcherVolumeFoldingDecides,
    /// A real watcher, and which of two refusals is required is the backend's:
    /// a backend that adds a watch per directory (inotify) fails to install
    /// coverage before the heal walks, and one that covers a tree without
    /// reading each directory under it (FSEvents) leaves the walk to meet the
    /// denial. **Covered by a run on each backend.**
    RealWatcherBackendDecides,
}

impl Lane {
    pub fn name(&self) -> &'static str {
        match self {
            Lane::Any => "any",
            Lane::RealWatcher => "real-watcher",
            Lane::RealWatcherVolumeFoldingDecides => "real-watcher/volume-folding-decides",
            Lane::RealWatcherBackendDecides => "real-watcher/backend-decides",
        }
    }
}

/// One required certification case.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Case {
    /// Stable, kebab-case, and never reused. A ledger from an older run cites
    /// it, so renaming one is renaming the thing that run certified.
    pub id: &'static str,
    pub suite: Suite,
    pub lane: Lane,
    /// The obligation the case discharges, in the present tense.
    pub states: &'static str,
    /// The test that carries it, as `<workspace-relative file>::<fn>`.
    pub carrier: &'static str,
    /// The cargo feature the carrier compiles behind, where it compiles behind
    /// one. A case declaring none whose carrier is in fact gated fails the
    /// reconciliation, because cargo lists nothing for it.
    pub feature: Option<&'static str>,
}

/// An integration target the inventory claims whole: every test in it is a
/// certification case, so a test there and not here is a problem.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClaimedTarget {
    pub package: &'static str,
    /// The file stem under the package's `tests/`.
    pub stem: &'static str,
    pub feature: Option<&'static str>,
}

/// A trust-transition arm the contract names and nothing carries at the
/// production path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnreachedArm {
    pub arm: &'static str,
    /// What has to exist before a test can carry it — stated as a fact about
    /// the code, so the ruling that assigns the work is reading evidence.
    pub awaits: &'static str,
}

/// The feature every induced-failure arrangement sits behind.
const INDUCED_FAILURE: &str = "induced-failure";

/// The targets whose whole contents are certification cases.
///
/// `norn-fs`'s lockdown suite and `norn-store`'s environment suite name no
/// feature: each crate is its own dev-dependency with the fault seam on, so the
/// suites compile with it whether or not a caller asks. `norn-host`'s do name
/// it: nothing turns it on for that crate but a lane that says so, and without
/// it both files compile to zero tests.
pub const CLAIMED_TARGETS: &[ClaimedTarget] = &[
    ClaimedTarget {
        package: "norn-host",
        stem: "churn",
        feature: None,
    },
    ClaimedTarget {
        package: "norn-host",
        stem: "equivalence",
        feature: None,
    },
    ClaimedTarget {
        package: "norn-host",
        stem: "lockdown",
        feature: Some(INDUCED_FAILURE),
    },
    ClaimedTarget {
        package: "norn-host",
        stem: "kill_recovery",
        feature: Some(INDUCED_FAILURE),
    },
    ClaimedTarget {
        package: "norn-fs",
        stem: "lockdown",
        feature: None,
    },
    ClaimedTarget {
        package: "norn-store",
        stem: "environment",
        feature: None,
    },
];

/// **The inventory.** Every case Layer 2 certification requires.
pub const REQUIRED_CASES: &[Case] = &[
    // ---- the churn suite: rung 1, reached by ordinary operation ----
    Case {
        id: "churn-ordinary-editing",
        suite: Suite::Churn,
        lane: Lane::RealWatcher,
        states: "editing across nested directories converges on a build from zero",
        carrier: "crates/norn-host/tests/churn.rs::ordinary_editing_converges_on_a_build_from_zero",
        feature: None,
    },
    Case {
        id: "churn-edits-while-detached",
        suite: Suite::Churn,
        lane: Lane::RealWatcher,
        states: "a changing phase applied while nothing is attached converges through the attach \
                 heal rather than through a watcher report",
        carrier: "crates/norn-host/tests/churn.rs::\
                  edits_made_while_nothing_was_attached_converge_on_a_build_from_zero",
        feature: None,
    },
    Case {
        id: "churn-atomic-replacement-and-movement",
        suite: Suite::Churn,
        lane: Lane::RealWatcher,
        states: "content arriving whole over a derived document, and a name a row stands at \
                 moving, both converge on a build from zero",
        carrier: "crates/norn-host/tests/churn.rs::\
                  atomic_replacement_and_movement_converge_on_a_build_from_zero",
        feature: None,
    },
    Case {
        id: "churn-case-flip",
        suite: Suite::Churn,
        lane: Lane::RealWatcherVolumeFoldingDecides,
        states: "a name whose case flips leaves one row at the identity the volume resolves it to, \
                 spelled the way the directory renders it",
        carrier: "crates/norn-host/tests/churn.rs::a_case_flip_converges_on_a_build_from_zero",
        feature: None,
    },
    Case {
        id: "churn-burst-and-coalescing",
        suite: Suite::Churn,
        lane: Lane::RealWatcher,
        states: "a burst of writes to one path converges on the last bytes written",
        carrier: "crates/norn-host/tests/churn.rs::a_burst_converges_on_the_last_bytes_written",
        feature: None,
    },
    Case {
        id: "churn-validity-boundaries",
        suite: Suite::Churn,
        lane: Lane::RealWatcher,
        states: "documents crossing between readable, quarantined and degraded under a schema \
                 replacement converge on a build from zero",
        carrier: "crates/norn-host/tests/churn.rs::\
                  documents_crossing_validity_boundaries_converge_on_a_build_from_zero",
        feature: None,
    },
    Case {
        id: "churn-external-tool-catch-up",
        suite: Suite::Churn,
        lane: Lane::RealWatcher,
        states: "an editor's saves and a sleep's catch-up batch converge on a build from zero",
        carrier: "crates/norn-host/tests/churn.rs::\
                  an_external_tools_catch_up_converges_on_a_build_from_zero",
        feature: None,
    },
    Case {
        id: "churn-edits-during-an-active-heal",
        suite: Suite::Churn,
        lane: Lane::RealWatcher,
        states: "edits landing while a heal is half way through its walk converge, and the store \
                 records its deaths under the provenance that says so",
        carrier: "crates/norn-host/tests/churn.rs::\
                  edits_during_an_active_heal_converge_on_a_build_from_zero",
        feature: None,
    },
    Case {
        id: "churn-ambiguity-class-membership",
        suite: Suite::Churn,
        lane: Lane::RealWatcher,
        states: "a change to an ambiguity class's membership converges on a build from zero",
        carrier: "crates/norn-host/tests/churn.rs::\
                  an_ambiguity_classs_membership_change_converges_on_a_build_from_zero",
        feature: None,
    },
    Case {
        id: "churn-rendering-collision-clears",
        suite: Suite::Churn,
        lane: Lane::RealWatcher,
        states: "a rendering collision that clears releases the finding it withheld",
        carrier: "crates/norn-host/tests/churn.rs::\
                  a_rendering_collision_that_clears_converges_on_a_build_from_zero",
        feature: None,
    },
    Case {
        id: "churn-maintenance-account-moves",
        suite: Suite::Churn,
        lane: Lane::RealWatcher,
        states: "one seeded edit moves the maintenance account, which is what says the cost bars \
                 beside every other case are reading a counter that counts",
        carrier: "crates/norn-host/tests/churn.rs::one_seeded_edit_moves_the_maintenance_account",
        feature: None,
    },
    Case {
        id: "churn-work-bound-arithmetic",
        suite: Suite::Churn,
        lane: Lane::Any,
        states: "the churn cost bound admits its ceiling and refuses one step past it",
        carrier: "crates/norn-host/tests/churn.rs::\
                  the_work_bound_arithmetic_admits_its_ceiling_and_refuses_one_past_it",
        feature: None,
    },
    // ---- the induced-failure suite: rungs 2 and 3 ----
    Case {
        id: "induced-full-disk-refuses-the-heal",
        suite: Suite::InducedFailure,
        lane: Lane::RealWatcher,
        states: "a full disk under the heal's increment refuses, leaves the entry untrusted, and \
                 prunes nothing",
        carrier: "crates/norn-host/tests/lockdown.rs::\
                  a_full_disk_refuses_the_heal_and_the_entry_stays_untrusted",
        feature: Some(INDUCED_FAILURE),
    },
    Case {
        id: "induced-unreadable-document-refuses",
        suite: Suite::InducedFailure,
        lane: Lane::RealWatcher,
        states: "a document the heal cannot open is refused rather than quarantined: nothing was \
                 read, so nothing is known about what it holds",
        carrier: "crates/norn-host/tests/lockdown.rs::\
                  an_unreadable_document_refuses_the_heal_rather_than_quarantining_it",
        feature: Some(INDUCED_FAILURE),
    },
    Case {
        id: "induced-unreadable-subtree-refuses",
        suite: Suite::InducedFailure,
        lane: Lane::RealWatcherBackendDecides,
        states: "a subtree whose mode is revoked leaves the entry untrusted naming the path, \
                 through the refused heal or the lost watcher depending on which meets the denial \
                 first",
        carrier: "crates/norn-host/tests/lockdown.rs::\
                  an_unreadable_subtree_refuses_the_heal_rather_than_pruning_it",
        feature: Some(INDUCED_FAILURE),
    },
    Case {
        id: "induced-tear-at-a-chunk-boundary",
        suite: Suite::InducedFailure,
        lane: Lane::Any,
        states: "a process that dies between two changesets of one increment leaves every \
                 committed chunk whole and no part of the one in flight",
        carrier: "crates/norn-host/tests/lockdown.rs::\
                  a_tear_at_a_chunk_boundary_leaves_every_committed_chunk_whole",
        feature: Some(INDUCED_FAILURE),
    },
    Case {
        id: "induced-tear-between-a-flush-and-its-findings",
        suite: Suite::InducedFailure,
        lane: Lane::Any,
        states: "a tear between an increment and the findings recorded after it is healed by the \
                 rows' own state, with no edit to either file",
        carrier: "crates/norn-host/tests/lockdown.rs::\
                  a_tear_between_a_flush_and_its_findings_is_healed_by_the_rows_themselves",
        feature: Some(INDUCED_FAILURE),
    },
    Case {
        id: "induced-host-arm-attestation",
        suite: Suite::InducedFailure,
        lane: Lane::Any,
        states: "one child role serves both kinds of arm a host case gives it — the tear a torn \
                 case arms before its demand, and the watcher condition a watcher case arms \
                 outside the process — and the control spawned with nothing armed records \
                 nothing, which is what makes every arm assertion beside either kind mean \
                 something",
        carrier: "crates/norn-host/tests/lockdown.rs::\
                  the_child_role_attaches_under_whatever_it_was_armed_at",
        feature: Some(INDUCED_FAILURE),
    },
    Case {
        id: "induced-kill-mid-increment-converges",
        suite: Suite::InducedFailure,
        lane: Lane::RealWatcher,
        states: "the next attach after a process died mid-increment converges, and the work the \
                 tear lost returns through the heal's ordinary content-hash comparison",
        carrier: "crates/norn-host/tests/kill_recovery.rs::\
                  next_attach_converges_after_process_death_mid_increment",
        feature: Some(INDUCED_FAILURE),
    },
    Case {
        id: "induced-write-kernel-drifted-destination",
        suite: Suite::InducedFailure,
        lane: Lane::Any,
        states: "a destination that drifted between the fingerprint and the swap is refused \
                 without being replaced",
        carrier: "crates/norn-fs/tests/lockdown.rs::\
                  a_destination_that_drifted_is_refused_without_being_replaced",
        feature: None,
    },
    Case {
        id: "induced-write-kernel-full-disk-at-the-parent-sync",
        suite: Suite::InducedFailure,
        lane: Lane::Any,
        states: "a disk that fills at the parent directory sync leaves the new document published, \
                 because the swap already landed",
        carrier: "crates/norn-fs/tests/lockdown.rs::\
                  a_full_disk_at_the_parent_sync_leaves_the_new_document_published",
        feature: None,
    },
    Case {
        id: "induced-write-kernel-full-disk-before-the-swap",
        suite: Suite::InducedFailure,
        lane: Lane::Any,
        states: "a disk that fills while the shadow is staged refuses before the swap and leaves \
                 the destination alone",
        carrier: "crates/norn-fs/tests/lockdown.rs::\
                  a_full_disk_before_the_swap_refuses_and_leaves_the_destination_alone",
        feature: None,
    },
    Case {
        id: "induced-write-kernel-death-at-every-checkpoint",
        suite: Suite::InducedFailure,
        lane: Lane::Any,
        states: "process death at each named stage of a compare-and-swap publication leaves one \
                 whole document — the old one or the new one, never a torn one",
        carrier: "crates/norn-fs/tests/lockdown.rs::\
                  process_death_at_every_checkpoint_leaves_one_whole_document",
        feature: None,
    },
    Case {
        id: "induced-write-kernel-arm-attestation",
        suite: Suite::InducedFailure,
        lane: Lane::Any,
        states: "the child a torn write case spawns publishes under whatever it was armed at, \
                 which is what makes the write kernel's absence assertions mean something",
        carrier: "crates/norn-fs/tests/lockdown.rs::\
                  the_child_role_publishes_under_whatever_it_was_armed_at",
        feature: None,
    },
    Case {
        id: "induced-store-full-disk-is-not-damage",
        suite: Suite::InducedFailure,
        lane: Lane::Any,
        states: "a full disk under the store's own increment refuses and is never typed as damage, \
                 because rung 3 is for damaged state and never for a hostile environment",
        carrier: "crates/norn-store/tests/environment.rs::\
                  a_full_disk_refuses_the_increment_and_is_never_typed_as_damage",
        feature: None,
    },
    Case {
        id: "induced-store-schema-refusal-names-its-statement",
        suite: Suite::InducedFailure,
        lane: Lane::Any,
        states: "a store schema refused at a statement reports which statement met the condition, \
                 and does not call a refused creation damage",
        carrier: "crates/norn-store/tests/environment.rs::\
                  a_store_schema_refused_at_a_statement_names_the_statement",
        feature: None,
    },
    Case {
        id: "induced-store-corruption-is-damage",
        suite: Suite::InducedFailure,
        lane: Lane::Any,
        states: "a store schema that meets a corrupt database types it as damage, which is what \
                 sends the entry to rung 3",
        carrier: "crates/norn-store/tests/environment.rs::\
                  a_store_schema_that_meets_a_corrupt_database_is_damage",
        feature: None,
    },
    Case {
        id: "induced-committed-changesets-are-counted",
        suite: Suite::InducedFailure,
        lane: Lane::Any,
        states: "every committed changeset moves the count a tear is armed against, so an arm \
                 neither fires immediately nor never",
        carrier: "crates/norn-store/tests/environment.rs::every_committed_changeset_is_counted",
        feature: None,
    },
    // ---- the operational leg ----
    Case {
        id: "operational-two-derivations-from-zero-agree",
        suite: Suite::Operational,
        lane: Lane::RealWatcher,
        states: "one vault derived twice from zero holds the same derived facts, which is what \
                 says the comparator every churn case is judged by reports agreement where \
                 agreement is what there is",
        carrier: "crates/norn-host/tests/equivalence.rs::\
                  one_vault_derived_twice_from_zero_holds_the_same_derived_facts",
        feature: None,
    },
    // ---- the trust-transition arms ----
    //
    // Two doors reach these, and each row's carrier says which it took.
    //
    // The rows carried in `norn-host`'s library drive the transition through the
    // entry operations a host is generic over: the condition is handed to the
    // lifecycle rather than produced by a backend, which is what lets an arm no
    // seam outside the crate reaches be stated at all. The library target is
    // named by those entries and never claimed whole: it holds every unit test
    // the crate has.
    //
    // The rows carried in the lockdown suite meet the condition at the
    // production path. A child process is started armed at `norn-fs`'s watcher
    // seam and attaches a real host over a real backend, so what answers the arm
    // is the registration call, the event the backend delivered and the trust
    // state a client reads — and the seam's own record of the boundary that
    // fired is what says the case met the condition it names.
    Case {
        id: "trust-vault-wide-overflow-reconcile",
        suite: Suite::TrustTransition,
        lane: Lane::Any,
        states: "a vault-wide overflow publishes the overflow reason and schedules a full-tree \
                 reconcile while live coverage stays installed",
        carrier: "crates/norn-host/src/lifecycle.rs::\
                  a_polled_rescan_publishes_the_overflow_and_schedules_its_reconcile",
        feature: None,
    },
    Case {
        id: "trust-attach-time-backend-failure",
        suite: Suite::TrustTransition,
        lane: Lane::Any,
        states: "an attach that cannot install coverage acquires nothing and schedules no \
                 recovery: a lease held across it does not re-acquire coverage",
        carrier: "crates/norn-host/src/lifecycle.rs::\
                  a_lease_held_across_a_terminal_attach_does_not_re_acquire_coverage",
        feature: None,
    },
    Case {
        id: "trust-post-ready-backend-failure",
        suite: Suite::TrustTransition,
        lane: Lane::Any,
        states: "a backend failure after readiness withdraws trust publishing the cause it \
                 carried, and resumes only through a recovery demand",
        carrier: "crates/norn-host/src/lifecycle.rs::\
                  a_terminal_watcher_failure_publishes_the_cause_it_carried",
        feature: None,
    },
    Case {
        id: "trust-root-coverage-loss",
        suite: Suite::TrustTransition,
        lane: Lane::Any,
        states: "coverage lost at the vault root withdraws trust and reclassifies the root at \
                 every alias of it, rather than treating it as a change inside the vault",
        carrier: "crates/norn-host/src/lifecycle.rs::\
                  coverage_lost_at_the_root_reclassifies_every_alias_of_it",
        feature: None,
    },
    Case {
        id: "trust-terminal-subscription-wins",
        suite: Suite::TrustTransition,
        lane: Lane::Any,
        states: "a published watcher cause outlives the ticks that follow it: a later rescan never \
                 replaces the cause that ended coverage, and the subscription never revives",
        carrier: "crates/norn-host/src/lifecycle.rs::\
                  a_published_watcher_cause_outlives_the_ticks_that_follow_it",
        feature: None,
    },
    Case {
        id: "trust-production-watcher-install-refusal",
        suite: Suite::TrustTransition,
        lane: Lane::RealWatcher,
        states: "an attach whose registration a real backend refuses acquires nothing — no store \
                 opened, no changeset committed — and nothing re-acquires coverage while the \
                 demand that asked for it stands: a second demand is what serves the vault again, \
                 and that second child is the suite's unarmed watcher control, which installs a \
                 live subscription, reports a change through it, and records nothing",
        carrier: "crates/norn-host/tests/lockdown.rs::\
                  an_attach_that_cannot_install_coverage_acquires_nothing",
        feature: Some(INDUCED_FAILURE),
    },
    Case {
        id: "trust-production-watcher-stream-failure",
        suite: Suite::TrustTransition,
        lane: Lane::RealWatcher,
        states: "a real backend that stops reporting after readiness withdraws trust naming the \
                 backend as the cause, and the entry resumes through the recovery a demand asks \
                 for rather than on its own",
        carrier: "crates/norn-host/tests/lockdown.rs::\
                  a_backend_failure_after_readiness_resumes_only_through_a_recovery_demand",
        feature: Some(INDUCED_FAILURE),
    },
    Case {
        id: "trust-production-watcher-overflow",
        suite: Suite::TrustTransition,
        lane: Lane::RealWatcher,
        states: "a real backend that reports its path set lost publishes the overflow, rereads the \
                 whole vault through a reconcile rather than a recovery, and keeps reporting on \
                 the coverage the overflow was delivered over",
        carrier: "crates/norn-host/tests/lockdown.rs::\
                  a_vault_wide_overflow_reconciles_under_coverage_that_stays_installed",
        feature: Some(INDUCED_FAILURE),
    },
];

/// **The unreached table.** Trust-transition arms no test reaches at the
/// production path.
///
/// Two kinds of row, and the distinction is in each row's own words. An arm
/// nothing carries at all: no test drives the condition anywhere. And an arm
/// carried only against the fake entry operations a host is generic over, where
/// the transition is asserted and the condition that triggers it is handed over
/// by the fake rather than produced by a real watcher. The second kind is listed
/// beside the first because a fake-carried arm is a real obligation with a real
/// gap, and a table holding only the first kind would read as "everything else
/// is met by the production path".
///
/// This is the committed record the ownership ruling reads. Nothing here fails
/// the reconciliation: what is missing is a decision about who writes the case
/// and what it needs, not a broken reference. A row leaves by a test arriving
/// and an entry joining [`REQUIRED_CASES`], or by a ruling that withdraws the
/// obligation.
pub const UNREACHED_ARMS: &[UnreachedArm] = &[
    UnreachedArm {
        arm: "synchronization expiry makes that subscription terminal",
        awaits: "`lifecycle.rs`'s `watcher_lost` maps `WatchError::SynchronizationExpired` to \
                 `WatcherLossCause::SynchronizationExpired`, and no test drives the entry \
                 operations into returning that error, so nothing exercises the transition the \
                 mapping feeds. The expiry itself is carried at the `norn-fs` seam, where a \
                 terminal boundary is proven un-revivable, and at the wire seam, where the cause \
                 round-trips — but the trust transition it drives is asserted for `Backend` and \
                 `CoverageLost` only. The cheapest carrier is a third row in the fake-ops cause \
                 table. The door onto the production path is already open: the watcher fault \
                 seam's barrier stage, armed `barrier=expires`, withholds the `Live` publication \
                 and ends the wait at once, so a child spawned with that pair meets the expiry \
                 through a real backend without an authored deadline having to elapse. What the \
                 carrier still owes is the case itself — a lockdown child that arms the barrier, \
                 asserts the entry goes untrusted under `SynchronizationExpired`, and states what \
                 an attach that never proved its coverage may leave behind.",
    },
    UnreachedArm {
        arm: "a published watcher cause outliving production ticks rather than fake ones",
        awaits: "`a_published_watcher_cause_outlives_the_ticks_that_follow_it` publishes the cause \
                 by handing `FakeOps` a terminal poll error and then ticks the same fake over two \
                 kinds of tick: one that reports nothing, and one that reports a vault-wide \
                 rescan. The first kind is reached at the production path — \
                 `a_backend_failure_after_readiness_resumes_only_through_a_recovery_demand` drives \
                 a real backend terminal through the watcher seam's stream stage and then holds \
                 the entry still across the polls that follow. The second is not: a rescan \
                 delivered after coverage ended needs a second stream answer on one \
                 establishment, and the seam holds one answer per establishment by construction.",
    },
    UnreachedArm {
        arm: "root coverage loss met end to end through a production attachment",
        awaits: "the two halves are each carried and never joined: `norn-fs`'s watcher suite \
                 proves a removed vault root reports `CoverageLost` off a real backend, and \
                 `norn-host` proves that cause reclassifies the root — against fake operations. A \
                 case that removes a root under a live production attachment needs the real \
                 watcher lease and a settle bound over a condition the backend reports on its own \
                 schedule.",
    },
];

/// One value over the whole inventory.
///
/// Every field a reviewer weighs goes in, cases digested in id order, each field
/// absorbed behind its own length. The suite-manifest digest folds this in, so
/// an obligation retitled, re-laned, re-bound or deleted moves the value a
/// qualifying run is recorded under.
pub fn contract_digest() -> String {
    let mut cases: Vec<&Case> = REQUIRED_CASES.iter().collect();
    cases.sort_by_key(|case| case.id);

    let mut hasher = Sha256::new();
    hasher.update_framed(&(cases.len() as u64).to_be_bytes());
    for case in cases {
        hasher.update_framed(case.id.as_bytes());
        hasher.update_framed(case.suite.name().as_bytes());
        hasher.update_framed(case.lane.name().as_bytes());
        hasher.update_framed(case.states.as_bytes());
        hasher.update_framed(case.carrier.as_bytes());
        hasher.update_framed(case.feature.unwrap_or("").as_bytes());
    }
    hasher.update_framed(&(UNREACHED_ARMS.len() as u64).to_be_bytes());
    for arm in UNREACHED_ARMS {
        hasher.update_framed(arm.arm.as_bytes());
        hasher.update_framed(arm.awaits.as_bytes());
    }
    hex(&hasher.finish())
}

/// The case with this id, or nothing.
pub fn case(id: &str) -> Option<&'static Case> {
    REQUIRED_CASES.iter().find(|case| case.id == id)
}

/// **The reconciliation.** Everything wrong with the inventory as a record of
/// what the suites hold, one line each. A sound inventory produces none.
///
/// `workspace_root` is where a carrier reference resolves from, and cargo is
/// asked from there once per feature set the entries declare.
pub fn reconciliation_problems(workspace_root: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    audit_shape(&mut problems);

    let indices = index_by_feature(workspace_root);
    audit_carriers(&indices, &mut problems);
    audit_claimed_targets(&indices, &mut problems);
    problems
}

/// The ids, lanes and references, judged without asking cargo anything.
fn audit_shape(problems: &mut Vec<String>) {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut carriers: BTreeMap<&str, &str> = BTreeMap::new();
    for case in REQUIRED_CASES {
        if !seen.insert(case.id) {
            problems.push(format!("`{}` is named twice", case.id));
        }
        if !is_kebab_case(case.id) {
            problems.push(format!(
                "`{}` is not kebab-case: an id is lowercase words joined by single hyphens",
                case.id
            ));
        }
        if case.states.trim().is_empty() {
            problems.push(format!("`{}` states no obligation", case.id));
        }
        if let Some(first) = carriers.insert(case.carrier, case.id) {
            problems.push(format!(
                "`{}` and `{first}` both name the carrier `{}`. One test carrying two obligations \
                 makes the inventory count what the suites hold twice.",
                case.id, case.carrier
            ));
        }
        if let Err(problem) = TestRef::parse(case.carrier) {
            problems.push(format!("`{}` {problem}", case.id));
        }
    }
    if REQUIRED_CASES.is_empty() {
        problems.push(
            "the inventory is empty, so a run certified against it certified nothing".to_string(),
        );
    }
}

/// One index per feature set the inventory declares, keyed by that feature.
///
/// A target is asked about under the feature its entries name, because a suite
/// behind an off-by-default feature compiles to zero tests without it.
fn index_by_feature(workspace_root: &Path) -> BTreeMap<Option<&'static str>, TestIndex> {
    let mut wanted: BTreeMap<Option<&'static str>, BTreeSet<TargetRef>> = BTreeMap::new();
    for case in REQUIRED_CASES {
        if let Ok(target) = TestRef::parse(case.carrier).and_then(|test| test.target()) {
            wanted.entry(case.feature).or_default().insert(target);
        }
    }
    for claimed in CLAIMED_TARGETS {
        wanted
            .entry(claimed.feature)
            .or_default()
            .insert(claimed.target());
    }
    wanted
        .into_iter()
        .map(|(feature, targets)| {
            let features: Vec<&str> = feature.into_iter().collect();
            (
                feature,
                TestIndex::from_cargo_with_features(workspace_root, targets, &features),
            )
        })
        .collect()
}

/// Forward: every id resolves to exactly one test cargo compiled.
fn audit_carriers(indices: &BTreeMap<Option<&'static str>, TestIndex>, problems: &mut Vec<String>) {
    for case in REQUIRED_CASES {
        let Ok(test) = TestRef::parse(case.carrier) else {
            continue; // Already reported by the shape audit.
        };
        let target = match test.target() {
            Ok(target) => target,
            Err(problem) => {
                problems.push(format!("`{}` names `{test}`, whose {problem}", case.id));
                continue;
            }
        };
        let Some(index) = indices.get(&case.feature) else {
            problems.push(format!(
                "`{}` declares the feature {:?} and no listing was collected under it",
                case.id, case.feature
            ));
            continue;
        };
        match index.resolve(&target, &test.function) {
            Err(problem) => problems.push(format!(
                "`{}` names `{test}`, and {problem}. A case cargo compiled nothing for is a case \
                 no run executes, whatever the suite's own result line says.",
                case.id
            )),
            Ok(ignored) => {
                if ignored {
                    problems.push(format!(
                        "`{}` names `{test}`, which cargo compiled as ignored. A plain run of the \
                         suite skips it, so the case is one the inventory counts and no \
                         certification run executes. A certification carrier is never `#[ignore]`d: \
                         the lanes that adopt ignored cases wholesale are the counter, memory and \
                         soak measurement lanes, and none of them certifies this layer.",
                        case.id
                    ));
                }
                if let Ok(listing) = index.compiled(&target) {
                    let matched = listing
                        .all
                        .iter()
                        .filter(|listed| listed.rsplit("::").next() == Some(test.function.as_str()))
                        .count();
                    if matched > 1 {
                        problems.push(format!(
                            "`{}` names `{test}`, and cargo compiled {matched} tests with that \
                             function name into the target. A reference that resolves to more \
                             than one test names none of them.",
                            case.id
                        ));
                    }
                }
            }
        }
    }
}

/// Backward: every test in a claimed target is named by an entry.
///
/// The names are gathered per claimed target, from the entries whose carrier
/// sits in that target's own file. A bare function name matched workspace-wide
/// would let a test in one certification suite be covered by an entry naming a
/// same-named test in another — which is a name collision reading as coverage.
fn audit_claimed_targets(
    indices: &BTreeMap<Option<&'static str>, TestIndex>,
    problems: &mut Vec<String>,
) {
    for claimed in CLAIMED_TARGETS {
        let target = claimed.target();
        let file = claimed.file();
        let named: BTreeSet<&str> = REQUIRED_CASES
            .iter()
            .filter_map(|case| case.carrier.rsplit_once("::"))
            .filter(|(carried_in, _)| *carried_in == file)
            .map(|(_, function)| function)
            .collect();
        let Some(index) = indices.get(&claimed.feature) else {
            problems.push(format!(
                "`{} {}` is claimed under the feature {:?} and no listing was collected under it",
                claimed.package, claimed.stem, claimed.feature
            ));
            continue;
        };
        let listing = match index.compiled(&target) {
            Ok(listing) => listing,
            Err(problem) => {
                problems.push(format!(
                    "`{} {}` is claimed whole, and {problem}",
                    claimed.package, claimed.stem
                ));
                continue;
            }
        };
        if listing.all.is_empty() {
            problems.push(format!(
                "`{} {}` is claimed whole and cargo compiled no test into it. A claimed target \
                 that compiles to nothing is a suite the layer counts and no run executes.",
                claimed.package, claimed.stem
            ));
        }
        for listed in &listing.all {
            let function = listed.rsplit("::").next().unwrap_or(listed);
            if !named.contains(function) {
                problems.push(format!(
                    "`{} {}` compiled `{listed}`, which no inventory entry names. Every test in a \
                     claimed certification suite is a certification case: add the entry in the \
                     same diff, or move the test out of a claimed target.",
                    claimed.package, claimed.stem
                ));
            }
        }
    }
}

impl ClaimedTarget {
    /// The workspace-relative file this target compiles from, spelled the way a
    /// carrier reference spells it.
    fn file(&self) -> String {
        format!("crates/{}/tests/{}.rs", self.package, self.stem)
    }

    fn target(&self) -> TargetRef {
        TargetRef {
            package: self.package.to_string(),
            target: crate::regression::Target::Integration(self.stem.to_string()),
            module_prefix: String::new(),
        }
    }
}

fn is_kebab_case(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .chars()
            .all(|c| c == '-' || c.is_ascii_lowercase() || c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::{
        CLAIMED_TARGETS, Lane, REQUIRED_CASES, Suite, UNREACHED_ARMS, audit_shape, case,
        contract_digest, is_kebab_case,
    };
    use std::collections::BTreeSet;

    /// The shape audit finds nothing wrong with the inventory as authored. What
    /// the suites actually compiled is the reconciliation suite's question, and
    /// it asks cargo; this asks only that the record holds together.
    #[test]
    fn the_inventory_is_internally_sound() {
        let mut problems = Vec::new();
        audit_shape(&mut problems);
        assert_eq!(problems, Vec::<String>::new());
    }

    /// Every claimed target is one some entry's carrier actually sits in. A
    /// target claimed whole with no entry pointing into it would require the
    /// suite to be empty and fail the reverse check for every test in it.
    #[test]
    fn every_claimed_target_holds_at_least_one_named_case() {
        for claimed in CLAIMED_TARGETS {
            let prefix = format!("crates/{}/tests/{}.rs::", claimed.package, claimed.stem);
            assert!(
                REQUIRED_CASES
                    .iter()
                    .any(|case| case.carrier.starts_with(&prefix)),
                "`{} {}` is claimed whole and no entry names a carrier in it",
                claimed.package,
                claimed.stem
            );
        }
    }

    /// A carrier in a claimed target declares the feature that target is
    /// claimed under. The two are asked of cargo separately, so a disagreement
    /// would ask about one target twice under two feature sets and reconcile
    /// each against half the answer.
    #[test]
    fn a_carrier_in_a_claimed_target_declares_that_targets_feature() {
        for claimed in CLAIMED_TARGETS {
            let prefix = format!("crates/{}/tests/{}.rs::", claimed.package, claimed.stem);
            for case in REQUIRED_CASES
                .iter()
                .filter(|c| c.carrier.starts_with(&prefix))
            {
                assert_eq!(
                    case.feature, claimed.feature,
                    "`{}` sits in a target claimed under {:?} and declares {:?}",
                    case.id, claimed.feature, case.feature
                );
            }
        }
    }

    /// Each suite carries cases, so a suite named in the vocabulary and carried
    /// by nothing is caught rather than read as certified.
    #[test]
    fn every_suite_in_the_vocabulary_carries_cases() {
        let carried: BTreeSet<Suite> = REQUIRED_CASES.iter().map(|case| case.suite).collect();
        assert_eq!(
            carried,
            BTreeSet::from([
                Suite::Churn,
                Suite::InducedFailure,
                Suite::Operational,
                Suite::TrustTransition,
            ])
        );
    }

    /// The two lanes a single machine cannot certify are carried by real cases.
    /// They are the whole reason a certification campaign is more than one run,
    /// so a build that lost both would quietly turn the campaign into one.
    #[test]
    fn the_platform_deciding_lanes_are_carried() {
        for lane in [
            Lane::RealWatcherVolumeFoldingDecides,
            Lane::RealWatcherBackendDecides,
        ] {
            assert!(
                REQUIRED_CASES.iter().any(|case| case.lane == lane),
                "no case is in the `{}` lane",
                lane.name()
            );
        }
    }

    #[test]
    fn an_id_is_kebab_case() {
        assert!(is_kebab_case("churn-case-flip"));
        assert!(!is_kebab_case("Churn-Case-Flip"));
        assert!(!is_kebab_case("churn--flip"));
        assert!(!is_kebab_case("-churn"));
        assert!(!is_kebab_case(""));
    }

    #[test]
    fn a_case_is_found_by_its_id() {
        assert!(case("churn-case-flip").is_some());
        assert!(case("no-such-case").is_none());
    }

    /// The digest is a function of the inventory alone, so two reads of one
    /// build agree — which is what lets a ledger record it and a later reader
    /// compare.
    #[test]
    fn the_digest_is_stable_across_two_reads() {
        assert_eq!(contract_digest(), contract_digest());
        assert_eq!(contract_digest().len(), 64);
    }

    /// The unreached table is not empty and every row says what it awaits. An
    /// empty table would read as "every arm is carried", which is the claim the
    /// sweep found to be false.
    #[test]
    fn every_unreached_arm_says_what_it_awaits() {
        assert!(!UNREACHED_ARMS.is_empty());
        for arm in UNREACHED_ARMS {
            assert!(!arm.arm.trim().is_empty());
            assert!(
                arm.awaits.trim().len() > 40,
                "`{}` awaits `{}`, which does not say enough for a ruling to read it",
                arm.arm,
                arm.awaits
            );
        }
    }
}
