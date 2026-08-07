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
//! window that watcher is live, and the number of live watchers on the machine
//! is one however many binaries the runner started.
//!
//! A second isolation concern joins here rather than bringing a second
//! mechanism: [`Lease::hold`] is keyed, and a key is what a new concern
//! declares.
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
//! # Acquisition is a budgeted wait, and queueing is the expected state
//!
//! Taking a lease is [`crate::wait`]'s wait like any other: a bound the caller
//! declares, and a failure naming the holder it was waiting on. The bound is
//! not sized for the busy machine — queueing behind every other holder is what
//! the lease is *for*, and a caller sizes its bound with
//! [`crate::wait::Budget::dominating`] over the holders that can queue ahead of
//! it. What the bound catches is a holder that is stuck rather than working.

use std::fs::TryLockError;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::wait::{Budget, Observed, WaitFailure, wait_until};

/// The lease a case holds while it owns a real platform watcher.
///
/// Every such case across the workspace names this one key, because the
/// service they contend for is one service. A suite that watches a real
/// filesystem and does not hold it is a suite that starves the others.
pub const REAL_WATCHER: &str = "real-watcher";

/// The environment variable naming where the leases live.
///
/// A runner that needs them somewhere other than the system temporary
/// directory — a filesystem that supports locking, or a directory of its own so
/// that two independent runs on one machine do not queue against each other —
/// sets this.
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
    #[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The lease is this crate's own machine-local state, and a file lock is what makes it cross-process.
    pub fn try_hold(key: &str, budget: Budget) -> Result<Self, WaitFailure> {
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

        wait_until(&format!("the {key} lease"), budget, || {
            match file.try_lock() {
                Ok(()) => Observed::Met(()),
                Err(TryLockError::WouldBlock) => {
                    Observed::pending(format!("held by {}", holder(&path)))
                }
                Err(TryLockError::Error(error)) => {
                    panic!("locking the lease file {}: {error}", path.display())
                }
            }
        })?;

        record_holder(&file, key);
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
    fn drop(&mut self) {
        // Closing the file would release the lock on its own; unlocking first
        // says so where a reader is looking for the release.
        let _ = self.file.unlock();
    }
}

/// Write who holds the lease, for whoever is waiting on it.
///
/// The record is diagnostic only: a waiter reads it while the holder writes
/// it, so a torn read is possible and costs a confusing line in a failure that
/// was going to name the wait's own bound anyway. What it buys is a timeout
/// that names a process a reader can go look at.
#[allow(clippy::disallowed_types)] // This crate's own lease handle; see `try_hold`.
fn record_holder(mut file: &std::fs::File, key: &str) {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let record = format!(
        "pid {} holding {key} since unix {since}",
        std::process::id()
    );
    let written = file
        .seek(SeekFrom::Start(0))
        .and_then(|_| file.write_all(record.as_bytes()))
        .and_then(|()| file.set_len(record.len() as u64));
    let _ = written;
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;

    use super::*;

    /// A key nothing else on the machine holds, so these cases queue behind
    /// each other and never behind a real watcher a sibling binary is running.
    fn private_key(label: &str) -> String {
        format!("test-{label}-{}", std::process::id())
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
        let key = private_key("exclusion");
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
    #[test]
    fn a_hold_that_never_comes_free_names_the_holder_it_waited_on() {
        let key = private_key("occupied");
        let occupier = Lease::hold(&key, budget());

        let failure = Lease::try_hold(
            &key,
            Budget::new(Duration::from_millis(50), Duration::from_millis(500)),
        )
        .expect_err("a lease another holder has");

        let rendered = failure.to_string();
        assert!(
            rendered.contains(&format!("pid {}", std::process::id())),
            "the failure does not name the holding process: {rendered}"
        );
        assert!(
            rendered.contains(&key),
            "the failure does not name the lease it waited for: {rendered}"
        );
        drop(occupier);
    }

    /// Two keys are two leases: holding one leaves the other free, so a second
    /// isolation concern joining here does not queue behind the watcher.
    #[test]
    fn two_keys_are_two_leases() {
        let watcher = Lease::hold(&private_key("one"), budget());
        let other = Lease::hold(&private_key("two"), budget());
        drop(other);
        drop(watcher);
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
