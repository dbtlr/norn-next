//! What one request the host served cost, attributed to that request alone.
//!
//! **A request is measured on the thread that runs it.** A read runs on its
//! caller's thread, so what it costs is what that thread did while it ran:
//! the reads `norn-fs` counted in a `ReadWindow` opened around it, the store
//! statements and VM steps its snapshot ran, and the registrations and entries
//! it looked at in the serving set. Each is kept in this thread's storage, so
//! concurrent requests on other threads never mix into it.
//!
//! **An apply is measured on the worker that runs its job**, because that is
//! where its planning and publication run; the worker records the job's
//! tally on the apply's progress record before the answer is sent, and
//! [`PendingApply::wait`](crate::PendingApply::wait) adds it to the waiting
//! thread's tally, so a write verb is measured like a read.
//!
//! **The tallies are written on every build and read only under the
//! `evidence` feature**, as the job account is: a thread-local add is the
//! whole cost, and nothing in a shipped build reads it back.

use std::cell::Cell;
#[cfg(feature = "evidence")]
use std::sync::atomic::{AtomicU64, Ordering};

use norn_fs::reads::ReadTally;
#[cfg(feature = "evidence")]
use norn_fs::reads::ReadWindow;
use norn_store::SnapshotCounters;

/// What one request cost.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RequestTally {
    /// Store statements its snapshots ran.
    pub statements: u64,
    /// SQLite VM steps those statements took.
    pub vm_steps: u64,
    /// Counted `norn-fs` acts: content reads, stats, directory entries
    /// walked or read by the write protocol, name queries, and the write
    /// protocol's target and shadow reads.
    pub fs_reads: u64,
    /// Registrations the request looked at in the serving set.
    pub registrations_visited: u64,
    /// Entries the request looked up or walked in the serving set.
    pub entries_visited: u64,
}

impl RequestTally {
    /// This tally and `other` together.
    #[must_use]
    pub fn plus(self, other: RequestTally) -> RequestTally {
        RequestTally {
            statements: self.statements + other.statements,
            vm_steps: self.vm_steps + other.vm_steps,
            fs_reads: self.fs_reads + other.fs_reads,
            registrations_visited: self.registrations_visited + other.registrations_visited,
            entries_visited: self.entries_visited + other.entries_visited,
        }
    }

    /// What `counters`, one snapshot's reading, ran.
    pub(crate) fn of_snapshot(counters: SnapshotCounters) -> RequestTally {
        RequestTally {
            statements: counters.statements_executed(),
            vm_steps: counters.vm_steps(),
            ..RequestTally::default()
        }
    }

    /// The counted acts a read window reported.
    pub(crate) fn of_reads(reads: &ReadTally) -> RequestTally {
        RequestTally {
            fs_reads: reads.document_opens
                + reads.stats
                + reads.walk_dirents
                + reads.write_dirents
                + reads.name_queries
                + reads.target_reads
                + reads.shadow_reads,
            ..RequestTally::default()
        }
    }
}

thread_local! {
    /// What the request running on this thread has cost so far. A thread no
    /// window stands on adds here too, and nothing reads it: each window
    /// empties it as it opens.
    static TALLY: Cell<RequestTally> = const { Cell::new(RequestTally {
        statements: 0,
        vm_steps: 0,
        fs_reads: 0,
        registrations_visited: 0,
        entries_visited: 0,
    }) };
}

/// Add `tally` to the request running on this thread.
pub(crate) fn add(tally: RequestTally) {
    TALLY.set(TALLY.get().plus(tally));
}

/// Count `count` registrations looked at by the request on this thread.
pub(crate) fn visit_registrations(count: usize) {
    add(RequestTally {
        registrations_visited: count as u64,
        ..RequestTally::default()
    });
}

/// Count `count` serving-set entries looked at by the request on this thread.
pub(crate) fn visit_entries(count: usize) {
    add(RequestTally {
        entries_visited: count as u64,
        ..RequestTally::default()
    });
}

/// The host's running totals over every request measured through
/// [`Host::tallied`](crate::Host::tallied).
#[cfg(feature = "evidence")]
#[derive(Debug, Default)]
pub(crate) struct RequestEvidence {
    requests: AtomicU64,
    statements: AtomicU64,
    vm_steps: AtomicU64,
    fs_reads: AtomicU64,
    registrations_visited: AtomicU64,
    entries_visited: AtomicU64,
}

/// The host's request totals at one instant.
#[cfg(feature = "evidence")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RequestReading {
    /// Requests measured.
    pub requests: u64,
    /// Their tallies, summed.
    pub tally: RequestTally,
}

#[cfg(feature = "evidence")]
impl RequestEvidence {
    fn absorb(&self, tally: RequestTally) {
        self.requests.fetch_add(1, Ordering::Relaxed);
        for (field, value) in [
            (&self.statements, tally.statements),
            (&self.vm_steps, tally.vm_steps),
            (&self.fs_reads, tally.fs_reads),
            (&self.registrations_visited, tally.registrations_visited),
            (&self.entries_visited, tally.entries_visited),
        ] {
            field.fetch_add(value, Ordering::Relaxed);
        }
    }

    pub(crate) fn read(&self) -> RequestReading {
        let get = |field: &AtomicU64| field.load(Ordering::Relaxed);
        RequestReading {
            requests: get(&self.requests),
            tally: RequestTally {
                statements: get(&self.statements),
                vm_steps: get(&self.vm_steps),
                fs_reads: get(&self.fs_reads),
                registrations_visited: get(&self.registrations_visited),
                entries_visited: get(&self.entries_visited),
            },
        }
    }

    /// Run one request on this thread under a window of its own, and add what
    /// it cost to the totals.
    ///
    /// **Not reentrant**, as a read window is not: a request runs one host
    /// call, and the window refuses a second standing on the same thread.
    pub(crate) fn measure<T>(&self, run: impl FnOnce() -> T) -> (T, RequestTally) {
        TALLY.set(RequestTally::default());
        let window = ReadWindow::open();
        let answer = run();
        let reads = window.finish();
        let tally = TALLY
            .replace(RequestTally::default())
            .plus(RequestTally::of_reads(&reads));
        self.absorb(tally);
        (answer, tally)
    }
}

#[cfg(all(test, feature = "evidence"))]
mod tests {
    use super::*;
    use crate::ApplyProgress;

    /// A request's tally is what its own thread added while it ran: what this
    /// thread added before the window opened is no request's, and what
    /// another thread adds at the same time is never this one's.
    #[test]
    fn a_tally_is_what_its_own_thread_added_while_it_ran() {
        let evidence = RequestEvidence::default();
        visit_registrations(100);
        let ((), tally) = evidence.measure(|| {
            visit_registrations(2);
            visit_entries(1);
            std::thread::spawn(|| visit_entries(50)).join().unwrap();
        });
        assert_eq!(
            tally,
            RequestTally {
                registrations_visited: 2,
                entries_visited: 1,
                ..RequestTally::default()
            }
        );
        let reading = evidence.read();
        assert_eq!(reading.requests, 1);
        assert_eq!(reading.tally, tally);
    }

    /// An apply's job records its cost on the worker; the caller's request
    /// counts it once the apply is answered, and only once.
    #[test]
    fn an_apply_tally_travels_back_on_its_progress_once() {
        let evidence = RequestEvidence::default();
        let progress = ApplyProgress::default();
        let worker = progress.clone();
        std::thread::spawn(move || {
            worker.tally(RequestTally {
                statements: 7,
                vm_steps: 70,
                fs_reads: 3,
                ..RequestTally::default()
            });
        })
        .join()
        .unwrap();
        let ((), tally) = evidence.measure(|| {
            progress.hand_tally_to_this_thread();
            progress.hand_tally_to_this_thread();
        });
        assert_eq!(
            (tally.statements, tally.vm_steps, tally.fs_reads),
            (7, 70, 3)
        );
    }

    /// The totals sum every measured request.
    #[test]
    fn the_totals_sum_every_measured_request() {
        let evidence = RequestEvidence::default();
        for count in [1, 2, 3] {
            evidence.measure(|| visit_entries(count));
        }
        let reading = evidence.read();
        assert_eq!(reading.requests, 3);
        assert_eq!(reading.tally.entries_visited, 6);
    }

    /// **A refused read still counts what it ran.** A `get` of a target that
    /// does not exist, and a `count` continuing a cursor of another grouping,
    /// each run store statements before they refuse, and the request's tally
    /// holds them: the refusal path adds the snapshot's counters as the
    /// answer path does.
    #[test]
    #[allow(clippy::disallowed_methods)] // This test owns its vault fixture.
    fn a_refused_read_tallies_the_statements_it_ran() {
        use std::time::Duration;

        use norn_testkit::isolation;
        use norn_testkit::scratch::Scratch;
        use norn_testkit::wait::{Budget, Observed, wait_until};
        use norn_wire::{
            CountParams, GetParams, GroupKey, ResolutionTarget, TrustState, VaultAddress, VaultName,
        };

        use crate::{
            Host, LifecyclePolicy, ProductionEntryOps, ProductionPolicy, Registration, RegistryRead,
        };

        const BUDGET: Budget = Budget::new(Duration::from_secs(15), Duration::from_millis(100));
        let scratch = Scratch::new("refused-read-tally");
        let _watcher = isolation::Lease::hold(
            isolation::REAL_WATCHER,
            isolation::acquisition_budget(BUDGET),
        );
        let root = scratch.join("vault");
        std::fs::create_dir_all(root.join(".norn")).unwrap();
        std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
        std::fs::write(root.join("a.md"), "---\nstatus: draft\n---\n# A\n").unwrap();
        std::fs::write(root.join("b.md"), "---\nstatus: done\n---\n# B\n").unwrap();
        norn_fs::watch::settle_prior_changes(scratch.root(), BUDGET.work()).unwrap();
        let name = VaultName::new("notes").unwrap();
        let host = Host::new(
            RegistryRead::from_entries([Registration::new(
                name.clone(),
                norn_config::registry::VaultRoot::new(&root).unwrap(),
            )]),
            ProductionEntryOps::new(
                norn_config::ConfigDirs::new(scratch.join("config"), scratch.join("data")).unwrap(),
                ProductionPolicy::new(2, 2).unwrap(),
            ),
            LifecyclePolicy {
                idle_after: Duration::from_secs(3600),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(25),
                read_settle_bound: Duration::from_secs(5),
                background_threads: crate::BACKGROUND_THREADS,
                background_turn_budget: crate::BACKGROUND_TURN_BUDGET,
            },
        )
        .unwrap();
        let _lease = host.demand(&name).unwrap();
        wait_until("ready", BUDGET, || {
            if host.state(&name) == Ok(TrustState::Ready) {
                Observed::Met(())
            } else {
                Observed::pending("still warming")
            }
        })
        .unwrap();
        let missing = ResolutionTarget::new("missing").unwrap();
        let address = VaultAddress::name(name);
        let get = GetParams::new(address.clone(), missing.clone());
        let (answer, tally) = host.tallied(|| host.get(&get));
        assert!(answer.is_err(), "a get of a missing target answered");
        assert!(tally.statements > 0, "the refused get counted {tally:?}");
        // A cursor of a grouping by one key names no position among the
        // tallies of a grouping by two.
        let by_status = CountParams::new(address).with_by([GroupKey::field("status")]);
        let first = host.count(&by_status.clone().with_limit(1)).unwrap().answer;
        let cursor = first.report.next.clone().expect("a second group");
        let count = by_status
            .with_by([GroupKey::field("status"), GroupKey::tag()])
            .with_after(cursor);
        let (answer, tally) = host.tallied(|| host.count(&count));
        assert!(
            answer.is_err(),
            "a count past another grouping's cursor answered"
        );
        assert!(tally.statements > 0, "the refused count counted {tally:?}");
    }
}
