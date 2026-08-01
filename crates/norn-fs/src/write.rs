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
//! 3. **The shadow is staged**: a file nothing else knows the name of, opened
//!    exclusively, given the replaced file's permission bits before a byte of
//!    content goes in, then holding the whole composed content, fsynced before
//!    any name points at it.
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
//! # Two durability barriers, and no third
//!
//! A replacement fsyncs exactly twice: **the shadow's bytes before the rename**,
//! and **the destination's parent directory after it**. The second is
//! best-effort, because past the rename the change is at the name.
//!
//! The barrier that is deliberately absent is an fsync of the *shadow's own*
//! directory before the rename. It costs about a third of a write's latency, and
//! what it buys is that a power cut leaves the shadow's directory entry
//! recorded — a guarantee about residue. Residue is already the one class this
//! protocol is allowed to leak: a shadow that survives a crash is inert and
//! swept, and one whose directory entry did not survive is a shadow that swept
//! itself. Nothing a reader can see depends on it.
//!
//! # Crash windows, as recovery claims
//!
//! A process that dies mid-write leaves one of these, and nothing else:
//!
//! 1. **Inside the shadow write, before its fsync.** The destination is
//!    untouched. The shadow is leaked, and a leaked shadow is inert (see
//!    [`crate::shadow`]) and swept.
//! 2. **After the shadow's fsync, before the rename.** The same: the
//!    destination is untouched and the shadow is leaked-inert. Whether the
//!    shadow's *name* survives a power cut is not guaranteed, and nothing needs
//!    it to.
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
use std::path::{Path, PathBuf};

use crate::faults::{Faults, Stage, Window};
use crate::hash::{ContentHash, hashed_from};
use crate::identity::{Identity, PostState, identity_of, name_identity, post_state};
use crate::refusal::{Refusal, environment};
use crate::shadow::{NAME_ATTEMPTS, ShadowHome};

/// What must be true of the destination for a write to happen.
///
/// The two arms are the whole vocabulary. **A third arm meaning "whatever is
/// there, replace it" is deliberately absent**, and its absence is the
/// contract: an agent must never find overwriting cheaper than merging, and a
/// range that drifted invalidates an edit intent only the caller can derive
/// again.
///
/// The two that exist:
///
/// ```
/// use norn_fs::{ContentHash, Precondition};
///
/// let replace = Precondition::Replace(ContentHash::of(b"what the caller read"));
/// let create = Precondition::Create;
/// assert_ne!(replace, create);
/// ```
///
/// The one that does not, spelled out so that gaining it fails. Every name an
/// unconditional overwrite could plausibly wear is pinned here at once: an arm
/// of this enum is where one would have to appear, and any of these compiling is
/// that having happened.
///
/// ```compile_fail
/// use norn_fs::Precondition;
///
/// let force = Precondition::Force;
/// ```
///
/// ```compile_fail
/// use norn_fs::Precondition;
///
/// let anything = Precondition::Any;
/// ```
///
/// ```compile_fail
/// use norn_fs::Precondition;
///
/// let clobber = Precondition::Overwrite;
/// ```
///
/// And a write with no precondition at all has no spelling either — the argument
/// is not optional and there is no second entry point that omits it.
///
/// ```compile_fail
/// use norn_fs::{ShadowHome, write};
///
/// fn overwrite(destination: &std::path::Path, content: &[u8], shadows: &ShadowHome) {
///     write(destination, content, shadows).expect("an unconditional write");
/// }
/// ```
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
    /// staged and no rename ran.
    ///
    /// **Never reported as written**, and this is why: a suppression path keyed
    /// on Norn's own writes would be primed for a filesystem event that is
    /// never coming, and would then absorb the next foreign change to that
    /// document. The identity carried is the existing file's.
    ///
    /// The bytes are not re-read — there is nothing to compare them against a
    /// second time — but the **name is confirmed to still resolve to the file
    /// they were read from**. This outcome says the destination holds exactly
    /// this content, and a foreign replacement that landed during the
    /// precondition's read makes that false about a file a caller can go and
    /// look at.
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
    /// The identity of what was removed, as it was read just before the unlink.
    ///
    /// **None of the five fields is re-observable after a removal**, so this is
    /// not something a caller can go and check. What it is for is the caller's
    /// own ledger: the removal comes back as a filesystem event about the path
    /// that was vacated, and the correlation is by that path — this value is what
    /// the ledger entry for it says, so an event Norn caused is not re-derived
    /// for nothing.
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
    /// The destination could not be created, and the source was read and not
    /// touched.
    ///
    /// The destination is what a create's cleanup arm promises about: this call
    /// never removes what it did not create, and a cleanup the filesystem blocks
    /// can leave this call's own partial bytes at a name that had nothing at it.
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
    write_disturbed(
        destination,
        content,
        precondition,
        shadows,
        Faults::NONE,
        &mut |_| {},
    )
}

/// [`write`], with a stage made to fail rather than waiting for a machine that
/// fails there.
#[cfg(test)]
fn write_where(
    destination: &Path,
    content: &[u8],
    precondition: Precondition,
    shadows: &ShadowHome,
    faults: Faults,
) -> Result<Landed, Refusal> {
    write_disturbed(
        destination,
        content,
        precondition,
        shadows,
        faults,
        &mut |_| {},
    )
}

/// [`write`], with a stage made to fail and something allowed to happen inside
/// the windows a foreign writer would land in.
///
/// The two parameters are what make the contract's claims checkable at all. A
/// disk that fills mid-shadow and a directory fsync that fails after the rename
/// already published a name are conditions no temporary directory produces; a
/// foreign writer arriving inside a window one call wide is a race nobody can
/// arrange, and a defense against it would otherwise be asserted rather than
/// checked.
fn write_disturbed(
    destination: &Path,
    content: &[u8],
    precondition: Precondition,
    shadows: &ShadowHome,
    faults: Faults,
    disturb: &mut dyn FnMut(Window),
) -> Result<Landed, Refusal> {
    refuse_symlink(destination)?;
    match precondition {
        Precondition::Create => create_exclusively(
            destination,
            content,
            ContentHash::of(content),
            faults,
            disturb,
        ),
        Precondition::Replace(expected) => {
            replace_observed(destination, content, expected, shadows, faults, disturb)
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
    vacate_disturbed(path, expected, &mut |_| {})
}

/// [`vacate`], with something allowed to happen inside the window between the
/// precondition's reading and the identity confirmation.
fn vacate_disturbed(
    path: &Path,
    expected: ContentHash,
    disturb: &mut dyn FnMut(Window),
) -> Result<Vacated, Refusal> {
    refuse_symlink(path)?;
    let (_held, observed, _) = observed_under(path, expected)?;
    disturb(Window::Vacating);
    confirm_name_still_resolves(path, observed.identity(), expected)?;

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
    move_disturbed(source, destination, expected, shadows, &mut |_| {})
}

/// [`move_document`], with something allowed to happen inside the source leg's
/// read window and inside the window between the two legs.
///
/// The shadow home is taken and not used: both legs of a move are an exclusive
/// create and a hash-verified removal, and neither stages a shadow. It is a
/// parameter of the pair for the reason it is a parameter of [`write`] — a
/// precondition is data a caller passes, not a shape the callee picks — and a
/// signature that dropped it would have the two verbs disagree about that.
fn move_disturbed(
    source: &Path,
    destination: &Path,
    expected: ContentHash,
    _shadows: &ShadowHome,
    disturb: &mut dyn FnMut(Window),
) -> Result<Moved, MoveRefusal> {
    let content = read_verified(source, expected, disturb).map_err(MoveRefusal::Source)?;
    // The bytes came back from a read that hashed them to `expected`, so the
    // create's own hash of them is already known and is not computed twice.
    let created = create_destination(destination, &content, expected)
        .map_err(MoveRefusal::Destination)?
        .post_state();
    disturb(Window::BetweenLegs);
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

/// A move's destination leg: the same exclusive create every other create runs,
/// over content whose hash the source leg already established.
fn create_destination(
    destination: &Path,
    content: &[u8],
    composed: ContentHash,
) -> Result<Landed, Refusal> {
    refuse_symlink(destination)?;
    create_exclusively(destination, content, composed, Faults::NONE, &mut |_| {})
}

/// Claim `destination` exclusively and put `content` in it.
///
/// `create_new` is `O_EXCL`: the name is claimed atomically, so a destination
/// that springs into existence between a caller's look and this call refuses
/// with the racer's bytes untouched. No precheck decides this — a precheck
/// would be a second answer to a question the open already answers correctly.
///
/// A failure after the name is claimed takes the name back, but **only while it
/// still means the file this call created**. The identity of the handle is
/// compared against a fresh look at the name, and a mismatch leaves the name
/// alone: between the failure and the cleanup a foreign writer can publish its
/// own document over that name, and a removal by name would delete somebody
/// else's atomically published work while reporting a refusal about this call's.
///
/// What the comparison cannot close is the width of one `unlink`: a foreign
/// publish landing between the comparison and the removal is removed anyway.
/// That window is one call wide and nothing widens it. The claim this arm makes
/// is exact — **it never removes what this call did not create** — and the price
/// is the other direction: a cleanup that is skipped or that the filesystem
/// blocks leaves this call's own bytes, complete or partial, at a name that had
/// nothing at it.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The vault filesystem seam: this crate owns vault handles.
fn create_exclusively(
    destination: &Path,
    content: &[u8],
    composed: ContentHash,
    faults: Faults,
    disturb: &mut dyn FnMut(Window),
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
    // Read from the handle, so it is the identity of the file the exclusive open
    // just made and not of whatever the name means by the time cleanup runs.
    let claimed = identity_of(&observed_metadata(&file, destination)?);

    disturb(Window::Claimed);
    let filled = fill(&mut file, content, destination, faults);
    let state = filled.and_then(|()| observed_metadata(&file, destination));
    let state = match state {
        Ok(metadata) => post_state(composed, content.len() as u64, &metadata),
        Err(refusal) => {
            // A file this call created and could not finish is a file this call
            // takes back — and nothing else is.
            remove_if_still_claimed(destination, claimed, faults);
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
    disturb: &mut dyn FnMut(Window),
) -> Result<Landed, Refusal> {
    // Stage 2. One handle, opened here and held until the swap.
    let (mut held, observed, replaced) = observed_under(destination, expected)?;
    disturb(Window::Composed);

    let composed = ContentHash::of(content);
    if composed == observed.content_hash {
        // Nothing to do, and doing it anyway would cost a new inode, a shadow,
        // and a filesystem event for a document that did not change. The name is
        // still confirmed to resolve to the handle that was read: `Unchanged`
        // says the destination holds exactly this content, and a foreign
        // replacement that landed while the precondition was being read makes
        // that sentence false about a file the caller can see.
        confirm_name_still_resolves(destination, observed.identity(), expected)?;
        return Ok(Landed::Unchanged(observed));
    }

    // Stage 3.
    let (shadow, staged) = open_shadow(shadows, faults)?;
    // Stages 3 through 5 in one arm, because cleanup-on-refusal is one clause and
    // an auditor should find one place it happens rather than four.
    let published = match fill_verify_swap(
        staged,
        &shadow,
        content,
        composed,
        &replaced,
        Destination {
            path: destination,
            held: &mut held,
            expected,
            read: observed.identity(),
        },
        faults,
    ) {
        Ok(published) => published,
        Err(refusal) => {
            remove_quietly(&shadow, faults);
            return Err(refusal);
        }
    };

    // Past the rename the change is at the name and every reader sees it, so
    // what is left is durability alone — and a failure of it is not a write
    // that did not happen.
    sync_directory(parent_of(destination), faults);
    Ok(Landed::Written(published))
}

/// What the swap is about to happen to, and what has to still be true of it.
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns vault handles.
struct Destination<'a> {
    path: &'a Path,
    /// The handle the precondition read, through which the verification re-reads.
    held: &'a mut std::fs::File,
    /// The hash the caller observed and composed against.
    expected: ContentHash,
    /// The identity the name resolved to when the bytes were read.
    read: Identity,
}

/// Fill the staged shadow, verify the destination through the held handle, and
/// publish — the three stages whose failure leaves a shadow to clean up.
///
/// The mode is carried **first, onto an unpublished file with no content in it
/// yet**, and that ordering is the point: an open handle is unaffected by
/// `fchmod`, so the write goes in either way, and the alternative leaves a full
/// replacement of a `0600` document sitting at the umask's defaults for as long
/// as the write and its fsync take. It is best-effort and never fatal — a mode
/// that could not be read or set is a cosmetic loss, and failing the write over
/// it would refuse a correct document to protect a permission bit.
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns vault handles.
fn fill_verify_swap(
    mut staged: std::fs::File,
    shadow: &Path,
    content: &[u8],
    composed: ContentHash,
    replaced: &std::fs::Metadata,
    destination: Destination<'_>,
    faults: Faults,
) -> Result<PostState, Refusal> {
    carry_mode_forward(&staged, replaced);
    fill(&mut staged, content, shadow, faults)?;
    // Read before the swap: after it, a failure here would have to be reported
    // over a write that already landed.
    let published = post_state(
        composed,
        content.len() as u64,
        &observed_metadata(&staged, shadow)?,
    );

    // Stage 4.
    verify(
        destination.held,
        destination.path,
        destination.expected,
        destination.read,
    )?;
    // Stage 5.
    swap(shadow, destination.path, faults)?;
    Ok(published)
}

/// Open a shadow nothing has taken, advancing past any name that is already
/// taken.
///
/// **The open is `create_new` and never anything else.** A shadow name is unique
/// per attempt, so a name that is somehow taken is residue of a previous life —
/// a process whose identifier this one now reuses, most plainly — and truncating
/// it is the forbidden shape: the residue may be a second link to a live
/// document, and the truncation would go through it. So a taken name is skipped
/// rather than opened or refused, because refusing would make one leaked shadow
/// break the first write of every process that inherits its identifier.
///
/// Bounded at [`NAME_ATTEMPTS`]. Exhausting it means every one of that many
/// distinct names is taken, which is the shadow home being unusable rather than
/// a name being unlucky, and it reads as the environmental refusal the last open
/// gave.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The vault filesystem seam: this crate owns vault handles.
fn open_shadow(shadows: &ShadowHome, faults: Faults) -> Result<(PathBuf, std::fs::File), Refusal> {
    open_named_shadow(&mut || shadows.next_shadow(), faults)
}

/// [`open_shadow`], with the names it tries handed to it.
///
/// The generator is what makes the collision arm checkable: a name a test can
/// occupy first is a name it can watch survive.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The vault filesystem seam: this crate owns vault handles.
fn open_named_shadow(
    next: &mut dyn FnMut() -> PathBuf,
    faults: Faults,
) -> Result<(PathBuf, std::fs::File), Refusal> {
    let mut last = None;
    for _ in 0..NAME_ATTEMPTS {
        let shadow = next();
        faults
            .check(Stage::Create)
            .map_err(|error| environment("creating", &shadow, &error))?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&shadow)
        {
            Ok(file) => return Ok((shadow, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                last = Some(environment("creating", &shadow, &error));
            }
            Err(error) => return Err(environment("creating", &shadow, &error)),
        }
    }
    Err(last.expect("a bound of at least one attempt"))
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

/// Carry the replaced file's permission bits onto the staged shadow.
///
/// **Replacement is the mechanism; a surgical edit is the observable
/// contract.** What a caller sees is a document whose content outside the edit,
/// permission bits, path and name are all as they were.
///
/// **The permission bits and nothing else**: the mode is masked to `0o777`, so
/// the **set-user-id and set-group-id bits are dropped**. Carrying them would
/// take a bit that means "run as the file's owner" and put it on a file this
/// process owns — a `04755` document written by one user comes back `04755`
/// owned by another, which is a privilege the document did not have before Norn
/// touched it. A Markdown document has no use for either bit, so there is
/// nothing to weigh against that.
///
/// What cannot be carried is named rather than papered over:
///
/// - **Ownership.** The published file is owned by the writing user and group.
///   Changing that needs privileges Norn does not have and would not ask for, so
///   a document a second user replaces changes hands. This is irreducible.
/// - **The inode number**, which is why any **foreign hard link** to the
///   document keeps the old content, and on macOS why the **birth time** is the
///   new file's.
/// - **Extended attributes and access-control lists** — the standard library has
///   no API for them and no dependency has been taken for one.
///
/// A mode that denies writing is carried like any other and **does not protect
/// the document**: `0444` comes back `0444`, having been replaced. That is
/// ordinary atomic-replace semantics — the rename needs the *directory*, not the
/// file — and it is said here so that a `chmod` is not mistaken for a lock.
#[cfg(unix)]
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The vault filesystem seam: this crate owns vault handles.
fn carry_mode_forward(file: &std::fs::File, replaced: &std::fs::Metadata) {
    use std::os::unix::fs::PermissionsExt;
    let permissions = replaced.permissions().mode() & 0o777;
    let _ = file.set_permissions(std::fs::Permissions::from_mode(permissions));
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
    confirm_name_still_resolves(destination, read, expected)
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
///
/// Two outcomes, and they are two events rather than one shape with a hole in
/// it. A name that resolves to a *different* file was republished by somebody
/// else. A name that resolves to *nothing* is a document that was removed, which
/// is the same event a precondition met at an empty path reports, so it reports
/// it the same way — `expected` is carried for exactly that. A caller re-planning
/// against absence matches one variant either way.
fn confirm_name_still_resolves(
    path: &Path,
    read: Identity,
    expected: ContentHash,
) -> Result<(), Refusal> {
    let Some(current) = name_identity(path)? else {
        return Err(drifted(path, expected, None));
    };
    if current != read {
        return Err(Refusal::Republished {
            path: path.to_path_buf(),
            read,
            current,
        });
    }
    Ok(())
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

/// Open `path`, hash it through the handle this opened, and refuse unless it
/// holds `expected`.
///
/// The prologue every precondition-bearing verb runs, in one place: a
/// replacement and a removal ask the identical question and an answer that
/// differed between them would be two preconditions wearing one name. The handle
/// comes back because the verb that asked is not finished with it — the
/// replacement verifies through it, and the removal confirms the name against
/// what it says.
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns vault handles.
fn observed_under(
    path: &Path,
    expected: ContentHash,
) -> Result<(std::fs::File, PostState, std::fs::Metadata), Refusal> {
    let (mut held, metadata) = match opened_for_reading(path)? {
        Some(open) => open,
        None => return Err(drifted(path, expected, None)),
    };
    let observed = observed_state(&mut held, path)?;
    if observed.content_hash != expected {
        return Err(drifted(path, expected, Some(observed)));
    }
    Ok((held, observed, metadata))
}

/// Read `path`'s bytes through one handle and confirm they hash to `expected`.
///
/// The bytes hashed are the bytes returned — one read of one handle — so a
/// caller that acts on the content is acting on the content that satisfied the
/// precondition.
///
/// And the name is confirmed to still resolve to that handle, for the reason the
/// replacement's verify stage does it: a foreign writer that staged its own copy
/// and renamed it over `path` leaves this handle on an orphaned inode whose bytes
/// hash to exactly what the caller asked for. Without the confirmation a move
/// would publish an orphan's bytes at the destination and then measure the source
/// against them.
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns vault handles.
fn read_verified(
    path: &Path,
    expected: ContentHash,
    disturb: &mut dyn FnMut(Window),
) -> Result<Vec<u8>, Refusal> {
    refuse_symlink(path)?;
    let (mut held, metadata) = match opened_for_reading(path)? {
        Some(open) => open,
        None => return Err(drifted(path, expected, None)),
    };
    let read = identity_of(&metadata);
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
    disturb(Window::SourceRead);
    confirm_name_still_resolves(path, read, expected)?;
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

/// Remove a shadow and say nothing about a failure to.
///
/// **Cleanup is attempted on every path that does not publish, and its failure
/// is swallowed by design.** Three legs hold that up: it is always attempted, a
/// shadow left behind is inert because its name is unique per attempt, and the
/// residue is bounded because the host sweeps the shadow home
/// ([`ShadowHome::sweep`](crate::ShadowHome::sweep)). Reporting the failure
/// instead would turn a clean refusal into two problems, and tell the caller
/// something other than why its write did not happen.
#[allow(clippy::disallowed_methods)] // The vault filesystem seam: this crate owns vault paths.
fn remove_quietly(path: &Path, faults: Faults) {
    if faults.check(Stage::Cleanup).is_err() {
        return;
    }
    let _ = std::fs::remove_file(path);
}

/// Remove a name a create claimed, but only while it still resolves to
/// `claimed`.
///
/// Same swallowed failure as [`remove_quietly`], and one question more. A shadow
/// has a name nothing else computes, so removing it by name is removing this
/// call's own file by construction. A destination's name belongs to the vault and
/// to everybody who writes there, so the file is identified before it is removed:
/// a foreign writer that published over the name between this call's failure and
/// this line is a document this call must not delete.
#[allow(clippy::disallowed_methods)] // The vault filesystem seam: this crate owns vault paths.
fn remove_if_still_claimed(path: &Path, claimed: Identity, faults: Faults) {
    if faults.check(Stage::Cleanup).is_err() {
        return;
    }
    if name_identity(path).ok().flatten() != Some(claimed) {
        return;
    }
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

/// The directory a path sits in.
///
/// Two cases the standard library spells oddly, and both are real. A
/// single-component relative path — `note.md` — has a parent, and it is the
/// **empty path**, which opens as nothing; the directory it means is the working
/// directory, so that is what comes back. A path with no parent at all is a root,
/// which the swap could not have reached, and the path itself is the honest
/// answer for it.
///
/// Getting the first case wrong is silent: the fsync is best-effort, so an open
/// of `""` fails with nothing to report and the published name is left
/// undurable.
fn parent_of(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if parent.as_os_str().is_empty() => Path::new("."),
        Some(parent) => parent,
        None => path,
    }
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
    use super::*;
    use crate::scratch::Scratch;

    /// **The bar on the shadow's fsync.** The shadow's bytes are on the disk
    /// before any name points at them, so a stage that cannot sync them refuses
    /// before the swap.
    ///
    /// The forbidden shape is a swap that publishes bytes only the page cache
    /// has seen: a power cut then leaves the destination's name pointing at a
    /// file whose content never reached the platter.
    #[test]
    fn a_shadow_that_cannot_be_synced_refuses_before_the_swap() {
        let scratch = Scratch::new("write-shadow-sync");
        let path = scratch.place("note.md", b"old");
        let refusal = write_where(
            &path,
            b"new",
            Precondition::Replace(ContentHash::of(b"old")),
            scratch.shadows(),
            Faults::at(&[(Stage::Sync, std::io::ErrorKind::Other)]),
        )
        .expect_err("a shadow that cannot be synced");

        assert!(
            matches!(&refusal, Refusal::Environment { operation, .. } if *operation == "syncing"),
            "{refusal}"
        );
        assert_eq!(scratch.read(&path), b"old", "the destination was touched");
        assert!(
            scratch.shadow_names().is_empty(),
            "a shadow was left behind"
        );
    }

    /// **The bar on a full disk.** A shadow that cannot take the content
    /// refuses, the destination is byte-identical, and the shadow is cleaned up.
    ///
    /// `ENOSPC` reaches the same arm and is the case this stands in for: the
    /// forbidden shape is a partial shadow renamed into place, which publishes a
    /// truncated document.
    #[test]
    fn a_shadow_that_cannot_take_the_content_refuses_and_cleans_up() {
        let scratch = Scratch::new("write-shadow-write");
        let path = scratch.place("note.md", b"old");
        let refusal = write_where(
            &path,
            b"new",
            Precondition::Replace(ContentHash::of(b"old")),
            scratch.shadows(),
            Faults::at(&[(Stage::Write, std::io::ErrorKind::Other)]),
        )
        .expect_err("a shadow that cannot be written");

        assert!(
            matches!(&refusal, Refusal::Environment { operation, .. } if *operation == "writing"),
            "{refusal}"
        );
        assert_eq!(scratch.read(&path), b"old");
        assert!(
            scratch.shadow_names().is_empty(),
            "a shadow was left behind"
        );
    }

    /// **The bar on the swap.** A rename that fails refuses, leaves the
    /// destination as it was, and takes the shadow with it.
    #[test]
    fn a_swap_that_fails_leaves_the_destination_as_it_was() {
        let scratch = Scratch::new("write-swap");
        let path = scratch.place("note.md", b"old");
        let refusal = write_where(
            &path,
            b"new",
            Precondition::Replace(ContentHash::of(b"old")),
            scratch.shadows(),
            Faults::at(&[(Stage::Swap, std::io::ErrorKind::PermissionDenied)]),
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
        assert!(
            scratch.shadow_names().is_empty(),
            "a shadow was left behind"
        );
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
        let scratch = Scratch::new("write-parent-sync");
        let path = scratch.place("note.md", b"old");
        let landed = write_where(
            &path,
            b"new",
            Precondition::Replace(ContentHash::of(b"old")),
            scratch.shadows(),
            Faults::at(&[(Stage::ParentSync, std::io::ErrorKind::Other)]),
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
            scratch.shadows(),
            Faults::at(&[(Stage::ParentSync, std::io::ErrorKind::Other)]),
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
        let scratch = Scratch::new("write-create-write");
        let path = scratch.at("fresh.md");
        let refusal = write_where(
            &path,
            b"fresh",
            Precondition::Create,
            scratch.shadows(),
            Faults::at(&[(Stage::Write, std::io::ErrorKind::Other)]),
        )
        .expect_err("a create that cannot be written");

        assert!(
            matches!(&refusal, Refusal::Environment { operation, .. } if *operation == "writing"),
            "{refusal}"
        );
        assert!(
            !scratch.exists(&path),
            "a half-written document was left at the destination"
        );
    }

    /// **The bar on the create arm's cleanup.** A create that fails after
    /// claiming its name removes only the file it created, so a foreign document
    /// published at that name in the meantime survives.
    ///
    /// The forbidden shape is a cleanup that unlinks by name. A foreign writer
    /// with its own atomic-replace protocol lands a complete document at the name
    /// while this call is failing; an unlink by name then deletes it, and the
    /// caller is handed a refusal about a write that never happened while
    /// somebody else's published work is gone. The identity comparison is what
    /// distinguishes the two files.
    #[test]
    fn a_create_that_fails_never_removes_a_foreign_document() {
        let scratch = Scratch::new("write-create-foreign");
        let path = scratch.at("fresh.md");
        let mut landed_first = false;
        let refusal = write_disturbed(
            &path,
            b"ours",
            Precondition::Create,
            scratch.shadows(),
            Faults::at(&[(Stage::Write, std::io::ErrorKind::Other)]),
            &mut |window| {
                if window != Window::Claimed {
                    return;
                }
                // A foreign writer publishing over the name this call just
                // claimed: staged elsewhere, renamed into place.
                #[allow(clippy::disallowed_methods)]
                // Harness scaffolding: playing the foreign writer.
                {
                    let theirs = scratch.at("theirs");
                    std::fs::write(&theirs, b"a foreign document").expect("a foreign staging");
                    std::fs::rename(&theirs, &path).expect("a foreign publish");
                }
                landed_first = true;
            },
        )
        .expect_err("a create that cannot be written");

        assert!(landed_first, "the window was never entered");
        assert!(
            matches!(&refusal, Refusal::Environment { operation, .. } if *operation == "writing"),
            "{refusal}"
        );
        assert_eq!(
            scratch.read(&path),
            b"a foreign document",
            "the cleanup removed a document this call did not create"
        );
    }

    /// **The bar on the create arm's blocked cleanup.** A removal the filesystem
    /// refuses does not change the refusal the caller is given, and what it
    /// leaves is this call's own file.
    ///
    /// Two failures are injected: the write, which is why the create refuses, and
    /// the removal after it. The forbidden shape is reporting the cleanup — the
    /// caller is then told something other than why its write did not happen.
    /// What that costs is stated rather than claimed away: the name holds this
    /// call's own bytes, and nothing else ever will.
    #[test]
    fn a_create_whose_cleanup_cannot_happen_still_returns_the_original_outcome() {
        let scratch = Scratch::new("write-create-cleanup");
        let path = scratch.at("fresh.md");
        let refusal = write_where(
            &path,
            b"fresh",
            Precondition::Create,
            scratch.shadows(),
            Faults::at(&[
                (Stage::Write, std::io::ErrorKind::Other),
                (Stage::Cleanup, std::io::ErrorKind::PermissionDenied),
            ]),
        )
        .expect_err("a create that cannot be written with a removal that cannot happen");

        assert!(
            matches!(
                &refusal,
                Refusal::Environment { operation, kind, .. }
                    if *operation == "writing" && *kind == std::io::ErrorKind::Other
            ),
            "the caller was told about the cleanup instead of the write: {refusal}"
        );
        assert_eq!(
            scratch.read(&path),
            b"",
            "the blocked cleanup ran anyway, so the seam is not what stopped it"
        );
    }

    /// **The bar on the verification.** A foreign write that lands inside the
    /// window is caught before the swap, and the foreign bytes are what remain.
    ///
    /// The window is one call wide, so the foreign writer is injected into it
    /// rather than raced for. The forbidden shape is a protocol that checks the
    /// precondition and then renames: the foreign write is silently overwritten,
    /// and the caller is told its own content landed over the bytes it was
    /// composed against.
    #[test]
    fn a_foreign_write_inside_the_window_is_caught_before_the_swap() {
        let scratch = Scratch::new("write-verify-drift");
        let path = scratch.place("note.md", b"old");
        let mut landed_first = false;
        let refusal = write_disturbed(
            &path,
            b"ours",
            Precondition::Replace(ContentHash::of(b"old")),
            scratch.shadows(),
            Faults::NONE,
            &mut |window| {
                if window != Window::Composed {
                    return;
                }
                // A foreign writer editing the same file: the inode is the same
                // and only the bytes moved, which is what the re-hash sees.
                #[allow(clippy::disallowed_methods)]
                // Harness scaffolding: playing the foreign writer.
                std::fs::write(&path, b"theirs").expect("a foreign write");
                landed_first = true;
            },
        )
        .expect_err("a foreign write inside the window");

        assert!(landed_first, "the window was never entered");
        let Refusal::Drifted {
            expected, observed, ..
        } = &refusal
        else {
            panic!("a foreign write was not reported as drift: {refusal}");
        };
        assert_eq!(*expected, ContentHash::of(b"old"));
        assert_eq!(
            observed.as_ref().expect("the observed state").content_hash,
            ContentHash::of(b"theirs")
        );
        assert_eq!(
            scratch.read(&path),
            b"theirs",
            "the swap ran over the foreign writer"
        );
        assert!(
            scratch.shadow_names().is_empty(),
            "a shadow was left behind"
        );
    }

    /// **The bar the hash cannot carry.** A foreign writer that *replaced* the
    /// document leaves bytes that still hash to what the caller observed, and the
    /// identity comparison is what refuses.
    ///
    /// This is the whole reason one handle binds the protocol. Under two opens by
    /// name, the second read would see the newcomer and agree with the
    /// precondition; under one handle, the held description still refers to the
    /// orphaned inode, so the hash agrees for the wrong reason and only the
    /// identity says so. The forbidden shape is a verify that compares content
    /// alone: it publishes over a document somebody else just published.
    #[test]
    fn a_foreign_replacement_that_keeps_the_bytes_is_still_refused() {
        let scratch = Scratch::new("write-verify-republished");
        let path = scratch.place("note.md", b"old");
        let original = identity_at(&path);

        let refusal = write_disturbed(
            &path,
            b"ours",
            Precondition::Replace(ContentHash::of(b"old")),
            scratch.shadows(),
            Faults::NONE,
            &mut |window| {
                if window == Window::Composed {
                    republish(&scratch, &path, b"old");
                }
            },
        )
        .expect_err("a foreign replacement inside the window");

        let Refusal::Republished { read, current, .. } = &refusal else {
            panic!("a foreign replacement was not reported as one: {refusal}");
        };
        assert_eq!(*read, original);
        assert_ne!(*current, original);
        assert_eq!(
            scratch.read(&path),
            b"old",
            "the swap ran over the foreign writer"
        );
        assert!(
            scratch.shadow_names().is_empty(),
            "a shadow was left behind"
        );
    }

    /// **The bar on the unchanged outcome's honesty.** Content identical to what
    /// was read is only `Unchanged` while the name still resolves to the file it
    /// was read from.
    ///
    /// The forbidden shape is returning the outcome on the strength of the hash
    /// alone. A foreign writer that replaced the document with a copy of itself
    /// leaves the held handle on an orphaned inode, so the hashes agree and the
    /// caller is told the destination holds exactly this content — about a file
    /// nothing points at. Its own write is suppressed for a document it never
    /// looked at.
    #[test]
    fn an_unchanged_outcome_whose_name_was_republished_is_refused() {
        let scratch = Scratch::new("write-unchanged-republished");
        let path = scratch.place("note.md", b"already right");
        let original = identity_at(&path);

        let refusal = write_disturbed(
            &path,
            b"already right",
            Precondition::Replace(ContentHash::of(b"already right")),
            scratch.shadows(),
            Faults::NONE,
            &mut |window| {
                if window == Window::Composed {
                    republish(&scratch, &path, b"already right");
                }
            },
        )
        .expect_err("an unchanged outcome over a republished name");

        let Refusal::Republished { read, current, .. } = &refusal else {
            panic!("a republished name was reported as unchanged: {refusal}");
        };
        assert_eq!(*read, original);
        assert_ne!(*current, original);
    }

    /// A document removed inside the window is drift onto nothing — the same
    /// answer a precondition met at an empty path gets, because it is the same
    /// event — and the swap does not resurrect it.
    #[test]
    fn a_document_removed_inside_the_window_is_not_resurrected() {
        let scratch = Scratch::new("write-verify-removed");
        let path = scratch.place("note.md", b"old");
        let refusal = write_disturbed(
            &path,
            b"ours",
            Precondition::Replace(ContentHash::of(b"old")),
            scratch.shadows(),
            Faults::NONE,
            &mut |window| {
                if window != Window::Composed {
                    return;
                }
                #[allow(clippy::disallowed_methods)]
                // Harness scaffolding: playing the foreign writer.
                std::fs::remove_file(&path).expect("a foreign removal");
            },
        )
        .expect_err("a removal inside the window");

        assert!(
            matches!(&refusal, Refusal::Drifted { observed: None, .. }),
            "{refusal}"
        );
        assert!(
            !scratch.exists(&path),
            "the swap ran after the document was removed"
        );
        assert!(
            scratch.shadow_names().is_empty(),
            "a shadow was left behind"
        );
    }

    /// **The bar on a cleanup that cannot happen.** A shadow the filesystem
    /// refuses to remove does not change the refusal the caller is given, and
    /// what it leaks is inert.
    ///
    /// Two failures are injected: the swap, which is why the write refuses, and
    /// the removal after it, which is why the shadow is still there. The
    /// forbidden shape is reporting the cleanup's failure — a clean refusal
    /// becomes two problems, and the caller is told something other than why its
    /// write did not happen. What the leak costs is bounded instead: its name is
    /// one nothing recomputes, so nothing reopens it, and the host's sweep clears
    /// it.
    #[test]
    fn a_cleanup_that_cannot_happen_still_returns_the_original_outcome() {
        let scratch = Scratch::new("write-cleanup-blocked");
        let path = scratch.place("note.md", b"old");
        let refusal = write_where(
            &path,
            b"new",
            Precondition::Replace(ContentHash::of(b"old")),
            scratch.shadows(),
            Faults::at(&[
                (Stage::Swap, std::io::ErrorKind::PermissionDenied),
                (Stage::Cleanup, std::io::ErrorKind::PermissionDenied),
            ]),
        )
        .expect_err("a rename that fails with a removal that cannot happen");

        assert!(
            matches!(
                &refusal,
                Refusal::Environment { operation, kind, .. }
                    if *operation == "renaming onto"
                        && *kind == std::io::ErrorKind::PermissionDenied
            ),
            "the caller was told about the cleanup instead of the write: {refusal}"
        );
        assert_eq!(scratch.read(&path), b"old", "the destination was touched");

        let leaked = scratch.shadow_names();
        assert_eq!(
            leaked.len(),
            1,
            "expected one leaked shadow, got {leaked:?}"
        );
        assert!(
            crate::shadow::is_shadow_name(std::ffi::OsStr::new(&leaked[0])),
            "the leaked shadow {:?} is not recognizable as ours, so no sweep would clear it",
            leaked[0]
        );

        // And the leak is inert: the next write to the same document neither
        // trips over it nor touches it.
        write_where(
            &path,
            b"new",
            Precondition::Replace(ContentHash::of(b"old")),
            scratch.shadows(),
            Faults::NONE,
        )
        .expect("a replacement over a leaked shadow");
        assert_eq!(scratch.read(&path), b"new");
        assert_eq!(
            scratch.shadow_names(),
            leaked,
            "the leaked shadow was reopened or removed by a later write"
        );
    }

    /// **The bar on the mask.** Carrying a mode forward carries the permission
    /// bits and drops the set-user-id and set-group-id bits.
    ///
    /// Asked of the carry itself, because the published answer has more than one
    /// cause: some kernels strip these bits again when a file is written, so an
    /// end-to-end case agrees with the mask on those platforms whether the mask is
    /// there or not. The forbidden shape is carrying `st_mode` whole — "run as the
    /// file's owner" then names the user who ran the write, on a document that
    /// never granted it.
    #[test]
    fn carrying_a_mode_forward_drops_the_setuid_bits() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new("write-mode-mask");
        let source = scratch.place("note.md", b"old");
        scratch.set_mode(&source, 0o6755);
        let (document, replaced) = opened_for_reading(&source)
            .expect("the document")
            .expect("a document");
        let _ = document;
        assert_eq!(
            replaced.permissions().mode() & 0o7777,
            0o6755,
            "the filesystem did not keep the bits this case is about"
        );

        let (shadow, staged) = open_shadow(scratch.shadows(), Faults::NONE).expect("a shadow");
        carry_mode_forward(&staged, &replaced);

        assert_eq!(
            scratch.mode_at(&shadow),
            0o755,
            "the carry put a set-user-id or set-group-id bit on a file this process owns"
        );
    }

    /// **The bar on when the mode is carried.** A staged shadow has the replaced
    /// file's permission bits before any content goes into it.
    ///
    /// Read off a shadow that leaked before its first byte: the write is made to
    /// fail at the content, and the cleanup is made to fail after it, so what is
    /// left at rest is the file exactly as staging made it.
    ///
    /// The forbidden shape is carrying the mode after the content. A full
    /// replacement of a `0600` document then sits in the shadow home at the
    /// umask's defaults — world-readable on an ordinary machine — for as long as
    /// the write and its fsync take, which for a large document is not a moment.
    #[test]
    fn a_shadow_carries_its_mode_before_any_content() {
        let scratch = Scratch::new("write-mode-first");
        let path = scratch.place("note.md", b"old");
        scratch.set_mode(&path, 0o600);

        write_where(
            &path,
            b"new",
            Precondition::Replace(ContentHash::of(b"old")),
            scratch.shadows(),
            Faults::at(&[
                (Stage::Write, std::io::ErrorKind::Other),
                (Stage::Cleanup, std::io::ErrorKind::PermissionDenied),
            ]),
        )
        .expect_err("a shadow that cannot take the content");

        let leaked = scratch.shadow_names();
        assert_eq!(
            leaked.len(),
            1,
            "expected one leaked shadow, got {leaked:?}"
        );
        let leaked = scratch.shadows().directory().join(&leaked[0]);
        assert_eq!(
            scratch.read(&leaked),
            b"",
            "the shadow took content, so this is not the pre-content state"
        );
        assert_eq!(
            scratch.mode_at(&leaked),
            0o600,
            "a shadow held content-shaped bytes at a mode the document did not have"
        );
    }

    /// **The bar on a shadow name that is already taken.** The taken name is
    /// skipped, the write lands under the next one, and what was at the taken
    /// name is neither truncated nor reopened.
    ///
    /// The forbidden shape is an open that creates or truncates. A process
    /// identifier is reused across boots, so a shadow leaked by a dead writer is
    /// a name a fresh process really does compute — and if that residue had
    /// become a second link to a live document, the truncation would go through
    /// it. Refusing instead is the other forbidden shape: one leaked shadow would
    /// then break the first write of every process that inherits its identifier.
    #[test]
    fn a_taken_shadow_name_is_skipped_rather_than_opened() {
        let scratch = Scratch::new("write-shadow-collision");
        let taken = scratch.shadows().directory().join("norn-shadow-1-0");
        let free = scratch.shadows().directory().join("norn-shadow-1-1");
        #[allow(clippy::disallowed_methods)] // Harness scaffolding: a dead writer's residue.
        std::fs::write(&taken, b"a dead writer's bytes").expect("residue");
        let residue = identity_at(&taken);

        let mut names = vec![free.clone(), taken.clone()];
        let (opened, mut handle) =
            open_named_shadow(&mut || names.pop().expect("a name to try"), Faults::NONE)
                .expect("a shadow under a free name");

        assert_eq!(opened, free, "the taken name was opened");
        assert_eq!(
            scratch.read(&taken),
            b"a dead writer's bytes",
            "the residue was truncated or written through"
        );
        assert_eq!(identity_at(&taken), residue, "the residue was replaced");
        // And the handle is a usable, empty shadow rather than something reopened.
        handle.write_all(b"ours").expect("writing the shadow");
        assert_eq!(scratch.read(&opened), b"ours");
    }

    /// **The bar on the removal's identity confirmation.** A document replaced
    /// while its removal's precondition was being read is refused, and the
    /// replacement is still there.
    ///
    /// The forbidden shape is a removal justified by the hash alone. A foreign
    /// writer that published a copy of the same bytes leaves the read handle on
    /// an orphaned inode, so the hash agrees, and the unlink then takes a
    /// document nobody read.
    #[test]
    fn a_removal_whose_name_was_republished_is_refused() {
        let scratch = Scratch::new("write-vacate-republished");
        let path = scratch.place("note.md", b"to be removed");
        let original = identity_at(&path);

        let refusal = vacate_disturbed(&path, ContentHash::of(b"to be removed"), &mut |window| {
            if window == Window::Vacating {
                republish(&scratch, &path, b"to be removed");
            }
        })
        .expect_err("a removal over a republished name");

        let Refusal::Republished { read, current, .. } = &refusal else {
            panic!("a republished name was not reported as one: {refusal}");
        };
        assert_eq!(*read, original);
        assert_ne!(*current, original);
        assert_eq!(
            scratch.read(&path),
            b"to be removed",
            "the removal took a document nobody read"
        );
    }

    /// **The bar on the move's source read.** A source republished while it is
    /// being read refuses the source leg, so nothing is published from an
    /// orphaned inode's bytes.
    ///
    /// The forbidden shape is a read that hashes and returns. The bytes would
    /// satisfy the precondition — a foreign atomic replace can keep them exactly
    /// — while belonging to a file the name no longer resolves to; the move would
    /// then publish them at the destination and go on to measure the source
    /// against them.
    #[test]
    fn a_move_whose_source_is_republished_while_it_is_read_is_refused() {
        let scratch = Scratch::new("write-move-source-read");
        let source = scratch.place("from.md", b"the document");
        let destination = scratch.at("to.md");

        let refusal = move_disturbed(
            &source,
            &destination,
            ContentHash::of(b"the document"),
            scratch.shadows(),
            &mut |window| {
                if window == Window::SourceRead {
                    republish(&scratch, &source, b"the document");
                }
            },
        )
        .expect_err("a source republished while it was read");

        assert!(
            matches!(&refusal, MoveRefusal::Source(Refusal::Republished { .. })),
            "{refusal}"
        );
        assert!(
            !scratch.exists(&destination),
            "a refused source leg created the destination"
        );
        assert_eq!(scratch.read(&source), b"the document");
    }

    /// **The bar on the move's second leg, through the move itself.** A source
    /// rewritten between the legs is not removed: the vacate has a precondition
    /// of its own, which is why the source is read twice.
    ///
    /// The forbidden shape is one handle, or one reading, held across both legs.
    /// The removal would then be justified by a reading taken before the
    /// destination existed, and a document somebody edited in between would be
    /// gone. The residue this leaves is the one the pair promises — both paths
    /// hold a document, never neither.
    #[test]
    fn a_move_whose_source_is_rewritten_between_the_legs_keeps_it() {
        let scratch = Scratch::new("write-move-between-legs");
        let source = scratch.place("from.md", b"the document");
        let destination = scratch.at("to.md");
        let mut rewritten = false;

        let refusal = move_disturbed(
            &source,
            &destination,
            ContentHash::of(b"the document"),
            scratch.shadows(),
            &mut |window| {
                if window != Window::BetweenLegs {
                    return;
                }
                #[allow(clippy::disallowed_methods)]
                // Harness scaffolding: playing the foreign writer.
                std::fs::write(&source, b"what somebody else wrote").expect("a foreign write");
                rewritten = true;
            },
        )
        .expect_err("a source rewritten between the legs");

        assert!(rewritten, "the window was never entered");
        let MoveRefusal::SourceRemains { created, refusal } = &refusal else {
            panic!("the leg that refused was not named: {refusal}");
        };
        assert_eq!(created.content_hash, ContentHash::of(b"the document"));
        assert!(matches!(refusal, Refusal::Drifted { .. }), "{refusal}");
        assert_eq!(
            scratch.read(&source),
            b"what somebody else wrote",
            "the second leg removed a source nobody looked at again"
        );
        assert_eq!(scratch.read(&destination), b"the document");
    }

    /// **The bar on `O_NOFOLLOW`.** A symbolic link planted at a path between the
    /// link check and the reading open fails the open, and reads as the refusal
    /// the check would have given.
    ///
    /// Driven at the open directly, because that gap is a window one call wide.
    /// The forbidden shape is an open without the flag: the link is followed, the
    /// precondition is evaluated against a file somewhere else, and the swap
    /// publishes at a name the caller did not ask about.
    #[test]
    fn the_reading_open_refuses_a_symlink_rather_than_following_it() {
        let scratch = Scratch::new("write-nofollow");
        let real = scratch.place("real.md", b"the target's bytes");
        let link = scratch.at("link.md");
        #[allow(clippy::disallowed_methods)] // Harness scaffolding: planting a link.
        std::os::unix::fs::symlink(&real, &link).expect("a link");

        let refusal = opened_for_reading(&link).expect_err("a link at the open's path");
        assert_eq!(
            refusal,
            Refusal::SymlinkDestination { path: link.clone() },
            "{refusal}"
        );
    }

    /// The directory a destination sits in, including the two spellings the
    /// standard library answers oddly.
    ///
    /// The forbidden shape is `path.parent().unwrap_or(path)`. A single-component
    /// relative destination has `Some("")` as its parent, `""` opens as nothing,
    /// and the fsync is best-effort — so the published name is silently left
    /// undurable.
    #[test]
    fn the_parent_of_a_destination_is_a_directory_that_can_be_opened() {
        assert_eq!(parent_of(Path::new("/vault/note.md")), Path::new("/vault"));
        assert_eq!(parent_of(Path::new("folder/note.md")), Path::new("folder"));
        assert_eq!(
            parent_of(Path::new("note.md")),
            Path::new("."),
            "a single-component destination named a directory nothing can open"
        );
        assert_eq!(parent_of(Path::new("/")), Path::new("/"));

        #[allow(clippy::disallowed_methods)] // Asserting that the answer is openable.
        for path in ["/vault/note.md", "note.md", "/"] {
            let parent = parent_of(Path::new(path));
            assert!(
                !parent.as_os_str().is_empty(),
                "the parent of {path} is the empty path, which opens as nothing"
            );
        }
    }

    /// A foreign writer with its own atomic-replace protocol: `content` at
    /// `path`, in a different file.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: playing the foreign writer.
    fn republish(scratch: &Scratch, path: &Path, content: &[u8]) {
        let theirs = scratch.at("norn-fs-foreign-staging");
        std::fs::write(&theirs, content).expect("a foreign staging");
        std::fs::rename(&theirs, path).expect("a foreign publish");
    }

    /// The `(device, inode)` pair `path` resolves to.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: judging which file a name means.
    fn identity_at(path: &Path) -> Identity {
        identity_of(&std::fs::metadata(path).expect("a file at the path"))
    }
}
