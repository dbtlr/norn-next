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
//! - [`write`] — the compare-and-swap kernel: precondition, shadow, verify,
//!   swap. [`vacate`] and [`move_document`] are the same kernel with different
//!   endings.
//! - [`shadow`] — where a write's bytes wait, why they wait outside the vault,
//!   and why a leaked one is inert.
//! - [`lock`] — the per-vault maintainer lock: at most one host maintains a
//!   vault's derived state, and that is the *only* thing it decides.
//! - [`ContentHash`] and [`PostState`] — the hash that concludes, and the
//!   identity a landed write reports.
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

pub mod lock;
pub mod shadow;
pub mod write;

mod faults;
mod hash;
mod identity;
mod refusal;

pub use hash::ContentHash;
pub use identity::{Identity, PostState};
pub use lock::{Acquisition, Incumbent, Maintainership, try_acquire};
pub use refusal::Refusal;
pub use shadow::{Placement, ShadowHome, is_shadow_name};
pub use write::{Landed, MoveRefusal, Moved, Precondition, Vacated, move_document, vacate, write};
