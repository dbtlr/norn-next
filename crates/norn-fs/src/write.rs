//! The staging and publishing kernel: the one way vault bytes change.
//!
//! A change to one file is a [`Transition`]: a **create**, which needs nothing
//! at the name; a **replace** and a **remove**, which each need the hash the
//! caller composed against; and a **respell**, a case-only rename on a root
//! that folds case, which needs the hash too. **There is no transition without
//! a before-state**, and that is the point — an unconditional overwrite has no
//! spelling here, so no plan, verb or flag can reach one through this crate.
//!
//! Every transition runs in two phases, and **no handle is held between
//! them**. A plan stages every one of its targets before it publishes any,
//! so a refusal found while staging writes nothing (ADR 0031); the kernel
//! offers the per-target phases and the applier composes them.
//!
//! **A target at its after-state has landed, whichever writer put it there.**
//! Staging reports it as [`Staging::Landed`]; publication reports it as
//! [`Publication::Found`], discards the shadow and syncs the folder. Only
//! [`Publication::Wrote`] says this call changed the vault.
//!
//! # Staging
//!
//! [`stage`] checks the target and, for a write, stages its content. Nothing
//! it does is visible in the vault, and it makes no folder.
//!
//! 1. **The root is opened as it is spelled** and its `(device, inode)` is
//!    recorded. The root is the boundary, not a name inside it.
//! 2. **The target is reached from the root one folder at a time**, each
//!    opened with `O_NOFOLLOW` and `O_DIRECTORY`, so a linked folder refuses
//!    as [`Refusal::LinkedAncestor`] instead of carrying the change out of the
//!    vault. A folder that is not there means the target is absent; a folder
//!    that is a file refuses.
//! 3. **The target is read through a no-follow, non-blocking open** whose kind
//!    is proven through the descriptor, hashed through that descriptor, and
//!    confirmed to still be the file at the name. A target already at its
//!    after-state stages as landed, with no shadow; a target at neither state
//!    refuses.
//! 4. **A write's content goes into a fresh shadow**, opened exclusively in the
//!    shadow home, given a replaced file's permission bits before a byte of
//!    content goes in, and fsynced. [`Staged`] records the shadow's name and
//!    `(device, inode)` and nothing that holds it open.
//!
//! A staging refusal leaves no shadow behind.
//!
//! # Publication
//!
//! [`publish`] asks every question again, because the world had the whole
//! interval between the phases to change:
//!
//! 1. **The root is reopened** and refuses as [`Refusal::RootReplaced`] when
//!    its `(device, inode)` is not the one staging recorded.
//! 2. **The descent is made again** to the target's folder, through no link.
//! 3. **The shadow is confirmed**: opened through the shadow home's
//!    directory without following a link, compared by identity with what
//!    staging made, and hashed again. A shadow that is gone or changed is an
//!    environmental refusal for this target — an I/O failure, never drift —
//!    because a staged shadow outlives its staging call and a sweep or a sync
//!    client can reach it.
//! 4. **The target is verified again** exactly as staging read it: at its
//!    after-state it is found landed, and otherwise a create must still be
//!    absent, and a replace or a remove still at its before-state.
//! 5. **A create makes its missing folders**, one level at a time with
//!    `mkdirat` and each opened again through no link, just before its
//!    rename. A create refused after that removes the folders it made that
//!    are still empty.
//! 6. **The publication act runs through the folder's handle.** A create is a
//!    rename that never replaces a name (`renameat2` with `RENAME_NOREPLACE`
//!    on Linux, `renameatx_np` with `RENAME_EXCL` on macOS); a replace is a
//!    plain rename; a remove is an `unlinkat`. A name taken at the last
//!    moment refuses the create as [`Refusal::DestinationExists`], and a
//!    filesystem that cannot rename exclusively refuses it as
//!    [`Refusal::ExclusiveCreateUnsupported`] — there is no fallback, because
//!    every fallback is a check followed by a rename.
//! 7. **The folder is synced through the same handle**, and so is each folder
//!    that holds one the create made. The result is reported as
//!    [`Durability`] rather than dropped.
//!
//! A refusal before the publication act removes the target's shadow, where the
//! shadow is still the file staging made.
//!
//! # A respell
//!
//! A case-only rename is refused on a root that tells the two spellings apart:
//! there they are two names, and renaming one onto the other is a move. On a
//! root that folds, the spelling that counts is the one the folder's listing
//! holds, and the fold is proven on that folder, from the target's own entry,
//! before anything is judged. The respell is then one of three states — the
//! old spelling at the before-state, the old spelling at the after-state
//! (halfway: the content changed and the rename did not happen), or the new
//! spelling at the after-state (landed) — and anything else is drift.
//! Publication is two steps: where the content changes and has not yet, an
//! ordinary replace under the old spelling; then, with the after-state read
//! again, a plain rename of the old spelling to the new. Both folder syncs are
//! in the durability reported. `RENAME_NOREPLACE` cannot serve the second
//! step, because on a folding root the new spelling *is* the entry.
//!
//! # What durability means here
//!
//! Past the publication act the change is at the name and every reader sees
//! it, so a failed folder sync is never a write that did not happen: a caller
//! told "nothing happened" would write a second time over its own first
//! write. It is not silent either. [`Published::durability`] carries
//! [`Durability::NotSynced`] with the error, and what that means is the
//! caller's: an applier publishing a plan whose later targets depend on this
//! one having durably landed stops there. [`confirm_landed`] and a found
//! landing make the same sync, so a plan finished by re-sending it reaches the
//! same durability as one that ran whole.
//!
//! On macOS a sync is `F_FULLFSYNC`, because a plain `fsync` there reaches the
//! drive's cache and not its platter.
//!
//! # The residual race, stated rather than claimed away
//!
//! **The window between the last verification and the publication act cannot
//! be closed.** POSIX offers no compare-and-rename, so a foreign write into the
//! target in that window is overwritten by a replace or a respell, or taken by
//! a remove. The window is the width of one call. A create has no such window:
//! the exclusive rename is its whole question.
//!
//! **A folder a foreign writer moves after the descent receives the
//! publication at its new place.** The handle is the folder, not its name, so
//! the publication follows the folder; nothing leaves the vault through a
//! link, and the watcher reports where the document landed.
//!
//! What either costs is bounded by the watcher rather than by exclusion:
//! whatever is finally at the path is what the watcher reports, and derived
//! state converges on it.
//!
//! # Crash windows, as recovery claims
//!
//! A process that dies at any point leaves the target old-complete or
//! new-complete, never a mixture, and at most one inert shadow (see
//! [`crate::shadow`]). A create is no exception: its content is staged and
//! renamed like a replacement's, so no name ever holds a prefix of it. A
//! create that dies after making folders may leave them empty; a respell that
//! dies between its steps leaves the halfway state a re-send finishes. Past
//! the publication act and before the folder sync, a power cut may lose the
//! directory entry, which a re-send's [`confirm_landed`] repairs.
//!
//! # Single-shot outcomes
//!
//! Nothing here retries. Observed drift is returned, not absorbed: a caller
//! that wants to try again reads the observed state, re-composes against it,
//! and stages once more, under a bound it declared.

use std::ffi::{OsStr, OsString};
use std::io::Write as _;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{
    AtFlags, Dir, FileType, Mode, OFlags, RenameFlags, fstat, mkdirat, open, openat, renameat,
    renameat_with, statat, unlinkat,
};
use rustix::io::Errno;

use crate::faults::{Faults, Stage, Window};
use crate::hash::{ContentHash, hashed_from};
use crate::identity::{Identity, PostState, identity_of, identity_of_stat, post_state};
use crate::open::{anchor_flags, directory_flags, regular_flags};
use crate::path::{CaseSensitivity, Lookup, fold_together, probe_case_behavior_by};
use crate::refusal::{Refusal, environment, environment_at};
use crate::shadow::{NAME_ATTEMPTS, ShadowHome};

/// One file's change, as the kernel is asked to make it: the state the file
/// must hold before, and for a write the content it holds after.
///
/// The four kinds are the whole vocabulary a plan resolves into. **A kind
/// meaning "whatever is there, replace it" is deliberately absent**, and its
/// absence is the contract: an agent must never find overwriting cheaper than
/// merging.
///
/// ```compile_fail
/// use norn_fs::Transition;
///
/// let force = Transition::Overwrite { content: b"anything" };
/// ```
///
/// ```compile_fail
/// use norn_fs::Transition;
///
/// let unconditional = Transition::Replace { content: b"anything" };
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Transition<'a> {
    /// Nothing may be at the name, and `content` is what will be.
    Create { content: &'a [u8] },
    /// The file must hash to `before`, and `content` is what will be there.
    Replace {
        before: ContentHash,
        content: &'a [u8],
    },
    /// The file must hash to `before`, and nothing will be there.
    Remove { before: ContentHash },
    /// The file must hash to `before`, and will be spelled `to` — the staged
    /// path with only the ASCII case of its final name changed — holding
    /// `content`, or its own bytes where `content` is `None`.
    ///
    /// Only on a root proven to fold case, where the two spellings are one
    /// entry. Anywhere else they are two names, and the change is a move.
    Respell {
        to: &'a Path,
        before: ContentHash,
        content: Option<&'a [u8]>,
    },
}

/// What staging found a target needs.
#[derive(Debug, Eq, PartialEq)]
pub enum Staging {
    /// The target is at its before-state and is ready to publish.
    Staged(Staged),
    /// The target already holds its after-state, so nothing was staged. A
    /// re-send of a plan that landed this target meets this, and
    /// [`confirm_landed`] is what makes the landing durable.
    Landed(Landed),
}

/// A target checked and staged, waiting for [`publish`] or [`discard`].
///
/// **Plain data and no handle.** A plan holds one of these per target between
/// the phases, and a handle held that long would be one open descriptor per
/// document of a vault-wide plan — and would tempt publication to trust what
/// the handle saw instead of asking again. What is here is what publication
/// needs to ask: which root, which name below it, which transition, and which
/// shadow.
#[derive(Debug, Eq, PartialEq)]
pub struct Staged {
    root: Identity,
    path: PathBuf,
    pending: Pending,
}

impl Staged {
    /// The target, relative to the vault root it was staged under. A respell's
    /// is its old spelling.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The `(device, inode)` of the vault root it was staged under.
    pub fn root(&self) -> Identity {
        self.root
    }
}

/// What a staged target is waiting to become, and the shadow that carries a
/// write's content.
#[derive(Debug, Eq, PartialEq)]
enum Pending {
    Create {
        after: ContentHash,
        shadow: StagedShadow,
    },
    Replace {
        before: ContentHash,
        after: ContentHash,
        shadow: StagedShadow,
    },
    Remove {
        before: ContentHash,
    },
    /// `shadow` is staged only where the content changes and the file does not
    /// hold it yet: a respell found halfway has nothing left to write.
    Respell {
        to: OsString,
        before: ContentHash,
        after: ContentHash,
        shadow: Option<StagedShadow>,
    },
}

impl Pending {
    fn shadow(&self) -> Option<&StagedShadow> {
        match self {
            Pending::Create { shadow, .. } | Pending::Replace { shadow, .. } => Some(shadow),
            Pending::Respell { shadow, .. } => shadow.as_ref(),
            Pending::Remove { .. } => None,
        }
    }
}

/// A shadow as staging made it: its name in the shadow home and which file
/// that name meant.
#[derive(Clone, Debug, Eq, PartialEq)]
struct StagedShadow {
    name: OsString,
    identity: Identity,
}

/// A target staging found already at its after-state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Landed {
    root: Identity,
    path: PathBuf,
    /// The after-state: the content hash, or `None` for absence.
    after: Option<ContentHash>,
    /// Whether the spelling is part of the after-state: a respell has landed
    /// only where the folder's listing spells the name exactly as `path` does.
    spelled: bool,
}

impl Landed {
    /// The target, relative to the vault root it was staged under. A landed
    /// respell's is its new spelling.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The `(device, inode)` of the vault root it was staged under.
    pub fn root(&self) -> Identity {
        self.root
    }
}

/// What a target holds once its transition has landed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AfterState {
    /// A file, and the identity of what is there.
    Present(PostState),
    /// Nothing: a removal landed.
    Absent,
}

/// Whether a landed change is on the disk, and the error that says why not.
///
/// It covers every sync the call made — the target's folder, and any folder a
/// create made or a respell renamed in twice. A change the filesystem has not
/// confirmed durable is still a change every reader sees, so this is never a
/// refusal; whether it stops what comes next is the caller's to decide.
#[derive(Debug)]
pub enum Durability {
    /// Every folder the change touched was synced.
    Synced,
    /// A sync failed, and this is what the filesystem said: the first failure,
    /// where more than one sync failed.
    NotSynced(std::io::Error),
}

impl Durability {
    /// Whether every sync the call made succeeded.
    pub fn is_synced(&self) -> bool {
        matches!(self, Durability::Synced)
    }

    /// Both syncs' durability: synced only where both are.
    fn and(self, next: Durability) -> Durability {
        match self {
            Durability::Synced => next,
            not_synced => not_synced,
        }
    }
}

/// What a publication did.
#[derive(Debug)]
pub enum Publication {
    /// This call changed the vault. The own-write ledger records this, because
    /// a filesystem event about the target is coming.
    Wrote(Published),
    /// The target was already at its after-state — another writer put it
    /// there — so this call wrote nothing, discarded its shadow and synced the
    /// folder. No event of this call's is coming, and nothing is recorded.
    Found(Confirmed),
}

/// A target this call published.
#[derive(Debug)]
#[non_exhaustive]
pub struct Published {
    /// What the target holds now.
    pub after: AfterState,
    /// Whether the change is on the disk.
    pub durability: Durability,
    /// The folders a create made on the way to its name, relative to the vault
    /// root, shallowest first.
    pub made_folders: Vec<PathBuf>,
}

/// A target found at its after-state, verified and its folder synced.
#[derive(Debug)]
#[non_exhaustive]
pub struct Confirmed {
    /// What the target holds.
    pub after: AfterState,
    /// Whether the target's folder is on the disk.
    pub durability: Durability,
}

/// What emptying folders upward removed.
#[derive(Debug)]
#[non_exhaustive]
pub struct RemovedFolders {
    /// The folders removed, relative to the vault root, deepest first.
    pub removed: Vec<PathBuf>,
    /// Whether every removal is on the disk.
    pub durability: Durability,
}

/// Check `path` below `anchor` for `transition`, and stage a write's content.
///
/// See the [module documentation](self) for what staging asks and what it
/// leaves behind. `anchor` is the vault root, resolved as it is spelled;
/// `path` is relative to it and names a file below it, never a parent, a
/// root or a prefix.
///
/// **A dormant carrier.** Its consumer is the one applier (NORN-295, Layer 4
/// plan-apply), which stages every target of a plan before it publishes any.
/// Nothing outside `norn-fs` calls it until that applier lands, so its
/// contract is held by this crate's suites alone.
pub fn stage(
    anchor: &Path,
    path: &Path,
    transition: Transition<'_>,
    shadows: &ShadowHome,
) -> Result<Staging, Refusal> {
    // The public entry points are where the fault seam is reachable from
    // outside this crate, and only under the `induced-failure` feature: the
    // process-death bars are stated about a process that does not return.
    stage_where(anchor, path, transition, shadows, Faults::entry())
}

/// Publish what [`stage`] staged.
///
/// See the [module documentation](self) for what publication asks again, the
/// residual race and the crash windows. `anchor` and `shadows` are the ones
/// the target was staged under; a different root refuses, and a different home
/// holds no such shadow.
///
/// **A dormant carrier.** Its consumer is the one applier (NORN-295, Layer 4
/// plan-apply), which publishes creates, then replaces, then removes, once
/// every target staged. Nothing outside `norn-fs` calls it until then.
pub fn publish(
    anchor: &Path,
    staged: Staged,
    shadows: &ShadowHome,
) -> Result<Publication, Refusal> {
    publish_disturbed(anchor, staged, shadows, Faults::publication(), &mut |_| {})
}

/// Verify a target staging found already landed, and sync its folder again.
///
/// A re-send of a plan that a crash interrupted finds its landed targets here,
/// and the sync is why it is more than a look: the crash may have come
/// between the publication act and the folder sync, and the directory entry is
/// not durable until one runs. A target that no longer holds the after-state
/// refuses — drift where a content hash was expected, a taken name where
/// absence was.
///
/// **A dormant carrier.** Its consumer is the one applier (NORN-295, Layer 4
/// plan-apply), on a re-sent resolved plan. Nothing outside `norn-fs` calls it
/// until then.
pub fn confirm_landed(anchor: &Path, landed: &Landed) -> Result<Confirmed, Refusal> {
    confirm_landed_where(anchor, landed, Faults::entry())
}

/// Remove a staged target's shadow without publishing it.
///
/// For a plan torn down between the phases: a refusal while staging a later
/// target, or a lifecycle end. The shadow is removed only while its name still
/// means the file staging made, and a removal that fails is left to the shadow
/// home's sweep, for the reason a publication's own cleanup is (see
/// [`crate::shadow`]).
///
/// **A dormant carrier.** Its consumer is the one applier (NORN-295, Layer 4
/// plan-apply). Nothing outside `norn-fs` calls it until then.
pub fn discard(staged: Staged, shadows: &ShadowHome) {
    if let Some(shadow) = staged.pending.shadow() {
        remove_shadow(
            shadows,
            shadow,
            Faulted {
                faults: Faults::entry(),
                path: &staged.path,
            },
        );
    }
}

/// Remove `folder` below `anchor` where it is empty, and each folder above it
/// that is then empty, stopping at the first that is not and never at the
/// root.
///
/// The applier calls this after a plan's removals, and for the folders a
/// refused create left: a document is removed, and a folder that held only it
/// goes with it. The descent is anchored and follows no link, as every change
/// here is; a folder already gone is where the emptying starts from, and the
/// folders above it are emptied as if it had been removed here. Each removal's
/// holder is synced, and the durability of all of them is reported.
///
/// A removal the filesystem refuses for a reason other than a folder that is
/// not empty ends the call as that refusal; the folders removed before it are
/// removed, and their holders were synced.
///
/// **A dormant carrier.** Its consumer is the one applier (NORN-295, Layer 4
/// plan-apply). Nothing outside `norn-fs` calls it until then.
pub fn remove_empty_folders(anchor: &Path, folder: &Path) -> Result<RemovedFolders, Refusal> {
    remove_empty_folders_where(anchor, folder, Faults::entry())
}

// ---------------------------------------------------------------------------
// Staging
// ---------------------------------------------------------------------------

/// [`stage`], with a stage made to fail rather than waiting for a machine that
/// fails there.
fn stage_where(
    anchor: &Path,
    path: &Path,
    transition: Transition<'_>,
    shadows: &ShadowHome,
    faults: Faults,
) -> Result<Staging, Refusal> {
    let faults = Faulted { faults, path };
    let full = anchor.join(path);
    let target = Target::of(path, &full)?;
    let respelled = match &transition {
        Transition::Respell { to, .. } => Some(respelled_name(&target, to, &full)?),
        _ => None,
    };
    let (root, root_identity) = open_root(anchor)?;
    let folder = descend(root, &target, anchor, &full)?;
    let after = after_of(&transition);
    if let (
        Some(to),
        Transition::Respell {
            before, content, ..
        },
    ) = (respelled, transition)
    {
        return stage_respell(
            folder,
            &target,
            to,
            before,
            content,
            (root_identity, path.to_path_buf()),
            &full,
            shadows,
            faults,
        );
    }
    let found = match folder {
        Folder::Reached(folder) => observe(folder.as_fd(), target.name, &full, &mut |_| {})?,
        Folder::Missing { .. } => Found::Absent,
    };
    // Every descriptor the look opened is closed by here: what staging hands
    // back holds none.
    let mode = match judge_staging(&transition, after, found, &full)? {
        Judged::Landed => {
            return Ok(Staging::Landed(Landed {
                root: root_identity,
                path: path.to_path_buf(),
                after,
                spelled: false,
            }));
        }
        Judged::Proceed { mode } => mode,
    };
    let pending = match (transition, after) {
        (Transition::Create { content }, Some(after)) => Pending::Create {
            after,
            shadow: stage_shadow(shadows, content, None, faults)?,
        },
        (Transition::Replace { before, content }, Some(after)) => Pending::Replace {
            before,
            after,
            shadow: stage_shadow(shadows, content, mode, faults)?,
        },
        (Transition::Remove { before }, _) => Pending::Remove { before },
        (
            Transition::Create { .. } | Transition::Replace { .. } | Transition::Respell { .. },
            _,
        ) => {
            unreachable!("a write's after-state is its content's hash, and a respell stages above")
        }
    };
    Ok(Staging::Staged(Staged {
        root: root_identity,
        path: path.to_path_buf(),
        pending,
    }))
}

/// The after-state a transition names: its content's hash, a respell's own
/// bytes where it brings none, or absence.
fn after_of(transition: &Transition<'_>) -> Option<ContentHash> {
    match transition {
        Transition::Create { content } | Transition::Replace { content, .. } => {
            Some(ContentHash::of(content))
        }
        Transition::Respell {
            before, content, ..
        } => Some(content.map_or(*before, ContentHash::of)),
        Transition::Remove { .. } => None,
    }
}

/// What staging concluded about a target.
enum Judged {
    /// The target is at its before-state. `mode` is a replaced file's
    /// permission bits, to carry onto the shadow.
    Proceed { mode: Option<u32> },
    /// The target already holds its after-state.
    Landed,
}

/// Decide what `found` means for `transition` at staging.
///
/// **The after-state is asked first**, so a replacement whose content is what
/// it read is landed rather than staged, and a target a re-send finds landed
/// is recognized whatever its before-state was.
///
/// A refusal speaks in the terms of what the transition expected: where it
/// expected absence and found a name taken, [`Refusal::DestinationExists`];
/// where it expected a hash and found other bytes or nothing,
/// [`Refusal::Drifted`].
fn judge_staging(
    transition: &Transition<'_>,
    after: Option<ContentHash>,
    found: Found,
    full: &Path,
) -> Result<Judged, Refusal> {
    match (transition, found) {
        (_, Found::Link) => Err(Refusal::SymlinkDestination {
            path: full.to_path_buf(),
        }),
        (_, Found::Regular { state, .. }) if Some(state.content_hash) == after => {
            Ok(Judged::Landed)
        }
        (Transition::Remove { .. }, Found::Absent) => Ok(Judged::Landed),
        (Transition::Create { .. }, Found::Absent) => Ok(Judged::Proceed { mode: None }),
        (Transition::Create { .. }, Found::Regular { .. } | Found::Other) => {
            Err(Refusal::DestinationExists {
                path: full.to_path_buf(),
            })
        }
        (
            Transition::Replace { before, .. }
            | Transition::Remove { before }
            | Transition::Respell { before, .. },
            Found::Regular { state, mode },
        ) => {
            if state.content_hash == *before {
                Ok(Judged::Proceed { mode: Some(mode) })
            } else {
                Err(drifted(full, *before, Some(state)))
            }
        }
        (
            Transition::Replace { before, .. } | Transition::Respell { before, .. },
            Found::Absent,
        ) => Err(drifted(full, *before, None)),
        (
            Transition::Replace { .. } | Transition::Remove { .. } | Transition::Respell { .. },
            Found::Other,
        ) => Err(not_regular(full)),
    }
}

/// The new spelling of a respell's final name, where `to` changes only the
/// ASCII case of the name `target` ends in.
///
/// Anything more — another folder, another name, a folder's own case, a case
/// change outside ASCII, or no change at all — is not a respell, and is
/// refused before anything is read.
fn respelled_name(target: &Target<'_>, to: &Path, full: &Path) -> Result<OsString, Refusal> {
    let not_a_respell = || {
        environment(
            "respelling",
            full,
            &std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a respell changes only the ASCII case of the final name",
            ),
        )
    };
    let respelled = Target::of(to, full).map_err(|_| not_a_respell())?;
    let from = target.name.as_bytes();
    let to = respelled.name.as_bytes();
    if respelled.folders != target.folders || from == to || !fold_together(from, to) {
        return Err(not_a_respell());
    }
    Ok(respelled.name.to_owned())
}

/// Stage a respell: judge the three states through the listed spelling, and
/// stage a shadow only where the content still has to change.
#[allow(clippy::too_many_arguments)] // One target's whole staging, spelled out rather than bundled.
fn stage_respell(
    folder: Folder,
    target: &Target<'_>,
    to: OsString,
    before: ContentHash,
    content: Option<&[u8]>,
    (root, path): (Identity, PathBuf),
    full: &Path,
    shadows: &ShadowHome,
    faults: Faulted<'_>,
) -> Result<Staging, Refusal> {
    let after = content.map_or(before, ContentHash::of);
    let Folder::Reached(folder) = folder else {
        return Err(drifted(full, before, None));
    };
    let Some((spelling, found)) = observe_spelling(folder.as_fd(), target.name, full, &mut |_| {})?
    else {
        return Err(drifted(full, before, None));
    };
    drop(folder);
    let shadow = match judge_respell(&spelling, found, target.name, &to, before, after, full)? {
        Respelled::Landed { .. } => {
            return Ok(Staging::Landed(Landed {
                root,
                path: path.with_file_name(&to),
                after: Some(after),
                spelled: true,
            }));
        }
        Respelled::Halfway { .. } => None,
        Respelled::Before { mode, .. } => match content {
            Some(content) if after != before => {
                Some(stage_shadow(shadows, content, Some(mode), faults)?)
            }
            _ => None,
        },
    };
    Ok(Staging::Staged(Staged {
        root,
        path,
        pending: Pending::Respell {
            to,
            before,
            after,
            shadow,
        },
    }))
}

/// Where a respell stands.
#[derive(Debug, Eq, PartialEq)]
enum Respelled {
    /// The old spelling at the before-state. `mode` is the file's permission
    /// bits, to carry onto a shadow.
    Before { state: PostState, mode: u32 },
    /// The old spelling at the after-state: the content changed and the rename
    /// did not happen.
    Halfway { state: PostState },
    /// The new spelling at the after-state.
    Landed { state: PostState },
}

/// Decide where a respell of `from` to `to` stands, from the spelling the
/// folder lists and what is at it.
///
/// A respell whose content does not change is at its before-state and its
/// after-state at once under the old spelling, and that is the before-state:
/// the rename is what is left to do. Every other combination is drift.
fn judge_respell(
    spelling: &OsStr,
    found: Found,
    from: &OsStr,
    to: &OsStr,
    before: ContentHash,
    after: ContentHash,
    full: &Path,
) -> Result<Respelled, Refusal> {
    let (state, mode) = match found {
        Found::Regular { state, mode } => (state, mode),
        Found::Link => {
            return Err(Refusal::SymlinkDestination {
                path: full.to_path_buf(),
            });
        }
        Found::Absent => return Err(drifted(full, before, None)),
        Found::Other => return Err(not_regular(full)),
    };
    let hash = state.content_hash;
    if spelling == to && hash == after {
        Ok(Respelled::Landed { state })
    } else if spelling == from && hash == before {
        Ok(Respelled::Before { state, mode })
    } else if spelling == from && hash == after {
        Ok(Respelled::Halfway { state })
    } else {
        Err(drifted(full, before, Some(state)))
    }
}

/// Write `content` into a fresh shadow and get it onto the disk.
///
/// A failure after the shadow exists removes it, so a staging refusal leaves
/// nothing in the home.
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns the shadow's handle.
fn stage_shadow(
    shadows: &ShadowHome,
    content: &[u8],
    mode: Option<u32>,
    faults: Faulted<'_>,
) -> Result<StagedShadow, Refusal> {
    let home = open_home(shadows)?;
    let (name, file) = create_shadow(
        home.as_fd(),
        shadows,
        &mut || shadows.next_shadow_name(),
        faults,
    )?;
    let mut file = std::fs::File::from(file);
    let shadow_path = shadows.directory().join(&name);
    let filled = (|| {
        if let Some(mode) = mode {
            carry_mode_forward(&file, mode);
        }
        fill(&mut file, content, &shadow_path, faults)?;
        let metadata = file
            .metadata()
            .map_err(|error| environment("reading the identity of", &shadow_path, &error))?;
        Ok(identity_of(&metadata))
    })();
    match filled {
        Ok(identity) => Ok(StagedShadow { name, identity }),
        Err(refusal) => {
            // A shadow this call created and could not finish is its own by
            // construction: the exclusive open made it a moment ago.
            if faults.check(Stage::Cleanup).is_ok() {
                let _ = unlinkat(home.as_fd(), name.as_os_str(), AtFlags::empty());
            }
            Err(refusal)
        }
    }
}

/// Open a shadow nothing has taken, advancing past any name that is already
/// taken.
///
/// **The open is exclusive and never anything else.** A shadow name is unique
/// per attempt, so a name that is somehow taken is residue of a previous life —
/// a process whose identifier this one now reuses, most plainly — and
/// truncating it is the forbidden shape: the residue may be a second link to a
/// live document, and the truncation would go through it. So a taken name is
/// skipped rather than opened or refused, because refusing would make one
/// leaked shadow break the first write of every process that inherits its
/// identifier.
///
/// Bounded at [`NAME_ATTEMPTS`]. Exhausting it means every one of that many
/// distinct names is taken, which is the shadow home being unusable rather than
/// a name being unlucky, and it reads as the environmental refusal the last open
/// gave.
fn create_shadow(
    home: BorrowedFd<'_>,
    shadows: &ShadowHome,
    next: &mut dyn FnMut() -> OsString,
    faults: Faulted<'_>,
) -> Result<(OsString, OwnedFd), Refusal> {
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let mut last = None;
    for _ in 0..NAME_ATTEMPTS {
        let name = next();
        let shadow = shadows.directory().join(&name);
        faults
            .check(Stage::ShadowCreate)
            .map_err(|error| environment("creating", &shadow, &error))?;
        // The mode an ordinary create takes, so a created document comes out
        // as the umask says and a replacement's is carried over it.
        match openat(home, name.as_os_str(), flags, Mode::from_raw_mode(0o666)) {
            Ok(file) => return Ok((name, file)),
            Err(Errno::EXIST) => last = Some(errno_refusal("creating", &shadow, Errno::EXIST)),
            Err(errno) => return Err(errno_refusal("creating", &shadow, errno)),
        }
    }
    // Every attempt met a taken name and recorded its refusal; NAME_ATTEMPTS
    // is a non-zero constant, so the loop ran at least once.
    Err(last.expect("a bound of at least one attempt"))
}

/// Put `content` in an already-open handle and get it onto the disk.
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns the shadow's handle.
fn fill(
    file: &mut std::fs::File,
    content: &[u8],
    path: &Path,
    faults: Faulted<'_>,
) -> Result<(), Refusal> {
    faults
        .check(Stage::ShadowWrite)
        .map_err(|error| environment("writing", path, &error))?;
    file.write_all(content)
        .map_err(|error| environment("writing", path, &error))?;
    faults
        .check(Stage::ShadowSync)
        .map_err(|error| environment("syncing", path, &error))?;
    // `sync_all` is `F_FULLFSYNC` on macOS, which is what reaches the platter.
    file.sync_all()
        .map_err(|error| environment("syncing", path, &error))
}

/// Carry a replaced file's permission bits onto the staged shadow.
///
/// **Replacement is the mechanism; a surgical edit is the observable
/// contract.** What a caller sees is a document whose content outside the edit,
/// permission bits, path and name are all as they were.
///
/// **The permission bits and nothing else**: the mode is masked to `0o777`, so
/// the **set-user-id and set-group-id bits are dropped**. Carrying them would
/// take a bit that means "run as the file's owner" and put it on a file this
/// process owns — a privilege the document did not have before Norn touched
/// it.
///
/// The mode is carried **before any content goes in**, so a full replacement
/// of a `0600` document never sits in the shadow home at the umask's defaults.
/// It is best-effort and never fatal: a mode that could not be set is a
/// cosmetic loss, and failing the write over it would refuse a correct
/// document to protect a permission bit.
///
/// What cannot be carried is named rather than papered over: **ownership**
/// (the published file is the writing user's), **the inode number** (so a
/// foreign hard link keeps the old content, and on macOS the birth time is the
/// new file's), and **extended attributes and access-control lists**. A mode
/// that denies writing is carried like any other and **does not protect the
/// document** — the rename needs the folder, not the file.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The vault filesystem seam: this crate owns the shadow's handle.
fn carry_mode_forward(file: &std::fs::File, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = file.set_permissions(std::fs::Permissions::from_mode(mode & 0o777));
}

// ---------------------------------------------------------------------------
// Publication
// ---------------------------------------------------------------------------

/// [`publish`], with a stage made to fail and something allowed to happen
/// inside the windows a foreign writer would land in.
///
/// The two parameters are what make the contract's claims checkable at all: a
/// folder sync that fails after the rename, and a foreign writer inside a
/// window one call wide, are conditions no temporary directory produces.
fn publish_disturbed(
    anchor: &Path,
    staged: Staged,
    shadows: &ShadowHome,
    faults: Faults,
    disturb: &mut dyn FnMut(Window),
) -> Result<Publication, Refusal> {
    let faults = Faulted {
        faults,
        path: &staged.path,
    };
    let publication = publish_checked(anchor, &staged, shadows, faults, disturb);
    // A publication that wrote consumed its shadow, except a respell's that
    // was found halfway and had nothing left to write.
    let consumed = matches!(publication, Ok(Publication::Wrote(_)))
        && !matches!(staged.pending, Pending::Respell { .. });
    if !consumed && let Some(shadow) = staged.pending.shadow() {
        // One cleanup for every refusal and every found landing, so an auditor
        // finds one place it happens. It removes the shadow only while the name
        // still means the file staging made, which also makes it right for a
        // shadow the refusal was about — a missing one is not there, and one
        // replaced by a foreign file is somebody else's — and for a respell's
        // shadow its first step already renamed away.
        remove_shadow(shadows, shadow, faults);
    }
    publication
}

/// Ask every question again, and publish.
fn publish_checked(
    anchor: &Path,
    staged: &Staged,
    shadows: &ShadowHome,
    faults: Faulted<'_>,
    disturb: &mut dyn FnMut(Window),
) -> Result<Publication, Refusal> {
    let full = anchor.join(&staged.path);
    let target = Target::of(&staged.path, &full)?;
    let root = open_staged_root(anchor, staged.root)?;
    let folder = descend(root, &target, anchor, &full)?;
    #[cfg(any(test, feature = "induced-failure"))]
    act_as_foreign_writer(&folder, &target, &staged.pending, faults);
    let at = Place {
        anchor,
        target: &target,
        full: &full,
        faults,
    };
    match &staged.pending {
        Pending::Create { after, shadow } => {
            let (home, state) = confirm_shadow(shadows, shadow, *after)?;
            publish_create(&at, folder, *after, (home.as_fd(), shadow, state), disturb)
        }
        Pending::Replace {
            before,
            after,
            shadow,
        } => {
            let Folder::Reached(folder) = folder else {
                return Err(drifted(&full, *before, None));
            };
            let (home, state) = confirm_shadow(shadows, shadow, *after)?;
            publish_replace(
                &at,
                folder.as_fd(),
                *before,
                *after,
                (home.as_fd(), shadow, state),
                disturb,
            )
        }
        Pending::Remove { before } => publish_remove(&at, folder, *before, disturb),
        Pending::Respell {
            to,
            before,
            after,
            shadow,
        } => {
            let Folder::Reached(folder) = folder else {
                return Err(drifted(&full, *before, None));
            };
            let shadow = match shadow {
                Some(shadow) => Some((shadow, confirm_shadow(shadows, shadow, *after)?.0)),
                None => None,
            };
            publish_respell(&at, folder.as_fd(), to, *before, *after, shadow, disturb)
        }
    }
}

/// Where a publication acts, and under which faults.
struct Place<'a> {
    anchor: &'a Path,
    target: &'a Target<'a>,
    full: &'a Path,
    faults: Faulted<'a>,
}

/// A confirmed shadow: the home's handle, the shadow, and the identity it will
/// publish.
type ConfirmedShadow<'a> = (BorrowedFd<'a>, &'a StagedShadow, PostState);

/// A landing another writer made: report it, syncing the folder that holds it.
fn found_landed(after: AfterState, folder: BorrowedFd<'_>, faults: Faulted<'_>) -> Publication {
    Publication::Found(Confirmed {
        after,
        durability: sync_folder(folder, faults),
    })
}

/// A publication this call made, with no folder made on the way.
fn wrote(after: AfterState, durability: Durability) -> Publication {
    Publication::Wrote(Published {
        after,
        durability,
        made_folders: Vec::new(),
    })
}

/// Publish a create: found where the name already holds its content, and
/// otherwise the exclusive rename, after making any folder that is missing.
fn publish_create(
    at: &Place<'_>,
    folder: Folder,
    after: ContentHash,
    (home, shadow, state): ConfirmedShadow<'_>,
    disturb: &mut dyn FnMut(Window),
) -> Result<Publication, Refusal> {
    let name = at.target.name;
    let (parent, made) = match folder {
        Folder::Reached(parent) => {
            let found = observe(parent.as_fd(), name, at.full, disturb)?;
            if let Some(landed) = at_after(&found, after) {
                return Ok(found_landed(landed, parent.as_fd(), at.faults));
            }
            judge_absent(found, at.full)?;
            (Some(parent), None)
        }
        // Nothing can be at a name whose folder is missing, so the folders are
        // made and the exclusive rename is the create's whole question.
        Folder::Missing { deepest, reached } => (
            None,
            Some(MadeFolders::make(at, deepest, reached, disturb)?),
        ),
    };
    let folder = match (&parent, &made) {
        (Some(parent), _) => parent.as_fd(),
        (None, Some(made)) => made.parent(),
        (None, None) => unreachable!("a create reaches its folder or makes it"),
    };
    disturb(Window::Publishing);
    if let Err(refusal) = publish_exclusively(home, shadow, folder, name, at.full, at.faults) {
        if let Some(made) = &made {
            made.remove_empty(at.faults);
        }
        // The name was taken at the last moment. Where what took it is this
        // create's own after-state, the create has landed.
        if matches!(refusal, Refusal::DestinationExists { .. })
            && let Some(landed) = at_after(&observe(folder, name, at.full, &mut |_| {})?, after)
        {
            return Ok(found_landed(landed, folder, at.faults));
        }
        return Err(refusal);
    }
    Ok(Publication::Wrote(match made {
        Some(made) => Published {
            after: AfterState::Present(state),
            durability: made.sync(at.faults),
            made_folders: made.into_paths(),
        },
        None => Published {
            after: AfterState::Present(state),
            durability: sync_folder(folder, at.faults),
            made_folders: Vec::new(),
        },
    }))
}

/// Publish a replace: found at its after-state, and otherwise a rename over a
/// target still at its before-state.
fn publish_replace(
    at: &Place<'_>,
    folder: BorrowedFd<'_>,
    before: ContentHash,
    after: ContentHash,
    (home, shadow, state): ConfirmedShadow<'_>,
    disturb: &mut dyn FnMut(Window),
) -> Result<Publication, Refusal> {
    let found = observe(folder, at.target.name, at.full, disturb)?;
    if let Some(landed) = at_after(&found, after) {
        return Ok(found_landed(landed, folder, at.faults));
    }
    judge_before(found, before, at.full)?;
    disturb(Window::Publishing);
    rename_shadow(home, shadow, folder, at.target.name, at.full, at.faults)?;
    Ok(wrote(
        AfterState::Present(state),
        sync_folder(folder, at.faults),
    ))
}

/// Publish a remove: found where the target, or its folder, is already gone,
/// and otherwise an unlink of a target still at its before-state.
fn publish_remove(
    at: &Place<'_>,
    folder: Folder,
    before: ContentHash,
    disturb: &mut dyn FnMut(Window),
) -> Result<Publication, Refusal> {
    let folder = match folder {
        Folder::Reached(folder) => folder,
        // The folder is gone, and the target with it: the deepest folder still
        // there is the one that records the absence.
        Folder::Missing { deepest, .. } => {
            return Ok(found_landed(AfterState::Absent, deepest.as_fd(), at.faults));
        }
    };
    let found = observe(folder.as_fd(), at.target.name, at.full, disturb)?;
    if matches!(found, Found::Absent) {
        return Ok(found_landed(AfterState::Absent, folder.as_fd(), at.faults));
    }
    judge_before(found, before, at.full)?;
    disturb(Window::Publishing);
    at.faults
        .check(Stage::Unlink)
        .map_err(|error| environment("removing", at.full, &error))?;
    match unlinkat(folder.as_fd(), at.target.name, AtFlags::empty()) {
        Ok(()) => Ok(wrote(
            AfterState::Absent,
            sync_folder(folder.as_fd(), at.faults),
        )),
        // Gone between the verification and the unlink: another writer landed
        // it.
        Err(Errno::NOENT) => Ok(found_landed(AfterState::Absent, folder.as_fd(), at.faults)),
        Err(errno) => Err(errno_refusal("removing", at.full, errno)),
    }
}

/// Publish a respell in its two steps: where the content still has to change,
/// an ordinary replace under the old spelling; then the rename to the new.
fn publish_respell(
    at: &Place<'_>,
    folder: BorrowedFd<'_>,
    to: &OsStr,
    before: ContentHash,
    after: ContentHash,
    shadow: Option<(&StagedShadow, OwnedFd)>,
    disturb: &mut dyn FnMut(Window),
) -> Result<Publication, Refusal> {
    let from = at.target.name;
    let respelled = |disturb: &mut dyn FnMut(Window)| {
        let Some((spelling, found)) = observe_spelling(folder, from, at.full, disturb)? else {
            return Err(drifted(at.full, before, None));
        };
        judge_respell(&spelling, found, from, to, before, after, at.full)
    };
    let mut durability = Durability::Synced;
    let wrote_content = match respelled(disturb)? {
        Respelled::Landed { state } => {
            return Ok(found_landed(AfterState::Present(state), folder, at.faults));
        }
        Respelled::Before { state, .. } if after != before => {
            // Step 1. The content is at its before-state and has to change; a
            // respell staged halfway has no shadow for that, so the content
            // moved back under it and that is drift.
            let Some((shadow, home)) = shadow else {
                return Err(drifted(at.full, after, Some(state)));
            };
            disturb(Window::Publishing);
            rename_shadow(home.as_fd(), shadow, folder, from, at.full, at.faults)?;
            durability = sync_folder(folder, at.faults);
            true
        }
        Respelled::Before { .. } | Respelled::Halfway { .. } => false,
    };
    // Step 2, with the after-state read again under the old spelling.
    let state = match respelled(disturb)? {
        Respelled::Landed { state } if !wrote_content => {
            return Ok(found_landed(AfterState::Present(state), folder, at.faults));
        }
        Respelled::Landed { state } => {
            // Another writer renamed this call's content into place.
            return Ok(wrote(
                AfterState::Present(state),
                durability.and(sync_folder(folder, at.faults)),
            ));
        }
        Respelled::Halfway { state } => state,
        Respelled::Before { state, .. } if after == before => state,
        Respelled::Before { state, .. } => return Err(drifted(at.full, after, Some(state))),
    };
    at.faults
        .check(Stage::Respell)
        .map_err(|error| environment("respelling", at.full, &error))?;
    renameat(folder, from, folder, to)
        .map_err(|errno| errno_refusal("respelling", at.full, errno))?;
    Ok(wrote(
        AfterState::Present(state),
        durability.and(sync_folder(folder, at.faults)),
    ))
}

/// The after-state `found` is, where it is this transition's after-state.
fn at_after(found: &Found, after: ContentHash) -> Option<AfterState> {
    match found {
        Found::Regular { state, .. } if state.content_hash == after => {
            Some(AfterState::Present(*state))
        }
        _ => None,
    }
}

/// Refuse unless nothing is at a create's name.
fn judge_absent(found: Found, full: &Path) -> Result<(), Refusal> {
    match found {
        Found::Absent => Ok(()),
        Found::Link => Err(Refusal::SymlinkDestination {
            path: full.to_path_buf(),
        }),
        Found::Regular { .. } | Found::Other => Err(Refusal::DestinationExists {
            path: full.to_path_buf(),
        }),
    }
}

/// Refuse unless the target still holds `before`.
fn judge_before(found: Found, before: ContentHash, full: &Path) -> Result<(), Refusal> {
    match found {
        Found::Regular { state, .. } if state.content_hash == before => Ok(()),
        Found::Regular { state, .. } => Err(drifted(full, before, Some(state))),
        Found::Absent => Err(drifted(full, before, None)),
        Found::Link => Err(Refusal::SymlinkDestination {
            path: full.to_path_buf(),
        }),
        Found::Other => Err(not_regular(full)),
    }
}

/// Rename a confirmed shadow over the name `name` in `folder`.
fn rename_shadow(
    home: BorrowedFd<'_>,
    shadow: &StagedShadow,
    folder: BorrowedFd<'_>,
    name: &OsStr,
    full: &Path,
    faults: Faulted<'_>,
) -> Result<(), Refusal> {
    faults
        .check(Stage::Swap)
        .map_err(|error| environment("renaming onto", full, &error))?;
    renameat(home, shadow.name.as_os_str(), folder, name)
        .map_err(|errno| errno_refusal("renaming onto", full, errno))
}

/// The folders a create made on the way to its name, and the handles that
/// reach them.
struct MadeFolders {
    /// Every folder from the deepest one that was there down to the target's
    /// own, each opened through no link.
    chain: Vec<OwnedFd>,
    /// Each folder made: the index in `chain` of the folder holding it, its
    /// name, and its path relative to the vault root.
    made: Vec<(usize, OsString, PathBuf)>,
}

impl MadeFolders {
    /// Make every folder of `at`'s target past the first `reached`, one level
    /// at a time from `deepest`, the last folder that was there.
    ///
    /// Each is made with `mkdirat` in the folder above it and then opened like
    /// any folder of a descent — `O_NOFOLLOW` and `O_DIRECTORY` — so a folder
    /// another writer made first is used rather than refused, and a name that
    /// became a link or a file refuses. A refusal removes the folders this
    /// call made that are still empty before it is returned.
    fn make(
        at: &Place<'_>,
        deepest: OwnedFd,
        reached: usize,
        disturb: &mut dyn FnMut(Window),
    ) -> Result<MadeFolders, Refusal> {
        let folders = &at.target.folders;
        let mut made = MadeFolders {
            chain: vec![deepest],
            made: Vec::new(),
        };
        let mut relative: PathBuf = folders[..reached].iter().collect();
        for name in &folders[reached..] {
            relative.push(name);
            match made.make_one(at, name, &relative, disturb) {
                Ok(folder) => made.chain.push(folder),
                Err(refusal) => {
                    made.remove_empty(at.faults);
                    return Err(refusal);
                }
            }
        }
        Ok(made)
    }

    /// Make `name` in the deepest folder of the chain, and open it.
    fn make_one(
        &mut self,
        at: &Place<'_>,
        name: &OsStr,
        relative: &Path,
        disturb: &mut dyn FnMut(Window),
    ) -> Result<OwnedFd, Refusal> {
        let holder = self.chain.len() - 1;
        at.faults
            .at(relative)
            .check(Stage::Mkdir)
            .map_err(|error| environment_at("making the folder", at.full, name, &error))?;
        match mkdirat(&self.chain[holder], name, Mode::from_raw_mode(0o777)) {
            Ok(()) => self
                .made
                .push((holder, name.to_owned(), relative.to_path_buf())),
            // Another writer made it first: it is used, and it is not ours.
            Err(Errno::EXIST) => {}
            Err(errno) => return Err(errno_refusal_at("making the folder", at.full, name, errno)),
        }
        disturb(Window::FolderMade);
        match open_folder(
            self.chain[holder].as_fd(),
            name,
            at.full,
            &at.anchor.join(relative),
        )? {
            Some(folder) => Ok(folder),
            None => Err(errno_refusal_at("opening", at.full, name, Errno::NOENT)),
        }
    }

    /// The target's own folder.
    fn parent(&self) -> BorrowedFd<'_> {
        self.chain
            .last()
            .expect("the chain starts at a folder")
            .as_fd()
    }

    /// Remove the folders this call made that are still empty, deepest first,
    /// stopping at the first that is not: every folder above holds it.
    fn remove_empty(&self, faults: Faulted<'_>) {
        if faults.check(Stage::Cleanup).is_err() {
            return;
        }
        for (holder, name, _) in self.made.iter().rev() {
            if unlinkat(&self.chain[*holder], name.as_os_str(), AtFlags::REMOVEDIR).is_err() {
                break;
            }
        }
    }

    /// Sync the target's folder and each folder holding one this call made.
    fn sync(&self, faults: Faulted<'_>) -> Durability {
        self.made.iter().rev().fold(
            sync_folder(self.parent(), faults),
            |durability, (holder, ..)| {
                durability.and(sync_folder(self.chain[*holder].as_fd(), faults))
            },
        )
    }

    /// The paths of the folders made, shallowest first.
    fn into_paths(self) -> Vec<PathBuf> {
        self.made.into_iter().map(|(_, _, path)| path).collect()
    }
}

/// Rename the shadow to the target's name, refusing rather than replacing
/// anything there.
///
/// A name taken between the verification and this call refuses as
/// [`Refusal::DestinationExists`] with the racer's file untouched. A
/// filesystem that cannot rename exclusively refuses as
/// [`Refusal::ExclusiveCreateUnsupported`]: `EINVAL` and `ENOTSUP` are how
/// Linux and macOS say so, and `ENOSYS` is a kernel that predates the call.
/// There is no fallback — a hard link and an unlink, or a look and a rename,
/// are each a check followed by an act, which is the race this call exists
/// to close.
fn publish_exclusively(
    home: BorrowedFd<'_>,
    shadow: &StagedShadow,
    folder: BorrowedFd<'_>,
    name: &OsStr,
    full: &Path,
    faults: Faulted<'_>,
) -> Result<(), Refusal> {
    faults
        .check(Stage::Swap)
        .map_err(|error| environment("renaming onto", full, &error))?;
    match renameat_with(
        home,
        shadow.name.as_os_str(),
        folder,
        name,
        RenameFlags::NOREPLACE,
    ) {
        Ok(()) => Ok(()),
        Err(Errno::EXIST) => Err(Refusal::DestinationExists {
            path: full.to_path_buf(),
        }),
        Err(errno) if cannot_rename_exclusively(errno) => {
            Err(Refusal::ExclusiveCreateUnsupported {
                path: full.to_path_buf(),
                raw_os_error: errno.raw_os_error(),
            })
        }
        Err(errno) => Err(errno_refusal("renaming onto", full, errno)),
    }
}

/// Whether `errno` is a filesystem saying it has no exclusive rename.
///
/// Compared one by one because `ENOTSUP` and `EOPNOTSUPP` are one number on
/// Linux and two on macOS, and a pattern naming both is unreachable on one of
/// them.
fn cannot_rename_exclusively(errno: Errno) -> bool {
    [Errno::INVAL, Errno::NOTSUP, Errno::OPNOTSUPP, Errno::NOSYS].contains(&errno)
}

/// Confirm a staged shadow is the file staging made and still holds `after`,
/// and hand back the home's handle and the identity it will publish.
///
/// Opened through the home's own directory without following a link, so a
/// link planted at a shadow's name is refused rather than published. A shadow
/// that is gone, is another file, or holds other bytes is an environmental
/// refusal for this target: an I/O failure a re-send stages again, never drift
/// the caller would re-plan.
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns the shadow's handle.
fn confirm_shadow(
    shadows: &ShadowHome,
    shadow: &StagedShadow,
    after: ContentHash,
) -> Result<(OwnedFd, PostState), Refusal> {
    const OPERATION: &str = "confirming the shadow";
    let home = open_home(shadows)?;
    let path = shadows.directory().join(&shadow.name);
    let opened = openat(
        home.as_fd(),
        shadow.name.as_os_str(),
        regular_flags(),
        Mode::empty(),
    )
    .map_err(|errno| errno_refusal(OPERATION, &path, errno))?;
    let mut file = std::fs::File::from(opened);
    let metadata = file
        .metadata()
        .map_err(|error| environment(OPERATION, &path, &error))?;
    if identity_of(&metadata) != shadow.identity || !metadata.file_type().is_file() {
        return Err(environment(
            OPERATION,
            &path,
            &invalid_data("the shadow is not the file staging made"),
        ));
    }
    let (hash, len) =
        hashed_from(&mut file).map_err(|error| environment(OPERATION, &path, &error))?;
    if hash != after {
        return Err(environment(
            OPERATION,
            &path,
            &invalid_data("the shadow no longer holds the content staging wrote"),
        ));
    }
    Ok((home, post_state(hash, len, &metadata)))
}

/// Remove a staged shadow while its name still means the file staging made,
/// and say nothing about a failure to.
///
/// **Cleanup is attempted on every path that does not publish, and its failure
/// is swallowed by design.** It is always attempted, a shadow left behind is
/// inert because its name is unique per attempt, and the residue is bounded
/// because the host sweeps the shadow home
/// ([`ShadowHome::sweep`](crate::ShadowHome::sweep)). Reporting the failure
/// instead would turn a clean refusal into two problems, and tell the caller
/// something other than why its write did not happen.
///
/// The identity is compared first because a shadow now outlives its staging
/// call: a name a sweep emptied is a name something else may hold, and this
/// removes only what staging made.
fn remove_shadow(shadows: &ShadowHome, shadow: &StagedShadow, faults: Faulted<'_>) {
    if faults.check(Stage::Cleanup).is_err() {
        return;
    }
    let Ok(home) = open_home(shadows) else {
        return;
    };
    match statat(&home, shadow.name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if identity_of_stat(&stat) == shadow.identity => {
            let _ = unlinkat(&home, shadow.name.as_os_str(), AtFlags::empty());
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Confirming a landing, and emptying folders
// ---------------------------------------------------------------------------

/// [`confirm_landed`], with a stage made to fail.
fn confirm_landed_where(
    anchor: &Path,
    landed: &Landed,
    faults: Faults,
) -> Result<Confirmed, Refusal> {
    let faults = Faulted {
        faults,
        path: &landed.path,
    };
    let full = anchor.join(&landed.path);
    let target = Target::of(&landed.path, &full)?;
    let root = open_staged_root(anchor, landed.root)?;
    // A removal whose folder is gone too has landed, and the folder that
    // records its absence is the deepest one still there.
    let (folder, found) = match descend(root, &target, anchor, &full)? {
        Folder::Reached(folder) => {
            let found = if landed.spelled {
                spelled_exactly(folder.as_fd(), target.name, &full)?
            } else {
                observe(folder.as_fd(), target.name, &full, &mut |_| {})?
            };
            (folder, found)
        }
        Folder::Missing { deepest, .. } => (deepest, Found::Absent),
    };
    let after = match (landed.after, found) {
        (_, Found::Link) => {
            return Err(Refusal::SymlinkDestination { path: full });
        }
        (None, Found::Absent) => AfterState::Absent,
        (None, Found::Regular { .. } | Found::Other) => {
            return Err(Refusal::DestinationExists { path: full });
        }
        (Some(after), Found::Regular { state, .. }) if state.content_hash == after => {
            AfterState::Present(state)
        }
        (Some(after), Found::Regular { state, .. }) => {
            return Err(drifted(&full, after, Some(state)));
        }
        (Some(after), Found::Absent) => return Err(drifted(&full, after, None)),
        (Some(_), Found::Other) => return Err(not_regular(&full)),
    };
    Ok(Confirmed {
        after,
        durability: sync_folder(folder.as_fd(), faults),
    })
}

/// What is at `name` in `folder` where the folder's listing spells it exactly
/// so, and absence where it spells it otherwise: a landed respell is the new
/// spelling, not merely a name the root resolves.
fn spelled_exactly(folder: BorrowedFd<'_>, name: &OsStr, full: &Path) -> Result<Found, Refusal> {
    Ok(match observe_spelling(folder, name, full, &mut |_| {})? {
        Some((spelling, found)) if spelling == name => found,
        _ => Found::Absent,
    })
}

/// [`remove_empty_folders`], with a stage made to fail.
fn remove_empty_folders_where(
    anchor: &Path,
    folder: &Path,
    faults: Faults,
) -> Result<RemovedFolders, Refusal> {
    let full = anchor.join(folder);
    let names = contained_names(folder, &full)?;
    let (root, _) = open_root(anchor)?;
    let mut chain = vec![root];
    let mut walked = anchor.to_path_buf();
    for name in &names {
        walked.push(name);
        let holder = chain.last().expect("the chain starts at the root").as_fd();
        match open_folder(holder, name, &full, &walked)? {
            Some(next) => chain.push(next),
            // Already gone: the emptying starts from the deepest folder there.
            None => break,
        }
    }
    let mut removed = Vec::new();
    let mut durability = Durability::Synced;
    // Depth 0 is the root, which is never removed.
    for depth in (1..chain.len()).rev() {
        let name = names[depth - 1];
        let relative: PathBuf = names[..depth].iter().collect();
        let faults = Faulted {
            faults,
            path: &relative,
        };
        faults
            .check(Stage::Rmdir)
            .map_err(|error| environment_at("removing the folder", &full, name, &error))?;
        match unlinkat(&chain[depth - 1], name, AtFlags::REMOVEDIR) {
            Ok(()) => {
                durability = durability.and(sync_folder(chain[depth - 1].as_fd(), faults));
                removed.push(relative);
            }
            // Another writer removed it first.
            Err(Errno::NOENT) => {}
            // Not empty, or no longer a folder: the emptying ends here.
            Err(Errno::NOTEMPTY | Errno::EXIST | Errno::NOTDIR) => break,
            Err(errno) => return Err(errno_refusal_at("removing the folder", &full, name, errno)),
        }
    }
    Ok(RemovedFolders {
        removed,
        durability,
    })
}

// ---------------------------------------------------------------------------
// The anchored descent, shared by every phase
// ---------------------------------------------------------------------------

/// A vault-relative path, split into the folders above the target and the
/// target's own name.
struct Target<'p> {
    folders: Vec<&'p OsStr>,
    name: &'p OsStr,
}

impl<'p> Target<'p> {
    /// The target `path` names, where it is a name below the root.
    fn of(path: &'p Path, full: &Path) -> Result<Target<'p>, Refusal> {
        let mut folders = contained_names(path, full)?;
        let Some(name) = folders.pop() else {
            return Err(uncontained(full));
        };
        Ok(Target { folders, name })
    }
}

/// The names `path` is made of, where each is an ordinary name below the root.
///
/// A parent component walks out of the root, an absolute path makes `openat`
/// ignore the handle it was given, and a current-directory component names no
/// file — each is refused before anything is opened, so containment is a
/// property of this kernel rather than of whoever calls it.
fn contained_names<'p>(path: &'p Path, full: &Path) -> Result<Vec<&'p OsStr>, Refusal> {
    path.components()
        .map(|component| match component {
            Component::Normal(name) => Ok(name),
            Component::RootDir
            | Component::Prefix(_)
            | Component::ParentDir
            | Component::CurDir => Err(uncontained(full)),
        })
        .collect()
}

/// Open the root as it is spelled, and read which directory that is.
fn open_root(anchor: &Path) -> Result<(OwnedFd, Identity), Refusal> {
    let root = open(anchor, anchor_flags(), Mode::empty())
        .map_err(|errno| errno_refusal("opening the vault root", anchor, errno))?;
    let stat =
        fstat(&root).map_err(|errno| errno_refusal("reading the vault root", anchor, errno))?;
    Ok((root, identity_of_stat(&stat)))
}

/// Open the root, refusing unless it is the directory `staged` identifies.
fn open_staged_root(anchor: &Path, staged: Identity) -> Result<OwnedFd, Refusal> {
    let (root, current) = open_root(anchor)?;
    if current != staged {
        return Err(Refusal::RootReplaced {
            path: anchor.to_path_buf(),
            staged,
            current,
        });
    }
    Ok(root)
}

/// Where a descent to a target's folder ended.
enum Folder {
    /// The target's own folder.
    Reached(OwnedFd),
    /// A folder on the way is not there, so neither is the target. `deepest`
    /// is the last folder that is, and `reached` how many of the target's
    /// folders the descent opened.
    Missing { deepest: OwnedFd, reached: usize },
}

/// Descend from `root` to the folder `target` sits in, one folder at a time
/// and through no link.
///
/// A single multi-component open would resolve the intermediate names in the
/// kernel, where `O_NOFOLLOW` binds only the last of them.
fn descend(
    root: OwnedFd,
    target: &Target<'_>,
    anchor: &Path,
    full: &Path,
) -> Result<Folder, Refusal> {
    let mut folder = root;
    let mut walked = anchor.to_path_buf();
    for (reached, name) in target.folders.iter().enumerate() {
        walked.push(name);
        match open_folder(folder.as_fd(), name, full, &walked)? {
            Some(next) => folder = next,
            None => {
                return Ok(Folder::Missing {
                    deepest: folder,
                    reached,
                });
            }
        }
    }
    Ok(Folder::Reached(folder))
}

/// Open the folder `name` in `holder` with `O_NOFOLLOW` and `O_DIRECTORY`, or
/// answer that nothing is there. `walked` is the folder's own path, which a
/// linked folder's refusal names.
fn open_folder(
    holder: BorrowedFd<'_>,
    name: &OsStr,
    full: &Path,
    walked: &Path,
) -> Result<Option<OwnedFd>, Refusal> {
    match openat(holder, name, directory_flags(), Mode::empty()) {
        Ok(folder) => Ok(Some(folder)),
        Err(Errno::NOENT) => Ok(None),
        Err(Errno::LOOP) => Err(linked_ancestor(full, walked.to_path_buf())),
        // One supported platform says a link opened `O_NOFOLLOW` is not a
        // directory, so the name is asked what it is before that is believed.
        Err(Errno::NOTDIR) => Err(match kind_at(holder, name) {
            Some(FileType::Symlink) => linked_ancestor(full, walked.to_path_buf()),
            _ => errno_refusal_at("opening", full, name, Errno::NOTDIR),
        }),
        Err(errno) => Err(errno_refusal_at("opening", full, name, errno)),
    }
}

/// What is at a target's name, as far as a transition cares.
enum Found {
    Absent,
    /// A symbolic link, which this crate never writes through.
    Link,
    /// Something that is not a regular file: a directory, a pipe, a device or
    /// a socket.
    Other,
    /// A regular file, hashed through its own handle, whose name still meant
    /// that file once the hash was taken.
    Regular {
        state: PostState,
        mode: u32,
    },
}

/// Read the target `name` in `folder`, and confirm the name still means the
/// file that was read.
///
/// Three flags carry the reader's discipline: `O_NOFOLLOW`, so a link is
/// reported rather than read through; `O_NONBLOCK`, so a FIFO at a document's
/// name does not hold this call inside `open` until somebody writes to it; and
/// the kind proven through the descriptor before any byte is read.
///
/// **The identity comparison after the hash is what the hash cannot carry.** A
/// foreign writer that renames a new document over the name while this call
/// hashes leaves the handle on an orphaned inode whose bytes still hash to the
/// before-state; publishing on that answer would replace or remove a document
/// nobody read. The comparison refuses as [`Refusal::Republished`].
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns vault handles.
fn observe(
    folder: BorrowedFd<'_>,
    name: &OsStr,
    full: &Path,
    disturb: &mut dyn FnMut(Window),
) -> Result<Found, Refusal> {
    use std::os::unix::fs::PermissionsExt;
    let opened = match openat(folder, name, regular_flags(), Mode::empty()) {
        Ok(opened) => opened,
        Err(Errno::NOENT) => return Ok(Found::Absent),
        Err(Errno::LOOP) => return Ok(Found::Link),
        // A socket cannot be opened at all, and says so differently per
        // platform; either way no regular file is there.
        Err(Errno::NXIO | Errno::OPNOTSUPP) => return Ok(Found::Other),
        Err(errno) => {
            return match kind_at(folder, name) {
                Some(FileType::Symlink) => Ok(Found::Link),
                _ => Err(errno_refusal("opening", full, errno)),
            };
        }
    };
    let mut file = std::fs::File::from(opened);
    let metadata = file
        .metadata()
        .map_err(|error| environment("reading the identity of", full, &error))?;
    if !metadata.file_type().is_file() {
        return Ok(Found::Other);
    }
    let (hash, len) =
        hashed_from(&mut file).map_err(|error| environment("reading", full, &error))?;
    disturb(Window::Verifying);
    let read = identity_of(&metadata);
    match statat(folder, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if identity_of_stat(&stat) == read => Ok(Found::Regular {
            state: post_state(hash, len, &metadata),
            mode: metadata.permissions().mode(),
        }),
        Ok(stat) => Err(Refusal::Republished {
            path: full.to_path_buf(),
            read,
            current: identity_of_stat(&stat),
        }),
        // Removed while it was read: the same event as finding nothing there.
        Err(Errno::NOENT) => Ok(Found::Absent),
        Err(errno) => Err(errno_refusal("reading the identity of", full, errno)),
    }
}

/// What kind of file `name` in `folder` is, without following it, where that
/// can be read.
fn kind_at(folder: BorrowedFd<'_>, name: &OsStr) -> Option<FileType> {
    statat(folder, name, AtFlags::SYMLINK_NOFOLLOW)
        .ok()
        .map(|stat| FileType::from_raw_mode(stat.st_mode as _))
}

/// The shadow home's directory, opened as it is spelled: the home is this
/// crate's own boundary, not a name inside the vault.
fn open_home(shadows: &ShadowHome) -> Result<OwnedFd, Refusal> {
    open(shadows.directory(), anchor_flags(), Mode::empty())
        .map_err(|errno| errno_refusal("opening", shadows.directory(), errno))
}

/// Where the foreign stage is armed, act as a foreign writer on the target:
/// inside a publication, after the root is checked and before anything is read
/// again, through the target's folder handle.
///
/// What each act leads to is ADR 0031's reading of that writer, which the
/// publication then meets with no help from here: an edit or a removal of a
/// replace's or a remove's target is drift — except that removing a remove's
/// target lands it for the remove, which is found landed by another writer — and
/// an edit or a take at a create's name is a name taken. A removal against a
/// create finds nothing and the create lands; a take against anything but a
/// create is recorded and does nothing. Where the target's folder is missing
/// there is no handle to act through, and the act is recorded and does nothing.
#[cfg(any(test, feature = "induced-failure"))]
#[allow(clippy::disallowed_types)] // The seam's own foreign writer, acting through a vault handle.
fn act_as_foreign_writer(
    folder: &Folder,
    target: &Target<'_>,
    pending: &Pending,
    faults: Faulted<'_>,
) {
    use crate::faults::{FOREIGN_BYTES, ForeignAct};
    let Some(act) = faults.faults.foreign(faults.path) else {
        return;
    };
    let Folder::Reached(folder) = folder else {
        return;
    };
    let write_at = |flags: OFlags| {
        let flags = flags | OFlags::WRONLY | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        if let Ok(file) = openat(folder, target.name, flags, Mode::from_raw_mode(0o644)) {
            let _ = std::fs::File::from(file).write_all(FOREIGN_BYTES);
        }
    };
    match act {
        ForeignAct::Edit => write_at(OFlags::TRUNC),
        ForeignAct::Remove => {
            let _ = unlinkat(folder, target.name, AtFlags::empty());
        }
        ForeignAct::Take if matches!(pending, Pending::Create { .. }) => write_at(OFlags::EXCL),
        ForeignAct::Take => {}
    }
}

/// The faults a call runs under, and the vault-relative path its stages act
/// on, which is what a fired arm's record names.
#[derive(Clone, Copy)]
struct Faulted<'a> {
    faults: Faults,
    path: &'a Path,
}

impl<'a> Faulted<'a> {
    /// The error `stage` is armed to meet here, if any.
    fn check(&self, stage: Stage) -> std::io::Result<()> {
        self.faults.check(stage, self.path)
    }

    /// The same faults, acting on `path`.
    fn at<'b>(&self, path: &'b Path) -> Faulted<'b> {
        Faulted {
            faults: self.faults,
            path,
        }
    }
}

/// Get a folder's own entries onto the disk, and say whether they got there.
///
/// Through a handle the change was made in, so the folder synced is the folder
/// that changed whatever its name means by now. `sync_all` is `F_FULLFSYNC` on
/// macOS.
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns vault handles.
fn sync_folder(folder: BorrowedFd<'_>, faults: Faulted<'_>) -> Durability {
    if let Err(error) = faults.check(Stage::ParentSync) {
        return Durability::NotSynced(error);
    }
    let synced = folder
        .try_clone_to_owned()
        .and_then(|folder| std::fs::File::from(folder).sync_all());
    match synced {
        Ok(()) => Durability::Synced,
        Err(error) => Durability::NotSynced(error),
    }
}

/// The spelling `folder`'s listing holds for the entry named `name`, and what
/// is at it, on a folder proven to fold case; `None` where no entry folds with
/// `name`.
///
/// **The fold is proven here, on the folder the respell happens in**, from the
/// entry itself: an alternate ASCII spelling of the listed name that resolves
/// to the same file, and that the listing does not hold, proves the folder
/// folds — the same probe [`PathNormalizer::detect`](crate::PathNormalizer::detect)
/// makes of a vault root, asked through the folder's handle. It costs one
/// listing, which a folding root needs anyway to read the spelling, and two
/// lookups. A folder whose listing holds two entries that fold together tells
/// them apart, and one whose probe proves nothing is not proven to fold;
/// either refuses as [`Refusal::NotCaseFolding`].
fn observe_spelling(
    folder: BorrowedFd<'_>,
    name: &OsStr,
    full: &Path,
    disturb: &mut dyn FnMut(Window),
) -> Result<Option<(OsString, Found)>, Refusal> {
    let mut listed = Vec::new();
    let entries = Dir::read_from(folder)
        .map_err(|errno| errno_refusal("reading the folder of", full, errno))?;
    for entry in entries {
        let entry = entry.map_err(|errno| errno_refusal("reading the folder of", full, errno))?;
        let spelled = entry.file_name().to_bytes();
        if spelled != b"." && spelled != b".." && fold_together(spelled, name.as_bytes()) {
            listed.push(OsStr::from_bytes(spelled).to_owned());
        }
    }
    let spelling = match listed.len() {
        0 => return Ok(None),
        1 => listed.remove(0),
        _ => return Err(not_case_folding(full)),
    };
    let lookup = |spelled: &OsStr| match statat(folder, spelled, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => Lookup::Found(identity_of_stat(&stat)),
        Err(Errno::NOENT) => Lookup::Missing,
        Err(_) => Lookup::Unanswered,
    };
    // The listing holds one entry that folds with the name, so it holds no
    // alternate spelling of it.
    if probe_case_behavior_by(&spelling, lookup, |_| Some(false))
        != Some(CaseSensitivity::Insensitive)
    {
        return Err(not_case_folding(full));
    }
    let found = observe(folder, &spelling, full, disturb)?;
    Ok(Some((spelling, found)))
}

fn drifted(path: &Path, expected: ContentHash, observed: Option<PostState>) -> Refusal {
    Refusal::Drifted {
        path: path.to_path_buf(),
        expected,
        observed: observed.map(Box::new),
    }
}

fn not_regular(path: &Path) -> Refusal {
    environment(
        "reading",
        path,
        &invalid_data("the name does not identify a regular file"),
    )
}

fn uncontained(path: &Path) -> Refusal {
    environment(
        "resolving",
        path,
        &std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the path is not a name below the vault root",
        ),
    )
}

fn linked_ancestor(full: &Path, ancestor: PathBuf) -> Refusal {
    Refusal::LinkedAncestor {
        path: full.to_path_buf(),
        ancestor,
    }
}

fn invalid_data(message: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

fn errno_refusal(operation: &'static str, path: &Path, errno: Errno) -> Refusal {
    environment(
        operation,
        path,
        &std::io::Error::from_raw_os_error(errno.raw_os_error()),
    )
}

fn errno_refusal_at(
    operation: &'static str,
    path: &Path,
    component: &OsStr,
    errno: Errno,
) -> Refusal {
    environment_at(
        operation,
        path,
        component,
        &std::io::Error::from_raw_os_error(errno.raw_os_error()),
    )
}

fn not_case_folding(path: &Path) -> Refusal {
    Refusal::NotCaseFolding {
        path: path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::faults::Answer;
    use crate::scratch::Scratch;

    /// Stage `transition` at `relative` in `scratch`'s vault under `faults`.
    fn stage_in(
        scratch: &Scratch,
        relative: &str,
        transition: Transition<'_>,
        faults: Faults,
    ) -> Result<Staging, Refusal> {
        stage_where(
            &scratch.at(""),
            Path::new(relative),
            transition,
            scratch.shadows(),
            faults,
        )
    }

    /// Stage `transition`, which must stage rather than land.
    #[track_caller]
    fn staged_in(scratch: &Scratch, relative: &str, transition: Transition<'_>) -> Staged {
        match stage_in(scratch, relative, transition, Faults::NONE).expect("staged") {
            Staging::Staged(staged) => staged,
            Staging::Landed(landed) => panic!("{:?} staged as landed", landed.path()),
        }
    }

    /// Publish `staged` in `scratch`'s vault under `faults`, with `disturb`
    /// standing in the windows inside publication.
    fn publish_in(
        scratch: &Scratch,
        staged: Staged,
        faults: Faults,
        disturb: &mut dyn FnMut(Window),
    ) -> Result<Publication, Refusal> {
        publish_disturbed(&scratch.at(""), staged, scratch.shadows(), faults, disturb)
    }

    /// The publication a call made, where it wrote.
    #[track_caller]
    fn written(publication: Publication) -> Published {
        match publication {
            Publication::Wrote(published) => published,
            Publication::Found(found) => panic!("found landed: {found:?}"),
        }
    }

    fn replace_old_with_new() -> Transition<'static> {
        Transition::Replace {
            before: ContentHash::of(b"old"),
            content: b"new",
        }
    }

    /// **The bar on the shadow's fsync.** The shadow's bytes are on the disk
    /// before any name points at them, so a shadow that cannot be synced
    /// refuses staging and leaves nothing behind.
    ///
    /// The forbidden shape is a publication of bytes only the page cache has
    /// seen: a power cut then leaves the name pointing at content that never
    /// reached the platter.
    #[test]
    fn a_shadow_that_cannot_be_synced_refuses_staging_and_leaves_no_shadow() {
        let scratch = Scratch::new("write-shadow-sync");
        let path = scratch.place("note.md", b"old");
        let refusal = stage_in(
            &scratch,
            "note.md",
            replace_old_with_new(),
            Faults::at(&[(Stage::ShadowSync, Answer::Fails(std::io::ErrorKind::Other))]),
        )
        .expect_err("a shadow that cannot be synced");

        assert!(
            matches!(&refusal, Refusal::Environment { operation, .. } if *operation == "syncing"),
            "{refusal}"
        );
        assert_eq!(scratch.read(&path), b"old");
        assert!(
            scratch.shadow_names().is_empty(),
            "a shadow was left behind"
        );
    }

    /// **The bar on a full disk while staging.** A shadow that cannot take the
    /// content refuses staging, for a create as for a replace, and the shadow
    /// is cleaned up: a staging refusal publishes nothing and leaves nothing.
    #[test]
    fn a_shadow_that_cannot_take_the_content_refuses_staging_and_cleans_up() {
        let scratch = Scratch::new("write-shadow-write");
        let path = scratch.place("note.md", b"old");
        for (relative, transition) in [
            ("note.md", replace_old_with_new()),
            ("fresh.md", Transition::Create { content: b"fresh" }),
        ] {
            let refusal = stage_in(
                &scratch,
                relative,
                transition,
                Faults::at(&[(Stage::ShadowWrite, Answer::MeetsAFullDisk)]),
            )
            .expect_err("a shadow that cannot be written");

            assert!(refusal.is_os_error(libc::ENOSPC), "{refusal}");
            assert!(
                scratch.shadow_names().is_empty(),
                "a shadow was left behind"
            );
        }
        assert_eq!(scratch.read(&path), b"old");
        assert!(!scratch.exists(&scratch.at("fresh.md")));
    }

    /// A shadow that cannot be opened refuses staging, and nothing is left at
    /// the target or in the home.
    #[test]
    fn a_shadow_that_cannot_be_opened_refuses_staging() {
        let scratch = Scratch::new("write-shadow-open");
        let refusal = stage_in(
            &scratch,
            "fresh.md",
            Transition::Create { content: b"fresh" },
            Faults::at(&[(
                Stage::ShadowCreate,
                Answer::Fails(std::io::ErrorKind::PermissionDenied),
            )]),
        )
        .expect_err("a shadow whose open fails");

        assert!(
            matches!(
                &refusal,
                Refusal::Environment { operation, kind, .. }
                    if *operation == "creating" && *kind == std::io::ErrorKind::PermissionDenied
            ),
            "{refusal}"
        );
        assert!(!scratch.exists(&scratch.at("fresh.md")));
        assert!(scratch.shadow_names().is_empty());
    }

    /// **The bar on the swap.** A rename that fails refuses, leaves the target
    /// as it was, and takes the shadow with it.
    #[test]
    fn a_swap_that_fails_leaves_the_target_as_it_was() {
        let scratch = Scratch::new("write-swap");
        let path = scratch.place("note.md", b"old");
        let staged = staged_in(&scratch, "note.md", replace_old_with_new());
        let refusal = publish_in(
            &scratch,
            staged,
            Faults::at(&[(
                Stage::Swap,
                Answer::Fails(std::io::ErrorKind::PermissionDenied),
            )]),
            &mut |_| {},
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

    /// **The bar on reported durability.** A folder sync that fails after the
    /// publication act is reported as not synced, with the error, and the
    /// target is landed — for every kind.
    ///
    /// Two forbidden shapes. Reporting it as a refusal: the change is at the
    /// name and every reader sees it, so a caller told "nothing happened"
    /// writes a second time over its own first write. And dropping it: an
    /// applier that removes a move's source once its destination "landed"
    /// would then destroy the only durable copy.
    #[test]
    fn a_folder_sync_that_fails_is_reported_with_the_target_landed() {
        let scratch = Scratch::new("write-parent-sync");
        let path = scratch.place("note.md", b"old");
        let parent_sync_fails = Faults::at(&[(Stage::ParentSync, Answer::MeetsAFullDisk)]);

        for (relative, transition, after) in [
            ("note.md", replace_old_with_new(), Some(&b"new"[..])),
            (
                "fresh.md",
                Transition::Create { content: b"fresh" },
                Some(&b"fresh"[..]),
            ),
            (
                "note.md",
                Transition::Remove {
                    before: ContentHash::of(b"new"),
                },
                None,
            ),
        ] {
            let staged = staged_in(&scratch, relative, transition);
            let published = written(
                publish_in(&scratch, staged, parent_sync_fails, &mut |_| {})
                    .expect("a landed publication whose durability was not confirmed"),
            );

            let Durability::NotSynced(error) = &published.durability else {
                panic!("{relative}: a failed sync was reported synced");
            };
            assert_eq!(error.raw_os_error(), Some(libc::ENOSPC));
            match after {
                Some(content) => {
                    assert_eq!(scratch.read(&scratch.at(relative)), content);
                    assert!(matches!(published.after, AfterState::Present(state)
                            if state.content_hash == ContentHash::of(content)));
                }
                None => {
                    assert!(!scratch.exists(&scratch.at(relative)));
                    assert!(matches!(published.after, AfterState::Absent));
                }
            }
        }
        let _ = path;
        assert!(scratch.shadow_names().is_empty());
    }

    /// **The bar on confirming a landing.** Confirming a target a re-send
    /// finds landed syncs its folder, and a sync that fails is reported.
    ///
    /// The forbidden shape is a confirmation that only looks: a crash between
    /// the rename and the folder sync leaves a directory entry a power cut can
    /// lose, and the re-send is the one chance to make it durable.
    #[test]
    fn confirming_a_landing_syncs_its_folder_and_reports_a_failure() {
        let scratch = Scratch::new("write-confirm-sync");
        scratch.place("note.md", b"new");
        let Staging::Landed(landed) =
            stage_in(&scratch, "note.md", replace_old_with_new(), Faults::NONE).expect("landed")
        else {
            panic!("a target at its after-state staged");
        };

        let confirmed = confirm_landed_where(
            &scratch.at(""),
            &landed,
            Faults::at(&[(Stage::ParentSync, Answer::Fails(std::io::ErrorKind::Other))]),
        )
        .expect("a confirmed landing");

        assert!(
            matches!(confirmed.durability, Durability::NotSynced(_)),
            "the confirmation never reached the folder sync"
        );
        assert!(matches!(confirmed.after, AfterState::Present(_)));
    }

    /// **The bar the hash cannot carry.** A foreign writer that renames a copy
    /// of the same bytes over the target while publication hashes it is caught
    /// by the identity comparison, for a replace and for a remove.
    ///
    /// The handle hashed an orphaned inode whose bytes still match the
    /// before-state, so the hash agrees for the wrong reason. The forbidden
    /// shape is a verification that compares content alone: it replaces or
    /// removes a document somebody else just published.
    #[test]
    fn a_foreign_replacement_while_publication_reads_is_refused() {
        let scratch = Scratch::new("write-verify-republished");
        for transition in [
            replace_old_with_new(),
            Transition::Remove {
                before: ContentHash::of(b"old"),
            },
        ] {
            let path = scratch.place("note.md", b"old");
            let original = identity_at(&path);
            let staged = staged_in(&scratch, "note.md", transition);
            let refusal = publish_in(&scratch, staged, Faults::NONE, &mut |window| {
                if window == Window::Verifying {
                    republish(&scratch, &path, b"old");
                }
            })
            .expect_err("a foreign replacement inside the window");

            let Refusal::Republished { read, current, .. } = &refusal else {
                panic!("a foreign replacement was not reported as one: {refusal}");
            };
            assert_eq!(*read, original);
            assert_ne!(*current, original);
            assert_eq!(
                identity_at(&path),
                *current,
                "publication ran over the foreign writer"
            );
            assert!(
                scratch.shadow_names().is_empty(),
                "a shadow was left behind"
            );
        }
    }

    /// **The bar on the exclusive publication.** A name taken after every
    /// check passed and before the rename refuses the create, and the racer's
    /// document survives.
    ///
    /// The window is one call wide, so the racer is injected into it. The
    /// forbidden shape is a plain rename after an absence check: it replaces
    /// the racer's document with this create's, and reports a create.
    #[test]
    fn a_name_taken_just_before_the_rename_refuses_the_create() {
        let scratch = Scratch::new("write-create-noreplace");
        let path = scratch.at("fresh.md");
        let staged = staged_in(
            &scratch,
            "fresh.md",
            Transition::Create { content: b"ours" },
        );
        let mut raced = false;
        let refusal = publish_in(&scratch, staged, Faults::NONE, &mut |window| {
            if window == Window::Publishing {
                republish(&scratch, &path, b"the racer's document");
                raced = true;
            }
        })
        .expect_err("a create onto a name taken at the last moment");

        assert!(raced, "the window was never entered");
        assert_eq!(refusal, Refusal::DestinationExists { path: path.clone() });
        assert_eq!(scratch.read(&path), b"the racer's document");
        assert!(
            scratch.shadow_names().is_empty(),
            "a shadow was left behind"
        );
    }

    /// **The bar on a cleanup that cannot happen.** A shadow the filesystem
    /// refuses to remove does not change the refusal the caller is given, and
    /// what it leaks is inert.
    #[test]
    fn a_cleanup_that_cannot_happen_still_returns_the_original_outcome() {
        let scratch = Scratch::new("write-cleanup-blocked");
        let path = scratch.place("note.md", b"old");
        let staged = staged_in(&scratch, "note.md", replace_old_with_new());
        let refusal = publish_in(
            &scratch,
            staged,
            Faults::at(&[
                (
                    Stage::Swap,
                    Answer::Fails(std::io::ErrorKind::PermissionDenied),
                ),
                (
                    Stage::Cleanup,
                    Answer::Fails(std::io::ErrorKind::PermissionDenied),
                ),
            ]),
            &mut |_| {},
        )
        .expect_err("a rename that fails with a removal that cannot happen");

        assert!(
            matches!(
                &refusal,
                Refusal::Environment { operation, .. } if *operation == "renaming onto"
            ),
            "the caller was told about the cleanup instead of the write: {refusal}"
        );
        assert_eq!(scratch.read(&path), b"old");
        let leaked = scratch.shadow_names();
        assert_eq!(
            leaked.len(),
            1,
            "expected one leaked shadow, got {leaked:?}"
        );
        assert!(crate::shadow::is_shadow_name(std::ffi::OsStr::new(
            &leaked[0]
        )));

        // And the leak is inert: the next publication neither trips over it
        // nor touches it.
        let staged = staged_in(&scratch, "note.md", replace_old_with_new());
        publish_in(&scratch, staged, Faults::NONE, &mut |_| {}).expect("a replacement");
        assert_eq!(scratch.read(&path), b"new");
        assert_eq!(scratch.shadow_names(), leaked);
    }

    /// **The bar on removing only our own shadow.** A discard whose shadow's
    /// name now holds another file leaves that file alone.
    ///
    /// A staged shadow outlives its staging call, so a sweep can empty its
    /// name and something else can take it. The forbidden shape is a removal
    /// by name, which deletes a file this call did not make.
    #[test]
    fn a_discard_never_removes_a_file_it_did_not_stage() {
        let scratch = Scratch::new("write-discard-foreign");
        let staged = staged_in(
            &scratch,
            "fresh.md",
            Transition::Create { content: b"ours" },
        );
        let names = scratch.shadow_names();
        let shadow = scratch.shadows().directory().join(&names[0]);
        republish(&scratch, &shadow, b"somebody else's file");

        discard(staged, scratch.shadows());

        assert_eq!(scratch.read(&shadow), b"somebody else's file");
    }

    /// **The bar on the mask.** Carrying a mode forward carries the permission
    /// bits and drops the set-user-id and set-group-id bits.
    #[test]
    #[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Harness scaffolding: a file to carry onto.
    fn carrying_a_mode_forward_drops_the_setuid_bits() {
        let scratch = Scratch::new("write-mode-mask");
        let path = scratch.place("shadow", b"");
        let file = std::fs::File::open(&path).expect("the file");
        carry_mode_forward(&file, 0o106755);

        assert_eq!(
            scratch.mode_at(&path),
            0o755,
            "the carry put a set-user-id or set-group-id bit on a file this process owns"
        );
    }

    /// **The bar on when the mode is carried.** A staged shadow has the
    /// replaced file's permission bits before any content goes into it.
    ///
    /// Read off a shadow that leaked before its first byte: staging is made to
    /// fail at the content and the cleanup after it, so what is left at rest is
    /// the file exactly as staging made it.
    #[test]
    fn a_shadow_carries_its_mode_before_any_content() {
        let scratch = Scratch::new("write-mode-first");
        let path = scratch.place("note.md", b"old");
        scratch.set_mode(&path, 0o600);

        stage_in(
            &scratch,
            "note.md",
            replace_old_with_new(),
            Faults::at(&[
                (Stage::ShadowWrite, Answer::Fails(std::io::ErrorKind::Other)),
                (
                    Stage::Cleanup,
                    Answer::Fails(std::io::ErrorKind::PermissionDenied),
                ),
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
        assert_eq!(scratch.read(&leaked), b"");
        assert_eq!(scratch.mode_at(&leaked), 0o600);
    }

    /// **The bar on a shadow name that is already taken.** The taken name is
    /// skipped, the shadow is opened under the next one, and what was at the
    /// taken name is neither truncated nor reopened.
    #[test]
    fn a_taken_shadow_name_is_skipped_rather_than_opened() {
        let scratch = Scratch::new("write-shadow-collision");
        let taken = scratch.shadows().directory().join("norn-shadow-1-0");
        #[allow(clippy::disallowed_methods)] // Harness scaffolding: a dead writer's residue.
        std::fs::write(&taken, b"a dead writer's bytes").expect("residue");
        let residue = identity_at(&taken);

        let home = open_home(scratch.shadows()).expect("the home");
        let mut names: Vec<OsString> = vec!["norn-shadow-1-1".into(), "norn-shadow-1-0".into()];
        let (opened, _) = create_shadow(
            home.as_fd(),
            scratch.shadows(),
            &mut || names.pop().expect("a name to try"),
            unarmed(),
        )
        .expect("a shadow under a free name");

        assert_eq!(
            opened,
            OsString::from("norn-shadow-1-1"),
            "the taken name was opened"
        );
        assert_eq!(scratch.read(&taken), b"a dead writer's bytes");
        assert_eq!(identity_at(&taken), residue, "the residue was replaced");
    }

    /// **The bar on the shadow-name bound.** A shadow home in which every name
    /// staging is handed is already taken refuses at the bound, with the
    /// refusal the last open gave.
    #[test]
    fn a_shadow_home_whose_every_name_is_taken_refuses_at_the_bound() {
        let scratch = Scratch::new("write-shadow-exhausted");
        let home = open_home(scratch.shadows()).expect("the home");
        let mut tried: Vec<PathBuf> = Vec::new();

        let refusal = create_shadow(
            home.as_fd(),
            scratch.shadows(),
            &mut || {
                let name = format!("norn-shadow-1-{}", tried.len());
                let taken = scratch.shadows().directory().join(&name);
                #[allow(clippy::disallowed_methods)]
                // Harness scaffolding: a home somebody else is filling.
                std::fs::write(&taken, b"somebody else's bytes").expect("a taken name");
                tried.push(taken);
                name.into()
            },
            unarmed(),
        )
        .expect_err("a home in which every name is taken");

        assert_eq!(tried.len(), 64, "the house bound is 64 attempts");
        assert_eq!(NAME_ATTEMPTS, 64, "the bound staging counts to moved");
        let last = tried.last().expect("a name tried");
        assert!(
            matches!(
                &refusal,
                Refusal::Environment { operation, kind, path, .. }
                    if *operation == "creating"
                        && *kind == std::io::ErrorKind::AlreadyExists
                        && path == last
            ),
            "the refusal is not the last open's: {refusal}"
        );
    }

    /// **The bar on a landing that races the exclusive rename.** A name taken
    /// at the last moment by exactly this create's content is found landed,
    /// not refused, and the file there is the other writer's.
    #[test]
    fn a_name_taken_with_this_content_just_before_the_rename_is_found() {
        let scratch = Scratch::new("write-create-raced-landed");
        let path = scratch.at("fresh.md");
        let staged = staged_in(
            &scratch,
            "fresh.md",
            Transition::Create { content: b"ours" },
        );
        let publication = publish_in(&scratch, staged, Faults::NONE, &mut |window| {
            if window == Window::Publishing {
                republish(&scratch, &path, b"ours");
            }
        })
        .expect("a landing another writer made at the last moment");

        let Publication::Found(found) = publication else {
            panic!("a landing another writer made was reported written");
        };
        assert!(
            matches!(found.after, AfterState::Present(state) if (state.dev, state.ino) == (identity_at(&path).dev, identity_at(&path).ino))
        );
        assert!(scratch.shadow_names().is_empty(), "the shadow was kept");
    }

    /// **The bar on a made folder swapped for a link.** A folder a create made
    /// that became a link before it was opened refuses the create, and nothing
    /// is made or published through the link.
    ///
    /// The window is one call wide — between the `mkdirat` and the open — so
    /// the swap is injected into it. The forbidden shape is a descent into the
    /// folder by name after making it, which follows the link out of the vault.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: playing the foreign writer.
    fn a_made_folder_swapped_for_a_link_refuses_the_create() {
        let scratch = Scratch::new("write-folder-link");
        let outside = scratch.directory("outside");
        let staged = staged_in(
            &scratch,
            "a/b/fresh.md",
            Transition::Create { content: b"ours" },
        );
        let made = scratch.at("a");
        let refusal = publish_in(&scratch, staged, Faults::NONE, &mut |window| {
            if window == Window::FolderMade && scratch.exists(&made) && !made.is_symlink() {
                std::fs::remove_dir(&made).expect("taking the made folder");
                std::os::unix::fs::symlink(&outside, &made).expect("a link in its place");
            }
        })
        .expect_err("a made folder that became a link");

        assert!(
            matches!(&refusal, Refusal::LinkedAncestor { ancestor, .. } if *ancestor == made),
            "{refusal}"
        );
        assert!(
            std::fs::read_dir(&outside)
                .expect("outside")
                .next()
                .is_none(),
            "something was made through the link"
        );
        assert!(scratch.shadow_names().is_empty());
    }

    /// **The bar on a create refused after making folders.** The folders it
    /// made that are still empty are removed, deepest first, and a made folder
    /// another writer put a file into stays with the file.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: playing the foreign writer.
    fn a_refused_create_removes_only_its_folders_that_are_still_empty() {
        let scratch = Scratch::new("write-folder-cleanup");
        let swap_fails = Faults::at(&[(Stage::Swap, Answer::Fails(std::io::ErrorKind::Other))]);

        let staged = staged_in(
            &scratch,
            "a/b/fresh.md",
            Transition::Create { content: b"ours" },
        );
        publish_in(&scratch, staged, swap_fails, &mut |_| {}).expect_err("a refused create");
        assert!(
            !scratch.exists(&scratch.at("a")),
            "a refused create left its folders"
        );

        let staged = staged_in(
            &scratch,
            "a/b/fresh.md",
            Transition::Create { content: b"ours" },
        );
        let foreign = scratch.at("a/theirs.md");
        publish_in(&scratch, staged, swap_fails, &mut |window| {
            if window == Window::FolderMade && !scratch.exists(&foreign) {
                std::fs::write(&foreign, b"theirs").expect("a foreign document");
            }
        })
        .expect_err("a refused create");
        assert!(
            !scratch.exists(&scratch.at("a/b")),
            "an empty made folder was left"
        );
        assert_eq!(scratch.read(&foreign), b"theirs");
        assert!(scratch.shadow_names().is_empty());
    }

    /// A folder sync that fails after a create made folders is reported, and
    /// the create and its folders have landed.
    #[test]
    fn a_failed_sync_of_made_folders_is_reported_with_the_create_landed() {
        let scratch = Scratch::new("write-folder-sync");
        let staged = staged_in(
            &scratch,
            "a/b/fresh.md",
            Transition::Create { content: b"ours" },
        );
        let published = written(
            publish_in(
                &scratch,
                staged,
                Faults::at(&[(Stage::ParentSync, Answer::MeetsAFullDisk)]),
                &mut |_| {},
            )
            .expect("a landed create"),
        );

        assert!(matches!(published.durability, Durability::NotSynced(_)));
        assert_eq!(
            published.made_folders,
            vec![PathBuf::from("a"), PathBuf::from("a/b")]
        );
        assert_eq!(scratch.read(&scratch.at("a/b/fresh.md")), b"ours");
    }

    /// Where a respell stands, judged from the listed spelling and the bytes:
    /// the three states, and drift for everything else.
    ///
    /// The judgment is asked directly because the states are reached on a
    /// folding root only, which a case-sensitive host cannot arrange; the
    /// suite's respell cases run the whole protocol where the root folds.
    #[test]
    fn a_respell_is_judged_before_halfway_landed_or_drifted() {
        let (from, to) = (OsStr::new("note.md"), OsStr::new("Note.md"));
        let (before, after) = (ContentHash::of(b"old"), ContentHash::of(b"new"));
        let at = |hash: ContentHash| Found::Regular {
            state: PostState {
                content_hash: hash,
                len: 3,
                mtime: std::time::SystemTime::UNIX_EPOCH,
                ino: 1,
                dev: 1,
            },
            mode: 0o644,
        };
        let judge = |spelling: &str, found, after| {
            judge_respell(
                OsStr::new(spelling),
                found,
                from,
                to,
                before,
                after,
                Path::new("note.md"),
            )
        };

        assert!(matches!(
            judge("note.md", at(before), after),
            Ok(Respelled::Before { .. })
        ));
        assert!(matches!(
            judge("note.md", at(after), after),
            Ok(Respelled::Halfway { .. })
        ));
        assert!(matches!(
            judge("Note.md", at(after), after),
            Ok(Respelled::Landed { .. })
        ));
        // Without new content, the old spelling is the before-state and the new
        // spelling is landed.
        assert!(matches!(
            judge("note.md", at(before), before),
            Ok(Respelled::Before { .. })
        ));
        assert!(matches!(
            judge("Note.md", at(before), before),
            Ok(Respelled::Landed { .. })
        ));
        for (spelling, found) in [
            ("Note.md", at(before)),
            ("NOTE.md", at(before)),
            ("NOTE.md", at(after)),
            ("note.md", at(ContentHash::of(b"theirs"))),
            ("note.md", Found::Absent),
        ] {
            assert!(
                matches!(judge(spelling, found, after), Err(Refusal::Drifted { .. })),
                "{spelling} was not drift"
            );
        }
    }

    /// No fault armed, acting on a path no record names.
    fn unarmed() -> Faulted<'static> {
        Faulted {
            faults: Faults::NONE,
            path: Path::new("note.md"),
        }
    }

    /// A foreign writer with its own atomic-replace protocol: `content` at
    /// `path`, in a different file.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: playing the foreign writer.
    fn republish(scratch: &Scratch, path: &Path, content: &[u8]) {
        let theirs = scratch.path("norn-fs-foreign-staging");
        std::fs::write(&theirs, content).expect("a foreign staging");
        std::fs::rename(&theirs, path).expect("a foreign publish");
    }

    /// The `(device, inode)` pair `path` resolves to.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: judging which file a name means.
    fn identity_at(path: &Path) -> Identity {
        identity_of(&std::fs::metadata(path).expect("a file at the path"))
    }
}
