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
//! A failure **after** the rename is [`MutationUnconfirmed`] rather than
//! [`Io`]: the change is at the name and every reader sees it, so a caller
//! that treats it as "nothing happened" and retries is writing a second time
//! over its own first write.
//!
//! [`MutationUnconfirmed`]: ConfigError::MutationUnconfirmed
//! [`Io`]: ConfigError::Io
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
//! Two things make the lock the file rather than the name. It is opened with
//! `O_NOFOLLOW`, so a symlink planted at the lock name is refused instead of
//! followed to somewhere nothing else guards; and after the lock is acquired
//! the handle's `(device, inode)` is compared against what the name resolves
//! to now, because a lock on a file that has since been unlinked and replaced
//! excludes nobody — two writers would each hold an exclusive lock on a
//! different inode. A mismatch drops the handle and takes the lock again.
//!
//! `norn-fs` locks the maintainer lock file with a second spelling of this same
//! idiom. The allowlist carries no edge between the two crates in either
//! direction, and invariant 11 is why it may not: a vault-effect mechanism
//! reaching machine-local state — or the reverse — blurs the seam that owns its
//! meaning, and a third crate holding the shared mechanics would be edges the
//! allowlist does not carry either.
//! `docs/architecture.md` records the split and names the three things that
//! stay aligned across the two: the `O_NOFOLLOW` open, this handle-versus-name
//! recheck with its bounded retry, and the
//! create-new/write/fsync-file/rename/fsync-parent publish order.
//!
//! Waits are unbounded. A blocked writer is waiting on another writer's
//! read-modify-write, which is bounded by a small file's rewrite, and a
//! timeout here would turn "somebody else is mid-write" into a failure a
//! caller has to handle for no gain.
//!
//! **`flock` is a local-filesystem guarantee.** On NFS it is emulated through
//! the lock manager and on some configurations degrades to advisory-only or to
//! nothing at all, so a config directory on a network mount has the exclusion
//! its server offers rather than the one this module states. Machine-local
//! state is local by definition; this is the limit of what a `.lock` file
//! buys if it is put somewhere it does not belong.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{ConfigError, io, unconfirmed};

/// The mode a file holding secrets is created with, and the mode a reader of
/// one demands. Owner read and write, nothing else.
const PRIVATE_MODE: u32 = 0o600;

/// The mode a file holding no secret is created with. The registry names vault
/// roots, which is machine layout rather than a credential.
const SHARED_MODE: u32 = 0o644;

/// The mode the config and data directories are created with. Owner only,
/// because the config directory holds the token file and a directory the group
/// can traverse is a token file the group can reach whatever its own mode is.
const DIRECTORY_MODE: u32 = 0o700;

/// The mode bits that put a file outside its owner's reach.
const BEYOND_OWNER: u32 = 0o077;

/// How many times the lock is taken before the file at the lock name is
/// declared unlockable.
///
/// One retry is the normal case — the name was replaced while this caller
/// waited. A caller that loses the race this many times in a row is racing
/// something that is removing the lock file in a loop, and saying so is more
/// use than waiting on it forever.
const LOCK_ATTEMPTS: usize = 64;

/// Distinguishes two temporary files taken in the same process.
static SERIAL: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// The files this thread is inside a mutation of, innermost last.
    static MUTATING: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

/// What a machine-local file holds, and therefore what it is created and read
/// at.
///
/// One value rather than a mode and a flag, because the two always move
/// together: the file that is created owner-only is the file whose reader
/// refuses anything looser, and a mode compared against a constant to work out
/// which is a rule stated twice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Sensitivity {
    /// Machine layout. Created at `0644` and read at whatever mode it has.
    Shared,
    /// A credential. Created at `0600`, and a read refuses the file, or the
    /// directory holding it, if either reaches beyond its owner.
    Private,
}

impl Sensitivity {
    /// The mode a file of this sensitivity is created with.
    fn mode(self) -> u32 {
        match self {
            Sensitivity::Shared => SHARED_MODE,
            Sensitivity::Private => PRIVATE_MODE,
        }
    }

    /// Whether a reader of this file demands owner-only access to it.
    fn demands_privacy(self) -> bool {
        matches!(self, Sensitivity::Private)
    }
}

/// One machine-local file: where it sits, what it holds, and how its bytes
/// become a value and back.
///
/// Both files are this value with different arguments, which is what keeps the
/// read protocol and the write protocol one implementation rather than two
/// that drift.
pub(crate) struct Stored<S> {
    path: PathBuf,
    sensitivity: Sensitivity,
    load: fn(&Path, Option<&[u8]>) -> Result<S, ConfigError>,
    render: fn(&mut S, &Path) -> Result<String, ConfigError>,
}

impl<S> Stored<S> {
    pub(crate) fn new(
        path: PathBuf,
        sensitivity: Sensitivity,
        load: fn(&Path, Option<&[u8]>) -> Result<S, ConfigError>,
        render: fn(&mut S, &Path) -> Result<String, ConfigError>,
    ) -> Self {
        Stored {
            path,
            sensitivity,
            load,
            render,
        }
    }

    /// The value the file holds. Never writes, whatever it finds.
    pub(crate) fn read(&self) -> Result<S, ConfigError> {
        let bytes = read(&self.path, self.sensitivity)?;
        (self.load)(&self.path, bytes.as_deref())
    }

    /// Read the file, hand the reading to `apply`, and write back what `apply`
    /// leaves behind — all under one exclusive lock.
    ///
    /// The lock is taken before the read and released after the rename, so a
    /// second writer's read cannot begin inside this one's window. That is
    /// what makes a lost update structurally impossible rather than merely
    /// unlikely: a reader-then-writer pair is never interleaved with another.
    ///
    /// A refusal from `apply` writes nothing. The file is left exactly as it
    /// was read, which is what makes a refused mutation — a duplicate label, a
    /// name the caller decided against — safe to attempt. So is a mutation
    /// that changes nothing: bytes identical to the ones read are not written
    /// back.
    ///
    /// **`apply` must not mutate this file again.** The lock is exclusive and
    /// held for the whole call, so a nested mutation would wait on its own
    /// caller forever; it is refused with [`NestedMutation`] instead.
    ///
    /// [`NestedMutation`]: ConfigError::NestedMutation
    pub(crate) fn mutate<T>(
        &self,
        apply: impl FnOnce(&mut S) -> Result<T, ConfigError>,
    ) -> Result<T, ConfigError> {
        let _reentry = Reentry::enter(&self.path)?;
        let _lock = lock(&self.path, self.sensitivity)?;
        sweep_temporaries(&self.path)?;

        let bytes = read(&self.path, self.sensitivity)?;
        let mut state = (self.load)(&self.path, bytes.as_deref())?;
        let outcome = apply(&mut state)?;
        let rendered = (self.render)(&mut state, &self.path)?;
        if bytes.as_deref() == Some(rendered.as_bytes()) {
            // The mutation left the file's bytes exactly as they were.
            // Writing them would replace the file with a copy of itself: a new
            // inode, a temporary file holding the same secrets, and a window
            // in which a crash leaves one behind — all of it for no change.
            return Ok(outcome);
        }
        write_atomically(&self.path, rendered.as_bytes(), self.sensitivity)?;
        Ok(outcome)
    }
}

/// The bytes at `path`, or `None` when nothing is there.
///
/// Absence is confirmed rather than assumed. A symlink pointing at nothing
/// fails an open the way an absent file does, and so does a file under a
/// directory symlink pointing at nothing, so both are asked about before the
/// absence is reported.
fn read(path: &Path, sensitivity: Sensitivity) -> Result<Option<Vec<u8>>, ConfigError> {
    let Some(mut file) = open_existing(path, sensitivity)? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| io("reading", path, error))?;
    Ok(Some(bytes))
}

#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Config-directory bytes: this crate owns them.
fn open_existing(
    path: &Path,
    sensitivity: Sensitivity,
) -> Result<Option<std::fs::File>, ConfigError> {
    if sensitivity.demands_privacy() {
        demand_owner_only(parent_of(path)?)?;
    }

    // The link is asked about before the open, because a machine-local data
    // file that is a link is refused rather than read through.
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => return Err(symlink_refusal(path)),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Nothing at the end of the path. A dangling link anywhere along
            // it answers the same way and means the opposite thing — state
            // that moved or was half removed rather than a first run — so the
            // ancestors are asked before absence is reported.
            dangling_ancestor(path)?;
            return Ok(None);
        }
        Err(error) => return Err(io("reading the type of", path, error)),
    }

    let file = match std::fs::OpenOptions::new().read(true).open(path) {
        Ok(file) => file,
        // It was there a moment ago and is not now: a writer's rename or a
        // removal landed in between. At the instant of the open nothing was
        // there, and absent is the honest reading of that.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io("opening", path, error)),
    };
    if sensitivity.demands_privacy() {
        // Judged from the open handle rather than from the path, so what is
        // checked is what is read.
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

/// The refusal a symlink at a data file's own name earns, precise about which
/// kind it is: a link to nothing is state that moved, and a link to something
/// is a file this crate would read from one place and replace in another.
///
/// Resolving the link can fail for a reason that is neither of those: the
/// target sits under a directory this process cannot search. That failure is
/// reported as itself rather than folded into "dangling", which would tell a
/// permissions problem it is a moved-state problem.
#[allow(clippy::disallowed_methods)] // Config-directory bytes: this crate owns them.
fn symlink_refusal(path: &Path) -> ConfigError {
    match std::fs::metadata(path) {
        Ok(_) => ConfigError::SymlinkedFile {
            path: path.to_path_buf(),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ConfigError::DanglingSymlink {
                path: path.to_path_buf(),
            }
        }
        Err(error) => io("resolving the target of", path, error),
    }
}

/// Refuse when a directory on the way to `path` is a symlink pointing at
/// nothing.
///
/// The walk stops at the first ancestor that is there: everything below it is
/// genuinely absent, which is the reading a first run deserves. A link that
/// resolves is not this function's business — it is somebody's arrangement of
/// their own directories, and the file at the end of it is a real file.
#[allow(clippy::disallowed_methods)] // Config-directory bytes: this crate owns them.
fn dangling_ancestor(path: &Path) -> Result<(), ConfigError> {
    for ancestor in path.ancestors().skip(1) {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return match std::fs::metadata(ancestor) {
                    Ok(_) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        Err(ConfigError::DanglingSymlink {
                            path: ancestor.to_path_buf(),
                        })
                    }
                    Err(error) => Err(io("resolving the target of", ancestor, error)),
                };
            }
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(io("reading the type of", ancestor, error)),
        }
    }
    Ok(())
}

/// Refuse a directory the group or the world can reach into.
///
/// A file's own mode is only half of who can read it: a directory anyone may
/// traverse is a token file anyone may open, whatever the file says. The
/// directory this crate creates is `0700`, and an existing one is checked
/// rather than tightened — a directory builder no-ops on a directory that is
/// already there, so "we created it at 0700" is not a fact about a directory
/// somebody else made first.
#[allow(clippy::disallowed_methods)] // Config-directory bytes: this crate owns them.
fn demand_owner_only(directory: &Path) -> Result<(), ConfigError> {
    let mode = match std::fs::metadata(directory) {
        Ok(metadata) => metadata.permissions().mode(),
        // Nothing to reach through, and therefore no file to reach.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io("reading the mode of", directory, error)),
    };
    if mode & BEYOND_OWNER != 0 {
        return Err(ConfigError::InsecurePermissions {
            path: directory.to_path_buf(),
            mode: mode & 0o7777,
        });
    }
    Ok(())
}

/// A file's `(device, inode)` pair: which file it is, whatever it is called.
///
/// An inode number is unique per device and nowhere else, so the pair is the
/// identity and neither half is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Identity {
    dev: u64,
    ino: u64,
}

/// The identity of what `metadata` describes.
fn identity_of(metadata: &std::fs::Metadata) -> Identity {
    Identity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    }
}

/// The identity `path` resolves to now, or `None` when it resolves to nothing.
///
/// **A stat that fails for any other reason is the machine failing, and it
/// travels as one.** Reading every failure as "the name means a different
/// file" would spend a lock attempt on a question nothing answered — and on a
/// directory that has become unreadable it spends all of them, ending in a
/// message about a lock file being replaced by a caller that never learned the
/// filesystem said `EACCES`.
#[allow(clippy::disallowed_methods)] // Config-directory bytes: this crate owns them.
fn name_identity(path: &Path) -> Result<Option<Identity>, ConfigError> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(Some(identity_of(&metadata))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io("reading the identity of", path, error)),
    }
}

/// An exclusive lock over one machine-local file, held for as long as the
/// value lives.
#[derive(Debug)]
#[allow(clippy::disallowed_types)] // Config-directory bytes: this crate owns them.
struct Lock {
    /// The `.lock` file's handle. Dropping it releases the lock, which is why
    /// it is held rather than discarded.
    _file: std::fs::File,
}

/// Take the exclusive lock guarding `path`, waiting for as long as it takes,
/// and prepare the directory `path` sits in.
fn lock(path: &Path, sensitivity: Sensitivity) -> Result<Lock, ConfigError> {
    lock_where(path, sensitivity, |_| {})
}

/// [`lock`], with something allowed to happen between taking the lock and
/// rechecking the name.
///
/// `disturb` is called with the attempt number at exactly the point the
/// dead-inode hazard opens: the lock is held and the name has not been asked
/// about yet. That is the only place a test can manufacture the hazard
/// deterministically — waiting for it to happen by chance is a race nobody can
/// arrange, and the defense would then be asserted rather than checked.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Config-directory bytes: this crate owns them.
fn lock_where(
    path: &Path,
    sensitivity: Sensitivity,
    mut disturb: impl FnMut(usize),
) -> Result<Lock, ConfigError> {
    let directory = parent_of(path)?;
    // Asked before the directory is built: a dangling directory symlink makes
    // `create_dir_all` fail with "file exists", which is the least useful true
    // thing that can be said about it.
    dangling_ancestor(path)?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(DIRECTORY_MODE)
        .create(directory)
        .map_err(|error| io("creating", directory, error))?;
    if sensitivity.demands_privacy() {
        demand_owner_only(directory)?;
    }

    let lock_path = lock_path(path)?;
    for attempt in 0..LOCK_ATTEMPTS {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(PRIVATE_MODE)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&lock_path)
            .map_err(|error| io("opening", &lock_path, error))?;
        file.lock()
            .map_err(|error| io("locking", &lock_path, error))?;

        disturb(attempt);

        // The lock is on the inode, so holding one says nothing until the name
        // is known to still resolve to it. A file unlinked and recreated while
        // this caller waited leaves two holders of two different inodes, each
        // believing it excludes the other.
        let held = identity_of(
            &file
                .metadata()
                .map_err(|error| io("reading the identity of", &lock_path, error))?,
        );
        if name_identity(&lock_path)? == Some(held) {
            return Ok(Lock { _file: file });
        }
        // The name means another file, or nothing. Dropping the handle releases
        // the lock on an inode this name no longer reaches, which is the only
        // correct thing to do with it, and the next attempt opens the name
        // again.
    }
    Err(ConfigError::Io {
        operation: "locking",
        path: lock_path,
        message: format!("the lock file was replaced on each of {LOCK_ATTEMPTS} attempts"),
    })
}

/// Remove the temporary files an interrupted writer left beside `path`.
///
/// Called with the lock held, which is what makes it safe: a temporary is only
/// ever created under that lock, so while it is held there is no live writer
/// and anything matching the temporary shape is the residue of one that died
/// before its rename. Left in place, a token file's residue holds a whole
/// secret at `0600` forever, and its name — process id and serial — comes
/// round again on a later boot, where `create_new` refuses it and wedges the
/// write.
#[allow(clippy::disallowed_methods)] // Config-directory bytes: this crate owns them.
fn sweep_temporaries(path: &Path) -> Result<(), ConfigError> {
    let directory = parent_of(path)?;
    let prefix = temporary_prefix(path)?;
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io("reading", directory, error)),
    };
    for entry in entries {
        let entry = entry.map_err(|error| io("reading", directory, error))?;
        // Only a regular file can be this protocol's leftover; anything else
        // wearing the prefix is not ours to remove, and failing the mutation
        // over it would let a planted directory wedge the file for good.
        let is_file = entry.file_type().is_ok_and(|kind| kind.is_file());
        if is_file && entry.file_name().to_string_lossy().starts_with(&prefix) {
            let stale = entry.path();
            std::fs::remove_file(&stale).map_err(|error| io("removing", &stale, error))?;
        }
    }
    Ok(())
}

/// Replace `path` with `bytes`, atomically, at the mode `sensitivity` names.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Config-directory bytes: this crate owns them.
fn write_atomically(
    path: &Path,
    bytes: &[u8],
    sensitivity: Sensitivity,
) -> Result<(), ConfigError> {
    let directory = parent_of(path)?;
    // A rename onto a link replaces the link rather than what it points at, so
    // the writer and the reader would disagree about where the state is. Both
    // directions refuse instead.
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => return Err(symlink_refusal(path)),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io("reading the type of", path, error)),
    }

    let temporary = directory.join(format!(
        "{}{}-{}",
        temporary_prefix(path)?,
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));

    // `create_new` is what makes the mode meaningful: the file is created with
    // it rather than adjusted to it afterwards, so there is no window in which
    // a file holding a secret is readable by anyone else. It also refuses a
    // name that is already taken — and a file this call did not create is a
    // file this call does not remove, whatever goes wrong afterwards.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(sensitivity.mode())
        .open(&temporary)
        .map_err(|error| io("creating", &temporary, error))?;

    let landed = fill(&mut file, bytes, &temporary).and_then(|()| {
        std::fs::rename(&temporary, path).map_err(|error| io("renaming onto", path, error))
    });
    if let Err(error) = landed {
        // The temporary file is this call's own; leaving one behind would
        // block the same name later, and its bytes were never anybody's.
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }

    // Past the rename the change is at the name and every reader sees it, so
    // what is left is durability alone — and a failure here says exactly that
    // rather than reading as a write that did not happen.
    //
    // The rename is a directory entry, so the directory is what has to reach
    // the disk for the replacement to survive a power cut.
    let handle = std::fs::File::open(directory)
        .map_err(|error| unconfirmed("opening the directory of", path, error))?;
    handle
        .sync_all()
        .map_err(|error| unconfirmed("syncing the directory of", path, error))
}

/// Put `bytes` in the temporary file and get them onto the disk.
#[allow(clippy::disallowed_types)] // Config-directory bytes: this crate owns them.
fn fill(file: &mut std::fs::File, bytes: &[u8], temporary: &Path) -> Result<(), ConfigError> {
    file.write_all(bytes)
        .map_err(|error| io("writing", temporary, error))?;
    file.sync_all()
        .map_err(|error| io("syncing", temporary, error))
}

/// Marks a file as under mutation by this thread, and refuses a second entry.
struct Reentry {
    path: PathBuf,
}

impl Reentry {
    fn enter(path: &Path) -> Result<Self, ConfigError> {
        MUTATING.with(|held| {
            let mut held = held.borrow_mut();
            if held.iter().any(|entered| entered == path) {
                return Err(ConfigError::NestedMutation {
                    path: path.to_path_buf(),
                });
            }
            held.push(path.to_path_buf());
            Ok(Reentry {
                path: path.to_path_buf(),
            })
        })
    }
}

impl Drop for Reentry {
    fn drop(&mut self) {
        MUTATING.with(|held| {
            let mut held = held.borrow_mut();
            if let Some(index) = held.iter().rposition(|entered| entered == &self.path) {
                held.remove(index);
            }
        });
    }
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

/// What every temporary file for `path` is named with, and therefore what a
/// sweep recognizes one by.
fn temporary_prefix(path: &Path) -> Result<String, ConfigError> {
    Ok(format!(".{}.tmp-", name_of(path)?))
}

fn lock_path(path: &Path) -> Result<PathBuf, ConfigError> {
    Ok(parent_of(path)?.join(format!(".{}.lock", name_of(path)?)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use norn_testkit::scratch::Scratch;

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

    /// Every temporary a writer takes for one file starts with the prefix a
    /// sweep looks for, so no residue of an interrupted write is invisible to
    /// the next writer.
    #[test]
    fn a_temporary_is_named_with_the_prefix_a_sweep_recognizes() {
        let path = Path::new("/config/norn/tokens.toml");
        let prefix = temporary_prefix(path).expect("a prefix");
        assert_eq!(prefix, ".tokens.toml.tmp-");
        assert!(
            !lock_path(path)
                .expect("a lock path")
                .file_name()
                .expect("a name")
                .to_string_lossy()
                .starts_with(&prefix),
            "a sweep would remove the lock file"
        );
    }

    /// The temporary name a write refuses to take is a name it also refuses to
    /// remove — a file this call did not create belongs to whoever created it,
    /// and the failure path used to unlink it. The sweep is what clears it,
    /// under the lock, where there is no live writer to mistake it for.
    #[test]
    #[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Harness scaffolding: arranging a dead writer's residue.
    fn a_write_never_removes_a_temporary_it_did_not_create() {
        let scratch = Scratch::new("norn-config-residue");
        let directory = scratch.root();
        let path = directory.join("registry.toml");

        // The name this process's next write will take: the same shape a
        // writer of an earlier boot took, with the same process id.
        let taken = directory.join(format!(
            "{}{}-{}",
            temporary_prefix(&path).expect("a prefix"),
            std::process::id(),
            SERIAL.load(Ordering::Relaxed)
        ));
        std::fs::write(&taken, b"somebody else's bytes").expect("a stranger's file");

        let error = write_atomically(&path, b"version = 1\n", Sensitivity::Shared)
            .expect_err("a name that is already taken");
        assert!(
            matches!(&error, ConfigError::Io { operation, .. } if *operation == "creating"),
            "{error}"
        );
        assert_eq!(
            std::fs::read(&taken).expect("the stranger's file"),
            b"somebody else's bytes",
            "the failure path removed a file it did not create"
        );

        // And the wedge does not survive a mutation, which sweeps first.
        let stored = Stored::new(
            path.clone(),
            Sensitivity::Shared,
            |_, bytes| Ok(String::from_utf8(bytes.unwrap_or_default().to_vec()).expect("text")),
            |state: &mut String, _| Ok(state.clone()),
        );
        stored
            .mutate(|state| {
                state.push_str("version = 1\n");
                Ok(())
            })
            .expect("a mutation over a dead writer's residue");
        assert!(
            !std::fs::exists(&taken).expect("asking about the residue"),
            "the sweep left a dead writer's temporary behind"
        );
        assert_eq!(
            std::fs::read(&path).expect("the file"),
            b"version = 1\n",
            "the mutation did not land"
        );
    }

    /// The recheck that guards the lock asks one question — does this name
    /// still mean this file — and absence is the only failure that answers it.
    /// Anything else is the machine, and it is reported rather than counted as
    /// a mismatch, because a lock loop that burns its attempts on unanswered
    /// stats ends by blaming a lock file for a filesystem that said `ENOTDIR`.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging a name that cannot be stat'd.
    fn a_stat_that_cannot_answer_is_reported_rather_than_read_as_a_mismatch() {
        let scratch = Scratch::new("norn-config-identity");
        let directory = scratch.root();

        let absent = directory.join("nothing-here");
        assert_eq!(
            name_identity(&absent).expect("absence is not a failure"),
            None
        );

        let file = directory.join("a-file");
        std::fs::write(&file, b"bytes").expect("a file");
        assert_eq!(
            name_identity(&file).expect("a stat"),
            Some(identity_of(
                &std::fs::metadata(&file).expect("the file's metadata")
            )),
        );

        // A regular file is not a directory, so a name beneath one resolves to
        // nothing — and the filesystem says so with something other than "not
        // found".
        let beneath = file.join("under-a-file");
        let error = name_identity(&beneath).expect_err("a stat that cannot answer");
        assert!(
            matches!(
                &error,
                ConfigError::Io { operation, .. } if *operation == "reading the identity of"
            ),
            "{error}"
        );
    }

    /// The tree a case is arranged in, removed when the handle drops.
    fn scratch(name: &str) -> Scratch {
        Scratch::new(&format!("norn-config-{name}"))
    }

    /// The identity the lock is actually held on, read off the handle it holds.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: judging which inode was locked.
    fn held_identity(lock: &Lock) -> Identity {
        identity_of(&lock._file.metadata().expect("the locked file's metadata"))
    }

    /// **The bar on the dead-inode hazard.** A lock file replaced between the
    /// lock and the recheck is not settled for: the acquirer drops it and
    /// converges on the file the name means now.
    ///
    /// The disturbance is injected at exactly the point the hazard opens, which
    /// is the only way to reach it deterministically. The forbidden shape is a
    /// lock that returns as soon as `flock` succeeds: it hands back a lock on an
    /// orphaned inode, and the next writer opening that path meets no contention
    /// and reads the file this one is mid-rewrite of.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: playing the foreign actor.
    fn a_lock_file_replaced_under_the_acquirer_is_not_settled_for() {
        let scratch = scratch("lock-aba");
        let directory = scratch.root();
        let path = directory.join("registry.toml");
        let lock_name = lock_path(&path).expect("a lock path");

        let mut disturbed = Vec::new();
        let taken = lock_where(&path, Sensitivity::Shared, |attempt| {
            // A foreign actor — nothing in this crate ever removes a lock file
            // — takes the name away and puts a different file at it. Once only,
            // so the retry has a stable name to converge on.
            if attempt == 0 {
                std::fs::remove_file(&lock_name).expect("removing the lock file");
                std::fs::write(&lock_name, b"").expect("a replacement lock file");
                disturbed.push(attempt);
            }
        })
        .expect("a lock");

        assert_eq!(disturbed, vec![0], "the hazard was not manufactured");
        let current = name_identity(&lock_name)
            .expect("the lock path")
            .expect("a lock file");
        assert_eq!(
            held_identity(&taken),
            current,
            "the lock settled for an inode the name no longer resolves to"
        );
    }

    /// Foreign interference that never stops is a refusal that names the bound,
    /// rather than a loop nobody can see.
    ///
    /// The bound is asserted as the number it is. Asserting it against its own
    /// constant says only that the loop counts to whatever it counts to: a bound
    /// of two would satisfy that and would refuse an ordinary write racing a
    /// busy machine.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: playing the foreign actor.
    fn a_lock_file_replaced_on_every_attempt_refuses_at_the_bound() {
        let scratch = scratch("lock-aba-forever");
        let directory = scratch.root();
        let path = directory.join("registry.toml");
        let lock_name = lock_path(&path).expect("a lock path");

        let mut attempts = 0usize;
        let error = lock_where(&path, Sensitivity::Shared, |_| {
            attempts += 1;
            let _ = std::fs::remove_file(&lock_name);
            std::fs::write(&lock_name, b"").expect("a replacement lock file");
        })
        .expect_err("a lock file that is never stable");

        assert_eq!(attempts, 64, "the house bound is 64 attempts");
        assert!(
            matches!(
                &error,
                ConfigError::Io { operation, path: reported, message }
                    if *operation == "locking"
                        && reported == &lock_name
                        && message.contains("64")
            ),
            "{error}"
        );
    }

    /// The recheck's stat failing is the machine failing, and the lock says so
    /// instead of spending an attempt on it.
    ///
    /// The forbidden shape is reading every stat failure as "the name means a
    /// different file". On a directory that has stopped being a directory that
    /// spends all sixty-four attempts and ends in a message about a lock file
    /// being replaced, by a caller that never learned what the filesystem said.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: playing the foreign actor.
    fn a_recheck_whose_stat_cannot_answer_reports_the_machine() {
        let scratch = scratch("lock-unanswerable");
        let directory = scratch.root();
        let path = directory.join("registry.toml");
        let lock_name = lock_path(&path).expect("a lock path");

        let mut attempts = 0usize;
        let error = lock_where(&path, Sensitivity::Shared, |_| {
            attempts += 1;
            // A regular file is not a directory, so the lock name beneath one
            // resolves to nothing — and the filesystem says so with something
            // other than "not found".
            std::fs::remove_file(&lock_name).expect("removing the lock file");
            std::fs::remove_dir(directory).expect("removing the directory");
            std::fs::write(directory, b"not a directory").expect("a file where a directory was");
        })
        .expect_err("a stat that cannot answer");

        assert_eq!(
            attempts, 1,
            "an unanswered stat spent more than the one try"
        );
        assert!(
            matches!(
                &error,
                ConfigError::Io { operation, .. } if *operation == "reading the identity of"
            ),
            "{error}"
        );
    }

    /// The mode is a property of what the file holds, and the reader's demand
    /// is the same property read the other way.
    #[test]
    fn sensitivity_carries_the_mode_and_the_demand_together() {
        assert_eq!(Sensitivity::Private.mode(), 0o600);
        assert!(Sensitivity::Private.demands_privacy());
        assert_eq!(Sensitivity::Shared.mode(), 0o644);
        assert!(!Sensitivity::Shared.demands_privacy());
    }
}
