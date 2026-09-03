#![forbid(unsafe_code)]
//! Everything that touches the vault filesystem, and nothing that doesn't.
//!
//! # The stance
//!
//! **Norn does not own the filesystem.** A vault is a directory of Markdown
//! files that editors, sync clients, snapshot tools, agents and people all write
//! to, and Norn is one consumer among them. What this crate guarantees is
//! narrow and exact: **atomicity and durability for Norn's own single-file
//! writes**. What it promises about what any other consumer does is nothing at
//! all. There is no lock here that keeps anybody out of a vault, no arrangement
//! that makes a vault Norn's, and no claim that a document Norn read a moment
//! ago is still the document that is there. Correctness against everybody else
//! is carried by hash preconditions at the write and by the watcher afterwards —
//! never by exclusion.
//!
//! That is why the vocabulary looks the way it does. Every write bears a
//! precondition and no write can be spelled without one; drift is a normal
//! outcome that comes back with what was actually observed; and a landed write
//! reports the identity of what it published so that Norn can recognize its own
//! change when the filesystem tells it about it.
//!
//! # What is here
//!
//! - [`write`](mod@write) — the compare-and-swap kernel: precondition, shadow, verify,
//!   swap. [`vacate`] and [`move_document`] are the same kernel with different
//!   endings.
//! - [`shadow`] — where a write's bytes wait, why they wait outside the vault,
//!   why a leaked one is inert, and the sweeps that bound what they cost: one
//!   [per home](ShadowHome::sweep), and two over the
//!   [fallback root](sweep_fallback_root) and [tree](sweep_fallback_tree) that
//!   reach what no home's key names.
//! - [`lock`] — the maintainer lock: at most one host maintains one derived
//!   store, and that is the *only* thing it decides.
//! - [`walk`](mod@walk) — the streaming, deterministic filesystem inventory and its typed
//!   skip notations.
//! - `open` — the one contained open every read of a file's content goes
//!   through: anchored at a directory, one component at a time, no link
//!   followed and no blocking on a name that is not a regular file.
//!   [`read_and_hash`] and [`read_optional_and_hash`] are the one-open/one-read
//!   content observation over it, and the walk's own reads take the same seam.
//! - [`path`] — the root-scoped, filesystem-case-aware normalization point used
//!   by walks today and watcher invalidation roots next.
//! - [`reads`] — what this thread asked the filesystem for while a caller's
//!   window stood over it: the opens, stats and directory entries that module
//!   names, counted where they happen. Evidence a caller folds into its own
//!   account, and nothing that decides anything.
//! - [`ContentHash`], [`hashed_from`] and [`PostState`] — the hash that
//!   concludes, the one act that produces one from a file, and the identity a
//!   landed write reports.
//!
//! **A vault's own mechanism files are part of that.** Norn keeps two per
//! [maintainership](MaintainershipKey) — the maintainer lock file and the shadow
//! home — and both live in the machine's norn data directory rather than among
//! documents, because a mechanism file in the vault tree gets committed, synced
//! and indexed by tools that cannot know to ignore it. A vault on another
//! filesystem is where that gives: the home falls back to the maintainership's
//! own directory under one dot-directory the walk excludes wholesale. Wherever
//! they sit they are this crate's: no other crate creates, reads, writes or
//! sweeps them.
//!
//! # Two rejected shapes, named so they stay rejected
//!
//! **An unconditional overwrite.** There is no `force`, no `clobber`, no
//! precondition-free write, and no combination of arguments that composes one.
//! A caller that means to replace a document reads it first and passes what it
//! read, which makes it accountable for the bytes it destroys. An affordance
//! that skipped that would make overwriting cheaper than merging, and the
//! cheapest path is the one that gets taken.
//!
//! **A drift policy.** Outcomes here are single-shot: nothing retries, and
//! there is no parameter that asks it to. A primitive that re-composed against
//! new content and tried again would convert an observation the caller was
//! promised into an invisible loop — and the content it re-composed would be
//! derived from bytes the caller never saw. Retry belongs to callers, who
//! re-compose against the observed state and declare their own bound.
//!
//! # The platform floor
//!
//! Linux and macOS. The floor is not a preference: a file's identity is its
//! `(device, inode)` pair, exclusion is `flock`, a published name is made
//! durable by fsyncing a directory, and a mode is carried forward with
//! `fchmod` — none of which the standard library spells portably. The atomic
//! rename at the centre of the protocol *is* portable, so another platform is a
//! later addition rather than a rewrite; what is not here is a pretence that it
//! already works.
//!
//! On NFSv2 a rename is not atomic, and `flock` on any NFS is whatever the
//! server's lock manager offers. A vault on a network mount has the guarantees
//! that mount provides rather than the ones stated here.

pub mod exclusion;
pub mod lock;
pub mod path;
pub mod reads;
pub mod shadow;
pub mod walk;
pub mod watch;
pub mod write;

mod faults;
mod hash;
mod identity;
mod open;
mod read;
mod refusal;
#[cfg(test)]
mod scratch;

pub use exclusion::{Excluded, ExclusionError, Exclusions};
pub use hash::{ContentHash, hashed_from};
pub use identity::{Identity, PostState, path_identity};
pub use lock::{Acquisition, Incumbent, Maintainership, try_acquire};
pub use path::{CaseSensitivity, NormalizedPath, NormalizerError, PathError, PathNormalizer};
pub use read::{
    PathKind, ReadAndHash, path_kind, read_and_hash, read_if_present_and_hash,
    read_optional_and_hash,
};
pub use refusal::Refusal;
pub use shadow::{
    FALLBACK, MaintainershipKey, Placement, SHADOW_AGE_THRESHOLD, ShadowHome, Swept,
    is_shadow_name, sweep_fallback_root, sweep_fallback_tree,
};
pub use walk::{
    FileFact, FileKind, FileStat, LinkKind, ReadFile, SkipFact, SkipReason, Vault, Walk, WalkError,
    WalkFact, walk, walk_subtree,
};
pub use watch::{
    Batch, OwnWrites, RescanScope, Subscription, SubscriptionState, WatchError, watch,
    watch_polling,
};
pub use write::{Landed, MoveRefusal, Moved, Precondition, Vacated, move_document, vacate, write};
