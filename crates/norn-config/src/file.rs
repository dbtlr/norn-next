//! The config directory's bytes: how they are read, locked and replaced.
//!
//! This module is the crate's whole filesystem effect, which is why the
//! use-site allows for the workspace `std::fs` rule are all here. Nothing
//! above it opens a handle.
//!
//! # The write protocol
//!
//! A machine-local file is replaced, never edited in place: bytes land in a
//! temporary file in the same directory, that file is fsynced, and a rename
//! puts it at the real name. A rename within a directory is atomic, so a
//! reader arriving at any moment sees either the whole previous file or the
//! whole new one — never a prefix of either, and never the temporary name.
//! The parent directory is fsynced afterwards, because the rename is what has
//! to survive a power cut and the rename lives in the directory rather than in
//! the file.
//!
//! # The lock, and why it is its own file
//!
//! A read-modify-write holds an exclusive `flock` from before the read until
//! after the rename, which is what makes a lost update impossible rather than
//! unlikely. The lock is taken on a **separate `.lock` file** that nothing
//! ever renames. Locking the target itself would not hold: a lock follows the
//! open file description, so a writer that renames a new file over the target
//! leaves the next waiter holding a lock on an inode that is no longer at that
//! path, reading bytes that are no longer current. The lock file is created
//! once and left in place; it holds no content.
//!
//! Waits are unbounded. A blocked writer is waiting on another writer's
//! read-modify-write, which is bounded by a small file's rewrite, and a
//! timeout here would turn "somebody else is mid-write" into a failure a
//! caller has to handle for no gain.

use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{ConfigError, io};

/// The mode a file holding secrets is created with, and the mode a reader of
/// one demands. Owner read and write, nothing else.
pub(crate) const PRIVATE_MODE: u32 = 0o600;

/// The mode a file holding no secret is created with. The registry names vault
/// roots, which is machine layout rather than a credential.
pub(crate) const SHARED_MODE: u32 = 0o644;

/// The mode the config and data directories are created with. Owner only,
/// because the config directory holds the token file and a directory the group
/// can traverse is a token file the group can reach whatever its own mode is.
const DIRECTORY_MODE: u32 = 0o700;

/// The mode bits that put a file outside its owner's reach.
const BEYOND_OWNER: u32 = 0o077;

/// Distinguishes two temporary files taken in the same process.
static SERIAL: AtomicU64 = AtomicU64::new(0);

/// The bytes at `path`, or `None` when nothing is there.
///
/// A dangling symlink is not nothing: opening one fails the same way an absent
/// file does, so the absent case is confirmed against the link itself before
/// it is reported.
pub(crate) fn read(path: &Path) -> Result<Option<Vec<u8>>, ConfigError> {
    match open_existing(path, false)? {
        Some(mut file) => {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|error| io("reading", path, error))?;
            Ok(Some(bytes))
        }
        None => Ok(None),
    }
}

/// The same, for a file holding secrets: the mode is judged from the open
/// handle before a byte is read, so what is checked is what is read.
pub(crate) fn read_private(path: &Path) -> Result<Option<Vec<u8>>, ConfigError> {
    match open_existing(path, true)? {
        Some(mut file) => {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|error| io("reading", path, error))?;
            Ok(Some(bytes))
        }
        None => Ok(None),
    }
}

#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Config-directory bytes: this crate owns them.
fn open_existing(path: &Path, private: bool) -> Result<Option<std::fs::File>, ConfigError> {
    let file = match std::fs::OpenOptions::new().read(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // The open failed because nothing is at the end of the path. A
            // link with nothing at its end fails identically, and the two mean
            // opposite things, so the link itself is asked about.
            //
            // The question is asked of the link and not of the path: a file
            // that arrived between the open and this call answers here as a
            // regular file, and reporting a dangling link for it would turn a
            // writer's rename into a refusal. Absent is the honest reading —
            // at the instant of the open, nothing was there.
            return match std::fs::symlink_metadata(path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    Err(ConfigError::DanglingSymlink {
                        path: path.to_path_buf(),
                    })
                }
                _ => Ok(None),
            };
        }
        Err(error) => return Err(io("opening", path, error)),
    };
    if private {
        let mode = file
            .metadata()
            .map_err(|error| io("reading the mode of", path, error))?
            .permissions()
            .mode();
        if mode & BEYOND_OWNER != 0 {
            return Err(ConfigError::InsecurePermissions {
                path: path.to_path_buf(),
                mode: mode & 0o7777,
            });
        }
    }
    Ok(Some(file))
}

/// An exclusive lock over one machine-local file, held for as long as the
/// value lives.
#[allow(clippy::disallowed_types)] // Config-directory bytes: this crate owns them.
pub(crate) struct Lock {
    /// The `.lock` file's handle. Dropping it releases the lock, which is why
    /// it is held rather than discarded.
    _file: std::fs::File,
}

/// Take the exclusive lock guarding `path`, waiting for as long as it takes,
/// and prepare the directory `path` sits in.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Config-directory bytes: this crate owns them.
pub(crate) fn lock(path: &Path) -> Result<Lock, ConfigError> {
    let directory = parent_of(path)?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(DIRECTORY_MODE)
        .create(directory)
        .map_err(|error| io("creating", directory, error))?;

    let lock_path = lock_path(path)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(PRIVATE_MODE)
        .open(&lock_path)
        .map_err(|error| io("opening", &lock_path, error))?;
    file.lock()
        .map_err(|error| io("locking", &lock_path, error))?;
    Ok(Lock { _file: file })
}

/// Replace `path` with `bytes`, atomically, at `mode`.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Config-directory bytes: this crate owns them.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8], mode: u32) -> Result<(), ConfigError> {
    let directory = parent_of(path)?;
    let temporary = directory.join(format!(
        ".{}.tmp-{}-{}",
        name_of(path)?,
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));

    let landed = land(&temporary, bytes, mode).and_then(|()| {
        std::fs::rename(&temporary, path).map_err(|error| io("renaming onto", path, error))
    });
    if landed.is_err() {
        // The temporary file is this call's own; leaving one behind would
        // block the same name later, and its bytes were never anybody's.
        let _ = std::fs::remove_file(&temporary);
        return landed;
    }

    // The rename is a directory entry, so the directory is what has to reach
    // the disk for the replacement to survive a power cut.
    let handle = std::fs::File::open(directory).map_err(|error| io("opening", directory, error))?;
    handle
        .sync_all()
        .map_err(|error| io("syncing", directory, error))
}

/// Write `bytes` into a file that does not exist yet, at `mode`, and get them
/// onto the disk.
///
/// `create_new` is what makes the mode meaningful: the file is created with it
/// rather than adjusted to it afterwards, so there is no window in which a
/// file holding a secret is readable by anyone else. It also refuses a
/// leftover temporary rather than overwriting one.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Config-directory bytes: this crate owns them.
fn land(temporary: &Path, bytes: &[u8], mode: u32) -> Result<(), ConfigError> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(temporary)
        .map_err(|error| io("creating", temporary, error))?;
    file.write_all(bytes)
        .map_err(|error| io("writing", temporary, error))?;
    file.sync_all()
        .map_err(|error| io("syncing", temporary, error))
}

/// Read `path`, hand the reading to `apply`, and write back what `apply`
/// leaves behind — all under one exclusive lock.
///
/// The lock is taken before the read and released after the rename, so a
/// second writer's read cannot begin inside this one's window. That is what
/// makes a lost update structurally impossible rather than merely unlikely: a
/// reader-then-writer pair is never interleaved with another.
///
/// A refusal from `apply` writes nothing. The file is left exactly as it was
/// read, which is what makes a refused mutation — a duplicate label, a name
/// the caller decided against — safe to attempt.
pub(crate) fn locked_update<S, T>(
    path: &Path,
    mode: u32,
    load: impl FnOnce(&Path, Option<Vec<u8>>) -> Result<S, ConfigError>,
    apply: impl FnOnce(&mut S) -> Result<T, ConfigError>,
    render: impl FnOnce(&S) -> Result<String, ConfigError>,
) -> Result<T, ConfigError> {
    let _lock = lock(path)?;
    let bytes = if mode == PRIVATE_MODE {
        read_private(path)?
    } else {
        read(path)?
    };
    let mut state = load(path, bytes)?;
    let outcome = apply(&mut state)?;
    write_atomically(path, render(&state)?.as_bytes(), mode)?;
    Ok(outcome)
}

fn parent_of(path: &Path) -> Result<&Path, ConfigError> {
    path.parent().ok_or_else(|| ConfigError::Io {
        operation: "resolving the directory of",
        path: path.to_path_buf(),
        message: "the path names no directory".to_string(),
    })
}

fn name_of(path: &Path) -> Result<&str, ConfigError> {
    path.file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ConfigError::Io {
            operation: "resolving the file name of",
            path: path.to_path_buf(),
            message: "the path names no file".to_string(),
        })
}

fn lock_path(path: &Path) -> Result<PathBuf, ConfigError> {
    Ok(parent_of(path)?.join(format!(".{}.lock", name_of(path)?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lock guards the file without ever being the file: a writer renames
    /// over the target, and a lock on the target would move out from under the
    /// next waiter with it.
    #[test]
    fn the_lock_sits_beside_the_file_it_guards() {
        let path = Path::new("/config/norn/registry.toml");
        assert_eq!(
            lock_path(path).expect("a lock path"),
            PathBuf::from("/config/norn/.registry.toml.lock")
        );
    }
}
