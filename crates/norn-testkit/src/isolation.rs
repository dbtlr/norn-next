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
//! window that watcher is live. The lease is taken where the watcher is
//! installed rather than where the run began, so it counts every binary the
//! runner started and every child those binaries re-execute. How many holders
//! it admits at once is the platform's, and the next section says which.
//!
//! # Linux shares the lease; every other platform holds it alone
//!
//! The silence is macOS's: every subscription there is a client of one
//! daemon, fseventsd, and that daemon is what goes quiet under a crowd. Linux
//! has no such daemon. Each watcher is an inotify instance of the process's
//! own, counted against a per-user instance limit and served by the kernel, and
//! the host suites pass with a dozen of them live at once, in one process or in
//! twelve. So the lease has two kinds of hold:
//!
//! - **An ordinary hold**, [`Lease::hold`], is what a case that attaches a
//!   production host takes. On Linux it is shared: any number of ordinary
//!   holders are live at once, in one process or in several. On every other
//!   platform it is exclusive.
//! - **An exclusive hold**, [`Lease::hold_exclusive`], is what the
//!   watcher-fidelity suites take: the cases that state what the platform
//!   watcher itself delivers, which are the cases a crowd of sibling watchers
//!   would turn into statements about the crowd. It excludes every other hold
//!   on every platform.
//!
//! [`ORDINARY_HOLDS_SHARE`] is the switch. Off Linux both kinds are exclusive,
//! so exactly one holder is live on macOS, as before the lease had two kinds.
//!
//! **Shared and exclusive locks on one file, rather than a number of slot
//! files.** A slot count is a number to choose and to keep in step with the
//! runner, and an exclusive window over slots means taking every slot in turn.
//! A shared lock needs no count, and the exclusive window is the lock's other
//! mode. What it costs is the holder record: one file cannot carry the name of
//! every holder sharing it, so each holder writes a record of its own (below).
//!
//! **No bound on how many ordinary holders are live, beyond the runner's.** The
//! count is the test threads of every binary running at once, and the limit it
//! meets is the per-user inotify instance limit (`fs.inotify.max_user_instances`),
//! whose kernel default is 128 and which GitHub's hosted Ubuntu runners raise
//! to 1,280. One attached host installs one inotify instance, for its one
//! subscription, so four test threads are four live instances per binary and
//! twelve are twelve: a case that attaches a few vaults at once multiplies
//! that by a few, and that product stays well inside the default. The watches
//! inside each instance, one per covered directory, count against
//! `fs.inotify.max_user_watches`, and a test vault covers a handful.
//!
//! # An exclusive waiter is not starved by a stream of ordinary holders
//!
//! A shared lock admits a new shared holder while an exclusive waiter is
//! queued, so ordinary holders that overlap one another would keep the lease
//! from ever coming free. Each lease therefore has a turnstile beside it, a
//! second lock file. An exclusive waiter stands in the turnstile, exclusively,
//! for as long as it waits; an ordinary hold passes through it, shared, on its
//! way to the lease and cannot pass while a waiter stands there.
//!
//! So once an exclusive waiter is in the turnstile no new ordinary holder
//! lands, and the holders already in drain side by side. What bounds that wait
//! is what bounds every acquisition: [`HOLDER_PATIENCE`] on the oldest holder
//! still in, which a stuck one stays, and the acquisition wall. A failure names
//! that oldest holder. An ordinary hold that is held at the turnstile names
//! the same holders, because those are what it is queued behind, and says an
//! exclusive hold is waiting ahead of it.
//!
//! # A holder records itself in a file of its own
//!
//! Every holder, of either kind, writes a record — its pid, the key, the kind
//! of hold and when it took it — into a file of its own in the lease's
//! holders directory, and keeps that file locked for as long as it holds. A
//! waiter lists the directory and names the oldest record whose file is still
//! locked, which is the holder it has been behind longest, and says how many
//! more stand beside it. A record whose file it can lock is one whose holder
//! died without withdrawing it, and the waiter removes it rather than naming a
//! pid that holds nothing. The record is published under its final name only
//! once it is locked, so a waiter never reads a live holder's record as a dead
//! one.
//!
//! # Exclusion joins here; partitioning is the other door
//!
//! A second isolation concern joins here rather than bringing a second
//! mechanism: a hold is keyed, and a key is what a new concern declares.
//!
//! **What joins here is exclusion**: concerns whose subject is one machine-wide
//! thing that has to be used by one holder at a time. Such a concern takes
//! [`Lease::hold_exclusive`]; the shared ordinary hold is shaped by the
//! watcher's platforms, and a concern whose platforms tolerate a crowd
//! differently says so where it declares its key. The other family is
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
//! that takes an exclusive hold while already holding the lease waits out its
//! whole acquisition bound against itself and then names its own process as
//! the holder. Off Linux an ordinary hold inside an ordinary hold does the
//! same. On Linux it lands, but only while no exclusive waiter stands in the
//! turnstile; with one there it waits at the turnstile behind a waiter that is
//! waiting for it. So each watcher window has exactly one hold: a helper that
//! takes the lease is the only thing that takes it, and its callers hold what
//! it hands back rather than taking one of their own.
//!
//! # The lock is a file lock, and that is what makes it cross-process
//!
//! [`Lease`] is a lock on an open file. Two properties are why:
//!
//! - **It is released when the holder dies.** A test that panics, and a
//!   process killed mid-run, both release — the lease, the turnstile a waiter
//!   stood in, and the lock on the holder's record. A lease built from a
//!   file's *existence* would leave a killed run's lease standing forever, and
//!   the next run would wait out its budget against a holder that is gone.
//! - **It excludes per open file description, not per process.** Two threads
//!   in one binary each open the lease file, and an exclusive hold on one
//!   excludes the other, which is the case the runner's own threads produce. A
//!   POSIX record lock is per-process and would let all four of a binary's
//!   test threads hold an exclusive lease at once — the exclusion nobody would
//!   notice was missing.
//!
//! # Acquisition is a budgeted wait, and what fires is per holder
//!
//! Taking a lease is [`crate::wait`]'s wait like any other: a bound, and a
//! failure naming the holder it was waiting on. Queueing behind other holders
//! is what the lease is *for*, so the thing worth diagnosing is not a long
//! queue but a holder that is stuck rather than working — and those two are
//! told apart by whether the queue is moving.
//!
//! So the acquisition carries two bounds over two different things, the same
//! way a [`Budget`] does:
//!
//! - [`HOLDER_PATIENCE`] is per holder, and it is what fires. Every look that
//!   finds a different oldest holder than the last one re-arms it, so a queue
//!   that is changing hands is never interrupted however deep it is.
//! - [`acquisition_budget`] is the wall on the whole acquisition, because a
//!   wait that runs on while it observes progress is a wait with no deadline
//!   at all — the shape [`crate::wait`] refuses to offer.
//!
//! The wall fits inside the full-regression memory job guard, which
//! is the constraint a depth-derived bound cannot meet: [`QUEUED_HOLDERS`]
//! holders at a fifteen-second hold window is hours, and a bound past the job's
//! own wall never fires. Under it a stuck holder ends the run as a job timeout
//! that names nothing, which is precisely the diagnostic this module exists to
//! produce.

use std::fs::TryLockError;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::wait::{Budget, FailureKind, Observed, WaitFailure, wait_until};

/// The lease a case holds while it owns a real platform watcher.
///
/// Every such case across the workspace names this one key, because the
/// service they contend for is one service. A suite that watches a real
/// filesystem and does not hold it is a suite that starves the others. A case
/// that attaches a production host takes an ordinary hold, [`Lease::hold`]; a
/// watcher-fidelity case takes [`Lease::hold_exclusive`].
pub const REAL_WATCHER: &str = "real-watcher";

/// Whether an ordinary hold shares the lease with other ordinary holds on this
/// platform.
///
/// True on Linux only, where each watcher is a kernel inotify instance of its
/// own process and a crowd of them stays heard. Everywhere else an ordinary
/// hold is exclusive, so one holder is live at a time; the module
/// documentation says why.
pub const ORDINARY_HOLDS_SHARE: bool = cfg!(target_os = "linux");

/// The real-watcher holders that can be queued ahead of one case, where
/// holders queue one at a time.
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
/// **It is the depth of the serial queue, which is the queue off Linux.** Where
/// every hold is exclusive each holder ahead is one hold window. On Linux
/// ordinary holders do not queue behind one another: an ordinary hold waits
/// only for an exclusive holder, or for an exclusive waiter standing in the
/// turnstile, and an exclusive hold waits for the exclusive waiters ahead of it
/// and for the ordinary holders in flight when it reached the turnstile, which
/// drain side by side in about one hold window. The queue ahead of a case on
/// Linux is therefore shorter than this depth, and the wall the depth derives
/// holds on both.
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

/// How long one holder may stay the oldest a waiter is behind before the
/// waiter names it.
///
/// **This is the bound that fires, and it is per holder rather than per
/// acquisition.** A waiter blocked by several holders tracks the oldest of
/// them, the one it has been behind longest: younger holders arriving or
/// leaving beside it do not move that, so a stuck holder among working ones is
/// still named. A look that finds a different oldest holder than the last one
/// re-arms it, so a queue that is changing hands runs on under the wall alone;
/// a record that has not moved for this long is a holder that stopped working
/// rather than one working slowly.
///
/// **The span held against it is a lower bound on how long the waiter saw one
/// record string unchanged**: it runs between two reads that both saw that
/// string, never through the time a look spent descheduled around its own read,
/// so a waiter starved on a busy runner cannot stretch a moving queue past it.
/// A string that recurs across hand-overs joins those standings; the
/// module's look tracker says which strings do and what that costs.
///
/// It is sized for the longest window a case honestly holds the lease: a whole
/// case rather than a single wait, since a case attaches a vault, runs a
/// sequence of waits over it, and detaches. Five minutes is far past any case
/// in the workspace suite and far inside [`MEMORY_JOB_WALL`].
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

/// How a hold shares the lease with other holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Beside any number of other shared holds, and never beside an exclusive
    /// one.
    Shared,
    /// Alone.
    Exclusive,
}

impl Mode {
    /// The mode an ordinary hold takes on this platform.
    const ORDINARY: Mode = if ORDINARY_HOLDS_SHARE {
        Mode::Shared
    } else {
        Mode::Exclusive
    };

    /// How a holder record says it holds.
    fn word(self) -> &'static str {
        match self {
            Mode::Shared => "shared",
            Mode::Exclusive => "exclusively",
        }
    }
}

/// The full-regression memory job's wall, which every bound here must fire
/// inside of.
///
/// A bound past this is a bound that never fires: the job ends first, and it
/// ends naming a timeout rather than a holder.
pub const MEMORY_JOB_WALL: Duration = Duration::from_secs(20 * 60);

/// The ceiling on one acquisition, whatever the queue depth derives.
///
/// Three quarters of [`MEMORY_JOB_WALL`], so the wait fails inside the job and
/// leaves the rest of it for the case to report the failure and the runner to
/// collect it. The scheduled soak lane's wall is six times the memory job's, so a
/// ceiling that fits inside the shorter fits inside both.
pub const ACQUISITION_WALL: Duration = Duration::from_secs(15 * 60);

/// The bound on taking a lease, given the window one holder holds it for.
///
/// The work bound is the shorter of two real constraints: what a fully queued
/// acquisition can honestly take — [`QUEUED_HOLDERS`] hold windows, the serial
/// queue — and [`ACQUISITION_WALL`]. At a fifteen-second hold window the first
/// of those is hours and the ceiling is what binds; at a hold window short
/// enough that the depth comes in under the ceiling, the depth binds instead.
/// The same bound serves both kinds of hold and every platform, because the
/// serial queue is the longest either kind meets anywhere.
///
/// A probe makes a non-blocking lock attempt and reads the holder records.
/// Its ten-second allowance is independent of the holder's own probes:
/// descheduling a waiter must not turn a moving queue into a failure.
pub fn acquisition_budget(hold_window: Budget) -> Budget {
    Budget::new(
        hold_window
            .work()
            .saturating_mul(QUEUED_HOLDERS)
            .min(ACQUISITION_WALL),
        Duration::from_secs(10),
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

/// The three places one lease lives, side by side under the root.
struct LeaseFiles {
    /// The lock every holder takes, shared or exclusive.
    lock: PathBuf,
    /// The lock an exclusive waiter stands in and an ordinary hold passes
    /// through.
    turnstile: PathBuf,
    /// The directory each holder writes its own record into.
    holders: PathBuf,
}

impl LeaseFiles {
    fn under(root: &Path, key: &str) -> Self {
        let lock = lease_path(root, key);
        LeaseFiles {
            lock,
            turnstile: root.join(format!("{key}.turnstile")),
            holders: root.join(format!("{key}.holders")),
        }
    }
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
    // This holder's own record, or nothing when it could not be published; a
    // waiter then names a holder that recorded nothing about itself.
    record: Option<Record>,
}

impl Lease {
    /// Take an ordinary hold on the lease named `key`, waiting under `budget`
    /// for whoever excludes it.
    ///
    /// An ordinary hold is shared on Linux and exclusive elsewhere; see
    /// [`ORDINARY_HOLDS_SHARE`]. A failure to acquire panics with the wait's
    /// own diagnostic, because a lease is harness scaffolding rather than a
    /// subject: the case that could not take it has asserted nothing yet, and
    /// there is nothing for it to report but the holder it waited on.
    /// [`Lease::try_hold`] is the same acquisition with the failure handed
    /// back.
    pub fn hold(key: &str, budget: Budget) -> Self {
        Self::try_hold(key, budget).unwrap_or_else(|failure| panic!("{failure}"))
    }

    /// Take an exclusive hold on the lease named `key`, waiting under `budget`
    /// for every other holder to let go.
    ///
    /// Exclusive on every platform. It panics on a failure to acquire, as
    /// [`Lease::hold`] does; [`Lease::try_hold_exclusive`] hands it back.
    pub fn hold_exclusive(key: &str, budget: Budget) -> Self {
        Self::try_hold_exclusive(key, budget).unwrap_or_else(|failure| panic!("{failure}"))
    }

    /// Take an ordinary hold on the lease named `key`, handing back the wait
    /// that failed.
    ///
    /// The probe is one non-blocking attempt at the turnstile and the lock, so
    /// it costs a few syscalls whatever the machine is doing, and a pending
    /// observation names the record the oldest holder wrote about itself.
    ///
    /// **Two bounds end this wait, and the one that ends it says which.**
    /// `budget`'s work bound is the wall on the whole acquisition; a failure
    /// that names it is a queue that kept moving for that long.
    /// [`HOLDER_PATIENCE`] is the per-holder bound, re-armed every time the
    /// oldest record names different hands than the last look and held against
    /// a lower bound on how long the waiter saw one record unchanged, and a
    /// failure that names it is one holder that stopped letting go. Both come
    /// back as the same [`WaitFailure`] shape, carrying the bound that was
    /// passed and the holder that was there when it went.
    pub fn try_hold(key: &str, budget: Budget) -> Result<Self, WaitFailure> {
        Self::try_hold_under(key, Mode::ORDINARY, budget, Patience::Standing)
    }

    /// Take an exclusive hold on the lease named `key`, handing back the wait
    /// that failed.
    ///
    /// The waiter stands in the lease's turnstile while it waits, so no new
    /// ordinary holder lands ahead of it, and the bounds are
    /// [`Lease::try_hold`]'s.
    pub fn try_hold_exclusive(key: &str, budget: Budget) -> Result<Self, WaitFailure> {
        Self::try_hold_under(key, Mode::Exclusive, budget, Patience::Standing)
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
    fn try_hold_under(
        key: &str,
        mode: Mode,
        budget: Budget,
        patience: Patience,
    ) -> Result<Self, WaitFailure> {
        Self::try_hold_noting(key, mode, budget, patience, |line| eprintln!("{line}"))
    }

    /// The acquisition, with the waiter's two notices handed to `note`.
    ///
    /// A waiter that finds the lease held says once who holds it, using the
    /// records the failure messages already read, and says when it acquires
    /// the lease how long it waited. A hold that finds the lease free says
    /// nothing, so uncontended runs stay quiet. The notices are why a caller
    /// whose own deadline counts this queue time is not left with an empty
    /// stderr when a long holder ends it. No bound moves with them.
    #[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The lease is this crate's own machine-local state, and a file lock is what makes it cross-process.
    fn try_hold_noting(
        key: &str,
        mode: Mode,
        budget: Budget,
        patience: Patience,
        mut note: impl FnMut(&str),
    ) -> Result<Self, WaitFailure> {
        let patience = patience.bound();
        let root = root();
        let files = LeaseFiles::under(&root, key);
        std::fs::create_dir_all(&files.holders).unwrap_or_else(|error| {
            panic!("the lease directory {}: {error}", files.holders.display())
        });
        let open = |path: &Path| {
            std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(path)
                .unwrap_or_else(|error| panic!("the lease file {}: {error}", path.display()))
        };
        let file = open(&files.lock);
        // Opened per acquisition, so it is this waiter's own open file
        // description, and dropped at the end of this function: an exclusive
        // waiter stands in it only while it waits.
        let turnstile = open(&files.turnstile);
        let attempt = |outcome: Result<(), TryLockError>, path: &Path| match outcome {
            Ok(()) => true,
            Err(TryLockError::WouldBlock) => false,
            Err(TryLockError::Error(error)) => {
                panic!("locking the lease file {}: {error}", path.display())
            }
        };

        let what = format!("the {key} lease");
        let started = Instant::now();
        let mut probes = 0usize;
        let mut looks = Unmoved::default();
        let mut queued = false;
        let mut in_turnstile = false;

        wait_until(&what, budget, || {
            probes += 1;
            // Where this look stopped: at the turnstile, behind an exclusive
            // waiter, or at the lock itself.
            let (landed, at_turnstile) = match mode {
                Mode::Exclusive => {
                    if !in_turnstile {
                        in_turnstile = attempt(turnstile.try_lock(), &files.turnstile);
                    }
                    (
                        in_turnstile && attempt(file.try_lock(), &files.lock),
                        !in_turnstile,
                    )
                }
                Mode::Shared => {
                    if attempt(turnstile.try_lock_shared(), &files.turnstile) {
                        let landed = attempt(file.try_lock_shared(), &files.lock);
                        let _ = turnstile.unlock();
                        (landed, false)
                    } else {
                        (false, true)
                    }
                }
            };
            if landed {
                return Observed::Met(Ok(()));
            }
            let mut beside = 0usize;
            let (record, unmoved) = looks.look(|| {
                let standing = standing(&files.holders);
                beside = standing.beside;
                standing.oldest
            });
            let behind = behind(&record, beside, at_turnstile);
            if !queued {
                queued = true;
                note(&format!("waiting for the {key} lease, held by {behind}"));
            }
            if unmoved >= patience {
                Observed::Met(Err(WaitFailure {
                    what: what.clone(),
                    kind: FailureKind::Elapsed,
                    budget: Budget::new(patience, budget.probe()),
                    elapsed: started.elapsed(),
                    probes,
                    last_state: format!(
                        "held by {behind}; the oldest has neither let go nor changed hands in \
                         {unmoved:?}"
                    ),
                }))
            } else {
                Observed::pending(format!("held by {behind}; the oldest for {unmoved:?}"))
            }
        })??;
        drop(turnstile);

        if queued {
            note(&format!(
                "acquired the {key} lease; waited {:?}",
                started.elapsed()
            ));
        }
        Ok(Lease {
            key: key.to_owned(),
            file,
            record: Record::publish(&files.holders, key, mode),
        })
    }

    /// The key this lease was taken under.
    pub fn key(&self) -> &str {
        &self.key
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        // The record goes before the lock does. A record left standing after
        // its writer let go names a process that no longer holds anything, and
        // the next waiter to read it would name that dead pid as the holder it
        // is queued behind.
        if let Some(record) = self.record.take() {
            record.withdraw();
        }
        // Closing the file would release the lock on its own; unlocking first
        // says so where a reader is looking for the release.
        let _ = self.file.unlock();
    }
}

/// How a waiter names what it is queued behind.
///
/// The oldest record is the holder; the count beside it and the turnstile are
/// context. Only the oldest record is what the look tracker compares, so
/// younger holders arriving and leaving do not re-arm the patience on a stuck
/// one.
fn behind(oldest: &str, beside: usize, at_turnstile: bool) -> String {
    let mut named = oldest.to_owned();
    if beside > 0 {
        named.push_str(&format!(", with {beside} more holding beside it"));
    }
    if at_turnstile {
        named.push_str(", and an exclusive hold waiting ahead in the turnstile");
    }
    named
}

/// How long the holder record one acquisition keeps reading has stood
/// unchanged, read as a lower bound.
///
/// **A look is one read of the record between two clock readings, and the span
/// is taken between the two that cannot overstate it**: from the reading after
/// the first look that saw this record to the reading before the look that sees
/// it now. Both reads saw the record, so it stood at least that long however
/// long either look was descheduled around its read. The other pair is the one
/// that overstates — a look that reads the record, is not scheduled, and only
/// then reads the clock charges the holder for time the waiter spent off the
/// processor, and a queue changing hands on a starved runner is named as a
/// holder that stopped. A holder that really stands still is still named at the
/// patience, at most one look later than the clock alone would name it.
///
/// **What it bounds is one record string, not one holder.** A string that
/// recurs across hand-overs joins those standings into one span: every empty
/// record reads as the same placeholder, whether no holder recorded anything,
/// one died recording nothing, or one failed to write its record, and a process
/// that holds the lease twice in the same second writes the same record both
/// times. So a run of consecutive looks that each land on such a string,
/// spanning the patience, names a queue that did move. The placeholder is not
/// made to re-arm all the same, because then a holder that died recording
/// nothing would never be named at all.
///
/// The look reads the clock and the record in one place, [`Unmoved::look`], so
/// no call site can order them the other way.
#[derive(Debug, Default)]
struct Unmoved {
    // The record the last look read, and the clock reading after the first look
    // that read it. Both move together, and only when the record changes.
    standing: Option<(String, Instant)>,
}

impl Unmoved {
    /// Take one look through `read`, handing back the record it read and the
    /// span the waiter has seen that record unchanged for, at least.
    fn look(&mut self, read: impl FnOnce() -> String) -> (String, Duration) {
        let before = Instant::now();
        let record = read();
        let after = Instant::now();
        let unmoved = self.charge(&record, before, after);
        (record, unmoved)
    }

    /// The arithmetic of one look: `record` was read after `before` and ahead
    /// of `after`.
    fn charge(&mut self, record: &str, before: Instant, after: Instant) -> Duration {
        match &self.standing {
            Some((named, since)) if named == record => before.saturating_duration_since(*since),
            _ => {
                self.standing = Some((record.to_owned(), after));
                Duration::ZERO
            }
        }
    }
}

/// One holder's record of itself: a file in the lease's holders directory that
/// the holder keeps locked for as long as it holds.
///
/// The lock is what tells a live record from a dead one. A holder that dies
/// releases it with every other lock it had, so a record a reader can lock is
/// one nobody is holding behind, and the reader removes it.
#[derive(Debug)]
struct Record {
    path: PathBuf,
    // Locked exclusively for the record's life; see the type.
    #[allow(clippy::disallowed_types)] // This crate's own lease record; see `try_hold`.
    file: std::fs::File,
}

impl Record {
    /// Write and publish this holder's record, or publish nothing when it
    /// cannot be written.
    ///
    /// The file is made and locked under a name readers do not list, and only
    /// renamed to its listed name once it is locked: a reader that could see
    /// it before the lock would read a live holder as a dead one and remove
    /// it. A holder killed between those two steps leaves an unlisted file
    /// behind, which no reader names. The listed name begins with the time it
    /// was taken, zero-padded, so names sort oldest first.
    #[allow(clippy::disallowed_methods, clippy::disallowed_types)] // This crate's own lease record; see `try_hold`.
    fn publish(holders: &Path, key: &str, mode: Mode) -> Option<Record> {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let taken = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let stem = format!(
            "{:020}-{}-{}",
            taken.as_nanos(),
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        );
        let unlisted = holders.join(format!(".{stem}.unlisted"));
        let path = holders.join(format!("{stem}.holder"));
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&unlisted)
            .ok()?;
        let listed = file.try_lock().is_ok() && {
            record_holder(&file, key, mode, write_record);
            std::fs::rename(&unlisted, &path).is_ok()
        };
        if listed {
            Some(Record { path, file })
        } else {
            let _ = std::fs::remove_file(&unlisted);
            None
        }
    }

    /// Take the record down, and only then let go of its lock.
    #[allow(clippy::disallowed_methods)] // This crate's own lease record; see `try_hold`.
    fn withdraw(self) {
        let _ = std::fs::remove_file(&self.path);
        drop(self.file);
    }
}

/// Write who holds the lease, for whoever is waiting on it, putting the line
/// down through `write`.
///
/// The record is diagnostic only: a waiter may read it while the holder writes
/// it, so a torn read is possible and costs a confusing line in a failure that
/// was going to name the wait's own bound anyway. What it buys is a timeout
/// that names a process a reader can go look at.
///
/// **A write that fails clears the record instead of leaving a partial one.**
/// The failure a swallowed error could otherwise cause is not a missing record
/// but a wrong one, and an empty record reads as the holder that recorded
/// nothing about itself, which is true.
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
    mode: Mode,
    write: impl FnOnce(&std::fs::File, &str) -> std::io::Result<()>,
) {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let record = format!(
        "pid {} holding {key} {} since unix {since}",
        std::process::id(),
        mode.word()
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

/// Leave the record saying nothing about who holds the lease.
///
/// It is the fallback for a record that could not be written: the honest
/// answer is that nobody has recorded anything, and [`standing`] renders an
/// empty record as exactly that.
#[allow(clippy::disallowed_types)] // This crate's own lease handle; see `try_hold`.
fn clear_record(file: &std::fs::File) {
    let _ = file.set_len(0);
}

/// What a waiter reads as the placeholder for a holder with no record.
const UNRECORDED: &str = "a holder that recorded nothing about itself";

/// The holders a waiter is behind, as their records name them.
#[derive(Debug, PartialEq, Eq)]
struct Standing {
    /// The oldest live holder's record, or [`UNRECORDED`] when no live holder
    /// has one.
    oldest: String,
    /// How many live records stand beside the oldest.
    beside: usize,
}

/// Read the live records under a lease's holders directory, for a wait that
/// is pending.
///
/// A record whose file this read can lock is one whose holder is gone, and it
/// is removed here rather than named.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Reading this crate's own lease records; see `try_hold`.
fn standing(holders: &Path) -> Standing {
    let mut listed: Vec<PathBuf> = std::fs::read_dir(holders)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "holder")
                })
                .collect()
        })
        .unwrap_or_default();
    listed.sort();
    let mut live = listed.iter().filter_map(|path| {
        let mut file = std::fs::OpenOptions::new().read(true).open(path).ok()?;
        match file.try_lock_shared() {
            Ok(()) => {
                let _ = std::fs::remove_file(path);
                None
            }
            Err(TryLockError::WouldBlock) => {
                let mut record = String::new();
                let read = file.read_to_string(&mut record);
                Some(match read {
                    Ok(_) if !record.trim().is_empty() => record.trim().to_owned(),
                    _ => UNRECORDED.to_owned(),
                })
            }
            Err(TryLockError::Error(_)) => None,
        }
    });
    let oldest = live.next().unwrap_or_else(|| UNRECORDED.to_owned());
    Standing {
        oldest,
        beside: live.count(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::thread;
    use std::time::Duration;

    use super::*;

    /// A key nothing else on the machine holds, whose lease files go with it.
    ///
    /// The key carries this process's id, so these cases queue behind each
    /// other and never behind a real watcher a sibling binary is running. The
    /// files it mints are this guard's to remove: a per-pid name nothing
    /// cleared would leave files behind per case per run, and the isolation
    /// root is shared by every run on the machine.
    struct PrivateKey(String);

    impl PrivateKey {
        fn new(label: &str) -> Self {
            PrivateKey(format!("test-{label}-{}", std::process::id()))
        }

        fn as_str(&self) -> &str {
            &self.0
        }

        fn files(&self) -> LeaseFiles {
            LeaseFiles::under(&root(), &self.0)
        }
    }

    impl Drop for PrivateKey {
        #[allow(clippy::disallowed_methods)] // Removing this module's own scratch lease files.
        fn drop(&mut self) {
            let files = self.files();
            let _ = std::fs::remove_file(files.lock);
            let _ = std::fs::remove_file(files.turnstile);
            let _ = std::fs::remove_dir_all(files.holders);
        }
    }

    fn budget() -> Budget {
        Budget::new(Duration::from_secs(10), Duration::from_millis(500))
    }

    /// A wall a hold that is excluded spends in full, and a hold that is free
    /// lands well inside.
    fn brief() -> Budget {
        Budget::new(Duration::from_millis(50), Duration::from_millis(500))
    }

    /// One way of taking the lease, named for the failure messages.
    type Take = (&'static str, fn(&str, Budget) -> Lease);
    const ORDINARY: Take = ("an ordinary hold", Lease::hold);
    const EXCLUSIVE: Take = ("an exclusive hold", Lease::hold_exclusive);

    /// The second hold lands only once the first has let go.
    fn waits_for_the_first((first_kind, first): Take, (second_kind, second): Take, label: &str) {
        let private = PrivateKey::new(label);
        let key = private.as_str().to_owned();
        let held = Arc::new(AtomicBool::new(false));
        let released = Arc::new(AtomicBool::new(false));

        let holder = {
            let (key, held, released) = (key.clone(), Arc::clone(&held), Arc::clone(&released));
            thread::spawn(move || {
                let lease = first(&key, budget());
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

        let lease = second(&key, budget());
        assert!(
            released.load(Ordering::SeqCst),
            "{second_kind} landed while {first_kind} was still holding the lease"
        );
        assert_eq!(lease.key(), key);
        drop(lease);
        holder.join().expect("the first holder");
    }

    /// **The bar on exclusion.** An exclusive hold is held by one holder at a
    /// time on every platform: it waits for any holder to let go, and any
    /// holder waits for it.
    ///
    /// The forbidden shape is a lease that admits everyone — a lock taken per
    /// process rather than per open file, a shared lock where an exclusive one
    /// was asked for, or a hold that returns without locking anything. Under it
    /// the second hold lands while the first is still holding, and the flag
    /// the first sets on its way out is still false.
    #[test]
    fn an_exclusive_hold_and_any_other_hold_wait_for_each_other() {
        waits_for_the_first(EXCLUSIVE, EXCLUSIVE, "exclusive-exclusive");
        waits_for_the_first(ORDINARY, EXCLUSIVE, "ordinary-exclusive");
        waits_for_the_first(EXCLUSIVE, ORDINARY, "exclusive-ordinary");
    }

    /// **The bar off Linux.** An ordinary hold waits for an ordinary holder,
    /// so exactly one holder of the watcher lease is live at a time.
    ///
    /// The forbidden shape is the Linux sharing reaching a platform whose
    /// watcher service goes silent under a crowd, which is the starvation the
    /// lease was written for.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn off_linux_an_ordinary_hold_waits_for_the_one_live_holder() {
        assert!(!ORDINARY_HOLDS_SHARE);
        waits_for_the_first(ORDINARY, ORDINARY, "ordinary-ordinary");
    }

    /// **The bar on Linux.** Two ordinary holds are live at once in one
    /// process: the second lands at once while the first still holds.
    ///
    /// The forbidden shape is the serial lease this replaced, under which every
    /// host case on the machine queues behind every other and threads buy
    /// nothing.
    #[cfg(target_os = "linux")]
    #[test]
    fn on_linux_two_ordinary_holds_are_live_at_once() {
        assert!(ORDINARY_HOLDS_SHARE);
        let key = PrivateKey::new("overlap");
        let first = Lease::hold(key.as_str(), budget());
        let second = Lease::try_hold(key.as_str(), brief()).unwrap_or_else(|failure| {
            panic!("a second ordinary hold waited for the first: {failure}")
        });
        drop(first);
        drop(second);
    }

    /// The variable that makes a run of the cross-process case the child half
    /// of it, naming the hold the child takes.
    const CHILD_TAKES: &str = "NORN_ISOLATION_CASE_CHILD_TAKES";
    /// The variable carrying the parent's private key to the child.
    const CHILD_KEY: &str = "NORN_ISOLATION_CASE_CHILD_KEY";

    /// **The bar across processes.** Holds in two processes share and exclude
    /// as holds in one do: an ordinary hold lands beside another process's
    /// ordinary hold on Linux and waits for it elsewhere, and an exclusive hold
    /// waits for any other process's hold and is waited for.
    ///
    /// The forbidden shape is a sharing that only threads see — a lock kept in
    /// process memory, which would leave every binary a runner starts
    /// excluding nothing in the others — or an exclusion that only threads
    /// see. The child is this binary run again on this one case; it takes one
    /// hold under a short wall while the parent holds the other for the
    /// child's whole life, and says whether it landed.
    #[test]
    #[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Harness scaffolding: starting this binary as the other process.
    fn holds_in_two_processes_share_and_exclude_as_holds_in_one_do() {
        if let Some(takes) = std::env::var_os(CHILD_TAKES) {
            let key = std::env::var(CHILD_KEY).expect("the parent's key");
            let wall = Budget::new(Duration::from_millis(500), Duration::from_millis(500));
            let taken = match takes.to_str() {
                Some("ordinary") => Lease::try_hold(&key, wall),
                Some("exclusive") => Lease::try_hold_exclusive(&key, wall),
                other => panic!("an unknown hold {other:?}"),
            };
            println!(
                "child {}",
                if taken.is_ok() { "landed" } else { "excluded" }
            );
            return;
        }

        let key = PrivateKey::new("processes");
        let child = |takes: &str| -> String {
            let output = std::process::Command::new(
                std::env::current_exe().expect("this test binary"),
            )
            .args([
                "--exact",
                "isolation::tests::holds_in_two_processes_share_and_exclude_as_holds_in_one_do",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_TAKES, takes)
            .env(CHILD_KEY, key.as_str())
            .env(ISOLATION_ROOT, root())
            .output()
            .expect("the child runs");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(output.status.success(), "the child failed: {stdout}");
            ["child landed", "child excluded"]
                .into_iter()
                .find(|said| stdout.contains(said))
                .unwrap_or_else(|| panic!("the child said neither: {stdout}"))
                .to_owned()
        };

        let ordinary = Lease::hold(key.as_str(), budget());
        assert_eq!(
            child("ordinary"),
            if ORDINARY_HOLDS_SHARE {
                "child landed"
            } else {
                "child excluded"
            },
            "an ordinary hold in another process, beside this one's ordinary hold"
        );
        assert_eq!(
            child("exclusive"),
            "child excluded",
            "an exclusive hold in another process, beside this one's ordinary hold"
        );
        drop(ordinary);

        let exclusive = Lease::hold_exclusive(key.as_str(), budget());
        assert_eq!(
            child("ordinary"),
            "child excluded",
            "an ordinary hold in another process, beside this one's exclusive hold"
        );
        drop(exclusive);
    }

    /// **The bar on the turnstile.** A stream of ordinary holders that overlap
    /// one another, so the lease is never free of them, does not keep an
    /// exclusive waiter out: it lands inside its wall, and while it holds, no
    /// ordinary holder does.
    ///
    /// The forbidden shape is the bare shared lock, which admits a new shared
    /// holder while an exclusive one waits. Under it the waiter here only ever
    /// finds the lease held, and ends at its wall naming holders that were
    /// working the whole time.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_stream_of_ordinary_holders_does_not_keep_an_exclusive_waiter_out() {
        let key = PrivateKey::new("stream");
        let stop = Arc::new(AtomicBool::new(false));
        let live = Arc::new(AtomicUsize::new(0));
        let landed = Arc::new(AtomicUsize::new(0));
        let stream: Vec<_> = (0..4u64)
            .map(|nth| {
                let (key, stop, live, landed) = (
                    key.as_str().to_owned(),
                    Arc::clone(&stop),
                    Arc::clone(&live),
                    Arc::clone(&landed),
                );
                thread::spawn(move || {
                    // Staggered, so the four windows overlap and the lease is
                    // never free of all of them at once.
                    thread::sleep(Duration::from_millis(10 * nth));
                    while !stop.load(Ordering::SeqCst) {
                        let lease = Lease::hold(&key, budget());
                        live.fetch_add(1, Ordering::SeqCst);
                        landed.fetch_add(1, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(40));
                        live.fetch_sub(1, Ordering::SeqCst);
                        drop(lease);
                    }
                })
            })
            .collect();

        wait_until("the stream to be running", budget(), || {
            if landed.load(Ordering::SeqCst) >= 8 {
                Observed::Met(())
            } else {
                Observed::pending("the stream has not got going")
            }
        })
        .unwrap_or_else(|failure| panic!("{failure}"));

        let wall = Budget::new(Duration::from_secs(5), Duration::from_millis(500));
        let exclusive = Lease::try_hold_exclusive(key.as_str(), wall);
        // Read while the exclusive hold, if it landed, is still held.
        let beside = live.load(Ordering::SeqCst);
        let outcome = exclusive.map(drop);
        stop.store(true, Ordering::SeqCst);
        for holder in stream {
            holder.join().expect("an ordinary holder");
        }
        outcome.unwrap_or_else(|failure| {
            panic!("a stream of ordinary holders kept the exclusive waiter out: {failure}")
        });
        assert_eq!(
            beside, 0,
            "{beside} ordinary holders were live while the exclusive hold held"
        );
    }

    /// A hold that cannot be taken inside its bound reports the holder rather
    /// than a bare timeout, and reports it as the wait's own failure, whichever
    /// kind of hold is waiting on whichever kind.
    ///
    /// The bound it is reported against is the one the caller passed, which is
    /// what says the acquisition carries a per-holder bound wider than a wall
    /// of milliseconds: an acquisition given fifty of them against a holder
    /// that never moves is the wall's failure and not the patience's.
    #[test]
    fn a_hold_that_never_comes_free_names_the_holder_it_waited_on() {
        let pairs = [
            (EXCLUSIVE, ORDINARY, "occupied-exclusively"),
            (ORDINARY, EXCLUSIVE, "occupied-ordinarily"),
        ];
        for ((holding_kind, holding), (waiting_kind, _), label) in pairs {
            let key = PrivateKey::new(label);
            let occupier = holding(key.as_str(), budget());

            let failure = if waiting_kind == EXCLUSIVE.0 {
                Lease::try_hold_exclusive(key.as_str(), brief())
            } else {
                Lease::try_hold(key.as_str(), brief())
            }
            .expect_err("a lease another holder excludes");

            assert_eq!(
                failure.budget.work(),
                brief().work(),
                "{waiting_kind} behind {holding_kind}: the failure is reported against {:?} \
                 rather than the {:?} wall the caller passed, so this acquisition ran under a \
                 per-holder bound of its own: {failure}",
                failure.budget.work(),
                brief().work()
            );

            let rendered = failure.to_string();
            assert!(
                rendered.contains(&format!("pid {}", std::process::id())),
                "{waiting_kind} behind {holding_kind}: the failure does not name the holding \
                 process: {rendered}"
            );
            assert!(
                rendered.contains(key.as_str()),
                "{waiting_kind} behind {holding_kind}: the failure does not name the lease it \
                 waited for: {rendered}"
            );
            drop(occupier);
        }
    }

    /// A waiter says who it is queued behind while it waits, and says when it
    /// got the lease and how long that took; a hold that never waits says
    /// nothing.
    ///
    /// The forbidden shape is the silent queue: a child whose own deadline
    /// counts its time behind a long holder and ends with an empty stderr that
    /// gives the operator nothing to go look at.
    #[test]
    fn a_waiter_names_its_holder_before_it_acquires_and_says_how_long_it_waited() {
        let key = PrivateKey::new("noted");
        let notes = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = |notes: &Arc<std::sync::Mutex<Vec<String>>>| {
            let notes = Arc::clone(notes);
            move |line: &str| notes.lock().expect("notes").push(line.to_owned())
        };

        let free = Lease::try_hold_noting(
            key.as_str(),
            Mode::Exclusive,
            budget(),
            Patience::Standing,
            sink(&notes),
        )
        .expect("a free lease");
        assert!(
            notes.lock().expect("notes").is_empty(),
            "a hold that did not wait wrote {:?}",
            notes.lock().expect("notes")
        );

        let waiter = {
            let (name, notes) = (key.as_str().to_owned(), Arc::clone(&notes));
            thread::spawn(move || {
                let lease = Lease::try_hold_noting(
                    &name,
                    Mode::ORDINARY,
                    budget(),
                    Patience::Standing,
                    sink(&notes),
                )
                .expect("the lease once it is let go");
                drop(lease);
            })
        };

        wait_until("the waiter's holder line", budget(), || {
            let seen = notes.lock().expect("notes").clone();
            if seen.is_empty() {
                Observed::pending("the waiter has said nothing")
            } else {
                Observed::Met(seen)
            }
        })
        .unwrap_or_else(|failure| panic!("{failure}"));
        let before = notes.lock().expect("notes").clone();
        assert_eq!(before.len(), 1, "one line while waiting: {before:?}");
        assert!(
            before[0].contains(&format!("pid {}", std::process::id()))
                && before[0].contains(key.as_str())
                && before[0].contains("exclusively")
                && before[0].contains("since unix"),
            "the waiting line does not name the holder: {}",
            before[0]
        );

        drop(free);
        waiter.join().expect("the waiter");
        let after = notes.lock().expect("notes").clone();
        assert_eq!(
            after.len(),
            2,
            "a waiting line and an acquired line: {after:?}"
        );
        assert!(
            after[1].contains("acquired")
                && after[1].contains(key.as_str())
                && after[1].contains("waited"),
            "the acquired line does not say it waited: {}",
            after[1]
        );
    }

    /// Two keys are two leases: holding one leaves the other free, so a second
    /// isolation concern joining here does not queue behind the watcher.
    #[test]
    fn two_keys_are_two_leases() {
        let (one, two) = (PrivateKey::new("one"), PrivateKey::new("two"));
        let watcher = Lease::hold_exclusive(one.as_str(), budget());
        let other = Lease::try_hold_exclusive(two.as_str(), brief())
            .unwrap_or_else(|failure| panic!("one key's holder excluded another's: {failure}"));
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
        for (kind, take) in [ORDINARY, EXCLUSIVE] {
            let key = PrivateKey::new("released");
            let holders = key.files().holders;

            let held = take(key.as_str(), budget());
            assert!(
                standing(&holders)
                    .oldest
                    .contains(&format!("pid {}", std::process::id())),
                "{kind} does not record its holder: {:?}",
                standing(&holders)
            );
            drop(held);

            let after = standing(&holders);
            assert_eq!(
                after,
                Standing {
                    oldest: UNRECORDED.to_owned(),
                    beside: 0
                },
                "the lease still names a holder after {kind} was let go"
            );
        }
    }

    /// **The bar on a holder that died.** A record whose holder is gone is not
    /// named, and the read that finds it removes it.
    ///
    /// The forbidden shape is the record a killed process leaves behind being
    /// read as a holder: the waiter's failure would name a pid that holds
    /// nothing, and the record would stand for every run after. The dead
    /// record here is the oldest, so a reader that named it would name it
    /// first.
    #[test]
    #[allow(clippy::disallowed_methods)] // Writing this module's own scratch lease record.
    fn a_record_whose_holder_died_is_neither_named_nor_left_standing() {
        let key = PrivateKey::new("died");
        let holders = key.files().holders;
        let occupier = Lease::hold_exclusive(key.as_str(), budget());
        let dead = holders.join(format!("{:020}-1-0.holder", 0));
        std::fs::write(&dead, "pid 1 holding a lease exclusively since unix 0")
            .expect("a record nobody holds");

        let read = standing(&holders);
        assert!(
            read.oldest.contains(&format!("pid {}", std::process::id())) && read.beside == 0,
            "a record nobody holds was read as a holder: {read:?}"
        );
        assert!(
            !std::fs::exists(&dead).expect("the holders directory"),
            "a record nobody holds was left standing"
        );
        drop(occupier);
    }

    /// **The bar on which holder a waiter names.** Behind several holders a
    /// waiter names the oldest and counts the rest; when the oldest lets go,
    /// the next oldest is named.
    ///
    /// The forbidden shape is naming whichever record the directory listed
    /// first, or the newest: under either, a stuck holder among working ones is
    /// a name that keeps changing, the patience keeps re-arming, and the stuck
    /// holder is never named.
    #[cfg(target_os = "linux")]
    #[test]
    fn behind_several_holders_a_waiter_names_the_oldest() {
        let key = PrivateKey::new("several");
        let holders = key.files().holders;
        let first = Lease::hold(key.as_str(), budget());
        let second = Lease::hold(key.as_str(), budget());
        for (lease, says) in [(&first, "the first holder"), (&second, "the second holder")] {
            let record = lease.record.as_ref().expect("a published record");
            write_record(&record.file, says).expect("a record");
        }

        assert_eq!(
            standing(&holders),
            Standing {
                oldest: "the first holder".to_owned(),
                beside: 1
            }
        );
        drop(first);
        assert_eq!(
            standing(&holders),
            Standing {
                oldest: "the second holder".to_owned(),
                beside: 0
            }
        );
        drop(second);
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
        let occupier = Lease::hold_exclusive(key.as_str(), budget());

        let patience = Duration::from_millis(200);
        let wall = Budget::new(Duration::from_secs(10), Duration::from_millis(500));
        let failure =
            Lease::try_hold_under(key.as_str(), Mode::ORDINARY, wall, Patience::Of(patience))
                .expect_err("a lease whose holder never lets go");

        assert_stopped_holder_named(&failure, patience, wall);
        drop(occupier);
    }

    /// **The bar on a stuck holder in a crowd.** An exclusive waiter behind an
    /// ordinary holder that never lets go names it at the patience, though
    /// other ordinary holders keep arriving and leaving around it.
    ///
    /// The forbidden shape is a crowd that hides a stuck holder: a waiter that
    /// tracks the whole set, or the newest of it, re-arms on every arrival, and
    /// the stuck holder is reported only at the acquisition wall, against a
    /// bound that says the queue kept moving.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_stuck_ordinary_holder_among_working_ones_is_named_at_the_patience_bound() {
        let key = PrivateKey::new("stuck-in-a-crowd");
        let stuck = Lease::hold(key.as_str(), budget());
        let stop = Arc::new(AtomicBool::new(false));
        let crowd = {
            let (key, stop) = (key.as_str().to_owned(), Arc::clone(&stop));
            thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let lease = Lease::hold(&key, budget());
                    thread::sleep(Duration::from_millis(5));
                    drop(lease);
                }
            })
        };

        let patience = Duration::from_millis(300);
        let wall = Budget::new(Duration::from_secs(10), Duration::from_millis(500));
        let failure =
            Lease::try_hold_under(key.as_str(), Mode::Exclusive, wall, Patience::Of(patience))
                .expect_err("a lease whose oldest holder never lets go");
        stop.store(true, Ordering::SeqCst);
        crowd.join().expect("the crowd");

        assert_stopped_holder_named(&failure, patience, wall);
        assert!(
            failure.last_state.contains("shared"),
            "the failure does not name the ordinary holder that stopped: {failure}"
        );
        drop(stuck);
    }

    fn assert_stopped_holder_named(failure: &WaitFailure, patience: Duration, wall: Budget) {
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
    }

    /// **The bar on what a look charges the holder.** The span a record is read
    /// as unmoved is one both looks saw it stand through, and never the time a
    /// look spent descheduled around its own read.
    ///
    /// The forbidden shape is the span read off the clock after the read — the
    /// shape class-a-lease-changing-hands recorded. Under it a look that read the
    /// record and then waited out a scheduling slice charges that slice to the
    /// holder, and a record that stood for less than the patience is named as a
    /// holder that stopped. The looks here are driven with the clock readings a
    /// starved runner produces, so the case is the arithmetic and not the
    /// scheduler.
    #[test]
    fn a_look_that_is_descheduled_around_its_read_charges_the_holder_only_what_both_looks_saw() {
        let t0 = Instant::now();
        let at = |millis| t0 + Duration::from_millis(millis);
        let mut unmoved = Unmoved::default();

        // The first look that sees the record is slow after its read: the
        // record is only known to have stood from the reading that ends it.
        assert_eq!(unmoved.charge("pid 1", at(0), at(400)), Duration::ZERO);
        // A later look is slow after its read too: it is charged up to the
        // reading that starts it, and the 900ms the clock shows after is not
        // the record's.
        assert_eq!(
            unmoved.charge("pid 1", at(700), at(1_300)),
            Duration::from_millis(300)
        );
        // Different hands re-arm the span, whatever the one before stood for.
        assert_eq!(
            unmoved.charge("pid 2", at(1_400), at(1_401)),
            Duration::ZERO
        );
        assert_eq!(
            unmoved.charge("pid 2", at(1_901), at(1_902)),
            Duration::from_millis(500)
        );
    }

    /// **The bar on where a look reads the clock.** A look run through a read
    /// that sleeps around the record is charged no more than the span between
    /// the reads that saw that record, however long the sleeps are.
    ///
    /// The forbidden shapes are the orderings that put a sleep inside the span:
    /// the record read ahead of the clock reading that starts the look, which
    /// is the ordering class-a-lease-changing-hands recorded, or a span taken
    /// from the reading that starts the first look or to the one that ends the
    /// current look. Under any of them a record read twice within microseconds
    /// is charged a whole sleep, and a queue that keeps changing hands on a
    /// starved runner is named as a holder that stopped. Each record here is
    /// read once after a sleep and once ahead of one, so each wrong ordering
    /// has a sleep to charge.
    #[test]
    fn a_look_charges_the_holder_no_more_than_the_reads_that_saw_its_record() {
        let asleep = Duration::from_millis(150);
        let looks = [
            ("pid 1", asleep, Duration::ZERO),
            ("pid 1", Duration::ZERO, asleep),
            ("pid 2", asleep, Duration::ZERO),
            ("pid 2", Duration::ZERO, asleep),
        ];
        let mut unmoved = Unmoved::default();
        let mut first_read: Option<(&str, Instant)> = None;

        for (record, ahead, behind) in looks {
            let mut read_at = None;
            let (read, span) = unmoved.look(|| {
                thread::sleep(ahead);
                read_at = Some(Instant::now());
                thread::sleep(behind);
                record.to_owned()
            });
            let read_at = read_at.expect("the look ran its read");
            let first = match first_read {
                Some((named, at)) if named == record => at,
                _ => {
                    first_read = Some((record, read_at));
                    read_at
                }
            };
            assert_eq!(
                read, record,
                "the look handed back a record it did not read"
            );
            assert!(
                span <= read_at - first,
                "{record} was charged {span:?}, past the {:?} between the reads that saw it, so a \
                 look charged the holder for the {asleep:?} it slept around its own read",
                read_at - first
            );
        }
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
        let occupier = Lease::hold_exclusive(key.as_str(), budget());

        let patience = Duration::from_millis(500);
        let wall = Budget::new(Duration::from_secs(2), Duration::from_millis(500));

        // The queue changing hands, written where a waiter reads it: each pass
        // leaves a record naming a different holder than the last, written in
        // place over one handle and at one width, so the file never reads empty
        // between two hand-overs. A read torn across a digit carry can still
        // match another hand-over's record; a look that lands on one joins it
        // to the waiter's neighboring look at most, so the join is bounded by
        // the waiter's gap between two looks rather than by these windows.
        //
        // **What is measured is the widest window a record could have stood**,
        // and it is an upper bound: from a clock reading before the write that
        // made the record to one after the next write that landed. A record is
        // readable from no earlier than the first and gone by the second,
        // however long this thread was descheduled around either write. The
        // waiter's unmoved span is a lower bound, taken between two reads that
        // both saw one record, so it sits inside that record's window. The guard
        // below holding every window under the patience is therefore what makes
        // a patience failure the re-arm's and nothing else's — a starved runner
        // can widen a window, but only past the guard, never past the patience
        // alone.
        //
        // The last window has no next write to close it before the waiter
        // stops, so it closes at the failure the waiter observed: a stall the
        // stop flag interrupts is measured rather than lost with the write it
        // prevented.
        let stop = Arc::new(AtomicBool::new(false));
        let handovers = Arc::new(AtomicUsize::new(0));
        let widest_window = Arc::new(AtomicU64::new(0));
        let epoch = Instant::now();
        // Nanoseconds from `epoch` to the clock reading before the last write
        // that landed.
        let last_made = Arc::new(AtomicU64::new(0));
        let record_file = std::fs::OpenOptions::new()
            .write(true)
            .open(&occupier.record.as_ref().expect("a published record").path)
            .expect("the occupier's record the hands write to");
        let hands = {
            let (stop, handovers, widest_window, last_made) = (
                Arc::clone(&stop),
                Arc::clone(&handovers),
                Arc::clone(&widest_window),
                Arc::clone(&last_made),
            );
            thread::spawn(move || {
                let mut made: Option<Instant> = None;
                let mut nth = 0u64;
                while !stop.load(Ordering::SeqCst) {
                    nth += 1;
                    let record = format!("pid {nth:010} holding a lease since unix 0");
                    let before = Instant::now();
                    if write_record(&record_file, &record).is_ok() {
                        let landed = Instant::now();
                        if let Some(made) = made {
                            widest_window.fetch_max(
                                landed.duration_since(made).as_nanos() as u64,
                                Ordering::SeqCst,
                            );
                        }
                        made = Some(before);
                        last_made.store(
                            before.duration_since(epoch).as_nanos() as u64,
                            Ordering::SeqCst,
                        );
                        handovers.fetch_add(1, Ordering::SeqCst);
                    }
                    thread::sleep(Duration::from_millis(2));
                }
            })
        };

        // The waiter starts once the record is these hands' own, so every
        // record it reads is one whose window the thread above measured.
        // Started ahead of that, it would be reading the occupier's line for
        // however long the thread waits to be scheduled.
        let spawned = Instant::now();
        while handovers.load(Ordering::SeqCst) == 0 && spawned.elapsed() < patience {
            thread::sleep(Duration::from_millis(1));
        }
        if handovers.load(Ordering::SeqCst) == 0 {
            stop.store(true, Ordering::SeqCst);
            hands.join().expect("the hands that never took the record");
            panic!(
                "this case's own hands did not take the record inside the {patience:?} patience, \
                 so the waiter below would read the occupier's line as one that stopped moving"
            );
        }

        let failure =
            Lease::try_hold_under(key.as_str(), Mode::ORDINARY, wall, Patience::Of(patience))
                .expect_err("a lease no waiter is ever handed");
        let at_failure = epoch.elapsed();
        stop.store(true, Ordering::SeqCst);
        hands.join().expect("the hands the record kept changing to");

        let changed = handovers.load(Ordering::SeqCst);
        // A write that landed after the failure closed the window it followed,
        // so that window is already measured and the tail is empty.
        let tail =
            at_failure.saturating_sub(Duration::from_nanos(last_made.load(Ordering::SeqCst)));
        let widest = Duration::from_nanos(widest_window.load(Ordering::SeqCst)).max(tail);
        assert!(
            widest < patience,
            "a record this case's own hands left could have stood for {widest:?}, past the \
             {patience:?} patience, so what follows would judge the stall rather than the re-arm"
        );
        // The bound the wait ended on is judged before the hand-over count, so
        // a wait that ended early is named by the bound that ended it. A probe
        // overrun carries the wall's budget like the wall does, so it is told
        // apart by its kind, and its rendering is what the flake tripwire reads.
        assert_eq!(
            failure.kind,
            FailureKind::Elapsed,
            "a queue that changed hands {changed} times ended on a probe rather than a bound: \
             {failure}"
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
        assert!(
            changed as u32 > wall.work().as_millis() as u32 / patience.as_millis() as u32,
            "the record changed hands {changed} times, too few to outlive a {patience:?} patience \
             inside a {:?} wall",
            wall.work()
        );
        drop(occupier);
    }

    /// **The bar on a holder record that cannot be written.** A failed write
    /// leaves the holder's record saying nothing, not saying what it said
    /// before.
    ///
    /// The forbidden shape is the swallowed error that leaves the previous
    /// record standing. Under it the next waiter's timeout names what the file
    /// last said rather than who holds it — and a wrong answer is worse than
    /// none.
    #[test]
    fn a_holder_record_that_cannot_be_written_leaves_no_record_at_all() {
        let key = PrivateKey::new("unwritable-record");
        let holders = key.files().holders;

        let held = Lease::hold(key.as_str(), budget());
        let named = standing(&holders).oldest;
        assert!(
            named.contains(&format!("pid {}", std::process::id())),
            "a held lease does not record its holder: {named}"
        );

        let record = held.record.as_ref().expect("a published record");
        record_holder(&record.file, key.as_str(), Mode::ORDINARY, |_, _| {
            Err(std::io::Error::other("the record could not be written"))
        });

        assert_eq!(
            standing(&holders).oldest,
            UNRECORDED,
            "a record that could not be written left the previous one standing"
        );
        assert_eq!(
            record.file.metadata().expect("the record file").len(),
            0,
            "the record still carries bytes a waiter would read as a holder"
        );
        drop(held);
    }

    /// The acquisition wall is the shorter of the depth the census derives and
    /// the ceiling the lanes' own walls set, and it fires inside the job.
    #[test]
    fn the_acquisition_wall_fits_inside_the_job_it_has_to_fail_in() {
        let hold_window = Budget::new(Duration::from_secs(15), Duration::from_millis(250));
        let acquisition = acquisition_budget(hold_window);
        assert!(
            acquisition.work() <= ACQUISITION_WALL,
            "an acquisition gets {:?}, past the {ACQUISITION_WALL:?} ceiling",
            acquisition.work()
        );
        assert!(
            ACQUISITION_WALL < MEMORY_JOB_WALL,
            "the acquisition ceiling is at or past the {MEMORY_JOB_WALL:?} job wall it has to \
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

    #[test]
    fn acquisition_tolerates_a_pending_probe_slower_than_the_holders_probe_bound() {
        let hold_window = Budget::new(Duration::from_secs(15), Duration::from_millis(1));
        let mut probes = 0;
        let outcome = wait_until("a queued lease", acquisition_budget(hold_window), || {
            probes += 1;
            if probes == 1 {
                std::thread::sleep(Duration::from_millis(20));
                Observed::pending("held by another case")
            } else {
                Observed::Met(())
            }
        });
        assert!(outcome.is_ok(), "acquisition ended before its next look");
        assert_eq!(probes, 2);
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
        let files = LeaseFiles::under(Path::new("/somewhere"), REAL_WATCHER);
        assert_eq!(
            files.lock,
            Path::new("/somewhere").join(format!("{REAL_WATCHER}.lease"))
        );
        assert_eq!(
            files.turnstile,
            Path::new("/somewhere").join(format!("{REAL_WATCHER}.turnstile"))
        );
        assert_eq!(
            files.holders,
            Path::new("/somewhere").join(format!("{REAL_WATCHER}.holders"))
        );
    }
}
