//! The maintainer lock: one host per vault's derived state, and nothing else.
//!
//! # What this lock carries, and what it does not
//!
//! One `flock` per vault admits at most one Norn host as the **maintainer** of
//! that vault's derived state. Two maintainers would corrupt one derived store
//! and double every filesystem event, which is the entire reason exclusion
//! exists here.
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
//! **`flock` is a local-filesystem guarantee.** On NFS it is emulated through
//! the lock manager and on some configurations degrades to advisory-only or to
//! nothing at all. The lock lives in the machine's own data directory, which is
//! local by definition; put somewhere else it has the exclusion that server
//! offers rather than the one stated here.

use std::fmt;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::identity::{Identity, identity_of};
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
pub const BODY_VERSION: u32 = 1;

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

impl Acquisition {
    /// The guard, or `None` when the lock was contended.
    pub fn acquired(self) -> Option<Maintainership> {
        match self {
            Acquisition::Acquired(held) => Some(held),
            Acquisition::Contended { .. } => None,
        }
    }
}

/// Maintainership of one vault, held for as long as this value lives.
///
/// Dropping it releases the lock. **It does not remove the lock file**, which is
/// what keeps the next acquirer's identity recheck a check of a stable name
/// rather than a race against this one's cleanup.
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns the lock handle.
pub struct Maintainership {
    /// The lock file's handle. Dropping it releases the lock, which is why it is
    /// held rather than discarded, and why nothing reads it.
    _file: std::fs::File,
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

/// Take maintainership of the vault whose lock file is at `path`, or say who
/// has it.
///
/// The parent directory is created if it is not there — the lock lives in
/// Norn's own per-vault data directory, which Norn makes. A parent that cannot
/// be created is an [environmental refusal](Refusal::Environment) carrying the
/// kind, so a missing path is distinguishable from a denied one and both are
/// distinguishable from contention.
pub fn try_acquire(path: &Path) -> Result<Acquisition, Refusal> {
    try_acquire_where(path, |_| {})
}

/// [`try_acquire`], with something allowed to happen between taking the lock
/// and rechecking the name.
///
/// `disturb` is called with the attempt number at exactly the point the
/// dead-inode hazard opens: the lock is held and the name has not been asked
/// about yet. That is the only place a test can manufacture the hazard
/// deterministically — waiting for it to happen by chance is a race nobody can
/// arrange, and the defense would then be asserted rather than checked.
fn try_acquire_where(path: &Path, mut disturb: impl FnMut(usize)) -> Result<Acquisition, Refusal> {
    prepare_directory(path)?;
    for attempt in 0..LOCK_ATTEMPTS {
        let file = open_lock_file(path)?;
        match file.try_lock() {
            Ok(()) => {}
            // Contention is the answer, not a failure.
            Err(std::fs::TryLockError::WouldBlock) => {
                return Ok(Acquisition::Contended {
                    incumbent: read_body(path),
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
            stamp(&file, path)?;
            return Ok(Acquisition::Acquired(Maintainership {
                _file: file,
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

/// The identity `path` resolves to now, or `None` when it resolves to nothing.
#[allow(clippy::disallowed_methods)] // The vault filesystem seam: this crate owns vault stat.
fn name_identity(path: &Path) -> Result<Option<Identity>, Refusal> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(Some(identity_of(&metadata))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(environment("reading the identity of", path, &error)),
    }
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

/// Read who holds the lock at `path`, tolerantly.
///
/// Every failure is [`Incumbent::Unknown`]. The body is a diagnostic, and the
/// caller already knows the answer to the question it asked — turning an
/// unreadable message into an error would fail an acquisition over prose.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The vault filesystem seam: this crate owns the lock file.
fn read_body(path: &Path) -> Incumbent {
    let Ok(mut file) = std::fs::OpenOptions::new().read(true).open(path) else {
        return Incumbent::Unknown;
    };
    let mut bytes = Vec::new();
    // A body is four short lines. A file grown past that is not one of ours,
    // and reading all of it would be reading whatever somebody put there.
    if Read::by_ref(&mut file)
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
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static SERIAL: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        #[allow(clippy::disallowed_methods)] // Harness scaffolding: the directory this test works over.
        fn new(label: &str) -> Scratch {
            let root = std::env::temp_dir().join(format!(
                "norn-fs-lock-{label}-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("a scratch directory");
            Scratch(root)
        }

        fn lock_path(&self) -> PathBuf {
            self.0.join("vaults/notes/maintainer.lock")
        }
    }

    impl Drop for Scratch {
        #[allow(clippy::disallowed_methods)] // Harness scaffolding: removing the tree this test made.
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
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
        let scratch = Scratch::new("aba");
        let path = scratch.lock_path();
        prepare_directory(&path).expect("the lock's directory");

        let mut disturbed = Vec::new();
        let acquisition = try_acquire_where(&path, |attempt| {
            // A foreign actor — nothing in this crate ever removes a lock file —
            // takes the name away and puts a different file at it. Once only, so
            // the retry has a stable name to converge on.
            if attempt == 0 {
                #[allow(clippy::disallowed_methods)]
                // Harness scaffolding: playing the foreign actor.
                {
                    std::fs::remove_file(&path).expect("removing the lock file");
                    std::fs::write(&path, b"").expect("a replacement lock file");
                }
                disturbed.push(attempt);
            }
        })
        .expect("an acquisition");

        assert_eq!(disturbed, vec![0], "the hazard was not manufactured");
        let held = acquisition.acquired().expect("the lock");
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
    #[test]
    fn a_lock_file_replaced_on_every_attempt_refuses_at_the_bound() {
        let scratch = Scratch::new("aba-forever");
        let path = scratch.lock_path();
        prepare_directory(&path).expect("the lock's directory");

        let mut attempts = 0usize;
        let refusal = try_acquire_where(&path, |_| {
            attempts += 1;
            #[allow(clippy::disallowed_methods)] // Harness scaffolding: playing the foreign actor.
            {
                let _ = std::fs::remove_file(&path);
                std::fs::write(&path, b"").expect("a replacement lock file");
            }
        })
        .expect_err("a lock file that is never stable");

        assert_eq!(attempts, LOCK_ATTEMPTS);
        assert_eq!(
            refusal,
            Refusal::LockFileReplaced {
                path: path.clone(),
                attempts: LOCK_ATTEMPTS
            }
        );
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

    /// A stamped body is read back through the file, so the format the winner
    /// writes is the format the reader parses. Two spellings of one format is
    /// the drift this forbids.
    #[test]
    fn the_body_a_winner_writes_is_the_body_a_reader_understands() {
        let scratch = Scratch::new("round-trip");
        let path = scratch.lock_path();
        let held = try_acquire(&path)
            .expect("an acquisition")
            .acquired()
            .expect("the lock");

        let Incumbent::Named { pid, version, .. } = read_body(held.path()) else {
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
        let scratch = Scratch::new("tail");
        let path = scratch.lock_path();
        prepare_directory(&path).expect("the lock's directory");
        #[allow(clippy::disallowed_methods)]
        // Harness scaffolding: a previous holder's longer body.
        std::fs::write(
            &path,
            b"version 1\npid 999999\nnorn 9.9.9\nstarted 1\nextra extra extra extra extra\n",
        )
        .expect("a previous body");

        let held = try_acquire(&path)
            .expect("an acquisition")
            .acquired()
            .expect("the lock");
        #[allow(clippy::disallowed_methods)] // Asserting on the bytes the stamp left.
        let bytes = std::fs::read(held.path()).expect("the lock body");
        let text = String::from_utf8(bytes).expect("text");
        assert!(!text.contains("extra"), "{text:?}");
        assert!(!text.contains("999999"), "{text:?}");
    }
}
