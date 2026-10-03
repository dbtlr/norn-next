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
//! so a refusal found while staging writes nothing (ADR 0032); the kernel
//! offers the per-target phases and the applier composes them.
//!
//! **A target at its after-state has landed, whichever writer put it there.**
//! Staging reports it as [`Staging::Landed`]; publication reports it as
//! [`Publication::Found`], discards the shadow and syncs the folder. Only
//! [`Publication::Wrote`], and the step [`Publication::Interrupted`] names,
//! say this call changed the vault.
//!
//! **Every call is held to the plan's vault root.** Staging and emptying
//! folders take the root's `(device, inode)` the plan was made against, and
//! publication and confirming a landing take it from what staging recorded; a
//! root whose spelling now names another directory refuses as
//! [`Refusal::RootReplaced`]. The one anchor that is no vault root is the
//! folder of a `schema_source` outside the vault, which a schema write is
//! anchored at (ADR 0034): its caller reads that folder's `(device, inode)`
//! when it stages, since no plan carries it, and the kernel holds the call
//! to it exactly as it holds one to a root.
//!
//! **On a root that folds case, a name is the spelling its folder lists.** A
//! target the volume resolves under another spelling is not this target: a
//! create refuses it as a taken name, a replace or a remove as drift, and it is
//! never found landed.
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
//!    shadow home — reached, where the home is the fallback under the vault
//!    root, by the same no-follow descent from that root — given a replaced
//!    file's permission bits before a byte of content goes in, and fsynced. [`Staged`] records the shadow's name and
//!    `(device, inode)` and nothing that holds it open.
//!
//! A staging refusal leaves no shadow behind.
//!
//! [`judge`] is the first three steps alone: staging's own judgment of a
//! target, reaching the refusal [`stage`] would and writing nothing, which a
//! preview of a plan answers from.
//!
//! # Publication
//!
//! [`publish`] asks every question again, because the world had the whole
//! interval between the phases to change:
//!
//! 1. **The root is reopened** and refuses as [`Refusal::RootReplaced`] when
//!    its `(device, inode)` is not the one staging recorded.
//! 2. **The descent is made again** to the target's folder, through no link.
//! 3. **The target is verified again** exactly as staging read it: at its
//!    after-state it is found landed, and otherwise a create must still be
//!    absent, and a replace or a remove still at its before-state.
//! 4. **A create makes its missing folders**, one level at a time with
//!    `mkdirat` and each opened again through no link. A create refused after
//!    that removes the folders it made that are still empty, and names any it
//!    had to leave.
//! 5. **The shadow is confirmed, last, just before the act**: opened through
//!    the shadow home's directory without following a link, compared by
//!    identity with what staging made, and hashed again. A shadow that is gone
//!    or changed is an environmental refusal for this target — an I/O failure,
//!    never drift — because a staged shadow outlives its staging call and a
//!    sweep or a sync client can reach it.
//! 6. **The publication act runs through the folder's handle.** A create is a
//!    rename that never replaces a name (`renameat2` with `RENAME_NOREPLACE`
//!    on Linux, `renameatx_np` with `RENAME_EXCL` on macOS); a replace is a
//!    plain rename; a remove is an `unlinkat`. A name taken at the last
//!    moment refuses the create as [`Refusal::DestinationExists`], and a
//!    filesystem that cannot rename exclusively refuses it as
//!    [`Refusal::ExclusiveCreateUnsupported`] — there is no fallback, because
//!    every fallback is a check followed by a rename.
//! 7. **The folder is synced through the same handle.** A create — published,
//!    or found landed — syncs every folder from the root down to its own, so a
//!    folder a crashed attempt made and never synced is made durable by the
//!    re-send that finds it. The result is reported as [`Durability`] rather
//!    than dropped.
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
//! step, because on a folding root the new spelling *is* the entry. A rename
//! that fails after the first step landed is [`Publication::Interrupted`]: the
//! content is published under the old spelling, and a re-send finds the
//! respell halfway.
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
//! a remove. The window is the width of one call. A create has no such window
//! at its name: the exclusive rename is its whole question.
//!
//! **The shadow has the same one-call window**, stated the same way. The
//! rename moves the shadow by its name, so a foreign replacement of the shadow
//! between its confirmation and the rename is what gets published. The
//! confirmation is the last check before the act to keep that window one call
//! wide, and nothing after the rename checks it again: the home is this crate's
//! own directory, which nothing but a sweep or a sync client reaches.
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
use std::path::{Path, PathBuf};

use rustix::fs::{
    AtFlags, Dir, Mode, OFlags, RenameFlags, fstat, mkdirat, open, openat, renameat, renameat_with,
    statat, unlinkat,
};
use rustix::io::Errno;

use crate::faults::{Faults, Stage, Window};
use crate::hash::{ContentHash, shadow_hashed_from, target_hashed_from};
use crate::identity::{Identity, PostState, identity_of, identity_of_stat, post_state};
use crate::open::{Step, Stopped, anchor_flags, contained_names, regular_flags, step_into};
use crate::path::{
    CaseSensitivity, Lookup, alternate_ascii_case, fold_together, probe_case_behavior_by,
};
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
    ///
    /// A create publishes at the mode an ordinary create takes under the
    /// process's umask, because there is no file to carry a mode from — and a
    /// move's destination, which a plan resolves into a create, is no
    /// exception: the moved document lands at the default mode, as the
    /// one-shot move did before the split.
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
#[must_use]
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
    /// Which transition landed, which decides what confirming it syncs and
    /// whether the spelling is part of the after-state.
    landing: Landing,
}

/// Which kind of transition a landing is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Landing {
    /// A create: every folder from the root down to its own is synced, since a
    /// crashed attempt may have made them.
    Created,
    /// A replace: its folder is synced.
    Replaced,
    /// A remove: the deepest folder still there is synced.
    Removed,
    /// A respell: landed only where the folder's listing spells the name as
    /// the path does, and synced like a create.
    Respelled,
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
/// It covers every sync the call made — the target's folder, every folder a
/// create's path runs through, and a respell's two steps. A change the
/// filesystem has not confirmed durable is still a change every reader sees,
/// so this is never a refusal; whether it stops what comes next is the
/// caller's to decide.
#[must_use]
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
#[must_use]
#[derive(Debug)]
pub enum Publication {
    /// This call changed the vault. The own-write ledger records this, because
    /// a filesystem event about the target is coming.
    Wrote(Published),
    /// The target was already at its after-state — another writer put it
    /// there — so this call wrote nothing, discarded its shadow and synced the
    /// folders. No event of this call's is coming, and nothing is recorded.
    Found(Confirmed),
    /// A respell whose first step landed and whose rename did not: ADR 0032's
    /// interrupted, for this one target.
    Interrupted(Interrupted),
}

/// A target this call published.
#[must_use]
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

/// A respell that published its content under the old spelling and did not
/// rename it.
///
/// `published` is the first step, and it is what the own-write ledger records:
/// an event is coming for the old spelling, which on a folding root is the one
/// key both spellings share. A re-send stages the respell halfway and finishes
/// the rename.
#[must_use]
#[derive(Debug)]
#[non_exhaustive]
pub struct Interrupted {
    /// The first step, landed under the old spelling.
    pub published: Published,
    /// Why the rename did not happen.
    pub cause: Refusal,
}

/// A target found at its after-state, verified and its folders synced.
#[must_use]
#[derive(Debug)]
#[non_exhaustive]
pub struct Confirmed {
    /// What the target holds.
    pub after: AfterState,
    /// Whether the folders holding the target are on the disk.
    pub durability: Durability,
}

/// What emptying folders upward removed.
#[must_use]
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
/// leaves behind. `anchor` is the vault root, resolved as it is spelled, and
/// `root` is the `(device, inode)` the plan was made against: an anchor that
/// names another directory refuses. For the one target outside the vault, a
/// schema write to a `schema_source` there (ADR 0034), `anchor` is the
/// source's folder, resolved as it is spelled, and `root` is that folder's
/// `(device, inode)` as its caller read it when staging. `path` is relative
/// to `anchor` and names a file below it, never a parent, a root or a prefix.
///
/// **Its caller is the one applier** (`norn-host`'s `applier`, Layer 4
/// plan-apply), which stages every target of a plan before it publishes any.
///
/// Its caller is `norn-host`'s one applier, which the apply job runs holding
/// an entry's claim.
pub fn stage(
    anchor: &Path,
    root: Identity,
    path: &Path,
    transition: Transition<'_>,
    shadows: &ShadowHome,
) -> Result<Staging, Refusal> {
    // The public entry points are where the fault seam is reachable from
    // outside this crate, and only under the `induced-failure` feature: the
    // process-death bars are stated about a process that does not return.
    stage_where(anchor, root, path, transition, shadows, Faults::entry())
}

/// Judge `transition` at `path` below `anchor` exactly as [`stage`] judges
/// it, and stage nothing.
///
/// **This is staging's own judgment, not a second one**: the request's shape,
/// the root held to `root`, the anchored descent through no link — so a target
/// beneath a linked folder refuses as [`Refusal::LinkedAncestor`] — and what
/// stands at the name judged against the transition, each refusing as
/// [`stage`] would. What it does not reach is the shadow itself, its home and
/// the bytes written into it, which are the write. It reads and opens, and
/// changes nothing.
///
/// **Its caller is the one applier's preview** (`norn-host`'s `applier`,
/// Layer 4 plan-apply), which judges a resolved plan as an apply of it would
/// and answers the refusal staging would meet, or the plan.
pub fn judge(
    anchor: &Path,
    root: Identity,
    path: &Path,
    transition: Transition<'_>,
) -> Result<(), Refusal> {
    look(anchor, root, path, &anchor.join(path), &transition).map(drop)
}

/// Publish what [`stage`] staged.
///
/// See the [module documentation](self) for what publication asks again, the
/// residual race and the crash windows. `anchor` and `shadows` are the ones
/// the target was staged under; the root is held to the one staging recorded,
/// and a different home holds no such shadow.
///
/// **Its caller is the one applier** (`norn-host`'s `applier`, Layer 4
/// plan-apply), which publishes creates, then replaces, then removes, once
/// every target staged.
///
/// Its caller is `norn-host`'s one applier, which the apply job runs holding
/// an entry's claim.
pub fn publish(
    anchor: &Path,
    staged: Staged,
    shadows: &ShadowHome,
) -> Result<Publication, Refusal> {
    publish_disturbed(anchor, staged, shadows, Faults::publication(), &mut |_| {})
}

/// Verify a target staging found already landed, and sync its folders again.
///
/// A re-send of a plan that a crash interrupted finds its landed targets here,
/// and the sync is why it is more than a look: the crash may have come
/// between the publication act and the folder syncs, and a directory entry is
/// not durable until one runs. A create's and a respell's landing syncs every
/// folder from the root down to its own, since the crashed attempt may have
/// made them. The root is held to the one staging recorded. A target that no
/// longer holds the after-state refuses — drift where a content hash was
/// expected, a taken name where absence was.
///
/// **Its caller is the one applier** (`norn-host`'s `applier`, Layer 4
/// plan-apply), on a target a re-sent resolved plan finds landed.
///
/// Its caller is `norn-host`'s one applier, which the apply job runs holding
/// an entry's claim.
pub fn confirm_landed(anchor: &Path, landed: &Landed) -> Result<Confirmed, Refusal> {
    confirm_landed_where(anchor, landed, Faults::entry())
}

/// Remove a staged target's shadow without publishing it.
///
/// For a plan torn down between the phases: a refusal while staging a later
/// target, or a lifecycle end. The shadow is removed only while its name still
/// means the file staging made, and a removal that fails is left to the shadow
/// home's sweep, for the reason a publication's own cleanup is (see
/// [`crate::shadow`]). A fallback home under a vault root that no longer is
/// the staged one is not reached, and its shadow is left to that sweep too.
///
/// **Its caller is the one applier** (`norn-host`'s `applier`, Layer 4
/// plan-apply).
///
/// Its caller is `norn-host`'s one applier, which the apply job runs holding
/// an entry's claim.
pub fn discard(anchor: &Path, staged: Staged, shadows: &ShadowHome) {
    let Some(shadow) = staged.pending.shadow() else {
        return;
    };
    let faults = Faulted {
        faults: Faults::entry(),
        path: &staged.path,
    };
    let root = open_staged_root(anchor, staged.root).ok();
    let reach = root.as_ref().map(|root| (root.as_fd(), anchor));
    remove_shadow(shadows, reach, shadow, faults);
}

/// Remove `folder` below `anchor` where it is empty, and each folder above it
/// that is then empty, stopping at the first that is not and never at the
/// root.
///
/// The applier calls this after a plan's removals, and for the folders a
/// refused create left: a document is removed, and a folder that held only it
/// goes with it. `root` is the plan's vault root, as [`stage`] takes it. The
/// descent is anchored and follows no link, as every change here is; a folder
/// already gone, or one that is now a file, is where the emptying starts from,
/// and the folders above it are emptied as if it had been removed here. Each
/// removal's holder is synced, and the durability of all of them is reported.
///
/// A removal the filesystem refuses for a reason other than a folder that is
/// not empty ends the call as that refusal; the folders removed before it are
/// removed, and their holders were synced.
///
/// **Its caller is the one applier** (`norn-host`'s `applier`, Layer 4
/// plan-apply).
///
/// Its caller is `norn-host`'s one applier, which the apply job runs holding
/// an entry's claim.
pub fn remove_empty_folders(
    anchor: &Path,
    root: Identity,
    folder: &Path,
) -> Result<RemovedFolders, Refusal> {
    remove_empty_folders_where(anchor, root, folder, Faults::entry())
}

// ---------------------------------------------------------------------------
// Staging
// ---------------------------------------------------------------------------

/// [`stage`], with a stage made to fail rather than waiting for a machine that
/// fails there.
fn stage_where(
    anchor: &Path,
    root: Identity,
    path: &Path,
    transition: Transition<'_>,
    shadows: &ShadowHome,
    faults: Faults,
) -> Result<Staging, Refusal> {
    let faults = Faulted { faults, path };
    let full = anchor.join(path);
    let (root_fd, looked) = look(anchor, root, path, &full, &transition)?;
    // Every descriptor the look opened is closed by here, the root's aside:
    // what staging hands back holds none.
    let after = after_of(&transition);
    let at = StageAt {
        anchor,
        root,
        root_fd: root_fd.as_fd(),
        path,
        shadows,
        faults,
    };
    let mode = match looked {
        Looked::Respell { to, standing } => {
            let Transition::Respell {
                before, content, ..
            } = transition
            else {
                unreachable!("only a respell is looked at as one");
            };
            return stage_respell(&at, to, standing, before, content);
        }
        Looked::One(Judged::Landed) => {
            return Ok(Staging::Landed(Landed {
                root,
                path: path.to_path_buf(),
                after,
                landing: match transition {
                    Transition::Create { .. } => Landing::Created,
                    Transition::Replace { .. } => Landing::Replaced,
                    Transition::Remove { .. } => Landing::Removed,
                    Transition::Respell { .. } => Landing::Respelled,
                },
            }));
        }
        Looked::One(Judged::Proceed { mode }) => mode,
    };
    let pending = match (transition, after) {
        (Transition::Create { content }, Some(after)) => Pending::Create {
            after,
            shadow: stage_shadow(&at, content, None)?,
        },
        (Transition::Replace { before, content }, Some(after)) => Pending::Replace {
            before,
            after,
            shadow: stage_shadow(&at, content, mode)?,
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
        root,
        path: path.to_path_buf(),
        pending,
    }))
}

/// What staging's look at a target concluded, before any shadow is made.
enum Looked {
    /// A create, a replace or a remove, judged.
    One(Judged),
    /// A respell, where it stands, and the new spelling of its final name.
    Respell { to: OsString, standing: Respelled },
}

/// Everything staging asks of a target before it writes anything: the
/// request's shape, the root, the anchored descent to the target's folder,
/// and what stands at the name judged against `transition`. The root is
/// handed back open, since a shadow in a home under it is reached from it,
/// and every other descriptor the look opened is closed.
fn look(
    anchor: &Path,
    root: Identity,
    path: &Path,
    full: &Path,
    transition: &Transition<'_>,
) -> Result<(OwnedFd, Looked), Refusal> {
    let target = Target::of(path, full)?;
    let respelled = match transition {
        Transition::Respell { to, .. } => Some(respelled_name(&target, to, full)?),
        _ => None,
    };
    let root_fd = open_staged_root(anchor, root)?;
    let folder = descend(root_fd.as_fd(), &target, anchor, full)?;
    let after = after_of(transition);
    let looked = match (respelled, transition) {
        (Some(to), Transition::Respell { before, .. }) => {
            let after = after.expect("a respell's after-state is a hash");
            let standing = look_respell(&folder, &target, &to, *before, after, full)?;
            Looked::Respell { to, standing }
        }
        _ => {
            let found = observe_target(&folder, &target, full, &mut |_| {})?;
            Looked::One(judge_staging(transition, after, found, full)?)
        }
    };
    drop(folder);
    Ok((root_fd, looked))
}

/// Where a staging acts: the root, the target, the home and the faults.
struct StageAt<'a> {
    anchor: &'a Path,
    root: Identity,
    root_fd: BorrowedFd<'a>,
    path: &'a Path,
    shadows: &'a ShadowHome,
    faults: Faulted<'a>,
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
/// A refusal speaks in the terms of what the transition expected. Where it
/// expected absence and found a name taken — by a file, a folder, or the same
/// document under another listed spelling — [`Refusal::DestinationExists`];
/// where a folder on the path is a file, [`Refusal::FolderIsFile`]. Where it
/// expected a hash and found other bytes, another spelling or nothing,
/// [`Refusal::Drifted`]; where it found no regular file,
/// [`Refusal::NotRegularFile`]. A remove whose target is gone — or whose path
/// runs through a file, so that nothing can be there — has landed.
fn judge_staging(
    transition: &Transition<'_>,
    after: Option<ContentHash>,
    found: Found,
    full: &Path,
) -> Result<Judged, Refusal> {
    match (transition, found) {
        (_, Found::Link) => Err(symlink_destination(full)),
        (_, Found::Regular { state, .. }) if Some(state.content_hash) == after => {
            Ok(Judged::Landed)
        }
        (Transition::Remove { .. }, Found::Absent | Found::Blocked { .. }) => Ok(Judged::Landed),
        (Transition::Create { .. }, Found::Absent) => Ok(Judged::Proceed { mode: None }),
        (Transition::Create { .. }, Found::Blocked { folder }) => Err(folder_is_file(full, folder)),
        (
            Transition::Create { .. },
            Found::Regular { .. } | Found::OtherSpelling { .. } | Found::Other,
        ) => Err(destination_exists(full)),
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
            Transition::Replace { before, .. }
            | Transition::Remove { before }
            | Transition::Respell { before, .. },
            Found::OtherSpelling { state },
        ) => Err(drifted(full, *before, Some(state))),
        (
            Transition::Replace { before, .. } | Transition::Respell { before, .. },
            Found::Absent | Found::Blocked { .. },
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
/// refused as an invalid request before anything is read.
fn respelled_name(target: &Target<'_>, to: &Path, full: &Path) -> Result<OsString, Refusal> {
    let not_a_respell = || {
        invalid_request(
            full,
            "a respell changes only the ASCII case of the final name",
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

/// Where a respell stands, judged through the spelling its folder lists.
fn look_respell(
    folder: &Folder<'_>,
    target: &Target<'_>,
    to: &OsStr,
    before: ContentHash,
    after: ContentHash,
    full: &Path,
) -> Result<Respelled, Refusal> {
    let Folder::Reached(chain) = folder else {
        return Err(drifted(full, before, None));
    };
    let Some((spelling, found)) = observe_spelling(chain.last(), target.name, full, &mut |_| {})?
    else {
        return Err(drifted(full, before, None));
    };
    judge_respell(&spelling, found, target.name, to, before, after, full)
}

/// Stage a respell standing as `standing`: a shadow only where the content
/// still has to change.
fn stage_respell(
    at: &StageAt<'_>,
    to: OsString,
    standing: Respelled,
    before: ContentHash,
    content: Option<&[u8]>,
) -> Result<Staging, Refusal> {
    let after = content.map_or(before, ContentHash::of);
    let shadow = match standing {
        Respelled::Landed { .. } => {
            return Ok(Staging::Landed(Landed {
                root: at.root,
                path: at.path.with_file_name(&to),
                after: Some(after),
                landing: Landing::Respelled,
            }));
        }
        Respelled::Halfway { .. } => None,
        Respelled::Before { mode, .. } => match content {
            Some(content) if after != before => Some(stage_shadow(at, content, Some(mode))?),
            _ => None,
        },
    };
    Ok(Staging::Staged(Staged {
        root: at.root,
        path: at.path.to_path_buf(),
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
        Found::OtherSpelling { state } => return Err(drifted(full, before, Some(state))),
        Found::Link => return Err(symlink_destination(full)),
        Found::Absent | Found::Blocked { .. } => return Err(drifted(full, before, None)),
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
/// A failure after the shadow exists removes it while its name still means the
/// file made here, so a staging refusal leaves nothing in the home; where the
/// file cannot even be identified, the empty shadow is left to the home's sweep.
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns the shadow's handle.
fn stage_shadow(
    at: &StageAt<'_>,
    content: &[u8],
    mode: Option<u32>,
) -> Result<StagedShadow, Refusal> {
    let shadows = at.shadows;
    let home = open_home(shadows, Some((at.root_fd, at.anchor)))?;
    let (name, file) = create_shadow(
        home.as_fd(),
        shadows,
        &mut || shadows.next_shadow_name(),
        at.faults,
    )?;
    let mut file = std::fs::File::from(file);
    let shadow_path = shadows.directory().join(&name);
    // The identity is read before the fill, so a fill that fails still knows
    // which file is this call's: the name can be taken over while the bytes
    // are written and synced, and the cleanup removes only what was made here.
    let identity = match file.metadata() {
        Ok(metadata) => identity_of(&metadata),
        Err(error) => {
            // Nothing identifies the file, so nothing is removed by its name;
            // the shadow is inert residue the home's sweep bounds.
            return Err(environment("reading the identity of", &shadow_path, &error));
        }
    };
    let shadow = StagedShadow { name, identity };
    if let Some(mode) = mode {
        carry_mode_forward(&file, mode);
    }
    match fill(&mut file, content, &shadow_path, at.faults) {
        Ok(()) => Ok(shadow),
        Err(refusal) => {
            if at.faults.check(Stage::Cleanup).is_ok() {
                unlink_shadow_if_ours(home.as_fd(), &shadow);
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
    let root = open_staged_root(anchor, staged.root);
    let publication = match &root {
        Ok(root) => publish_checked(anchor, root.as_fd(), &staged, shadows, faults, disturb),
        Err(refusal) => Err(refusal.clone()),
    };
    // A create or a replace that wrote consumed its shadow. Everything else —
    // a refusal, a found landing, and a respell whichever way it went — may
    // have left one, and removing it only while the name still means the file
    // staging made is right for all of them: a shadow a refusal was about may
    // be gone or somebody else's, and a respell's first step may have renamed
    // it away already. One cleanup, so an auditor finds one place it happens.
    let consumed = matches!(publication, Ok(Publication::Wrote(_)))
        && !matches!(staged.pending, Pending::Respell { .. });
    if !consumed && let Some(shadow) = staged.pending.shadow() {
        let reach = root.as_ref().ok().map(|root| (root.as_fd(), anchor));
        remove_shadow(shadows, reach, shadow, faults);
    }
    publication
}

/// Ask every question again, and publish.
fn publish_checked(
    anchor: &Path,
    root: BorrowedFd<'_>,
    staged: &Staged,
    shadows: &ShadowHome,
    faults: Faulted<'_>,
    disturb: &mut dyn FnMut(Window),
) -> Result<Publication, Refusal> {
    let full = anchor.join(&staged.path);
    let target = Target::of(&staged.path, &full)?;
    let folder = descend(root, &target, anchor, &full)?;
    #[cfg(any(test, feature = "induced-failure"))]
    act_as_foreign_writer(&folder, &target, &staged.pending, faults);
    let at = Place {
        anchor,
        root,
        target: &target,
        full: &full,
        shadows,
        faults,
    };
    match &staged.pending {
        Pending::Create { after, shadow } => publish_create(&at, folder, *after, shadow, disturb),
        Pending::Replace {
            before,
            after,
            shadow,
        } => publish_replace(&at, folder, *before, *after, shadow, disturb),
        Pending::Remove { before } => publish_remove(&at, folder, *before, disturb),
        Pending::Respell {
            to,
            before,
            after,
            shadow,
        } => publish_respell(&at, folder, to, *before, *after, shadow.as_ref(), disturb),
    }
}

/// Where a publication acts, and under which faults.
struct Place<'a> {
    anchor: &'a Path,
    root: BorrowedFd<'a>,
    target: &'a Target<'a>,
    full: &'a Path,
    shadows: &'a ShadowHome,
    faults: Faulted<'a>,
}

impl Place<'_> {
    /// Confirm `shadow`, the last step before the act that moves it.
    fn confirm(
        &self,
        shadow: &StagedShadow,
        after: ContentHash,
    ) -> Result<(OwnedFd, PostState), Refusal> {
        confirm_shadow(self.shadows, (self.root, self.anchor), shadow, after)
    }
}

/// A landing another writer made, with the durability of the syncs it made.
fn found(after: AfterState, durability: Durability) -> Publication {
    Publication::Found(Confirmed { after, durability })
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
///
/// Published or found, a create syncs every folder from the root down to its
/// own: a first attempt that crashed may have made some of them and synced
/// none, and the re-send that finds them is the one chance to make them
/// durable before the applier removes a move's source.
fn publish_create(
    at: &Place<'_>,
    folder: Folder<'_>,
    after: ContentHash,
    shadow: &StagedShadow,
    disturb: &mut dyn FnMut(Window),
) -> Result<Publication, Refusal> {
    let name = at.target.name;
    let made = match folder {
        Folder::Reached(chain) => {
            let observed = observe(chain.last(), name, at.full, disturb)?;
            if let Some(landed) = at_after(&observed, after) {
                return Ok(found(landed, chain.sync_all(at.faults)));
            }
            judge_absent(observed, at.full)?;
            MadeFolders::none(chain)
        }
        // Nothing can be at a name whose folder is missing, so the folders are
        // made and the exclusive rename is the create's whole question.
        Folder::Missing(chain) => MadeFolders::make(at, chain, disturb)?,
        Folder::Blocked { folder, .. } => return Err(folder_is_file(at.full, folder)),
    };
    let (home, state) = match at.confirm(shadow, after) {
        Ok(confirmed) => confirmed,
        Err(refusal) => return Err(made.abandon(refusal, at.faults)),
    };
    disturb(Window::Publishing);
    if let Err(refusal) = publish_exclusively(
        home.as_fd(),
        shadow,
        made.parent(),
        name,
        at.full,
        at.faults,
    ) {
        // The name was taken at the last moment. Where what took it is this
        // create's own after-state, the create has landed.
        // A failure to look is a refusal like any other here, so the folders
        // this create made are still taken back or named.
        if matches!(refusal, Refusal::DestinationExists { .. }) {
            let observed = match observe(made.parent(), name, at.full, &mut |_| {}) {
                Ok(observed) => observed,
                Err(error) => return Err(made.abandon(error, at.faults)),
            };
            if let Some(landed) = at_after(&observed, after) {
                return Ok(found(landed, made.sync_all(at.faults)));
            }
        }
        return Err(made.abandon(refusal, at.faults));
    }
    let durability = made.sync_all(at.faults);
    Ok(Publication::Wrote(Published {
        after: AfterState::Present(state),
        durability,
        made_folders: made.into_paths(),
    }))
}

/// Publish a replace: found at its after-state, and otherwise a rename over a
/// target still at its before-state.
fn publish_replace(
    at: &Place<'_>,
    folder: Folder<'_>,
    before: ContentHash,
    after: ContentHash,
    shadow: &StagedShadow,
    disturb: &mut dyn FnMut(Window),
) -> Result<Publication, Refusal> {
    let Folder::Reached(chain) = folder else {
        return Err(drifted(at.full, before, None));
    };
    let folder = chain.last();
    let observed = observe(folder, at.target.name, at.full, disturb)?;
    if let Some(landed) = at_after(&observed, after) {
        return Ok(found(landed, sync_folder(folder, at.faults)));
    }
    reverify_before(observed, before, at.full)?;
    let (home, state) = at.confirm(shadow, after)?;
    disturb(Window::Publishing);
    rename_shadow(
        home.as_fd(),
        shadow,
        folder,
        at.target.name,
        at.full,
        at.faults,
    )?;
    Ok(wrote(
        AfterState::Present(state),
        sync_folder(folder, at.faults),
    ))
}

/// Publish a remove: found where the target is already gone — its folder with
/// it, or a file standing where a folder on its path was — and otherwise an
/// unlink of a target still at its before-state.
fn publish_remove(
    at: &Place<'_>,
    folder: Folder<'_>,
    before: ContentHash,
    disturb: &mut dyn FnMut(Window),
) -> Result<Publication, Refusal> {
    let chain = match folder {
        Folder::Reached(chain) => chain,
        // The deepest folder still there is the one that records the absence.
        Folder::Missing(chain) | Folder::Blocked { chain, .. } => {
            return Ok(found(
                AfterState::Absent,
                sync_folder(chain.last(), at.faults),
            ));
        }
    };
    let folder = chain.last();
    let observed = observe(folder, at.target.name, at.full, disturb)?;
    if matches!(observed, Found::Absent) {
        return Ok(found(AfterState::Absent, sync_folder(folder, at.faults)));
    }
    reverify_before(observed, before, at.full)?;
    disturb(Window::Publishing);
    at.faults
        .check(Stage::Unlink)
        .map_err(|error| environment("removing", at.full, &error))?;
    match unlinkat(folder, at.target.name, AtFlags::empty()) {
        Ok(()) => Ok(wrote(AfterState::Absent, sync_folder(folder, at.faults))),
        // Gone between the verification and the unlink: another writer landed
        // it.
        Err(Errno::NOENT) => Ok(found(AfterState::Absent, sync_folder(folder, at.faults))),
        Err(errno) => Err(errno_refusal("removing", at.full, errno)),
    }
}

/// Publish a respell in its two steps: where the content still has to change,
/// an ordinary replace under the old spelling; then the rename to the new.
///
/// A second step that fails after the first published is not a refusal: the
/// content landed, and the answer says so, as [`Publication::Interrupted`].
fn publish_respell(
    at: &Place<'_>,
    folder: Folder<'_>,
    to: &OsStr,
    before: ContentHash,
    after: ContentHash,
    shadow: Option<&StagedShadow>,
    disturb: &mut dyn FnMut(Window),
) -> Result<Publication, Refusal> {
    let Folder::Reached(chain) = folder else {
        return Err(drifted(at.full, before, None));
    };
    let folder = chain.last();
    let from = at.target.name;
    let respelled = |disturb: &mut dyn FnMut(Window)| {
        let Some((spelling, observed)) = observe_spelling(folder, from, at.full, disturb)? else {
            return Err(drifted(at.full, before, None));
        };
        judge_respell(&spelling, observed, from, to, before, after, at.full)
    };
    let first = match respelled(disturb)? {
        Respelled::Landed { state } => {
            return Ok(found(AfterState::Present(state), chain.sync_all(at.faults)));
        }
        Respelled::Before { state, .. } if after != before => {
            // Step 1. The content is at its before-state and has to change; a
            // respell staged halfway has no shadow for that, so the content
            // moved back under it and that is drift.
            let Some(shadow) = shadow else {
                return Err(drifted(at.full, after, Some(state)));
            };
            let (home, published) = at.confirm(shadow, after)?;
            disturb(Window::Publishing);
            rename_shadow(home.as_fd(), shadow, folder, from, at.full, at.faults)?;
            Some(Published {
                after: AfterState::Present(published),
                durability: sync_folder(folder, at.faults),
                made_folders: Vec::new(),
            })
        }
        Respelled::Before { .. } | Respelled::Halfway { .. } => None,
    };
    disturb(Window::Respelling);
    // Step 2, with the after-state read again under the old spelling.
    let second = (|| {
        let state = match respelled(disturb)? {
            Respelled::Landed { state } => return Ok(Err(state)),
            Respelled::Halfway { state } => state,
            Respelled::Before { state, .. } if after == before => state,
            Respelled::Before { state, .. } => return Err(drifted(at.full, after, Some(state))),
        };
        at.faults
            .check(Stage::Respell)
            .map_err(|error| environment("respelling", at.full, &error))?;
        renameat(folder, from, folder, to)
            .map_err(|errno| errno_refusal("respelling", at.full, errno))?;
        Ok(Ok(state))
    })();
    match (second, first) {
        (Ok(Ok(state)), first) => {
            let durability = sync_folder(folder, at.faults);
            let durability = match first {
                Some(first) => first.durability.and(durability),
                None => durability,
            };
            Ok(wrote(AfterState::Present(state), durability))
        }
        // Another writer finished the rename. Where this call wrote the content,
        // that content is what landed.
        (Ok(Err(state)), Some(first)) => Ok(wrote(
            AfterState::Present(state),
            first.durability.and(chain.sync_all(at.faults)),
        )),
        (Ok(Err(state)), None) => Ok(found(AfterState::Present(state), chain.sync_all(at.faults))),
        (Err(cause), Some(published)) => {
            Ok(Publication::Interrupted(Interrupted { published, cause }))
        }
        (Err(cause), None) => Err(cause),
    }
}

/// The after-state `found` is, where it is this transition's after-state under
/// the spelling asked for.
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
        Found::Blocked { folder } => Err(folder_is_file(full, folder)),
        Found::Link => Err(symlink_destination(full)),
        Found::Regular { .. } | Found::OtherSpelling { .. } | Found::Other => {
            Err(destination_exists(full))
        }
    }
}

/// Refuse unless the target still holds `before`: publication's second
/// reading of a replace's or a removal's target.
///
/// Behind `induced-failure` a harness can switch the reading off (see
/// [`crate::faults`]), so a suite can show a refusal is this reading's; a
/// build without the feature holds no such switch.
fn reverify_before(found: Found, before: ContentHash, full: &Path) -> Result<(), Refusal> {
    #[cfg(feature = "induced-failure")]
    if crate::faults::publication_unverified() {
        return Ok(());
    }
    judge_before(found, before, full)
}

/// Refuse unless the target still holds `before`.
fn judge_before(found: Found, before: ContentHash, full: &Path) -> Result<(), Refusal> {
    match found {
        Found::Regular { state, .. } if state.content_hash == before => Ok(()),
        Found::Regular { state, .. } | Found::OtherSpelling { state } => {
            Err(drifted(full, before, Some(state)))
        }
        Found::Absent | Found::Blocked { .. } => Err(drifted(full, before, None)),
        Found::Link => Err(symlink_destination(full)),
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

/// The folders from the root down to a create's own — the ones the descent
/// found and the ones the create made — and which of them this call made.
struct MadeFolders<'a> {
    chain: Chain<'a>,
    /// Each folder made: the index in the chain of the folder holding it, its
    /// name, and its path relative to the vault root.
    made: Vec<(usize, OsString, PathBuf)>,
}

impl<'a> MadeFolders<'a> {
    /// A create whose folder was there: nothing made.
    fn none(chain: Chain<'a>) -> MadeFolders<'a> {
        MadeFolders {
            chain,
            made: Vec::new(),
        }
    }

    /// Make every folder of `at`'s target past the ones `chain` reached, one
    /// level at a time.
    ///
    /// Each is made with `mkdirat` in the folder above it and then opened like
    /// any folder of a descent — `O_NOFOLLOW` and `O_DIRECTORY` — so a folder
    /// another writer made first is used rather than refused, and a name that
    /// became a link or a file refuses. A refusal abandons the folders this
    /// call made before it is returned.
    fn make(
        at: &Place<'_>,
        chain: Chain<'a>,
        disturb: &mut dyn FnMut(Window),
    ) -> Result<MadeFolders<'a>, Refusal> {
        let folders = &at.target.folders;
        let reached = chain.folders.len();
        let mut made = MadeFolders::none(chain);
        let mut relative: PathBuf = folders[..reached].iter().collect();
        for name in &folders[reached..] {
            relative.push(name);
            match made.make_one(at, name, &relative, disturb) {
                Ok(folder) => made.chain.folders.push(folder),
                Err(refusal) => return Err(made.abandon(refusal, at.faults)),
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
        match mkdirat(self.chain.at(holder), name, Mode::from_raw_mode(0o777)) {
            Ok(()) => self
                .made
                .push((holder, name.to_owned(), relative.to_path_buf())),
            // Another writer made it first: it is used, and it is not ours.
            Err(Errno::EXIST) => {}
            Err(errno) => return Err(errno_refusal_at("making the folder", at.full, name, errno)),
        }
        disturb(Window::FolderMade);
        let walked = at.anchor.join(relative);
        match step_into(self.chain.at(holder), name)
            .map_err(|errno| errno_refusal_at("opening", at.full, name, errno))?
        {
            Step::Into(folder) => Ok(folder),
            Step::Linked => Err(linked_ancestor(at.full, walked)),
            Step::NotAFolder => Err(folder_is_file(at.full, walked)),
            Step::Missing(errno) => Err(errno_refusal_at("opening", at.full, name, errno)),
        }
    }

    /// The target's own folder.
    fn parent(&self) -> BorrowedFd<'_> {
        self.chain.last()
    }

    /// Remove the folders this call made that are still empty, deepest first,
    /// and answer with `refusal` — naming, where any is left standing, every
    /// folder this call made that it could not take back.
    ///
    /// The removal stops at the first folder it cannot remove, because every
    /// folder above holds that one. A name that no longer holds a folder at all
    /// — a link or a file another writer put in its place — is not one this
    /// call made any more, and is neither removed nor named.
    fn abandon(&self, refusal: Refusal, faults: Faulted<'_>) -> Refusal {
        let mut left = self.made.len();
        if faults.check(Stage::Cleanup).is_ok() {
            for (holder, name, _) in self.made.iter().rev() {
                let holder = self.chain.at(*holder);
                if unlinkat(holder, name.as_os_str(), AtFlags::REMOVEDIR).is_err()
                    && is_folder(holder, name)
                {
                    break;
                }
                left -= 1;
            }
        }
        if left == 0 {
            return refusal;
        }
        Refusal::FoldersLeft {
            refusal: Box::new(refusal),
            folders: self.made[..left]
                .iter()
                .map(|(_, _, path)| path.clone())
                .collect(),
        }
    }

    /// Sync every folder on the chain, deepest first.
    fn sync_all(&self, faults: Faulted<'_>) -> Durability {
        self.chain.sync_all(faults)
    }

    /// The paths of the folders made, shallowest first.
    fn into_paths(self) -> Vec<PathBuf> {
        self.made.into_iter().map(|(_, _, path)| path).collect()
    }
}

/// Rename the shadow to the target's name, refusing rather than replacing
/// anything there.
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
    renameat_with(
        home,
        shadow.name.as_os_str(),
        folder,
        name,
        RenameFlags::NOREPLACE,
    )
    .map_err(|errno| exclusive_rename_refusal(errno, full))
}

/// What an exclusive rename's failure means.
///
/// A name taken between the verification and the rename is
/// [`Refusal::DestinationExists`], with the racer's file untouched. A
/// filesystem that cannot rename exclusively is
/// [`Refusal::ExclusiveCreateUnsupported`]: `EINVAL` and `ENOTSUP` are how
/// Linux and macOS say so, and `ENOSYS` is a kernel that predates the call —
/// compared one by one because `ENOTSUP` and `EOPNOTSUPP` are one number on
/// Linux and two on macOS. There is no fallback: a hard link and an unlink, or
/// a look and a rename, are each a check followed by an act, which is the race
/// the call exists to close. Anything else is the machine's refusal.
fn exclusive_rename_refusal(errno: Errno, full: &Path) -> Refusal {
    if errno == Errno::EXIST {
        destination_exists(full)
    } else if [Errno::INVAL, Errno::NOTSUP, Errno::OPNOTSUPP, Errno::NOSYS].contains(&errno) {
        Refusal::ExclusiveCreateUnsupported {
            path: full.to_path_buf(),
            raw_os_error: errno.raw_os_error(),
        }
    } else {
        errno_refusal("renaming onto", full, errno)
    }
}

/// Confirm a staged shadow is the file staging made and still holds `after`,
/// and hand back the home's handle and the identity it will publish.
///
/// Opened through the home's own directory without following a link, so a
/// link planted at a shadow's name is refused rather than published. A shadow
/// that is gone, is another file — the same bytes in another file included —
/// or holds other bytes is an environmental refusal for this target: an I/O
/// failure a re-send stages again, never drift the caller would re-plan.
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns the shadow's handle.
fn confirm_shadow(
    shadows: &ShadowHome,
    reach: (BorrowedFd<'_>, &Path),
    shadow: &StagedShadow,
    after: ContentHash,
) -> Result<(OwnedFd, PostState), Refusal> {
    const OPERATION: &str = "confirming the shadow";
    let home = open_home(shadows, Some(reach))?;
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
    let (hash, len) = shadow_hashed_from(&mut file, &path)
        .map_err(|error| environment(OPERATION, &path, &error))?;
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
/// The identity is compared first because a shadow outlives its staging call:
/// a name a sweep emptied is a name something else may hold, and this removes
/// only what staging made. `reach` is the vault root a fallback home is reached
/// from; without one, a fallback home is not reached at all.
fn remove_shadow(
    shadows: &ShadowHome,
    reach: Option<(BorrowedFd<'_>, &Path)>,
    shadow: &StagedShadow,
    faults: Faulted<'_>,
) {
    if faults.check(Stage::Cleanup).is_err() {
        return;
    }
    let Ok(home) = open_home(shadows, reach) else {
        return;
    };
    unlink_shadow_if_ours(home.as_fd(), shadow);
}

/// Remove `shadow` from the home open at `home` only while its name still
/// means the file staging made.
fn unlink_shadow_if_ours(home: BorrowedFd<'_>, shadow: &StagedShadow) {
    match statat(home, shadow.name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if identity_of_stat(&stat) == shadow.identity => {
            let _ = unlinkat(home, shadow.name.as_os_str(), AtFlags::empty());
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
    let folder = descend(root.as_fd(), &target, anchor, &full)?;
    let found = match (&folder, landed.landing) {
        (Folder::Reached(chain), Landing::Respelled) => {
            spelled_exactly(chain.last(), target.name, &full)?
        }
        _ => observe_target(&folder, &target, &full, &mut |_| {})?,
    };
    let after = match (landed.after, found) {
        (_, Found::Link) => return Err(symlink_destination(&full)),
        (None, Found::Absent | Found::Blocked { .. }) => AfterState::Absent,
        (None, Found::Regular { .. } | Found::OtherSpelling { .. } | Found::Other) => {
            return Err(destination_exists(&full));
        }
        (Some(after), Found::Regular { state, .. }) if state.content_hash == after => {
            AfterState::Present(state)
        }
        (Some(after), Found::Regular { state, .. } | Found::OtherSpelling { state }) => {
            return Err(drifted(&full, after, Some(state)));
        }
        (Some(after), Found::Absent | Found::Blocked { .. }) => {
            return Err(drifted(&full, after, None));
        }
        (Some(_), Found::Other) => return Err(not_regular(&full)),
    };
    let chain = match &folder {
        Folder::Reached(chain) | Folder::Missing(chain) | Folder::Blocked { chain, .. } => chain,
    };
    let durability = match landed.landing {
        Landing::Created | Landing::Respelled => chain.sync_all(faults),
        Landing::Replaced | Landing::Removed => sync_folder(chain.last(), faults),
    };
    Ok(Confirmed { after, durability })
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
    root: Identity,
    folder: &Path,
    faults: Faults,
) -> Result<RemovedFolders, Refusal> {
    let full = anchor.join(folder);
    let names = contained_names(folder).map_err(|_| uncontained(&full))?;
    let root = open_staged_root(anchor, root)?;
    // A folder already gone, or a file where one was, is where the emptying
    // starts from: the folders above it are all the descent opened.
    let (opened, stopped) = crate::open::descend(root.as_fd(), &names)
        .map_err(|(at, errno)| errno_refusal_at("opening", &full, names[at], errno))?;
    if let Some(Stopped::Linked(at)) = stopped {
        return Err(linked_ancestor(&full, ancestry(anchor, &names[..=at])));
    }
    let chain = Chain {
        root: root.as_fd(),
        folders: opened,
    };
    let mut removed = Vec::new();
    let mut durability = Durability::Synced;
    // Index 0 is the root, which is never removed.
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
        let holder = chain.at(depth - 1);
        match unlinkat(holder, name, AtFlags::REMOVEDIR) {
            Ok(()) => {
                durability = durability.and(sync_folder(holder, faults));
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
    /// The target `path` names, where it is a name below the root, by the rule
    /// [`contained_names`] states for every contained path this crate reaches.
    /// Anything else is an invalid request, refused before anything is opened.
    fn of(path: &'p Path, full: &Path) -> Result<Target<'p>, Refusal> {
        let mut folders = contained_names(path).map_err(|_| uncontained(full))?;
        let Some(name) = folders.pop() else {
            return Err(uncontained(full));
        };
        Ok(Target { folders, name })
    }
}

/// Open the root as it is spelled, and read which directory that is.
fn open_root(anchor: &Path) -> Result<(OwnedFd, Identity), Refusal> {
    let root = open(anchor, anchor_flags(), Mode::empty())
        .map_err(|errno| errno_refusal("opening the vault root", anchor, errno))?;
    let stat =
        fstat(&root).map_err(|errno| errno_refusal("reading the vault root", anchor, errno))?;
    Ok((root, identity_of_stat(&stat)))
}

/// Open the root, refusing unless it is the directory `expected` identifies.
fn open_staged_root(anchor: &Path, expected: Identity) -> Result<OwnedFd, Refusal> {
    let (root, current) = open_root(anchor)?;
    if current != expected {
        return Err(Refusal::RootReplaced {
            path: anchor.to_path_buf(),
            staged: expected,
            current,
        });
    }
    Ok(root)
}

/// Every folder from the root down to where a descent stopped: the root, then
/// each folder opened below it, in order.
struct Chain<'a> {
    root: BorrowedFd<'a>,
    folders: Vec<OwnedFd>,
}

impl Chain<'_> {
    /// How many folders the chain holds, the root included.
    fn len(&self) -> usize {
        self.folders.len() + 1
    }

    /// The folder at `index`, the root being 0.
    fn at(&self, index: usize) -> BorrowedFd<'_> {
        match index {
            0 => self.root,
            index => self.folders[index - 1].as_fd(),
        }
    }

    /// The deepest folder: the target's own, where the descent reached it.
    fn last(&self) -> BorrowedFd<'_> {
        self.at(self.len() - 1)
    }

    /// Sync every folder on the chain, deepest first: each holds the name of
    /// the one below it, and the deepest holds the target's.
    fn sync_all(&self, faults: Faulted<'_>) -> Durability {
        (0..self.len())
            .rev()
            .fold(Durability::Synced, |durability, index| {
                durability.and(sync_folder(self.at(index), faults))
            })
    }
}

/// Where a descent to a target's folder ended.
enum Folder<'a> {
    /// The target's own folder is the chain's last.
    Reached(Chain<'a>),
    /// A folder on the way is not there, so neither is the target. The chain
    /// ends at the last folder that is.
    Missing(Chain<'a>),
    /// A name on the way is a file rather than a folder, so nothing can be at
    /// the target. The chain ends at the folder holding it, and `folder` is
    /// its path.
    Blocked { chain: Chain<'a>, folder: PathBuf },
}

/// Descend from `root` to the folder `target` sits in, through
/// [`crate::open::descend`] — the one anchored descent every read and every
/// change goes through.
fn descend<'a>(
    root: BorrowedFd<'a>,
    target: &Target<'_>,
    anchor: &Path,
    full: &Path,
) -> Result<Folder<'a>, Refusal> {
    let (opened, stopped) = crate::open::descend(root, &target.folders)
        .map_err(|(at, errno)| errno_refusal_at("opening", full, target.folders[at], errno))?;
    let chain = Chain {
        root,
        folders: opened,
    };
    Ok(match stopped {
        None => Folder::Reached(chain),
        Some(Stopped::Missing(..)) => Folder::Missing(chain),
        Some(Stopped::NotAFolder(at)) => Folder::Blocked {
            chain,
            folder: ancestry(anchor, &target.folders[..=at]),
        },
        Some(Stopped::Linked(at)) => {
            return Err(linked_ancestor(
                full,
                ancestry(anchor, &target.folders[..=at]),
            ));
        }
    })
}

/// Whether `name` in `holder` is a folder, asked without following a link.
fn is_folder(holder: BorrowedFd<'_>, name: &OsStr) -> bool {
    matches!(
        statat(holder, name, AtFlags::SYMLINK_NOFOLLOW),
        Ok(stat) if rustix::fs::FileType::from_raw_mode(stat.st_mode as _) == rustix::fs::FileType::Directory
    )
}

/// The path of the folder `names` spell below `anchor`.
fn ancestry(anchor: &Path, names: &[&OsStr]) -> PathBuf {
    names
        .iter()
        .fold(anchor.to_path_buf(), |path, name| path.join(name))
}

/// What is at a target's name, as far as a transition cares.
enum Found {
    Absent,
    /// A folder on the path is a file, at `folder`: nothing can be at the
    /// target.
    Blocked {
        folder: PathBuf,
    },
    /// A symbolic link, which this crate never writes through.
    Link,
    /// Something that is not a regular file: a directory, a pipe, a device or
    /// a socket.
    Other,
    /// A regular file, hashed through its own handle, whose name still meant
    /// that file once the hash was taken, and which its folder lists under
    /// the spelling asked for.
    Regular {
        state: PostState,
        mode: u32,
    },
    /// A regular file the root resolves at the name asked for, which its
    /// folder lists under another spelling: on a root that folds, not this
    /// target.
    OtherSpelling {
        state: PostState,
    },
}

/// What is at `target`, from where the descent to it ended.
fn observe_target(
    folder: &Folder<'_>,
    target: &Target<'_>,
    full: &Path,
    disturb: &mut dyn FnMut(Window),
) -> Result<Found, Refusal> {
    Ok(match folder {
        Folder::Reached(chain) => observe(chain.last(), target.name, full, disturb)?,
        Folder::Missing(_) => Found::Absent,
        Folder::Blocked { folder, .. } => Found::Blocked {
            folder: folder.clone(),
        },
    })
}

/// Read the target `name` in `folder`, confirm the name still means the file
/// that was read, and confirm the folder lists it under that spelling.
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
///
/// **The spelling is asked only where the root could have answered for
/// another one**: an alternate ASCII spelling of the name that resolves to the
/// same file is what a folding folder does, and only then is the folder listed
/// to see which spelling is the entry. A folder that tells spellings apart pays
/// one lookup.
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
            return if crate::open::is_link(folder, name) {
                Ok(Found::Link)
            } else {
                Err(errno_refusal("opening", full, errno))
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
    let (hash, len) = target_hashed_from(&mut file, full)
        .map_err(|error| environment("reading", full, &error))?;
    disturb(Window::Verifying);
    let read = identity_of(&metadata);
    match statat(folder, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if identity_of_stat(&stat) == read => {}
        Ok(stat) => {
            return Err(Refusal::Republished {
                path: full.to_path_buf(),
                read,
                current: identity_of_stat(&stat),
            });
        }
        // Removed while it was read: the same event as finding nothing there.
        Err(Errno::NOENT) => return Ok(Found::Absent),
        Err(errno) => return Err(errno_refusal("reading the identity of", full, errno)),
    }
    let state = post_state(hash, len, &metadata);
    if !listed_as_asked(folder, name, read, full)? {
        return Ok(Found::OtherSpelling { state });
    }
    Ok(Found::Regular {
        state,
        mode: metadata.permissions().mode(),
    })
}

/// Whether `folder` lists the file `identity` under `name` itself.
///
/// Only a folder that resolved an alternate ASCII spelling of `name` to the
/// same file could be answering for a spelling it does not list, so only there
/// is the listing read.
fn listed_as_asked(
    folder: BorrowedFd<'_>,
    name: &OsStr,
    identity: Identity,
    full: &Path,
) -> Result<bool, Refusal> {
    let Some(alternate) = alternate_ascii_case(name) else {
        return Ok(true);
    };
    match statat(folder, &alternate, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if identity_of_stat(&stat) == identity => {}
        _ => return Ok(true),
    }
    Ok(folded_entries(folder, name, full)?
        .iter()
        .any(|listed| listed == name))
}

/// Every entry of `folder` whose name folds together with `name`, as listed.
fn folded_entries(
    folder: BorrowedFd<'_>,
    name: &OsStr,
    full: &Path,
) -> Result<Vec<OsString>, Refusal> {
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
    Ok(listed)
}

/// The shadow home's directory.
///
/// A home under the data root is this crate's own boundary and is opened as it
/// is configured. **A fallback home is under the vault root**, where every
/// name belongs to whoever writes the vault, so it is reached from `reach`'s
/// root handle by the one anchored descent, through no link — a `.norn` a
/// foreign writer swapped for a link does not carry a shadow out of the vault.
/// Without a root to reach it from, a fallback home is not opened.
fn open_home(
    shadows: &ShadowHome,
    reach: Option<(BorrowedFd<'_>, &Path)>,
) -> Result<OwnedFd, Refusal> {
    let directory = shadows.directory();
    let Some(within) = shadows.within_vault() else {
        return open(directory, anchor_flags(), Mode::empty())
            .map_err(|errno| errno_refusal("opening", directory, errno));
    };
    let Some((root, anchor)) = reach else {
        return Err(errno_refusal("opening", directory, Errno::NOENT));
    };
    let names = contained_names(within).map_err(|_| uncontained(directory))?;
    let (mut opened, stopped) = crate::open::descend(root, &names)
        .map_err(|(at, errno)| errno_refusal_at("opening", directory, names[at], errno))?;
    match stopped {
        None => opened.pop().ok_or_else(|| uncontained(directory)),
        Some(Stopped::Linked(at)) => {
            Err(linked_ancestor(directory, ancestry(anchor, &names[..=at])))
        }
        Some(Stopped::NotAFolder(at)) => {
            Err(folder_is_file(directory, ancestry(anchor, &names[..=at])))
        }
        Some(Stopped::Missing(at, errno)) => {
            Err(errno_refusal_at("opening", directory, names[at], errno))
        }
    }
}

/// Where the foreign stage is armed, act as a foreign writer on the target:
/// inside a publication, after the root is checked and before anything is read
/// again, through the target's folder handle.
///
/// What each act leads to is ADR 0032's reading of that writer, which the
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
    folder: &Folder<'_>,
    target: &Target<'_>,
    pending: &Pending,
    faults: Faulted<'_>,
) {
    use crate::faults::{FOREIGN_BYTES, ForeignAct};
    let Some(act) = faults.faults.foreign(faults.path) else {
        return;
    };
    let Folder::Reached(chain) = folder else {
        return;
    };
    let folder = chain.last();
    let write_at = |flags: OFlags| {
        // Non-blocking, so a pipe at the target does not hold the writer.
        let flags = flags
            | OFlags::WRONLY
            | OFlags::CREATE
            | OFlags::NOFOLLOW
            | OFlags::NONBLOCK
            | OFlags::CLOEXEC;
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
/// macOS. The armed answer and the filesystem's own reach the caller through
/// one mapping, [`durability_of`], so what is asserted of one is true of the
/// other.
#[allow(clippy::disallowed_types)] // The vault filesystem seam: this crate owns vault handles.
fn sync_folder(folder: BorrowedFd<'_>, faults: Faulted<'_>) -> Durability {
    durability_of(faults.check(Stage::ParentSync).and_then(|()| {
        #[cfg(test)]
        tests::count_sync(folder);
        folder
            .try_clone_to_owned()
            .and_then(|folder| std::fs::File::from(folder).sync_all())
    }))
}

/// What a sync's outcome says about durability.
fn durability_of(synced: std::io::Result<()>) -> Durability {
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
    let mut listed = folded_entries(folder, name, full)?;
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
    Refusal::NotRegularFile {
        path: path.to_path_buf(),
    }
}

fn destination_exists(path: &Path) -> Refusal {
    Refusal::DestinationExists {
        path: path.to_path_buf(),
    }
}

fn symlink_destination(path: &Path) -> Refusal {
    Refusal::SymlinkDestination {
        path: path.to_path_buf(),
    }
}

fn folder_is_file(path: &Path, folder: PathBuf) -> Refusal {
    Refusal::FolderIsFile {
        path: path.to_path_buf(),
        folder,
    }
}

fn invalid_request(path: &Path, reason: &'static str) -> Refusal {
    Refusal::InvalidRequest {
        path: path.to_path_buf(),
        reason,
    }
}

fn uncontained(path: &Path) -> Refusal {
    invalid_request(path, "the path is not a name below the vault root")
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
    use norn_testkit::churn::{Folding, runs_where_the_volume_folds};

    /// Stage `transition` at `relative` in `scratch`'s vault under `faults`.
    fn stage_in(
        scratch: &Scratch,
        relative: &str,
        transition: Transition<'_>,
        faults: Faults,
    ) -> Result<Staging, Refusal> {
        stage_where(
            &scratch.at(""),
            root_of(scratch),
            Path::new(relative),
            transition,
            scratch.shadows(),
            faults,
        )
    }

    /// The identity of `scratch`'s vault root, which a plan is made against.
    fn root_of(scratch: &Scratch) -> Identity {
        crate::path_identity(&scratch.at(""))
            .expect("the vault root")
            .expect("a vault root")
    }

    thread_local! {
        /// The folders this thread synced, while a case counts them.
        static SYNCED: std::cell::RefCell<Option<Vec<Identity>>> =
            const { std::cell::RefCell::new(None) };
    }

    /// Record that `folder` is being synced, where the running case counts.
    pub(super) fn count_sync(folder: BorrowedFd<'_>) {
        SYNCED.with(|synced| {
            if let Some(synced) = synced.borrow_mut().as_mut()
                && let Ok(stat) = fstat(folder)
            {
                synced.push(identity_of_stat(&stat));
            }
        });
    }

    /// Run `work`, and answer with what it returned and the folders it synced.
    fn syncs_of<T>(work: impl FnOnce() -> T) -> (T, Vec<Identity>) {
        SYNCED.with(|synced| *synced.borrow_mut() = Some(Vec::new()));
        let answer = work();
        let synced = SYNCED.with(|synced| synced.borrow_mut().take().unwrap_or_default());
        (answer, synced)
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
            other => panic!("did not write: {other:?}"),
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
        let _ = publish_in(&scratch, staged, Faults::NONE, &mut |_| {}).expect("a replacement");
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
    #[allow(clippy::disallowed_methods)] // The kernel's own suite: its write entry points are what it exercises.
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

        discard(&scratch.at(""), staged, scratch.shadows());

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

        let home = open_home(scratch.shadows(), None).expect("the home");
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
        let home = open_home(scratch.shadows(), None).expect("the home");
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

    /// **Judging a target is staging's own judgment, writing nothing.** A
    /// create below a folder that is a link, a replace at a name holding
    /// other bytes, and a replace at its before-state each judge as staging
    /// them does — the first two refusing with staging's own refusal, the
    /// last passing — and judging stages no shadow, writes nothing through
    /// the link, and leaves the vault root and the shadow home unmodified, so
    /// not even a file made and removed again passes unseen.
    #[test]
    #[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Harness scaffolding: playing the foreign writer, and backdating the folders it watches.
    fn judging_a_target_answers_as_staging_it_does_and_writes_nothing() {
        let scratch = Scratch::new("write-judge");
        let outside = scratch.directory("outside");
        std::os::unix::fs::symlink(&outside, scratch.at("linked")).expect("a linked folder");
        scratch.place("drifted.md", b"theirs");
        scratch.place("ready.md", b"old");
        let replace = Transition::Replace {
            before: ContentHash::of(b"old"),
            content: b"new",
        };
        let judged = |relative: &str, transition| {
            judge(
                &scratch.at(""),
                root_of(&scratch),
                Path::new(relative),
                transition,
            )
        };
        // Each folder's modification time is set well into the past first,
        // so a change judging made is seen even within the clock's tick.
        let folders = [scratch.at(""), scratch.shadows().directory().to_owned()];
        let long_ago = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1 << 20);
        for folder in &folders {
            std::fs::File::open(folder)
                .and_then(|handle| handle.set_modified(long_ago))
                .expect("backdating a folder");
        }
        let modified = || {
            folders
                .iter()
                .map(|folder| {
                    std::fs::metadata(folder)
                        .and_then(|meta| meta.modified())
                        .expect("an mtime")
                })
                .collect::<Vec<_>>()
        };

        let refused = [
            ("linked/fresh.md", Transition::Create { content: b"ours" }),
            ("drifted.md", replace),
        ]
        .map(|(relative, transition)| (relative, transition, judged(relative, transition)));
        assert_eq!(judged("ready.md", replace), Ok(()));
        assert_eq!(modified(), vec![long_ago; 2], "judging modified a folder");
        for (relative, transition, judgment) in refused {
            let refusal = judgment.expect_err("a refused target");
            assert_eq!(
                Err(refusal),
                stage_in(&scratch, relative, transition, Faults::NONE),
                "{relative}"
            );
        }
        assert!(scratch.shadow_names().is_empty(), "judging staged a shadow");
        assert!(
            std::fs::read_dir(&outside)
                .expect("outside")
                .next()
                .is_none(),
            "something was made through the link"
        );
        assert!(matches!(
            stage_in(&scratch, "ready.md", replace, Faults::NONE),
            Ok(Staging::Staged(_))
        ));
    }

    /// **The bar on a create's shadow confirmation.** A shadow swapped for a
    /// copy of its own bytes while the create makes its folders refuses the
    /// create, and nothing is published at the name.
    ///
    /// The copy holds the staged content, so only its identity tells it apart.
    /// The forbidden shape is confirming the shadow before making the folders:
    /// the swap then lands after the confirmation, and the rename publishes a
    /// file staging never made.
    #[test]
    fn a_shadow_swapped_while_a_create_makes_its_folders_refuses_the_create() {
        let scratch = Scratch::new("write-create-shadow-swap");
        let staged = staged_in(
            &scratch,
            "a/fresh.md",
            Transition::Create { content: b"ours" },
        );
        let shadow = scratch
            .shadows()
            .directory()
            .join(&scratch.shadow_names()[0]);
        let mut swapped = false;
        let refusal = publish_in(&scratch, staged, Faults::NONE, &mut |window| {
            if window == Window::FolderMade && !swapped {
                republish(&scratch, &shadow, b"ours");
                swapped = true;
            }
        })
        .expect_err("a create whose shadow was swapped");

        assert!(swapped, "the window was never entered");
        assert!(
            matches!(
                &refusal,
                Refusal::Environment { operation, .. } if *operation == "confirming the shadow"
            ),
            "{refusal}"
        );
        assert!(
            !scratch.exists(&scratch.at("a/fresh.md")),
            "a file staging never made was published"
        );
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

    /// The identities of the folders `relatives` name in `scratch`'s vault,
    /// the root spelled as `""`.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: judging which folders were synced.
    fn folders_of(scratch: &Scratch, relatives: &[&str]) -> std::collections::BTreeSet<Identity> {
        relatives
            .iter()
            .map(|relative| identity_at(&scratch.at(relative)))
            .collect()
    }

    /// **The bar on a re-send over folders a crash made.** A create's landing,
    /// confirmed by a re-send, syncs every folder from the root down to its
    /// own — not only the one that holds it.
    ///
    /// The forbidden shape is syncing the target's folder alone: a first
    /// attempt that made `a/` and `a/b/` and died before syncing them leaves
    /// entries a power cut can lose, the re-send reports the landing synced,
    /// and the applier removes a move's source on the strength of it.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: folders a crashed attempt made.
    fn a_resent_create_confirmed_landed_syncs_every_folder_down_to_it() {
        let scratch = Scratch::new("write-sync-confirm");
        std::fs::create_dir_all(scratch.at("a/b")).expect("folders a crash made");
        scratch.place("a/b/fresh.md", b"ours");
        let Staging::Landed(landed) = stage_in(
            &scratch,
            "a/b/fresh.md",
            Transition::Create { content: b"ours" },
            Faults::NONE,
        )
        .expect("a landed create") else {
            panic!("a landed create staged");
        };

        let (confirmed, synced) =
            syncs_of(|| confirm_landed_where(&scratch.at(""), &landed, Faults::NONE));

        assert!(confirmed.expect("confirmed").durability.is_synced());
        assert_eq!(
            synced
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            folders_of(&scratch, &["", "a", "a/b"])
        );
    }

    /// The same bar on a publication: a create into folders that were already
    /// there — made by a crashed attempt, or by anybody — syncs each of them,
    /// and one found landed at the last check does too.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: folders a crashed attempt made.
    fn a_create_published_or_found_syncs_every_folder_down_to_it() {
        let scratch = Scratch::new("write-sync-publish");
        std::fs::create_dir_all(scratch.at("a/b")).expect("folders a crash made");
        let chain = folders_of(&scratch, &["", "a", "a/b"]);

        let staged = staged_in(
            &scratch,
            "a/b/one.md",
            Transition::Create { content: b"ours" },
        );
        let (published, synced) =
            syncs_of(|| publish_in(&scratch, staged, Faults::NONE, &mut |_| {}));
        assert!(
            matches!(published, Ok(Publication::Wrote(_))),
            "{published:?}"
        );
        assert_eq!(
            synced
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            chain
        );

        let staged = staged_in(
            &scratch,
            "a/b/two.md",
            Transition::Create { content: b"ours" },
        );
        scratch.place("a/b/two.md", b"ours");
        let (found, synced) = syncs_of(|| publish_in(&scratch, staged, Faults::NONE, &mut |_| {}));
        assert!(matches!(found, Ok(Publication::Found(_))), "{found:?}");
        assert_eq!(
            synced
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            chain
        );
    }

    /// The same bar on a create whose exclusive rename finds the name taken
    /// by its own after-state: the landing is found, and every folder from the
    /// root down to it is synced, not only the one that holds it.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: folders a crashed attempt made.
    fn a_create_found_at_its_rename_syncs_every_folder_down_to_it() {
        let scratch = Scratch::new("write-sync-raced");
        std::fs::create_dir_all(scratch.at("a/b")).expect("folders a crash made");
        let path = scratch.at("a/b/fresh.md");
        let staged = staged_in(
            &scratch,
            "a/b/fresh.md",
            Transition::Create { content: b"ours" },
        );

        let (found, synced) = syncs_of(|| {
            publish_in(&scratch, staged, Faults::NONE, &mut |window| {
                if window == Window::Publishing {
                    republish(&scratch, &path, b"ours");
                }
            })
        });
        assert!(matches!(found, Ok(Publication::Found(_))), "{found:?}");
        assert_eq!(
            synced
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            folders_of(&scratch, &["", "a", "a/b"])
        );
    }

    /// A create whose name was taken at its rename, and whose look at what took
    /// it fails, still answers for the folders it made: the failure reaches the
    /// caller through the same abandonment every other refusal takes.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the racer's file and its mode.
    fn a_create_that_cannot_read_what_took_its_name_names_the_folders_it_left() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new("write-raced-unreadable");
        let path = scratch.at("a/b/fresh.md");
        let staged = staged_in(
            &scratch,
            "a/b/fresh.md",
            Transition::Create { content: b"ours" },
        );

        let refusal = publish_in(&scratch, staged, Faults::NONE, &mut |window| {
            if window == Window::Publishing {
                republish(&scratch, &path, b"theirs");
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000))
                    .expect("the racer's file made unreadable");
            }
        })
        .expect_err("a create whose name was taken");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("the racer's file made readable again");

        let Refusal::FoldersLeft { refusal, folders } = &refusal else {
            panic!("the folders left were not named: {refusal}");
        };
        assert!(
            matches!(**refusal, Refusal::Environment { .. }),
            "{refusal}"
        );
        assert_eq!(folders, &[PathBuf::from("a"), PathBuf::from("a/b")]);
        assert_eq!(std::fs::read(&path).expect("the racer's file"), b"theirs");
    }

    /// A create that makes its folders syncs each folder holding one it made,
    /// and the root holding the first.
    #[test]
    fn a_create_that_makes_folders_syncs_each_one_and_its_holder() {
        let scratch = Scratch::new("write-sync-made");
        let staged = staged_in(
            &scratch,
            "a/b/fresh.md",
            Transition::Create { content: b"ours" },
        );
        let (published, synced) =
            syncs_of(|| publish_in(&scratch, staged, Faults::NONE, &mut |_| {}));
        assert!(
            matches!(published, Ok(Publication::Wrote(_))),
            "{published:?}"
        );
        assert_eq!(
            synced
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            folders_of(&scratch, &["", "a", "a/b"])
        );
    }

    /// **A found landing syncs, and says what the sync said.** A replace and a
    /// remove another writer finished are each reported with their folder's
    /// sync, not with a synced durability no sync stands behind.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: playing the foreign writer.
    fn a_found_replace_or_remove_syncs_its_folder() {
        let scratch = Scratch::new("write-sync-found");
        std::fs::create_dir_all(scratch.at("a")).expect("a folder");
        scratch.place("a/note.md", b"old");
        scratch.place("a/gone.md", b"going");
        let replace = staged_in(&scratch, "a/note.md", replace_old_with_new());
        let remove = staged_in(
            &scratch,
            "a/gone.md",
            Transition::Remove {
                before: ContentHash::of(b"going"),
            },
        );
        scratch.place("a/note.md", b"new");
        std::fs::remove_file(scratch.at("a/gone.md")).expect("a foreign removal");

        for staged in [replace, remove] {
            let (found, synced) =
                syncs_of(|| publish_in(&scratch, staged, Faults::NONE, &mut |_| {}));
            assert!(matches!(found, Ok(Publication::Found(_))), "{found:?}");
            assert_eq!(
                synced
                    .into_iter()
                    .collect::<std::collections::BTreeSet<_>>(),
                folders_of(&scratch, &["a"])
            );
        }
    }

    /// **A removal whose folder became a file has landed.** A file standing
    /// where a folder on the path was leaves nothing at the target's name, so
    /// the publication finds the removal rather than refusing it.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: playing the foreign writer.
    fn a_remove_whose_folder_became_a_file_is_found() {
        let scratch = Scratch::new("write-remove-folder-file");
        std::fs::create_dir_all(scratch.at("a")).expect("a folder");
        scratch.place("a/note.md", b"old");
        let staged = staged_in(
            &scratch,
            "a/note.md",
            Transition::Remove {
                before: ContentHash::of(b"old"),
            },
        );
        std::fs::remove_dir_all(scratch.at("a")).expect("a foreign removal");
        scratch.place("a", b"a file where the folder was");

        let publication = publish_in(&scratch, staged, Faults::NONE, &mut |_| {})
            .expect("a removal whose folder became a file");
        let Publication::Found(found) = publication else {
            panic!("a removal nothing made was reported written: {publication:?}");
        };
        assert!(matches!(found.after, AfterState::Absent));
    }

    /// **A removal that finds its target gone at the unlink has been landed by
    /// another writer**, and says so rather than claiming the removal.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: playing the foreign writer.
    fn a_target_removed_just_before_the_unlink_is_found() {
        let scratch = Scratch::new("write-unlink-gone");
        let path = scratch.place("note.md", b"old");
        let staged = staged_in(
            &scratch,
            "note.md",
            Transition::Remove {
                before: ContentHash::of(b"old"),
            },
        );
        let publication = publish_in(&scratch, staged, Faults::NONE, &mut |window| {
            if window == Window::Publishing {
                std::fs::remove_file(&path).expect("a foreign removal");
            }
        })
        .expect("a landing another writer made");
        let Publication::Found(found) = publication else {
            panic!("a removal another writer made was reported written: {publication:?}");
        };
        assert!(matches!(found.after, AfterState::Absent));
    }

    /// **One mapping from a sync's outcome to durability**, for the armed
    /// answer and the filesystem's own error alike: what a case asserts of an
    /// armed failure is what a real one is told.
    #[test]
    fn a_sync_outcome_maps_to_durability_one_way() {
        assert!(durability_of(Ok(())).is_synced());
        let Durability::NotSynced(error) =
            durability_of(Err(std::io::Error::from_raw_os_error(libc::EIO)))
        else {
            panic!("a failed sync was reported synced");
        };
        assert_eq!(error.raw_os_error(), Some(libc::EIO));
    }

    /// **The exclusive rename's failures are classified by what they mean**:
    /// a taken name, a filesystem with no exclusive rename, and the machine.
    #[test]
    fn exclusive_rename_failures_are_classified() {
        let path = Path::new("/vault/note.md");
        assert_eq!(
            exclusive_rename_refusal(Errno::EXIST, path),
            Refusal::DestinationExists {
                path: path.to_path_buf()
            }
        );
        for errno in [Errno::INVAL, Errno::NOTSUP, Errno::OPNOTSUPP, Errno::NOSYS] {
            assert_eq!(
                exclusive_rename_refusal(errno, path),
                Refusal::ExclusiveCreateUnsupported {
                    path: path.to_path_buf(),
                    raw_os_error: errno.raw_os_error(),
                },
                "{errno:?}"
            );
        }
        for errno in [Errno::ACCESS, Errno::XDEV, Errno::NOSPC] {
            assert!(
                exclusive_rename_refusal(errno, path).is_os_error(errno.raw_os_error()),
                "{errno:?}"
            );
        }
    }

    /// **The bar on a refused create's folders.** Where a create refused after
    /// making folders cannot take them back, the refusal names them, so the
    /// report can.
    #[test]
    fn a_refused_create_that_cannot_take_back_its_folders_names_them() {
        let scratch = Scratch::new("write-folders-left");
        let staged = staged_in(
            &scratch,
            "a/b/fresh.md",
            Transition::Create { content: b"ours" },
        );
        let refusal = publish_in(
            &scratch,
            staged,
            Faults::at(&[
                (Stage::Swap, Answer::Fails(std::io::ErrorKind::Other)),
                (
                    Stage::Cleanup,
                    Answer::Fails(std::io::ErrorKind::PermissionDenied),
                ),
            ]),
            &mut |_| {},
        )
        .expect_err("a refused create");

        let Refusal::FoldersLeft { refusal, folders } = &refusal else {
            panic!("the folders left were not named: {refusal}");
        };
        assert!(
            matches!(
                **refusal,
                Refusal::Environment {
                    operation: "renaming onto",
                    ..
                }
            ),
            "{refusal}"
        );
        assert_eq!(folders, &[PathBuf::from("a"), PathBuf::from("a/b")]);
        assert!(scratch.exists(&scratch.at("a/b")));
    }

    /// **The bar on a fallback home reached through a link.** Where the shadow
    /// home sits under the vault root, a `.norn` a foreign writer swapped for a
    /// link refuses staging and publication, and nothing lands outside the
    /// vault.
    ///
    /// The forbidden shape is opening the home by its path: every name under
    /// the vault root is one a foreign writer can replace, and a home reached
    /// through one would stage a document's bytes wherever it points.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: a foreign writer swapping `.norn`.
    fn a_fallback_home_reached_through_a_link_refuses_at_stage_and_publish() {
        let scratch = Scratch::new("write-home-link");
        let vault = scratch.at("");
        let home = ShadowHome::resolve_where(
            &vault,
            &scratch.directory("data/vaults/notes/tmp"),
            &crate::scratch::key(),
            false,
        )
        .expect("a fallback home");
        let path = scratch.place("note.md", b"old");
        let root = root_of(&scratch);
        let outside = scratch.directory("outside");
        let stage = |home: &ShadowHome| {
            stage_where(
                &vault,
                root,
                Path::new("note.md"),
                replace_old_with_new(),
                home,
                Faults::NONE,
            )
        };

        // A link in place of `.norn` before staging.
        let moved = outside.join(".norn");
        std::fs::rename(vault.join(".norn"), &moved).expect("moving `.norn` out");
        std::os::unix::fs::symlink(&moved, vault.join(".norn")).expect("a link in its place");
        let refusal = stage(&home).expect_err("a home reached through a link");
        assert!(
            matches!(refusal, Refusal::LinkedAncestor { .. }),
            "{refusal}"
        );
        assert!(
            std::fs::read_dir(moved.join("tmp").join(crate::scratch::key().as_path()))
                .expect("the moved home")
                .next()
                .is_none(),
            "staging wrote a shadow outside the vault"
        );

        // And after staging, before publication.
        std::fs::remove_file(vault.join(".norn")).expect("the link");
        std::fs::rename(&moved, vault.join(".norn")).expect("`.norn` back");
        let Staging::Staged(staged) = stage(&home).expect("staged") else {
            panic!("staged as landed");
        };
        std::fs::rename(vault.join(".norn"), &moved).expect("moving `.norn` out");
        std::os::unix::fs::symlink(&moved, vault.join(".norn")).expect("a link in its place");
        let refusal = publish_disturbed(&vault, staged, &home, Faults::NONE, &mut |_| {})
            .expect_err("a home reached through a link");
        assert!(
            matches!(refusal, Refusal::LinkedAncestor { .. }),
            "{refusal}"
        );
        assert_eq!(
            scratch.read(&path),
            b"old",
            "a shadow outside the vault was published"
        );
    }

    /// **The bar on a respell's second step after its first landed.** Content
    /// published under the old spelling and then moved back to its
    /// before-state by another writer is drift at the rename — and since the
    /// first step landed, the answer is interrupted, carrying that drift.
    ///
    /// Runs where the scratch root folds case; elsewhere a respell refuses
    /// before either step, so the case skips — and fails on macOS, where the
    /// scratch root is expected to fold.
    #[test]
    fn a_respell_whose_content_reverts_between_its_steps_is_interrupted_by_drift() {
        let scratch = Scratch::new("write-respell-revert");
        let folding = match crate::PathNormalizer::detect(&scratch.at(""))
            .expect("the scratch root's case behavior")
            .case_sensitivity()
        {
            crate::CaseSensitivity::Insensitive => Folding::Folded,
            crate::CaseSensitivity::Sensitive => Folding::Distinct,
        };
        if !runs_where_the_volume_folds(
            folding,
            "a_respell_whose_content_reverts_between_its_steps_is_interrupted_by_drift",
        ) {
            return;
        }
        let path = scratch.place("note.md", b"old");
        let staged = staged_in(
            &scratch,
            "note.md",
            Transition::Respell {
                to: Path::new("Note.md"),
                before: ContentHash::of(b"old"),
                content: Some(b"new"),
            },
        );
        let publication = publish_in(&scratch, staged, Faults::NONE, &mut |window| {
            if window == Window::Respelling {
                republish(&scratch, &path, b"old");
            }
        })
        .expect("an interrupted respell");
        let Publication::Interrupted(interrupted) = publication else {
            panic!("a respell drifted between its steps was not interrupted: {publication:?}");
        };
        assert!(
            matches!(interrupted.cause, Refusal::Drifted { .. }),
            "{:?}",
            interrupted.cause
        );
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
