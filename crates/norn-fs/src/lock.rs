//! The maintainer lock: one host per derived store, and nothing else.
//!
//! # What this lock carries, and what it does not
//!
//! One `flock`, in the derived directory it protects, admits at most one Norn
//! host as the **maintainer** of that store. Two maintainers of one store would
//! corrupt it and double every filesystem event it records, which is the entire
//! reason exclusion exists here. A vault root that two
//! [maintainership keys](crate::MaintainershipKey) reach has two derived stores
//! and two locks, and the hosts holding them are ordinary concurrent writers of
//! vault files to each other — on exactly the terms below.
//!
//! **It restricts nobody's access to vault files** — not another Norn's, not an
//! editor's, not a sync client's, not a human's. There is no on-disk mutation
//! lock and there will not be one: a vault is inherently multi-writer, so
//! correctness against concurrent writers is carried by the hash preconditions
//! in [`crate::write`], never by exclusion. A host serializes its *own*
//! mutations inside its own process.
//!
//! The inverse guarantee is worth stating as a bar rather than as a hope: **a
//! fully contended vault is fully readable**, and every operation in this crate
//! proves it by never taking the lock path as a parameter at all.
//!
//! # Acquisition is try-only
//!
//! [`try_acquire`] either takes the lock or names who holds it. There is no
//! deadline parameter and no waiting inside this crate. A caller with a reason
//! to wait — a host restarting while its predecessor tears down — owns that
//! retry and declares its own bound; a budget buried in a primitive is a bound
//! nobody at the call site chose.
//!
//! Contention is a normal outcome, never an error. Any *other* failure from the
//! lock call is a genuine environmental fault and travels as one: collapsing it
//! into synthesized contention would report a broken machine as a busy one.
//!
//! # Norn never unlinks a lock file
//!
//! Release is dropping the handle, and nothing else. A lock file is created once
//! and left in place forever, because removing it is what manufactures the
//! hazard below — and because a lock with a timeout is a lock somebody can
//! steal. The kernel releases a `flock` when the holding process dies, so a
//! lock still held is a process still alive; a wedged live holder is a health
//! finding, never something to take away.
//!
//! **What release waits for is the last descriptor closing, and a `fork`
//! postpones that.** A child forked while the lock is held carries a copy of the
//! descriptor until it reaches its `exec`, and the lock stays taken for as long
//! as that copy lives — so a released lock reads as held, by the process that
//! released it, for the length of somebody else's spawn. Norn spawns nothing
//! while holding a maintainership, which makes this a bar on what may be added
//! here rather than a hazard in the running system.
//!
//! # The dead-inode hazard, and the recheck that closes it
//!
//! An advisory lock follows the *file*, not the name. If something outside the
//! normal lifecycle unlinks the lock file and creates a new one at the same
//! path, an acquirer can end up holding an exclusive lock on the orphaned inode:
//! it believes it guards the path, while everything else opening that path finds
//! a different, unguarded file and meets no contention at all. Two "exclusive"
//! holders, each certain.
//!
//! So acquisition compares the held handle's `(device, inode)` against a fresh
//! stat of the path and, on a mismatch, drops the handle and takes the lock
//! again — bounded at [`LOCK_ATTEMPTS`]. One retry is the ordinary case.
//! Exhausting the bound is [`Refusal::LockFileReplaced`]: something is removing
//! the lock file in a loop, and saying so is more use than retrying forever.
//!
//! # The body is a diagnostic
//!
//! The winner stamps three fields — process id, Norn's version, and when it
//! started — behind a version number. They exist so an operator's refusal
//! message can name the incumbent, and for nothing else. **Nothing routes on
//! them**: reaching a host goes through the registry and its socket. The read is
//! tolerant in every direction — a body that is truncated, empty, from a newer
//! format, or not text at all yields [`Incumbent::Unknown`] rather than an
//! error, because failing to parse a diagnostic is not a reason to fail an
//! acquisition that already knows its answer.
//!
//! **Writing it is tolerant in the same direction.** A stamp that fails never
//! fails the acquisition: exclusivity was won by `flock` and confirmed by the
//! recheck before a byte was written, and a full data volume would otherwise
//! leave no host able to maintain any vault at exactly the moment maintenance is
//! what is needed. A body that was never written, or was written halfway, is one
//! every reader already reports as an unknown incumbent.
//!
//! **`flock` is a local-filesystem guarantee.** On NFS it is emulated through
//! the lock manager and on some configurations degrades to advisory-only or to
//! nothing at all. The lock lives in the machine's own data directory, which is
//! local by definition; put somewhere else it has the exclusion that server
//! offers rather than the one stated here.

use std::fmt;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::identity::{Identity, identity_of, name_identity};
use crate::refusal::{Refusal, environment};

/// How many times the lock is taken before the file at the lock name is
/// declared unlockable.
///
/// One retry is the normal case — the name was replaced while this caller was
/// between its open and its lock. A caller that loses that race this many times
/// in a row is racing something removing the lock file in a loop.
pub const LOCK_ATTEMPTS: usize = 64;

/// The version the lock body's format carries.
///
/// A reader that meets a number it does not know reports
/// [`Incumbent::Unknown`]: the body is a diagnostic, so a newer writer's shape
/// is a message this build cannot read rather than a state it must refuse.
///
/// In-crate: the body is written and read here and nothing outside routes on it,
/// so a number in another crate's hands could only be a second reader of a
/// diagnostic.
pub(crate) const BODY_VERSION: u32 = 1;

/// The version a winner stamps into the lock body.
const NORN_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The outcome of one attempt to become a vault's maintainer.
#[derive(Debug)]
pub enum Acquisition {
    /// This process is the maintainer for as long as the guard lives.
    Acquired(Maintainership),
    /// Somebody else is, and this is what their lock body said about them.
    Contended { incumbent: Incumbent },
}

/// Maintainership of one derived store, held for as long as this value lives.
///
/// Dropping it releases the lock. **It does not remove the lock file**, which is
/// what keeps the next acquirer's identity recheck a check of a stable name
/// rather than a race against this one's cleanup.
///
/// # What holding this stops guaranteeing, and when
///
/// The lock is on the *file*, and exclusivity is the file's. So a foreign actor
/// that unlinks the lock file and creates another at the same name **voids the
/// exclusivity of a maintainership already held**: this handle still locks the
/// file it took, and the next acquirer opens the new one and meets no contention.
/// Nothing here can prevent it — the same act is what the acquisition-time
/// recheck exists to survive, and after acquisition there is no moment to recheck
/// at that is not arbitrary.
///
/// What there is instead is [`Maintainership::still_current`], which a host asks
/// on its own health schedule. Its answer going false is a **health finding about
/// the machine**, never grounds to take a lock away from anybody: a lock still
/// held is a process still alive.
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns the lock handle.
pub struct Maintainership {
    /// The lock file's handle. Dropping it releases the lock, which is why it is
    /// held rather than discarded; it is also what
    /// [`Maintainership::still_current`] asks about the file it holds.
    file: std::fs::File,
    path: PathBuf,
    identity: Identity,
}

impl Maintainership {
    /// The lock file this maintainership is held on.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The `(device, inode)` the lock was confirmed to be held on.
    ///
    /// Recorded at acquisition, after the recheck agreed, so it is the identity
    /// the path resolved to at the moment exclusivity became true.
    pub fn identity(&self) -> Identity {
        self.identity
    }

    /// Whether the lock name still resolves to the file this maintainership is
    /// held on.
    ///
    /// `false` means the exclusivity is gone: something removed or replaced the
    /// lock file, so a second host acquiring at that name meets no contention and
    /// two maintainers can believe they are the one. Norn does not do this to
    /// itself — it never unlinks a lock file — so a `false` here is a report about
    /// something else on the machine.
    ///
    /// **This is a health question and never a decision to steal.** There is
    /// nothing to take: the other holder is holding a real lock on a real file.
    /// What a host does with the answer is surface it.
    pub fn still_current(&self) -> bool {
        let held = self.file.metadata().map(|metadata| identity_of(&metadata));
        held.is_ok_and(|held| held == self.identity)
            && name_identity(&self.path).ok().flatten() == Some(self.identity)
    }
}

impl fmt::Debug for Maintainership {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Maintainership")
            .field("path", &self.path)
            .field("identity", &self.identity)
            .finish()
    }
}

/// Who holds a contended lock, as far as its body says.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Incumbent {
    /// The body read cleanly and says this.
    Named {
        /// The holder's process id, as it reported it.
        pid: u32,
        /// The Norn version the holder is running.
        version: String,
        /// When the holder took the lock.
        started: SystemTime,
    },
    /// The body said nothing this build can read: empty, truncated, a version
    /// this build does not know, or not text. **Never an error** — the lock is
    /// held either way, and that is the answer the caller asked for.
    Unknown,
}

impl fmt::Display for Incumbent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Incumbent::Named { pid, version, .. } => {
                write!(f, "process {pid} running norn {version}")
            }
            Incumbent::Unknown => f.write_str("a process that did not identify itself"),
        }
    }
}

/// Take maintainership of the derived store whose lock file is at `path`, or
/// say who has it.
///
/// The parent directory is created if it is not there — the lock lives in the
/// derived directory it protects, which is Norn's own and which Norn makes. A
/// parent that cannot be created is an
/// [environmental refusal](Refusal::Environment) carrying the kind, so a missing
/// path is distinguishable from a denied one and both are distinguishable from
/// contention.
///
/// ```
/// use norn_fs::{Acquisition, try_acquire};
///
/// let directory = std::env::temp_dir().join(format!("norn-fs-doc-{}", std::process::id()));
/// let lock = directory.join("maintainer.lock");
///
/// let held = match try_acquire(&lock).expect("an acquisition") {
///     Acquisition::Acquired(held) => held,
///     Acquisition::Contended { incumbent } => panic!("held by {incumbent}"),
/// };
///
/// // A second attempt answers rather than waits.
/// assert!(matches!(
///     try_acquire(&lock).expect("an attempt"),
///     Acquisition::Contended { .. }
/// ));
///
/// // Release is dropping the guard. The lock file is never unlinked.
/// drop(held);
/// assert!(matches!(
///     try_acquire(&lock).expect("an attempt"),
///     Acquisition::Acquired(_)
/// ));
/// ```
///
/// **There is no bound to pass, because there is nothing here that waits.** The
/// deadline this does not take, spelled out so that gaining one fails:
///
/// ```compile_fail
/// use std::time::Duration;
///
/// use norn_fs::try_acquire;
///
/// let waited = try_acquire(&std::path::PathBuf::from("/tmp/x.lock"), Duration::from_secs(5));
/// ```
pub fn try_acquire(path: &Path) -> Result<Acquisition, Refusal> {
    try_acquire_where(path, |_| {}, stamp)
}

/// [`try_acquire`], with something allowed to happen between taking the lock
/// and rechecking the name, and with the stamping made to fail.
///
/// `disturb` is called with the attempt number at exactly the point the
/// dead-inode hazard opens: the lock is held and the name has not been asked
/// about yet. That is the only place a test can manufacture the hazard
/// deterministically — waiting for it to happen by chance is a race nobody can
/// arrange, and the defense would then be asserted rather than checked.
///
/// `stamp` is a parameter for the other half: a body this process cannot write is
/// what a full data volume looks like, and the claim is that it changes nothing
/// about the acquisition.
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns the lock handle.
fn try_acquire_where(
    path: &Path,
    mut disturb: impl FnMut(usize),
    stamp: impl Fn(&std::fs::File, &Path) -> Result<(), Refusal>,
) -> Result<Acquisition, Refusal> {
    prepare_directory(path)?;
    for attempt in 0..LOCK_ATTEMPTS {
        let mut file = open_lock_file(path)?;
        match file.try_lock() {
            Ok(()) => {}
            // Contention is the answer, not a failure.
            Err(std::fs::TryLockError::WouldBlock) => {
                return Ok(Acquisition::Contended {
                    // Through the handle that just met the contention, so the
                    // body read is the body of the file that refused the lock.
                    incumbent: read_body(&mut file),
                });
            }
            // Anything else is the machine, and it travels as itself rather
            // than as a busy vault.
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(environment("locking", path, &error));
            }
        }

        disturb(attempt);

        // The lock is on the file, so holding one says nothing until the name is
        // known to still resolve to it.
        let held = identity_of(
            &file
                .metadata()
                .map_err(|error| environment("reading the identity of", path, &error))?,
        );
        if name_identity(path)? == Some(held) {
            // Best-effort, and the discard is the contract: exclusivity is
            // already true and the body is a message for an operator.
            let _ = stamp(&file, path);
            return Ok(Acquisition::Acquired(Maintainership {
                file,
                path: path.to_path_buf(),
                identity: held,
            }));
        }
        // Dropping the handle releases the lock on the inode that is no longer
        // at this name, which is the only correct thing to do with it.
    }
    Err(Refusal::LockFileReplaced {
        path: path.to_path_buf(),
        attempts: LOCK_ATTEMPTS,
    })
}

/// Create the directory the lock file sits in.
#[allow(clippy::disallowed_methods)] // The vault filesystem seam: this crate owns the lock file.
fn prepare_directory(path: &Path) -> Result<(), Refusal> {
    let Some(directory) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(directory).map_err(|error| environment("creating", directory, &error))
}

/// Open the lock file, creating it if it is not there and never truncating it.
///
/// `O_NOFOLLOW` is what makes the lock the file rather than the name: a symbolic
/// link planted at the lock's own name is refused instead of followed to
/// somewhere nothing else guards.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The vault filesystem seam: this crate owns the lock file.
fn open_lock_file(path: &Path) -> Result<std::fs::File, Refusal> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| environment("opening", path, &error))
}

/// Write this process's three diagnostic fields into the lock body.
///
/// The body is replaced rather than appended to, and the file is truncated to
/// what was written: a previous holder's longer body would otherwise leave a
/// tail behind that reads as part of this one.
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns the lock file.
fn stamp(file: &std::fs::File, path: &Path) -> Result<(), Refusal> {
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);
    let body = format!(
        "version {BODY_VERSION}\npid {}\nnorn {NORN_VERSION}\nstarted {started}\n",
        std::process::id()
    );
    let mut file = file;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| environment("stamping", path, &error))?;
    file.write_all(body.as_bytes())
        .map_err(|error| environment("stamping", path, &error))?;
    file.set_len(body.len() as u64)
        .map_err(|error| environment("stamping", path, &error))?;
    file.flush()
        .map_err(|error| environment("stamping", path, &error))
}

/// Read who holds the lock, tolerantly, through the handle that met it.
///
/// **The handle rather than the name**, and for two reasons. It is the file that
/// refused the lock, so the body read describes the holder that was actually met
/// and not whatever the name has come to mean since. And it is the crate's one
/// remaining by-name open removed: an open of the lock's name would follow a
/// symbolic link planted at it, which is exactly what the acquiring open uses
/// `O_NOFOLLOW` to refuse.
///
/// Every failure is [`Incumbent::Unknown`]. The body is a diagnostic, and the
/// caller already knows the answer to the question it asked — turning an
/// unreadable message into an error would fail an acquisition over prose.
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns the lock file.
fn read_body(file: &mut std::fs::File) -> Incumbent {
    if file.seek(SeekFrom::Start(0)).is_err() {
        return Incumbent::Unknown;
    }
    let mut bytes = Vec::new();
    // A body is four short lines. A file grown past that is not one of ours,
    // and reading all of it would be reading whatever somebody put there.
    if Read::by_ref(file)
        .take(4096)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Incumbent::Unknown;
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Incumbent::Unknown;
    };
    parse_body(text)
}

/// The incumbent four `name value` lines describe, or [`Incumbent::Unknown`].
fn parse_body(text: &str) -> Incumbent {
    let mut version = None;
    let mut pid = None;
    let mut norn = None;
    let mut started = None;
    for line in text.lines() {
        match line.split_once(' ') {
            Some(("version", value)) => version = value.trim().parse::<u32>().ok(),
            Some(("pid", value)) => pid = value.trim().parse::<u32>().ok(),
            Some(("norn", value)) => norn = Some(value.trim().to_string()),
            Some(("started", value)) => started = value.trim().parse::<u64>().ok(),
            _ => {}
        }
    }
    match (version, pid, norn, started) {
        (Some(BODY_VERSION), Some(pid), Some(version), Some(started)) => Incumbent::Named {
            pid,
            version,
            started: UNIX_EPOCH + Duration::from_secs(started),
        },
        _ => Incumbent::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use norn_testkit::wait::{Budget, Observed, wait_until};

    /// What a take here waits: far above anything the mechanism should approach,
    /// so a failure is a lock that never comes free rather than a slow machine.
    fn budget() -> Budget {
        Budget::new(Duration::from_secs(30), Duration::from_secs(5))
    }

    /// The lock file a case works over: inside the norn data directory, keyed by
    /// the vault.
    fn lock_path(scratch: &Scratch) -> PathBuf {
        scratch.path("data/vaults/notes/maintainer.lock")
    }

    /// The guard, or a panic naming who has it. Callers of the real surface match
    /// on the outcome; a case that only ever wants the guard says so once.
    ///
    /// **The wait is what this binary costs, not slack in the lock.** Cases in
    /// this crate's own suite spawn child processes, and a child holds a copy of
    /// every descriptor this process had open when it was forked until it
    /// reaches its `exec` — a lock descriptor another case holds or has just
    /// dropped included. Release waits for the last descriptor to close, so for
    /// the length of somebody else's spawn a lock nothing in this process holds
    /// still reads as held, by this process. Every case here works over its own
    /// lock file and none of them contends through this helper — the cases whose
    /// subject is contention call [`try_acquire`] directly — so waiting is the
    /// honest shape, and it is what the try-only surface asks of a caller with a
    /// reason to wait: declare a bound. A lock that never comes free still
    /// fails.
    fn take(path: &Path) -> Maintainership {
        wait_until("the lock to be free", budget(), || {
            match try_acquire(path).expect("an acquisition") {
                Acquisition::Acquired(held) => Observed::Met(held),
                Acquisition::Contended { incumbent } => {
                    Observed::pending(format!("the lock is held by {incumbent}"))
                }
            }
        })
        .unwrap_or_else(|failure| panic!("{failure}"))
    }

    /// **The bar on the dead-inode hazard.** A lock file replaced between the
    /// lock and the recheck is not settled for: the acquirer drops it and
    /// converges on the file the name means now.
    ///
    /// The disturbance is injected at exactly the point the hazard opens, which
    /// is the only way to reach it deterministically. The forbidden shape is an
    /// acquire that returns as soon as `flock` succeeds: it hands back a lock on
    /// an orphaned inode, and everything else opening that path meets no
    /// contention at all.
    #[test]
    fn a_lock_file_replaced_under_the_acquirer_is_not_settled_for() {
        let scratch = Scratch::new("lock-aba");
        let path = lock_path(&scratch);
        prepare_directory(&path).expect("the lock's directory");

        let mut disturbed = Vec::new();
        let acquisition = try_acquire_where(
            &path,
            |attempt| {
                // A foreign actor — nothing in this crate ever removes a lock
                // file — takes the name away and puts a different file at it.
                // Once only, so the retry has a stable name to converge on.
                if attempt == 0 {
                    #[allow(clippy::disallowed_methods)]
                    // Harness scaffolding: playing the foreign actor.
                    {
                        std::fs::remove_file(&path).expect("removing the lock file");
                        std::fs::write(&path, b"").expect("a replacement lock file");
                    }
                    disturbed.push(attempt);
                }
            },
            stamp,
        )
        .expect("an acquisition");

        assert_eq!(disturbed, vec![0], "the hazard was not manufactured");
        let Acquisition::Acquired(held) = acquisition else {
            panic!("a free lock was reported as contended");
        };
        let current = name_identity(&path)
            .expect("the lock path")
            .expect("a lock file");
        assert_eq!(
            held.identity(),
            current,
            "the acquirer settled for an inode the name no longer resolves to"
        );
    }

    /// Foreign interference that never stops is a refusal that names the bound,
    /// rather than a loop nobody can see.
    ///
    /// The bound is asserted as the number it is. Asserting it against its own
    /// constant says only that the loop counts to whatever it counts to: a bound
    /// of two would satisfy that and would refuse an ordinary acquisition racing
    /// a busy machine.
    #[test]
    fn a_lock_file_replaced_on_every_attempt_refuses_at_the_bound() {
        let scratch = Scratch::new("lock-aba-forever");
        let path = lock_path(&scratch);
        prepare_directory(&path).expect("the lock's directory");

        let mut attempts = 0usize;
        let refusal = try_acquire_where(
            &path,
            |_| {
                attempts += 1;
                #[allow(clippy::disallowed_methods)]
                // Harness scaffolding: playing the foreign actor.
                {
                    let _ = std::fs::remove_file(&path);
                    std::fs::write(&path, b"").expect("a replacement lock file");
                }
            },
            stamp,
        )
        .expect_err("a lock file that is never stable");

        assert_eq!(attempts, 64, "the house bound is 64 attempts");
        assert_eq!(
            refusal,
            Refusal::LockFileReplaced {
                path: path.clone(),
                attempts: 64
            }
        );
    }

    /// **The bar on the stamp.** A body this process cannot write does not fail
    /// an acquisition that has already won.
    ///
    /// The forbidden shape is propagating it. Exclusivity was decided by `flock`
    /// and confirmed against the name before a byte was written, and the body is
    /// a message for an operator — so a full data volume would otherwise leave no
    /// host able to maintain any vault at exactly the moment maintenance is what
    /// is needed. The reader already reports an unwritten body as an unknown
    /// incumbent.
    #[test]
    fn a_stamp_that_cannot_be_written_still_yields_the_lock() {
        let scratch = Scratch::new("lock-stamp");
        let path = lock_path(&scratch);

        let acquisition = try_acquire_where(
            &path,
            |_| {},
            |_, path| {
                Err(environment(
                    "stamping",
                    path,
                    &std::io::Error::from_raw_os_error(libc::ENOSPC),
                ))
            },
        )
        .expect("an acquisition whose stamp failed");

        let Acquisition::Acquired(held) = acquisition else {
            panic!("a free lock was reported as contended");
        };
        assert_eq!(held.path(), path);
        // And the unwritten body reads as the tolerant answer rather than as an
        // error, which is what makes the discard safe.
        let mut file = open_lock_file(&path).expect("the lock file");
        assert_eq!(read_body(&mut file), Incumbent::Unknown);
    }

    /// The winner's three fields come back as the incumbent's, behind the
    /// version the format carries.
    #[test]
    fn a_stamped_body_names_the_holder() {
        let body = format!("version {BODY_VERSION}\npid 4821\nnorn 1.2.3\nstarted 1700000000\n");
        assert_eq!(
            parse_body(&body),
            Incumbent::Named {
                pid: 4821,
                version: "1.2.3".to_string(),
                started: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            }
        );
    }

    /// **The bar on tolerance.** Every body this build cannot read is an
    /// unknown incumbent, never an error.
    ///
    /// The forbidden shape is a strict parse: it fails an acquisition whose
    /// answer was already known, over prose that exists only for an operator to
    /// read.
    #[test]
    fn an_unreadable_body_is_an_unknown_incumbent() {
        for unreadable in [
            "",
            "\n\n",
            "not a body at all",
            "version 1\npid 4821\n",
            "version 1\nnorn 1.2.3\nstarted 1\n",
            "version 99\npid 4821\nnorn 1.2.3\nstarted 1\n",
            "version one\npid 4821\nnorn 1.2.3\nstarted 1\n",
            "version 1\npid -4821\nnorn 1.2.3\nstarted 1\n",
            "version 1\npid 4821\nnorn 1.2.3\nstarted tuesday\n",
            "{\"version\": 1, \"pid\": 4821}",
        ] {
            assert_eq!(
                parse_body(unreadable),
                Incumbent::Unknown,
                "{unreadable:?} parsed as a named incumbent"
            );
        }
    }

    /// A stamped body is read back through the acquisition that met it, so the
    /// format the winner writes is the format the reader parses. Two spellings of
    /// one format is the drift this forbids.
    #[test]
    fn the_body_a_winner_writes_is_the_body_a_reader_understands() {
        let scratch = Scratch::new("lock-round-trip");
        let path = lock_path(&scratch);
        let _held = take(&path);

        let Acquisition::Contended { incumbent } = try_acquire(&path).expect("an attempt") else {
            panic!("a held lock was taken twice");
        };
        let Incumbent::Named { pid, version, .. } = incumbent else {
            panic!("the stamped body did not read back as a named incumbent");
        };
        assert_eq!(pid, std::process::id());
        assert_eq!(version, NORN_VERSION);
    }

    /// A longer body replaced by a shorter one leaves no tail. Without the
    /// truncation the reader would see a previous holder's trailing lines and
    /// could take a field from each.
    #[test]
    fn stamping_leaves_no_tail_of_a_previous_body() {
        let scratch = Scratch::new("lock-tail");
        let path = lock_path(&scratch);
        prepare_directory(&path).expect("the lock's directory");
        #[allow(clippy::disallowed_methods)]
        // Harness scaffolding: a previous holder's longer body.
        std::fs::write(
            &path,
            b"version 1\npid 999999\nnorn 9.9.9\nstarted 1\nextra extra extra extra extra\n",
        )
        .expect("a previous body");

        let held = take(&path);
        #[allow(clippy::disallowed_methods)] // Asserting on the bytes the stamp left.
        let bytes = std::fs::read(held.path()).expect("the lock body");
        let text = String::from_utf8(bytes).expect("text");
        assert!(!text.contains("extra"), "{text:?}");
        assert!(!text.contains("999999"), "{text:?}");
    }

    /// **The bar on a maintainership whose file was taken away.** A held lock
    /// stops being exclusive when something outside the lifecycle unlinks the
    /// lock file and creates another at its name, and the guard says so.
    ///
    /// The forbidden shape is a guard that reports itself current on the strength
    /// of still existing. Two hosts then each hold a real lock on a different file
    /// and each believes it is the maintainer, which is the one condition the lock
    /// exists to prevent. Norn never does this to itself; what the answer is for is
    /// a host's own health check, and its only use is to be surfaced — there is
    /// nothing here to take away from anybody.
    #[test]
    fn a_maintainership_whose_lock_file_was_replaced_is_no_longer_current() {
        let scratch = Scratch::new("lock-voided");
        let path = lock_path(&scratch);
        let held = take(&path);
        assert!(
            held.still_current(),
            "a fresh maintainership is not current"
        );

        #[allow(clippy::disallowed_methods)] // Harness scaffolding: playing the foreign actor.
        {
            std::fs::remove_file(&path).expect("removing the lock file");
            std::fs::write(&path, b"").expect("a replacement lock file");
        }

        assert!(
            !held.still_current(),
            "a maintainership on an inode the name no longer resolves to called itself current"
        );
        // And the proof that the exclusivity really is gone: the replacement is
        // free to a second acquirer while this one still holds its file.
        let _second = take(&path);
    }
}
