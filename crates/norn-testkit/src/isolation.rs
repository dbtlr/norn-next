//! Isolating the machine-wide things a suite's cases cannot share.
//!
//! A test binary runs its cases on several threads and the runner runs several
//! binaries at once, so every case shares the machine with siblings it never
//! names. Most subjects tolerate that: two temporary trees do not collide, and
//! two stores are two files. Some do not, and this module is where those are
//! held apart.
//!
//! # A lease is the seam, and the watcher is its first holder
//!
//! [`REAL_WATCHER`] names the one machine-wide service a case can starve. A
//! platform filesystem watcher is a subscription to a service the operating
//! system runs once for the whole machine, and that service degrades by going
//! silent: past some number of live subscriptions it reports **nothing at all**
//! to some of them rather than reporting late. A case whose watcher is one of
//! the silent ones waits out its whole budget and then reports the paths it
//! never saw, which reads as a fidelity defect in the watcher and is a
//! statement about how many sibling processes were running.
//!
//! So a case that owns a real platform watcher holds [`REAL_WATCHER`] for the
//! window that watcher is live, and the number of live watchers this
//! workspace's suites have on the machine is one — however many binaries the
//! runner started, and however many children of their own those binaries
//! re-execute, because the lease is taken where the watcher is installed
//! rather than where the run began.
//!
//! # Exclusion joins here; partitioning is the other door
//!
//! A second isolation concern joins here rather than bringing a second
//! mechanism: [`Lease::hold`] is keyed, and a key is what a new concern
//! declares.
//!
//! **What joins here is exclusion**: concerns whose subject is one machine-wide
//! thing that has to be used by one holder at a time. The other family is
//! **partitioning** — a run wanting state of its own rather than a turn at
//! shared state — and that is [`crate::process::Sandbox`]'s: a private
//! directory tree per run, with `HOME`, `TMPDIR` and the XDG variables pointed
//! inside it. A concern that would be satisfied by a directory nobody else
//! writes belongs there and not here; a concern that is still one thing after
//! every run has its own directory belongs here.
//!
//! The two doors meet in one place, and it is deliberate: a sandbox forwards
//! this module's resolved [`root`] to the child it spawns, so a run that is
//! partitioned for everything else is still queued against the same leases as
//! its parent. Partitioning the lease root instead would give a child a lease
//! nobody else contends for, which reads as isolation and excludes nothing.
//!
//! **Scratch-tree isolation joined at the partition door.** A generated tree
//! is a directory nobody else writes, not a turn at shared state. In
//! `norn-fixtures`, every scratch tree the calibration, determinism, knobs
//! and preconditions suites generate into is built on
//! [`crate::process::Sandbox`] rather than declaring a key here.
//!
//! # A lease is not reentrant
//!
//! [`Lease`] is a lock on an open file and it excludes per open file
//! description, which is what makes two threads of one binary exclude each
//! other — and it makes a second hold in the *same* test exclude it too. A case
//! that takes [`REAL_WATCHER`] while already holding it waits out its whole
//! acquisition bound against itself and then names its own process as the
//! holder. So each watcher window has exactly one holder: a helper that takes
//! the lease is the only thing that takes it, and its callers hold what it
//! hands back rather than taking one of their own.
//!
//! # The lock is a file lock, and that is what makes it cross-process
//!
//! [`Lease`] is a lock on an open file. Two properties are why:
//!
//! - **It is released when the holder dies.** A test that panics, and a
//!   process killed mid-run, both release. A lease built from a file's
//!   *existence* would leave a killed run's lease standing forever, and the
//!   next run would wait out its budget against a holder that is gone.
//! - **It excludes per open file description, not per process.** Two threads
//!   in one binary each open the lease file and exclude each other, which is
//!   the case the runner's own threads produce. A POSIX record lock is
//!   per-process and would let all four of a binary's test threads hold the
//!   same lease at once — the exclusion nobody would notice was missing.
//!
//! # Acquisition is a budgeted wait, and what fires is per holder
//!
//! Taking a lease is [`crate::wait`]'s wait like any other: a bound, and a
//! failure naming the holder it was waiting on. Queueing behind every other
//! holder is what the lease is *for*, so the thing worth diagnosing is not a
//! long queue but a holder that is stuck rather than working — and those two
//! are told apart by whether the queue is moving.
//!
//! So the acquisition carries two bounds over two different things, the same
//! way a [`Budget`] does:
//!
//! - [`HOLDER_PATIENCE`] is per holder, and it is what fires. Every look that
//!   finds the lease in different hands than the last one re-arms it, so a
//!   queue that is changing hands is never interrupted however deep it is.
//! - [`acquisition_budget`] is the wall on the whole acquisition, because a
//!   wait that runs on while it observes progress is a wait with no deadline
//!   at all — the shape [`crate::wait`] refuses to offer.
//!
//! The wall is written against the walls the lanes themselves run under, which
//! is the constraint a depth-derived bound cannot meet: [`QUEUED_HOLDERS`]
//! holders at a fifteen-second hold window is forty-eight minutes, and a bound
//! past the job's own wall never fires. Under it a stuck holder ends the run as
//! a job timeout that names nothing, which is precisely the diagnostic this
//! module exists to produce.

use std::fs::TryLockError;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::wait::{Budget, FailureKind, Observed, WaitFailure, wait_until};

/// The lease a case holds while it owns a real platform watcher.
///
/// Every such case across the workspace names this one key, because the
/// service they contend for is one service. A suite that watches a real
/// filesystem and does not hold it is a suite that starves the others.
pub const REAL_WATCHER: &str = "real-watcher";

/// The real-watcher holders that can be queued ahead of one case.
///
/// **A census, written as its two factors.** Cases across the workspace hold
/// [`REAL_WATCHER`] by the hundred, and which ones is a rule rather than a
/// list: a case takes it when it attaches a vault through the host's production
/// ops or establishes a native filesystem subscription, because both install a
/// real platform watcher. The host's in-crate production suite holds most of
/// them. Beside it, a host integration target that serves through the attach
/// module those targets share takes one per attachment there; a target that
/// composes its own host takes its own, which the crash-recovery,
/// descriptor-budget and lockdown targets each do; and the filesystem crate
/// holds it in both its in-crate watch suite and its watcher target. The runner
/// starts up to four binaries of them at once, so the depth is written
/// `160 * 4`. Both factors are here rather than the product alone, because a
/// suite that gains cases moves one of them and a runner configured differently
/// moves the other.
///
/// **The first factor is a round figure for that census rather than a headcount
/// of it**, and it can be: the number is a queue depth and not a measurement —
/// queueing behind holders that are working is what the lease is for — and the
/// product it forms with the hold windows the cases here take is capped by
/// [`ACQUISITION_WALL`] well before the last holder in it, so a case arriving or
/// leaving does not move the bound. It stays a figure for the census all the
/// same: at a hold window short enough for the depth to come in under the
/// ceiling the depth is what binds, and a factor under the population derives a
/// wall a working queue outruns. It sizes the wall in [`acquisition_budget`],
/// and what actually diagnoses a stuck holder is [`HOLDER_PATIENCE`].
pub const QUEUED_HOLDERS: u32 = 160 * 4;

/// How long the lease may stay in one holder's hands before a waiter names it.
///
/// **This is the bound that fires, and it is per holder rather than per
/// acquisition.** A look that finds a different holder than the last one
/// re-arms it, so a queue that is changing hands runs on under the wall alone;
/// a record that has not moved for this long is a holder that stopped working
/// rather than one working slowly.
///
/// It is sized for the longest window a case honestly holds the lease: a whole
/// case rather than a single wait, since a case attaches a vault, runs a
/// sequence of waits over it, and detaches. Five minutes is far past any case
/// in the workspace suite and far inside [`PER_PR_JOB_WALL`].
///
/// One holder is outside that sizing by design — the soak lane's child holds
/// the lease for its whole hour-long load — and it is not a false diagnosis
/// waiting to happen, because that lane runs its one binary alone: nothing is
/// queued behind it to do the diagnosing.
pub const HOLDER_PATIENCE: Duration = Duration::from_secs(300);

/// The per-holder bound one acquisition runs under.
///
/// **Outside this module's own cases there is one of these and it is
/// [`HOLDER_PATIENCE`].** The other shape does not compile into a build that is
/// not running this module's tests, so no call site anywhere can widen the
/// bound past what [`ACQUISITION_WALL`] is sized against, or narrow it under
/// the window a case honestly holds the lease for. Moving the bound the
/// workspace runs under means moving the constant, which is where the relation
/// between the two is asserted.
#[derive(Debug, Clone, Copy)]
enum Patience {
    /// [`HOLDER_PATIENCE`], which is what every acquisition outside this
    /// module's tests runs under.
    Standing,
    /// A bound short enough to spend inside a case's own budget, so that the
    /// behavior five minutes describes can be driven in milliseconds.
    #[cfg(test)]
    Of(Duration),
}

impl Patience {
    fn bound(self) -> Duration {
        match self {
            Self::Standing => HOLDER_PATIENCE,
            #[cfg(test)]
            Self::Of(bound) => bound,
        }
    }
}

/// The wall the per-PR job runs under, which every bound here has to fire
/// inside of.
///
/// A bound past this is a bound that never fires: the job ends first, and it
/// ends naming a timeout rather than a holder.
pub const PER_PR_JOB_WALL: Duration = Duration::from_secs(20 * 60);

/// The ceiling on one acquisition, whatever the queue depth derives.
///
/// Three quarters of [`PER_PR_JOB_WALL`], so the wait fails inside the job and
/// leaves the rest of it for the case to report the failure and the runner to
/// collect it. The scheduled soak lane's wall is six times the per-PR one, so a
/// ceiling that fits inside the shorter fits inside both.
pub const ACQUISITION_WALL: Duration = Duration::from_secs(15 * 60);

/// The bound on taking a lease, given the window one holder holds it for.
///
/// The work bound is the shorter of two real constraints: what a fully queued
/// acquisition can honestly take — [`QUEUED_HOLDERS`] hold windows — and
/// [`ACQUISITION_WALL`]. At a fifteen-second hold window the first of those is
/// hours and the ceiling is what binds; at a hold window short enough that the
/// depth comes in under the ceiling, the depth binds instead.
///
/// The probe bound carries over unchanged: one probe is one non-blocking
/// attempt at the lock, which costs a syscall whatever the queue is doing.
pub fn acquisition_budget(hold_window: Budget) -> Budget {
    Budget::new(
        hold_window
            .work()
            .saturating_mul(QUEUED_HOLDERS)
            .min(ACQUISITION_WALL),
        hold_window.probe(),
    )
}

/// The environment variable naming where the leases live.
///
/// A runner that needs them somewhere other than the system temporary
/// directory — a filesystem that supports locking, or a directory of its own so
/// that two independent runs on one machine do not queue against each other —
/// sets this.
///
/// **A lease excludes only across the runs that resolve it to the same
/// directory.** The subject [`REAL_WATCHER`] covers is one service per machine,
/// so two concurrent runs pointed at different roots take two lease files, queue
/// behind nobody, and install the watchers each is meant to be excluding — an
/// opt-out that reports nothing and shows up as the silent-watcher failures this
/// module exists to prevent. Giving a run a root of its own is therefore a
/// statement that it contends with nothing else on the machine, and a run
/// holding this key contends with every other one.
pub const ISOLATION_ROOT: &str = "NORN_TEST_ISOLATION_DIR";

/// The directory the lease files live in.
///
/// [`ISOLATION_ROOT`] when the environment names one, and a
/// `norn-test-isolation` directory under the system temporary directory
/// otherwise. It is derived either way: no absolute path is written here, so
/// the leases follow the machine the suite is running on.
pub fn root() -> PathBuf {
    std::env::var_os(ISOLATION_ROOT)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("norn-test-isolation"))
}

/// Where the lease named `key` lives under `root`.
///
/// The key is the file's stem, so a key is a plain word: it names a file, and
/// two keys that differ only by a path separator would otherwise name one.
fn lease_path(root: &Path, key: &str) -> PathBuf {
    assert!(
        !key.is_empty()
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "a lease key is a word of ASCII letters, digits, dashes and underscores: {key:?}"
    );
    root.join(format!("{key}.lease"))
}

/// A held lease, released when it is dropped.
///
/// The lease is the value: holding it in a `let _ = ...` binding drops it at
/// once and holds nothing, so the binding names it. The window it covers is
/// the window the value is alive for.
#[derive(Debug)]
pub struct Lease {
    key: String,
    // The lock lives on this open file, so the field is the lease: closing the
    // file releases the lock, which is why nothing else may hold this handle.
    #[allow(clippy::disallowed_types)] // This crate's own lease handle; see `try_hold`.
    file: std::fs::File,
}

impl Lease {
    /// Hold the lease named `key`, waiting under `budget` for whoever has it.
    ///
    /// A failure to acquire panics with the wait's own diagnostic, because a
    /// lease is harness scaffolding rather than a subject: the case that could
    /// not take it has asserted nothing yet, and there is nothing for it to
    /// report but the holder it waited on. [`Lease::try_hold`] is the same
    /// acquisition with the failure handed back.
    pub fn hold(key: &str, budget: Budget) -> Self {
        Self::try_hold(key, budget).unwrap_or_else(|failure| panic!("{failure}"))
    }

    /// Hold the lease named `key`, handing back the wait that failed.
    ///
    /// The probe is one non-blocking attempt at the lock, so it costs a
    /// syscall whatever the machine is doing, and a pending observation names
    /// the record the holder wrote about itself.
    ///
    /// **Two bounds end this wait, and the one that ends it says which.**
    /// `budget`'s work bound is the wall on the whole acquisition; a failure
    /// that names it is a queue that kept moving for that long.
    /// [`HOLDER_PATIENCE`] is the per-holder bound, re-armed every time the
    /// record names different hands than the last look, and a failure that
    /// names it is one holder that stopped letting go. Both come back as the
    /// same [`WaitFailure`] shape, carrying the bound that was passed and the
    /// holder that was there when it went.
    pub fn try_hold(key: &str, budget: Budget) -> Result<Self, WaitFailure> {
        Self::try_hold_under(key, budget, Patience::Standing)
    }

    /// The acquisition, with the per-holder bound named rather than assumed.
    ///
    /// The bound is a parameter because it is the thing worth pinning and five
    /// minutes is longer than a case may run. [`Patience`] is what keeps that
    /// from being a way to run production under some other bound: the only
    /// bound nameable outside this module's tests is the constant. Nothing else
    /// about the acquisition moves with it — the failure a spent patience
    /// raises carries whatever bound spent it, which is what makes the two
    /// bounds tell themselves apart in the diagnostic.
    #[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The lease is this crate's own machine-local state, and a file lock is what makes it cross-process.
    fn try_hold_under(key: &str, budget: Budget, patience: Patience) -> Result<Self, WaitFailure> {
        let patience = patience.bound();
        let root = root();
        let path = lease_path(&root, key);
        std::fs::create_dir_all(&root)
            .unwrap_or_else(|error| panic!("the lease directory {}: {error}", root.display()));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .unwrap_or_else(|error| panic!("the lease file {}: {error}", path.display()));

        let what = format!("the {key} lease");
        let started = Instant::now();
        let mut probes = 0usize;
        // The holder the last look found, and when this wait first saw those
        // hands. Both move together, and only when the record changes.
        let mut queued: Option<(String, Instant)> = None;

        wait_until(&what, budget, || {
            probes += 1;
            match file.try_lock() {
                Ok(()) => Observed::Met(Ok(())),
                Err(TryLockError::WouldBlock) => {
                    let record = holder(&path);
                    let since = match &queued {
                        Some((named, since)) if *named == record => *since,
                        _ => {
                            let now = Instant::now();
                            queued = Some((record.clone(), now));
                            now
                        }
                    };
                    let unmoved = since.elapsed();
                    if unmoved >= patience {
                        Observed::Met(Err(WaitFailure {
                            what: what.clone(),
                            kind: FailureKind::Elapsed,
                            budget: Budget::new(patience, budget.probe()),
                            elapsed: started.elapsed(),
                            probes,
                            last_state: format!(
                                "held by {record}, which has neither let go nor changed hands in \
                                 {unmoved:?}"
                            ),
                        }))
                    } else {
                        Observed::pending(format!("held by {record} for {unmoved:?}"))
                    }
                }
                Err(TryLockError::Error(error)) => {
                    panic!("locking the lease file {}: {error}", path.display())
                }
            }
        })??;

        record_holder(&file, key, write_record);
        Ok(Lease {
            key: key.to_owned(),
            file,
        })
    }

    /// The key this lease was taken under.
    pub fn key(&self) -> &str {
        &self.key
    }
}

impl Drop for Lease {
    #[allow(clippy::disallowed_types)] // This crate's own lease handle; see `try_hold`.
    fn drop(&mut self) {
        // The record goes before the lock does. A record left standing after
        // its writer let go names a process that no longer holds anything, and
        // the next waiter to read it would name that dead pid as the holder it
        // is queued behind.
        clear_record(&self.file);
        // Closing the file would release the lock on its own; unlocking first
        // says so where a reader is looking for the release.
        let _ = self.file.unlock();
    }
}

/// Write who holds the lease, for whoever is waiting on it, putting the line
/// down through `write`.
///
/// The record is diagnostic only: a waiter reads it while the holder writes
/// it, so a torn read is possible and costs a confusing line in a failure that
/// was going to name the wait's own bound anyway. What it buys is a timeout
/// that names a process a reader can go look at.
///
/// **A write that fails clears the record instead of leaving the last one.**
/// The failure a swallowed error could otherwise cause is not a missing record
/// but a wrong one — the previous holder's line, read by a waiter as the name
/// of the process it is queued behind — and an empty record reads as the
/// holder that recorded nothing about itself, which is true.
///
/// That arm is why the write is a parameter: it is the arm worth pinning, and a
/// filesystem does not fail on request. The acquisition passes
/// [`write_record`]; the case over the failure arm passes a write that returns
/// an error. What follows the error is here, so it is the same clearing either
/// caller gets.
#[allow(clippy::disallowed_types)] // This crate's own lease handle; see `try_hold`.
fn record_holder(
    file: &std::fs::File,
    key: &str,
    write: impl FnOnce(&std::fs::File, &str) -> std::io::Result<()>,
) {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let record = format!(
        "pid {} holding {key} since unix {since}",
        std::process::id()
    );
    if write(file, &record).is_err() {
        clear_record(file);
    }
}

/// Replace the file's contents with `record`.
#[allow(clippy::disallowed_types)] // This crate's own lease handle; see `try_hold`.
fn write_record(mut file: &std::fs::File, record: &str) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    file.write_all(record.as_bytes())?;
    file.set_len(record.len() as u64)
}

/// Leave the lease file saying nothing about who holds it.
///
/// The truncation is what a released lease leaves behind, and it is also the
/// fallback for a record that could not be written: in both cases the honest
/// answer is that nobody has recorded anything, and [`holder`] renders an empty
/// file as exactly that.
#[allow(clippy::disallowed_types)] // This crate's own lease handle; see `try_hold`.
fn clear_record(file: &std::fs::File) {
    let _ = file.set_len(0);
}

/// What the lease file says about its holder, for a wait that is pending.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Reading this crate's own lease record; see `try_hold`.
fn holder(path: &Path) -> String {
    let mut record = String::new();
    let read = std::fs::OpenOptions::new()
        .read(true)
        .open(path)
        .and_then(|mut file| file.read_to_string(&mut record));
    match read {
        Ok(_) if !record.trim().is_empty() => record.trim().to_owned(),
        _ => "a holder that recorded nothing about itself".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::thread;
    use std::time::Duration;

    use super::*;

    /// A key nothing else on the machine holds, whose lease file goes with it.
    ///
    /// The key carries this process's id, so these cases queue behind each
    /// other and never behind a real watcher a sibling binary is running. The
    /// file it mints is this guard's to remove: a per-pid name nothing cleared
    /// would leave one file behind per case per run, and the isolation root is
    /// shared by every run on the machine.
    struct PrivateKey(String);

    impl PrivateKey {
        fn new(label: &str) -> Self {
            PrivateKey(format!("test-{label}-{}", std::process::id()))
        }

        fn as_str(&self) -> &str {
            &self.0
        }
    }

    impl Drop for PrivateKey {
        #[allow(clippy::disallowed_methods)] // Removing this module's own scratch lease file.
        fn drop(&mut self) {
            let _ = std::fs::remove_file(lease_path(&root(), &self.0));
        }
    }

    fn budget() -> Budget {
        Budget::new(Duration::from_secs(10), Duration::from_millis(500))
    }

    /// **The bar.** A lease is held by one holder at a time, and the second
    /// holder takes it only once the first has let go.
    ///
    /// The forbidden shape is a lease that admits everyone — a lock taken per
    /// process rather than per open file, or a hold that returns without
    /// locking anything. Under it the second hold lands while the first is
    /// still holding, and the flag the first sets on its way out is still
    /// false.
    #[test]
    fn a_second_hold_waits_for_the_first_to_let_go() {
        let private = PrivateKey::new("exclusion");
        let key = private.as_str().to_owned();
        let held = Arc::new(AtomicBool::new(false));
        let released = Arc::new(AtomicBool::new(false));

        let first = {
            let (key, held, released) = (key.clone(), Arc::clone(&held), Arc::clone(&released));
            thread::spawn(move || {
                let lease = Lease::hold(&key, budget());
                held.store(true, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(200));
                released.store(true, Ordering::SeqCst);
                drop(lease);
            })
        };

        wait_until("the first hold", budget(), || {
            if held.load(Ordering::SeqCst) {
                Observed::Met(())
            } else {
                Observed::pending("the first hold has not landed")
            }
        })
        .unwrap_or_else(|failure| panic!("{failure}"));

        let second = Lease::hold(&key, budget());
        assert!(
            released.load(Ordering::SeqCst),
            "the second hold landed while the first was still holding the lease"
        );
        assert_eq!(second.key(), key);
        drop(second);
        first.join().expect("the first holder");
    }

    /// A hold that cannot be taken inside its bound reports the holder rather
    /// than a bare timeout, and reports it as the wait's own failure.
    ///
    /// The bound it is reported against is the one the caller passed, which is
    /// what says [`Lease::try_hold`] carries a per-holder bound wider than a
    /// wall of milliseconds: an acquisition given fifty of them against a
    /// holder that never moves is the wall's failure and not the patience's.
    #[test]
    fn a_hold_that_never_comes_free_names_the_holder_it_waited_on() {
        let key = PrivateKey::new("occupied");
        let occupier = Lease::hold(key.as_str(), budget());

        let wall = Budget::new(Duration::from_millis(50), Duration::from_millis(500));
        let failure = Lease::try_hold(key.as_str(), wall).expect_err("a lease another holder has");

        assert_eq!(
            failure.budget.work(),
            wall.work(),
            "the failure is reported against {:?} rather than the {:?} wall the caller passed, so \
             this acquisition ran under a per-holder bound of its own: {failure}",
            failure.budget.work(),
            wall.work()
        );

        let rendered = failure.to_string();
        assert!(
            rendered.contains(&format!("pid {}", std::process::id())),
            "the failure does not name the holding process: {rendered}"
        );
        assert!(
            rendered.contains(key.as_str()),
            "the failure does not name the lease it waited for: {rendered}"
        );
        drop(occupier);
    }

    /// Two keys are two leases: holding one leaves the other free, so a second
    /// isolation concern joining here does not queue behind the watcher.
    #[test]
    fn two_keys_are_two_leases() {
        let (one, two) = (PrivateKey::new("one"), PrivateKey::new("two"));
        let watcher = Lease::hold(one.as_str(), budget());
        let other = Lease::hold(two.as_str(), budget());
        drop(other);
        drop(watcher);
    }

    /// **The bar on the holder record.** A lease that has been let go records
    /// no holder.
    ///
    /// The forbidden shape is the record that outlives its writer. Under it the
    /// next waiter to queue reads the line the last holder wrote, and its
    /// timeout names a process that let go long ago — a pid a reader goes and
    /// looks at, finds gone or belonging to something else, and learns nothing
    /// from.
    #[test]
    fn a_released_lease_records_no_holder() {
        let key = PrivateKey::new("released");
        let path = lease_path(&root(), key.as_str());

        let held = Lease::hold(key.as_str(), budget());
        assert!(
            holder(&path).contains(&format!("pid {}", std::process::id())),
            "a held lease does not record its holder: {}",
            holder(&path)
        );
        drop(held);

        let after = holder(&path);
        assert!(
            !after.contains("pid "),
            "the lease still names a holder after it was let go: {after}"
        );
    }

    /// **The bar on the per-holder bound.** A lease that stays in one holder's
    /// hands for the patience is reported against the patience, and the
    /// diagnostic says that is what happened.
    ///
    /// The forbidden shape is the bound that never fires — a look that re-arms
    /// whatever the record says, which is what a re-arm written without its
    /// comparison does. Under it a stuck holder is reported only when the whole
    /// acquisition wall runs out, minutes later and against a bound that says
    /// "the queue kept moving" about a queue that never moved.
    #[test]
    fn a_holder_that_never_changes_hands_is_named_at_the_patience_bound() {
        let key = PrivateKey::new("unmoving");
        let occupier = Lease::hold(key.as_str(), budget());

        let patience = Duration::from_millis(200);
        let wall = Budget::new(Duration::from_secs(10), Duration::from_millis(500));
        let failure = Lease::try_hold_under(key.as_str(), wall, Patience::Of(patience))
            .expect_err("a lease whose holder never lets go");

        assert_eq!(
            failure.budget.work(),
            patience,
            "the failure is reported against {:?} rather than the {patience:?} patience that is \
             what ran out: {failure}",
            failure.budget.work()
        );
        assert!(
            failure
                .last_state
                .contains("neither let go nor changed hands"),
            "the failure does not say the holder stopped rather than queued: {failure}"
        );
        assert!(
            failure
                .last_state
                .contains(&format!("pid {}", std::process::id())),
            "the failure does not name the holder that stopped: {failure}"
        );
        assert!(
            failure.elapsed < wall.work(),
            "the wait ran to its {:?} wall instead of ending at the patience: {failure}",
            wall.work()
        );
        drop(occupier);
    }

    /// **The bar on the re-arm.** A queue whose record keeps naming different
    /// hands runs on past the patience and ends at the acquisition wall
    /// instead, however many patience windows it outlives.
    ///
    /// The forbidden shape is the patience armed once for the whole
    /// acquisition — a look that keeps the first `since` whatever the record
    /// now says. Under it a deep queue that is changing hands every few
    /// milliseconds is cut off as a stuck holder, and the case that was next in
    /// line fails naming a process that was working the whole time.
    #[test]
    #[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Writing this module's own scratch lease record.
    fn a_queue_that_keeps_changing_hands_outlives_the_patience_bound() {
        let key = PrivateKey::new("changing-hands");
        let path = lease_path(&root(), key.as_str());
        let occupier = Lease::hold(key.as_str(), budget());

        let patience = Duration::from_millis(500);
        let wall = Budget::new(Duration::from_secs(2), Duration::from_millis(500));

        // The queue changing hands, written where a waiter reads it: each pass
        // leaves a record naming a different holder than the last. The widest
        // gap between two of them is kept because it, and not the count, is
        // what the waiter sees: a writer starved for longer than the patience
        // has handed the waiter a record that did not move, and the failure
        // that follows would be this thread's rather than the re-arm's.
        let stop = Arc::new(AtomicBool::new(false));
        let handovers = Arc::new(AtomicUsize::new(0));
        let widest_gap = Arc::new(AtomicU64::new(0));
        let hands = {
            let (path, stop, handovers, widest_gap) = (
                path.clone(),
                Arc::clone(&stop),
                Arc::clone(&handovers),
                Arc::clone(&widest_gap),
            );
            thread::spawn(move || {
                let mut last = Instant::now();
                while !stop.load(Ordering::SeqCst) {
                    let nth = handovers.fetch_add(1, Ordering::SeqCst);
                    let record = format!("pid {nth} holding a lease since unix 0");
                    let _ = std::fs::OpenOptions::new()
                        .write(true)
                        .truncate(true)
                        .open(&path)
                        .and_then(|mut file| file.write_all(record.as_bytes()));
                    let now = Instant::now();
                    widest_gap.fetch_max(
                        now.duration_since(last).as_micros() as u64,
                        Ordering::SeqCst,
                    );
                    last = now;
                    thread::sleep(Duration::from_millis(2));
                }
            })
        };

        let failure = Lease::try_hold_under(key.as_str(), wall, Patience::Of(patience))
            .expect_err("a lease no waiter is ever handed");
        stop.store(true, Ordering::SeqCst);
        hands.join().expect("the hands the record kept changing to");

        let changed = handovers.load(Ordering::SeqCst);
        let widest = Duration::from_micros(widest_gap.load(Ordering::SeqCst));
        assert!(
            widest < patience,
            "this case's own hands stalled for {widest:?}, past the {patience:?} patience, so the \
             record they left did stop moving and what follows judges the stall rather than the \
             re-arm"
        );
        assert!(
            changed as u32 > wall.work().as_millis() as u32 / patience.as_millis() as u32,
            "the record changed hands {changed} times, too few to outlive a {patience:?} patience \
             inside a {:?} wall",
            wall.work()
        );
        assert_eq!(
            failure.budget.work(),
            wall.work(),
            "a queue that changed hands {changed} times was cut off against the {:?} per-holder \
             bound instead of running to its wall: {failure}",
            failure.budget.work()
        );
        assert!(
            !failure
                .last_state
                .contains("neither let go nor changed hands"),
            "a queue that changed hands {changed} times is reported as a holder that stopped: \
             {failure}"
        );
        drop(occupier);
    }

    /// **The bar on a holder record that cannot be written.** A failed write
    /// leaves the lease file saying nothing, not saying what the last holder
    /// said.
    ///
    /// The forbidden shape is the swallowed error that leaves the previous
    /// record standing. Under it the next waiter's timeout names a process that
    /// let go before this holder ever took the lease — a pid a reader goes and
    /// looks at, and a wrong answer is worse than none.
    #[test]
    fn a_holder_record_that_cannot_be_written_leaves_no_record_at_all() {
        let key = PrivateKey::new("unwritable-record");
        let path = lease_path(&root(), key.as_str());

        let held = Lease::hold(key.as_str(), budget());
        let standing = holder(&path);
        assert!(
            standing.contains(&format!("pid {}", std::process::id())),
            "a held lease does not record its holder: {standing}"
        );

        record_holder(&held.file, key.as_str(), |_, _| {
            Err(std::io::Error::other("the record could not be written"))
        });

        assert_eq!(
            holder(&path),
            "a holder that recorded nothing about itself",
            "a record that could not be written left the previous one standing"
        );
        assert_eq!(
            held.file.metadata().expect("the lease file").len(),
            0,
            "the lease file still carries bytes a waiter would read as a holder"
        );
        drop(held);
    }

    /// The acquisition wall is the shorter of the depth the census derives and
    /// the ceiling the lanes' own walls set, and it fires inside the job.
    #[test]
    fn the_acquisition_wall_fits_inside_the_job_it_has_to_fail_in() {
        let hold_window = Budget::new(Duration::from_secs(15), Duration::from_millis(250));
        let acquisition = acquisition_budget(hold_window);
        assert_eq!(acquisition.probe(), hold_window.probe());
        assert!(
            acquisition.work() <= ACQUISITION_WALL,
            "an acquisition gets {:?}, past the {ACQUISITION_WALL:?} ceiling",
            acquisition.work()
        );
        assert!(
            ACQUISITION_WALL < PER_PR_JOB_WALL,
            "the acquisition ceiling is at or past the {PER_PR_JOB_WALL:?} job wall it has to \
             fail inside of"
        );
        assert!(
            HOLDER_PATIENCE < acquisition.work(),
            "a stuck holder is diagnosed at {HOLDER_PATIENCE:?}, which is not inside the {:?} \
             wall the acquisition would otherwise reach first",
            acquisition.work()
        );

        // A hold window short enough that the census binds instead of the
        // ceiling: the wall is derived, not a constant that ignores its input.
        let brief = Budget::new(Duration::from_secs(1), Duration::from_millis(250));
        assert_eq!(
            acquisition_budget(brief).work(),
            Duration::from_secs(1) * QUEUED_HOLDERS
        );
    }

    /// The lease directory is derived from the environment, never written
    /// down: nothing here names a path that only one machine has.
    #[test]
    fn a_lease_lives_under_the_derived_isolation_root() {
        let derived = root();
        assert!(
            derived.starts_with(std::env::temp_dir()) || std::env::var_os(ISOLATION_ROOT).is_some(),
            "the isolation root {} is neither the environment's nor under the system temporary \
             directory",
            derived.display()
        );
        assert_eq!(
            lease_path(Path::new("/somewhere"), REAL_WATCHER),
            Path::new("/somewhere").join(format!("{REAL_WATCHER}.lease"))
        );
    }
}
