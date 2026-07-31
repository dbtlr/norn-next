//! The compare-and-swap write kernel: the one way vault bytes change.
//!
//! Every write bears a precondition. [`Precondition::Replace`] names the hash
//! the caller observed and composed against; [`Precondition::Create`] says
//! nothing may be there at all. **There is no third option, and that is the
//! point** — an unconditional overwrite has no spelling here, so no plan, verb
//! or flag can reach one through this crate. A caller that means to overwrite
//! reads first and passes what it read, which is what makes it accountable for
//! the bytes it is about to destroy.
//!
//! # The protocol, stage by stage
//!
//! A **replacement** runs five stages, and the first four can only refuse:
//!
//! 1. **The destination is not a symbolic link.** Asked before anything else,
//!    and keyed on being a link rather than on where it points.
//! 2. **The precondition is evaluated against bytes read now**, through a
//!    handle this call opens and holds. Not against a caller-supplied snapshot,
//!    and not against anything an index remembers.
//! 3. **The shadow is staged**: a file nothing else knows the name of, holding
//!    the whole composed content, with the replaced file's mode carried
//!    forward, fsynced before any name points at it.
//! 4. **The verification re-observes the destination through the same held
//!    handle** — the same bytes, hashed again, plus one identity comparison
//!    between the handle and the name. Any difference refuses.
//! 5. **The swap is a rename**, which replaces the destination atomically:
//!    there is no moment in which a reader sees the destination absent, partial,
//!    or holding a mixture.
//!
//! An **exclusive create** is shorter, because there is nothing to compare
//! against and nothing to preserve. The destination is opened with
//! `create_new`, which is `O_EXCL`: the name is claimed atomically or the call
//! refuses with [`Refusal::DestinationExists`]. The content goes into that
//! handle and is fsynced. **No shadow is staged and no rename happens**, so the
//! link-then-unlink dance an exclusive publish would otherwise need does not
//! exist here — and neither does a shadow to leak. The shadow home is still a
//! parameter because the precondition is data a caller passes rather than a
//! function it picks.
//!
//! # One handle binds the whole replacement
//!
//! Stage 2 and stage 4 read **the same open file description**. That is what
//! makes the verification mean anything: two opens by name are two questions
//! about whatever the name resolved to at two moments, and a rename between
//! them makes the second answer about a different file while both look the
//! same. Holding one handle turns that into an observable — the handle's
//! `(device, inode)` compared against the name's is exactly the difference a
//! second open would have hidden, and it refuses as
//! [`Refusal::Republished`].
//!
//! # The residual race, stated rather than claimed away
//!
//! **The window between the verification and the rename cannot be closed.**
//! POSIX offers no compare-and-rename, so a foreign write landing in that
//! window is overwritten by the swap. The window is the width of one `rename`
//! call and nothing is arranged to widen it, but it is real and it is not
//! covered by any lock — Norn holds no lock over vault files and never will,
//! because a vault is inherently multi-writer.
//!
//! What that costs is bounded by the watcher rather than by exclusion: whatever
//! is finally at the path is what the watcher reports, so derived state
//! converges on the winner. A caller that needs to know whether it won compares
//! the [`PostState`] it was handed against what is at the path.
//!
//! # Crash windows, as recovery claims
//!
//! A process that dies mid-write leaves one of these, and nothing else:
//!
//! 1. **Inside the shadow write, before its fsync.** The destination is
//!    untouched. The shadow is leaked, and a leaked shadow is inert (see
//!    [`crate::shadow`]) and swept.
//! 2. **After the shadow's fsync, before the rename.** The same: the
//!    destination is untouched and the shadow is leaked-inert.
//! 3. **After the rename, before the parent directory's fsync.** The write is
//!    published — every reader sees it. What is not yet guaranteed is that the
//!    *directory entry* survives a power cut, which is what that fsync is for.
//!    A failure of it never un-lands the write and is never reported as one: the
//!    change is at the name, and a caller told "nothing happened" would write a
//!    second time over its own first write.
//!
//! **The invariant across all three: a replaced destination is old-complete or
//! new-complete, never a mixture.** The rename is what buys that, and it is why
//! an in-place streaming write is rejected outright — a torn document at rest,
//! and a truncate-then-crash window, are both disqualifying however much
//! copying they save.
//!
//! An exclusive create is the one case the invariant reads differently, because
//! "old" is absence: a create killed mid-write leaves a name that did not exist
//! holding a prefix of the composed content. There is no primitive that
//! atomically creates a name *with* content, so this is the floor rather than a
//! shortcut, and the bytes are fsynced before the parent is, so a power cut
//! that shows the name shows the content with it.
//!
//! # Single-shot outcomes
//!
//! Nothing here retries. Observed drift is returned, not absorbed: a
//! drift-policy parameter that re-composed and tried again is the rejected
//! shape, because it converts an observation the caller was promised into an
//! invisible loop, and the composed content was derived from bytes that have
//! since changed. A caller that wants to try again reads the observed state,
//! re-composes against it, and calls once more — under a bound it declared.

use std::io::{Read, Write as _};
use std::path::Path;

use crate::faults::{Faults, Stage};
use crate::hash::{ContentHash, hashed_from};
use crate::identity::{Identity, PostState, identity_of, post_state};
use crate::refusal::{Refusal, environment};
use crate::shadow::ShadowHome;

/// What must be true of the destination for a write to happen.
///
/// The two arms are the whole vocabulary. **A third arm meaning "whatever is
/// there, replace it" is deliberately absent**, and its absence is the
/// contract: an agent must never find overwriting cheaper than merging, and a
/// range that drifted invalidates an edit intent only the caller can derive
/// again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Precondition {
    /// Replace what was observed. The destination must hash to this now.
    Replace(ContentHash),
    /// Create. Nothing may be at the destination, and the exclusive open is
    /// what decides that — never a precheck.
    Create,
}

/// A write that happened, or that did not need to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Landed {
    /// The content was published, and this is the identity of what is there.
    Written(PostState),
    /// The destination already held exactly this content, so no shadow was
    /// staged, no rename ran, and nothing was verified.
    ///
    /// **Never reported as written**, and this is why: a suppression path keyed
    /// on Norn's own writes would be primed for a filesystem event that is
    /// never coming, and would then absorb the next foreign change to that
    /// document. The identity carried is the existing file's.
    Unchanged(PostState),
}

impl Landed {
    /// The identity of what is at the destination.
    pub fn post_state(&self) -> PostState {
        match self {
            Landed::Written(state) | Landed::Unchanged(state) => *state,
        }
    }

    /// Whether bytes were published. A caller recording an own-write for
    /// suppression asks this: there is no event coming for an unchanged file.
    pub fn wrote(&self) -> bool {
        matches!(self, Landed::Written(_))
    }
}

/// A removal that happened.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Vacated {
    /// The identity of what was removed. Carried for the same reason a write
    /// carries one: the removal comes back as a filesystem event, and an event
    /// Norn cannot recognize as its own is re-derived for nothing.
    pub removed: PostState,
}

/// A move that happened: both legs, each with its own identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Moved {
    /// What the destination now holds.
    pub created: PostState,
    /// What the source held when it was removed.
    pub vacated: PostState,
}

/// A move that did not complete, naming which leg refused and what is at rest.
///
/// **A move is two operations and is deliberately not atomic across the pair.**
/// There is no primitive that creates one name and removes another as one act,
/// and the pair is ordered so that the residue is always *both present* and
/// never *neither*: the destination is created from bytes read out of the
/// source, and only then is the source removed under its own hash
/// precondition. A caller or a later sweep can resolve two copies of a
/// document; nothing can resolve none.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MoveRefusal {
    /// The source did not satisfy its precondition. Nothing was written: both
    /// paths are as they were.
    Source(Refusal),
    /// The destination could not be created. Both paths are as they were — the
    /// source was read and not touched.
    Destination(Refusal),
    /// The destination holds the content and the source's removal was refused,
    /// so **both paths hold the document**. The created identity is carried
    /// because the write did land and a caller that treated this as "nothing
    /// happened" would be wrong about half of it.
    SourceRemains {
        /// Boxed for the reason [`Refusal::Drifted`]'s observed state is: this is
        /// the widest arm, and the two that say nothing was written should not
        /// pay for it.
        created: Box<PostState>,
        refusal: Refusal,
    },
}

impl std::fmt::Display for MoveRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MoveRefusal::Source(refusal) => write!(f, "the source was refused: {refusal}"),
            MoveRefusal::Destination(refusal) => {
                write!(f, "the destination was refused: {refusal}")
            }
            MoveRefusal::SourceRemains { refusal, .. } => write!(
                f,
                "the destination holds the document and removing the source was refused, so both \
                 paths hold it: {refusal}"
            ),
        }
    }
}

impl std::error::Error for MoveRefusal {}

/// Publish `content` at `destination` under `precondition`.
///
/// See the [module documentation](self) for the protocol, the residual race and
/// the crash windows.
pub fn write(
    destination: &Path,
    content: &[u8],
    precondition: Precondition,
    shadows: &ShadowHome,
) -> Result<Landed, Refusal> {
    write_where(destination, content, precondition, shadows, Faults::NONE)
}

/// [`write`], with a stage made to fail rather than waiting for a machine that
/// fails there.
///
/// The parameter is what makes two of the contract's claims checkable at all: a
/// disk that fills mid-shadow, and a directory fsync that fails after the
/// rename already published a name.
fn write_where(
    destination: &Path,
    content: &[u8],
    precondition: Precondition,
    shadows: &ShadowHome,
    faults: Faults,
) -> Result<Landed, Refusal> {
    refuse_symlink(destination)?;
    match precondition {
        Precondition::Create => create_exclusively(destination, content, faults),
        Precondition::Replace(expected) => {
            replace_observed(destination, content, expected, shadows, faults)
        }
    }
}

/// Remove `path`, which must currently hash to `expected`.
///
/// A removal is a write with no bytes, and it bears a precondition for the same
/// reason: the intent to remove a document was formed from its content, and
/// content that changed invalidates the intent. The evaluation is the same one
/// handle bound to the same two questions — hash the bytes, then confirm the
/// name still resolves to the handle that was read.
///
/// **Absence is drift, not success.** A path with nothing at it did not satisfy
/// a hash precondition; somebody else removed the document, and a caller told
/// "removed" would record an own-write for an event it did not cause.
///
/// The residual race is the unlink's, and it is the rename's race seen from the
/// other side: between the confirmation and the `unlink` the name could come to
/// mean a different file, and the removal would take that one. The window is
/// the width of one call and POSIX offers nothing to close it.
pub fn vacate(path: &Path, expected: ContentHash) -> Result<Vacated, Refusal> {
    refuse_symlink(path)?;
    let (mut held, _) = match opened_for_reading(path)? {
        Some(open) => open,
        None => return Err(drifted(path, expected, None)),
    };
    let observed = observed_state(&mut held, path)?;
    if observed.content_hash != expected {
        return Err(drifted(path, expected, Some(observed)));
    }
    confirm_name_still_resolves(path, observed.identity())?;

    remove(path)?;
    // The removal is a directory entry, so the directory is what has to reach
    // the disk for it to survive a power cut. Best-effort for the same reason
    // the swap's is: the name is already gone for every reader.
    sync_directory(parent_of(path), Faults::NONE);
    Ok(Vacated { removed: observed })
}

/// Move the document at `source` — which must hash to `expected` — to
/// `destination`, which must not exist.
///
/// Both legs go through this kernel: the destination is an exclusive create and
/// the source is a hash-verified vacate, so neither side is decided by an
/// existence precheck. See [`MoveRefusal`] for what the pair guarantees and
/// what it does not.
///
/// The source is read twice, once per leg, and that is deliberate: the second
/// read is the vacate's own precondition, so a source that changed while the
/// destination was being written is **not** removed. One handle held across
/// both legs would save an open and would remove a document nobody looked at
/// again.
pub fn move_document(
    source: &Path,
    destination: &Path,
    expected: ContentHash,
    shadows: &ShadowHome,
) -> Result<Moved, MoveRefusal> {
    let content = read_verified(source, expected).map_err(MoveRefusal::Source)?;
    let created = write(destination, &content, Precondition::Create, shadows)
        .map_err(MoveRefusal::Destination)?
        .post_state();
    match vacate(source, expected) {
        Ok(vacated) => Ok(Moved {
            created,
            vacated: vacated.removed,
        }),
        Err(refusal) => Err(MoveRefusal::SourceRemains {
            created: Box::new(created),
            refusal,
        }),
    }
}

/// Claim `destination` exclusively and put `content` in it.
///
/// `create_new` is `O_EXCL`: the name is claimed atomically, so a destination
/// that springs into existence between a caller's look and this call refuses
/// with the racer's bytes untouched. No precheck decides this — a precheck
/// would be a second answer to a question the open already answers correctly.
///
/// A failure after the name is claimed removes it. The file is one this call
/// created, so removing it is not a guess, and leaving a prefix of a document at
/// a name that had nothing at it would be a document nobody wrote.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The vault filesystem seam: this crate owns vault handles.
fn create_exclusively(
    destination: &Path,
    content: &[u8],
    faults: Faults,
) -> Result<Landed, Refusal> {
    faults
        .check(Stage::Create)
        .map_err(|error| environment("creating", destination, &error))?;
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(Refusal::DestinationExists {
                path: destination.to_path_buf(),
            });
        }
        Err(error) => return Err(environment("creating", destination, &error)),
    };

    let filled = fill(&mut file, content, destination, faults);
    let state = filled.and_then(|()| observed_metadata(&file, destination));
    let state = match state {
        Ok(metadata) => post_state(ContentHash::of(content), content.len() as u64, &metadata),
        Err(refusal) => {
            // A file this call created and could not finish is a file this call
            // takes back.
            remove_quietly(destination);
            return Err(refusal);
        }
    };

    sync_directory(parent_of(destination), faults);
    Ok(Landed::Written(state))
}

/// Replace what is at `destination` — which must hash to `expected` — with
/// `content`.
fn replace_observed(
    destination: &Path,
    content: &[u8],
    expected: ContentHash,
    shadows: &ShadowHome,
    faults: Faults,
) -> Result<Landed, Refusal> {
    // Stage 2. One handle, opened here and held until the swap.
    let (mut held, replaced) = match opened_for_reading(destination)? {
        Some(open) => open,
        None => return Err(drifted(destination, expected, None)),
    };
    let observed = observed_state(&mut held, destination)?;
    if observed.content_hash != expected {
        return Err(drifted(destination, expected, Some(observed)));
    }
    let composed = ContentHash::of(content);
    if composed == observed.content_hash {
        // Nothing to do, and doing it anyway would cost a new inode, a shadow,
        // and a filesystem event for a document that did not change.
        return Ok(Landed::Unchanged(observed));
    }

    // Stage 3.
    let shadow = shadows.next_shadow();
    let staged = match stage(&shadow, content, &replaced, faults) {
        Ok(staged) => staged,
        Err(refusal) => {
            remove_quietly(&shadow);
            return Err(refusal);
        }
    };
    // Read before the swap: after it, a failure here would have to be reported
    // over a write that already landed.
    let published = match observed_metadata(&staged, &shadow) {
        Ok(metadata) => post_state(composed, content.len() as u64, &metadata),
        Err(refusal) => {
            remove_quietly(&shadow);
            return Err(refusal);
        }
    };

    // Stage 4.
    if let Err(refusal) = verify(&mut held, destination, expected, observed.identity()) {
        remove_quietly(&shadow);
        return Err(refusal);
    }

    // Stage 5.
    if let Err(refusal) = swap(&shadow, destination, faults) {
        remove_quietly(&shadow);
        return Err(refusal);
    }

    // Past the rename the change is at the name and every reader sees it, so
    // what is left is durability alone — and a failure of it is not a write
    // that did not happen.
    sync_directory(shadows.directory(), faults);
    sync_directory(parent_of(destination), faults);
    Ok(Landed::Written(published))
}

/// Put `content` in a shadow nothing else knows the name of, with the replaced
/// file's mode carried forward, and get it onto the disk.
///
/// The mode is carried **here, before the swap**, so the published file has it
/// from the moment its name exists rather than for a moment after. It is
/// best-effort and never fatal: a mode that could not be read or set is a
/// cosmetic loss, and failing the write over it would refuse a correct document
/// to protect a permission bit.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The vault filesystem seam: this crate owns vault handles.
fn stage(
    shadow: &Path,
    content: &[u8],
    replaced: &std::fs::Metadata,
    faults: Faults,
) -> Result<std::fs::File, Refusal> {
    faults
        .check(Stage::Create)
        .map_err(|error| environment("creating", shadow, &error))?;
    // `create_new` and nothing else: a shadow name is unique per attempt, so a
    // name that is somehow taken belongs to somebody else and refuses the write
    // rather than truncating whatever is at it.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(shadow)
        .map_err(|error| environment("creating", shadow, &error))?;

    faults
        .check(Stage::Write)
        .map_err(|error| environment("writing", shadow, &error))?;
    file.write_all(content)
        .map_err(|error| environment("writing", shadow, &error))?;
    carry_mode_forward(&file, replaced);

    faults
        .check(Stage::Sync)
        .map_err(|error| environment("syncing", shadow, &error))?;
    file.sync_all()
        .map_err(|error| environment("syncing", shadow, &error))?;
    Ok(file)
}

/// Put `content` in an already-open handle and get it onto the disk.
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns vault handles.
fn fill(
    file: &mut std::fs::File,
    content: &[u8],
    path: &Path,
    faults: Faults,
) -> Result<(), Refusal> {
    faults
        .check(Stage::Write)
        .map_err(|error| environment("writing", path, &error))?;
    file.write_all(content)
        .map_err(|error| environment("writing", path, &error))?;
    faults
        .check(Stage::Sync)
        .map_err(|error| environment("syncing", path, &error))?;
    file.sync_all()
        .map_err(|error| environment("syncing", path, &error))
}

/// Carry the replaced file's permission mode onto the staged shadow.
///
/// **Replacement is the mechanism; a surgical edit is the observable
/// contract.** What a caller sees is a document whose content outside the edit,
/// mode, path and name are all as they were. What cannot be carried is named
/// rather than papered over: the **inode number** changes, so any **foreign
/// hard link** to the document keeps the old content, and on macOS the
/// **birth time** is the new file's. Extended attributes and access-control
/// lists are not carried either — the standard library has no API for them and
/// no dependency has been taken for one.
#[cfg(unix)]
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The vault filesystem seam: this crate owns vault handles.
fn carry_mode_forward(file: &std::fs::File, replaced: &std::fs::Metadata) {
    use std::os::unix::fs::PermissionsExt;
    let mode = replaced.permissions().mode();
    let _ = file.set_permissions(std::fs::Permissions::from_mode(mode));
}

/// A fresh create takes the umask's defaults, because there is nothing to
/// preserve.
#[cfg(not(unix))]
#[allow(clippy::disallowed_types)]
fn carry_mode_forward(_file: &std::fs::File, _replaced: &std::fs::Metadata) {}

/// Re-observe the destination through the handle the precondition read, and
/// confirm the name still resolves to it.
///
/// Two questions, and each catches what the other cannot. The **re-hash**
/// catches a foreign write into the same file. The **identity comparison**
/// catches a foreign *replacement* of the file — a rename over the destination
/// leaves the held handle pointing at an orphaned inode whose bytes still hash
/// to what the caller expected, so the hash agrees and the swap would clobber
/// somebody's newly published document.
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns vault handles.
fn verify(
    held: &mut std::fs::File,
    destination: &Path,
    expected: ContentHash,
    read: Identity,
) -> Result<(), Refusal> {
    let (again, len) =
        hashed_from(held).map_err(|error| environment("re-reading", destination, &error))?;
    if again != expected {
        let metadata = observed_metadata(held, destination)?;
        return Err(drifted(
            destination,
            expected,
            Some(post_state(again, len, &metadata)),
        ));
    }
    confirm_name_still_resolves(destination, read)
}

/// Publish the shadow's name as the destination's.
///
/// A rename within a directory replaces atomically: a reader arriving at any
/// moment sees the whole previous document or the whole new one, never a prefix
/// of either and never the shadow's name.
#[allow(clippy::disallowed_methods)] // The vault filesystem seam: the swap is this crate's.
fn swap(shadow: &Path, destination: &Path, faults: Faults) -> Result<(), Refusal> {
    faults
        .check(Stage::Swap)
        .map_err(|error| environment("renaming onto", destination, &error))?;
    std::fs::rename(shadow, destination)
        .map_err(|error| environment("renaming onto", destination, &error))
}

/// Confirm `path` still names the file identified by `read`.
fn confirm_name_still_resolves(path: &Path, read: Identity) -> Result<(), Refusal> {
    let current = match name_identity(path)? {
        Some(current) => current,
        None => {
            return Err(Refusal::Republished {
                path: path.to_path_buf(),
                read,
                current: None,
            });
        }
    };
    if current != read {
        return Err(Refusal::Republished {
            path: path.to_path_buf(),
            read,
            current: Some(current),
        });
    }
    Ok(())
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

/// Refuse a path that is a symbolic link.
#[allow(clippy::disallowed_methods)] // The vault filesystem seam: this crate owns vault stat.
fn refuse_symlink(path: &Path) -> Result<(), Refusal> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(Refusal::SymlinkDestination {
            path: path.to_path_buf(),
        }),
        Ok(_) => Ok(()),
        // Nothing there, which a create is entitled to and a replacement will
        // report as drift when it opens.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(environment("reading the type of", path, &error)),
    }
}

/// Open `path` for reading and hand back its handle and its metadata, or `None`
/// when nothing is there.
///
/// `O_NOFOLLOW` closes the window the link check leaves open: a symbolic link
/// planted between that check and this open fails the open rather than being
/// read through, and the failure reads as the refusal the check would have
/// given.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The vault filesystem seam: this crate owns vault handles.
fn opened_for_reading(path: &Path) -> Result<Option<(std::fs::File, std::fs::Metadata)>, Refusal> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Err(Refusal::SymlinkDestination {
                path: path.to_path_buf(),
            });
        }
        Err(error) => return Err(environment("opening", path, &error)),
    };
    let metadata = observed_metadata(&file, path)?;
    Ok(Some((file, metadata)))
}

/// The post-state identity of what `held` holds, hashed through `held` itself.
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns vault handles.
fn observed_state(held: &mut std::fs::File, path: &Path) -> Result<PostState, Refusal> {
    let (hash, len) = hashed_from(held).map_err(|error| environment("reading", path, &error))?;
    let metadata = observed_metadata(held, path)?;
    Ok(post_state(hash, len, &metadata))
}

/// Read `path`'s bytes through one handle and confirm they hash to `expected`.
///
/// The bytes hashed are the bytes returned — one read of one handle — so a
/// caller that acts on the content is acting on the content that satisfied the
/// precondition.
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns vault handles.
fn read_verified(path: &Path, expected: ContentHash) -> Result<Vec<u8>, Refusal> {
    refuse_symlink(path)?;
    let (mut held, _) = match opened_for_reading(path)? {
        Some(open) => open,
        None => return Err(drifted(path, expected, None)),
    };
    let mut content = Vec::new();
    held.read_to_end(&mut content)
        .map_err(|error| environment("reading", path, &error))?;
    let hash = ContentHash::of(&content);
    if hash != expected {
        let metadata = observed_metadata(&held, path)?;
        return Err(drifted(
            path,
            expected,
            Some(post_state(hash, content.len() as u64, &metadata)),
        ));
    }
    Ok(content)
}

/// What the operating system says about an open handle.
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns vault handles.
fn observed_metadata(file: &std::fs::File, path: &Path) -> Result<std::fs::Metadata, Refusal> {
    file.metadata()
        .map_err(|error| environment("reading the identity of", path, &error))
}

/// Remove `path`.
#[allow(clippy::disallowed_methods)] // The vault filesystem seam: this crate owns vault paths.
fn remove(path: &Path) -> Result<(), Refusal> {
    std::fs::remove_file(path).map_err(|error| environment("removing", path, &error))
}

/// Remove a shadow, or a destination this call created and could not finish,
/// and say nothing about a failure to.
///
/// **Cleanup is attempted on every path that does not publish, and its failure
/// is swallowed by design.** Three legs hold that up: it is always attempted, a
/// shadow left behind is inert because its name is unique per attempt, and the
/// residue is bounded because the host sweeps the shadow home. Reporting the
/// failure instead would turn a clean refusal into two problems, and on the
/// create path it would report a landed write as failed.
#[allow(clippy::disallowed_methods)] // The vault filesystem seam: this crate owns vault paths.
fn remove_quietly(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Get a directory's own entries onto the disk.
///
/// **Best-effort, always.** After the swap the change is at the name and every
/// reader sees it, so what is left is whether it survives a power cut. A
/// failure here is never allowed to read as a write that did not happen: a
/// caller that retried on it would write a second time over its own first
/// write.
///
/// This is unix-shaped. There is no portable Windows analogue for fsyncing a
/// directory, and pretending otherwise would put a claim in the contract that
/// nothing carries.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The vault filesystem seam: this crate owns vault paths.
fn sync_directory(directory: &Path, faults: Faults) {
    if faults.check(Stage::ParentSync).is_err() {
        return;
    }
    if let Ok(handle) = std::fs::File::open(directory) {
        let _ = handle.sync_all();
    }
}

/// The directory a path sits in, or the path itself when it names no parent.
///
/// A path with no parent is one the swap could not have reached — a rename onto
/// it would have failed first — so this is the shape of a directory fsync
/// having nothing to sync rather than a case to refuse.
fn parent_of(path: &Path) -> &Path {
    path.parent().unwrap_or(path)
}

fn drifted(path: &Path, expected: ContentHash, observed: Option<PostState>) -> Refusal {
    Refusal::Drifted {
        path: path.to_path_buf(),
        expected,
        observed: observed.map(Box::new),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::shadow::Placement;

    static SERIAL: AtomicU64 = AtomicU64::new(0);

    /// A vault and a shadow home, for the length of one test.
    struct Scratch {
        root: PathBuf,
        shadows: ShadowHome,
    }

    impl Scratch {
        #[allow(clippy::disallowed_methods)] // Harness scaffolding: the tree this test works over.
        fn new(label: &str) -> Scratch {
            let root = std::env::temp_dir().join(format!(
                "norn-fs-write-{label}-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("vault")).expect("a vault root");
            let shadows =
                ShadowHome::resolve(&root.join("vault"), &root.join("tmp")).expect("a shadow home");
            assert_eq!(shadows.placement(), Placement::DataRoot);
            Scratch { root, shadows }
        }

        fn at(&self, relative: &str) -> PathBuf {
            self.root.join("vault").join(relative)
        }

        #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging the bytes a case reads.
        fn place(&self, relative: &str, content: &[u8]) -> PathBuf {
            let path = self.at(relative);
            std::fs::write(&path, content).expect("placing a document");
            path
        }

        #[allow(clippy::disallowed_methods)] // Harness scaffolding: reading what the kernel wrote.
        fn read(&self, path: &Path) -> Vec<u8> {
            std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
        }

        #[allow(clippy::disallowed_methods)] // Harness scaffolding: judging what the protocol left behind.
        fn shadow_count(&self) -> usize {
            std::fs::read_dir(self.shadows.directory())
                .expect("the shadow home")
                .count()
        }
    }

    impl Drop for Scratch {
        #[allow(clippy::disallowed_methods)] // Harness scaffolding: removing the tree this test made.
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// **The bar on the shadow's fsync.** The shadow's bytes are on the disk
    /// before any name points at them, so a stage that cannot sync them refuses
    /// before the swap.
    ///
    /// The forbidden shape is a swap that publishes bytes only the page cache
    /// has seen: a power cut then leaves the destination's name pointing at a
    /// file whose content never reached the platter.
    #[test]
    fn a_shadow_that_cannot_be_synced_refuses_before_the_swap() {
        let scratch = Scratch::new("shadow-sync");
        let path = scratch.place("note.md", b"old");
        let refusal = write_where(
            &path,
            b"new",
            Precondition::Replace(ContentHash::of(b"old")),
            &scratch.shadows,
            Faults::at(Stage::Sync, std::io::ErrorKind::Other),
        )
        .expect_err("a shadow that cannot be synced");

        assert!(
            matches!(&refusal, Refusal::Environment { operation, .. } if *operation == "syncing"),
            "{refusal}"
        );
        assert_eq!(scratch.read(&path), b"old", "the destination was touched");
        assert_eq!(scratch.shadow_count(), 0, "a shadow was left behind");
    }

    /// **The bar on a full disk.** A shadow that cannot take the content
    /// refuses, the destination is byte-identical, and the shadow is cleaned up.
    ///
    /// `ENOSPC` reaches the same arm and is the case this stands in for: the
    /// forbidden shape is a partial shadow renamed into place, which publishes a
    /// truncated document.
    #[test]
    fn a_shadow_that_cannot_take_the_content_refuses_and_cleans_up() {
        let scratch = Scratch::new("shadow-write");
        let path = scratch.place("note.md", b"old");
        let refusal = write_where(
            &path,
            b"new",
            Precondition::Replace(ContentHash::of(b"old")),
            &scratch.shadows,
            Faults::at(Stage::Write, std::io::ErrorKind::Other),
        )
        .expect_err("a shadow that cannot be written");

        assert!(
            matches!(&refusal, Refusal::Environment { operation, .. } if *operation == "writing"),
            "{refusal}"
        );
        assert_eq!(scratch.read(&path), b"old");
        assert_eq!(scratch.shadow_count(), 0, "a shadow was left behind");
    }

    /// **The bar on the swap.** A rename that fails refuses, leaves the
    /// destination as it was, and takes the shadow with it.
    #[test]
    fn a_swap_that_fails_leaves_the_destination_as_it_was() {
        let scratch = Scratch::new("swap");
        let path = scratch.place("note.md", b"old");
        let refusal = write_where(
            &path,
            b"new",
            Precondition::Replace(ContentHash::of(b"old")),
            &scratch.shadows,
            Faults::at(Stage::Swap, std::io::ErrorKind::PermissionDenied),
        )
        .expect_err("a rename that fails");

        assert!(
            matches!(
                &refusal,
                Refusal::Environment { operation, kind, .. }
                    if *operation == "renaming onto" && *kind == std::io::ErrorKind::PermissionDenied
            ),
            "{refusal}"
        );
        assert_eq!(scratch.read(&path), b"old");
        assert_eq!(scratch.shadow_count(), 0, "a shadow was left behind");
    }

    /// **The bar on the parent fsync.** A directory fsync that fails after the
    /// rename never un-lands the write.
    ///
    /// The forbidden shape is reporting it as a failure. The change is at the
    /// name and every reader sees it, so a caller told "nothing happened" writes
    /// a second time over its own first write — and on a create it would remove
    /// a document it had just published.
    #[test]
    fn a_parent_fsync_that_fails_never_un_lands_the_write() {
        let scratch = Scratch::new("parent-sync");
        let path = scratch.place("note.md", b"old");
        let landed = write_where(
            &path,
            b"new",
            Precondition::Replace(ContentHash::of(b"old")),
            &scratch.shadows,
            Faults::at(Stage::ParentSync, std::io::ErrorKind::Other),
        )
        .expect("a landed write whose durability was not confirmed");

        assert!(landed.wrote());
        assert_eq!(scratch.read(&path), b"new");
        assert_eq!(landed.post_state().content_hash, ContentHash::of(b"new"));

        // And the same on the create path, where the failure arm would have to
        // remove a file it had just published.
        let created = scratch.at("fresh.md");
        let landed = write_where(
            &created,
            b"fresh",
            Precondition::Create,
            &scratch.shadows,
            Faults::at(Stage::ParentSync, std::io::ErrorKind::Other),
        )
        .expect("a landed create whose durability was not confirmed");
        assert!(landed.wrote());
        assert_eq!(scratch.read(&created), b"fresh");
    }

    /// **The bar on a create that cannot be finished.** A name this call
    /// claimed and could not fill is given back, so no document is left holding
    /// a prefix of one.
    #[test]
    fn a_create_that_cannot_be_finished_takes_its_name_back() {
        let scratch = Scratch::new("create-write");
        let path = scratch.at("fresh.md");
        let refusal = write_where(
            &path,
            b"fresh",
            Precondition::Create,
            &scratch.shadows,
            Faults::at(Stage::Write, std::io::ErrorKind::Other),
        )
        .expect_err("a create that cannot be written");

        assert!(
            matches!(&refusal, Refusal::Environment { operation, .. } if *operation == "writing"),
            "{refusal}"
        );
        #[allow(clippy::disallowed_methods)] // Asserting on what the failure path left behind.
        let left = std::fs::metadata(&path).is_ok();
        assert!(!left, "a half-written document was left at the destination");
    }

    /// A create that cannot claim its name at all leaves nothing behind and
    /// removes nothing. The injected failure stands in for a full disk met
    /// before the open.
    #[test]
    fn a_create_that_cannot_claim_its_name_leaves_nothing() {
        let scratch = Scratch::new("create-open");
        let existing = scratch.place("taken.md", b"somebody else's bytes");
        let refusal = write_where(
            &existing,
            b"ours",
            Precondition::Create,
            &scratch.shadows,
            Faults::at(Stage::Create, std::io::ErrorKind::Other),
        )
        .expect_err("a create that cannot open");
        assert!(
            matches!(&refusal, Refusal::Environment { operation, .. } if *operation == "creating"),
            "{refusal}"
        );
        assert_eq!(
            scratch.read(&existing),
            b"somebody else's bytes",
            "the failure path removed a file it did not create"
        );
    }
}
