//! `vault unregister`: stop serving a registered name and remove its
//! registration.
//!
//! **What is discarded is derived state.** The vault's own documents are never
//! touched, and neither is the maintainer lock file: a lock file is how two
//! processes agree who maintains a vault's derived state, so unlinking one
//! would let a second process take maintainership of state a first is still
//! holding. What `state_discarded` reports is the derived database, its
//! sidecars and the vault's shadow homes — the one in the data directory and
//! the one under the vault root, wherever either stands.
//!
//! **While the change runs the vault answers as held.** Every request that
//! asks the vault for anything is refused `host/entry-held` from the moment
//! the unregistration finds the vault idle until the change commits or is
//! refused, and `vault list` and `vault resolve` go on naming it until it
//! commits. After a commit the name is `host/unknown-vault`; after a refusal
//! the registration, the listing and the vault's serving state stand as they
//! were. A refusal met after the discard began — the registry file refusing
//! the write, or a later discard step refusing — may leave derived state
//! discarded; the next attach derives it again under a new store epoch, which
//! cursors report as a change.
//!
//! **A park is no refusal.** An entry standing on a park that nothing holds
//! is unregistered, and the park leaves with it; a name parked beside it on
//! one root is classified again at once and serves where nothing else reaches
//! that root.
//!
//! The refusals are `host/unknown-vault` where no such registration exists,
//! `host/entry-held` where the entry is in use, `host/maintainer-contended`
//! where another process maintains the derived state,
//! `host/registry-unwritable` where the registry file could not be read or
//! replaced, and `host/entry-untrusted` carrying the environmental-refusal
//! reason where the data directory refused the maintainer lock, or where the
//! discard was refused — in the data directory, or in the shadow home under
//! the vault root. After any of them the registration that stood before still
//! stands.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::name::VaultName;

/// What a `vault unregister` request carries.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct UnregisterParams {
    /// The registration to remove.
    pub name: VaultName,
    /// Whether to leave the vault's derived state on disk. `false` discards
    /// the derived database, its sidecars and the shadow homes; `true` keeps
    /// all of them where they are.
    pub keep_state: bool,
}

impl UnregisterParams {
    /// A request to remove `name` and discard its derived state.
    pub const fn new(name: VaultName) -> Self {
        UnregisterParams {
            name,
            keep_state: false,
        }
    }

    /// The request leaving the derived state on disk.
    #[must_use]
    pub const fn keeping_state(mut self) -> Self {
        self.keep_state = true;
        self
    }
}

/// What `vault unregister` answers with.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct UnregisterReport {
    /// The registration that was removed.
    pub name: VaultName,
    /// Whether the vault's derived database, its sidecars and its shadow homes
    /// were discarded. The maintainer lock file is never unlinked, and the
    /// vault's own documents are never touched.
    pub state_discarded: bool,
}

impl UnregisterReport {
    /// `name` was removed, its derived state discarded or kept.
    pub const fn new(name: VaultName, state_discarded: bool) -> Self {
        UnregisterReport {
            name,
            state_discarded,
        }
    }
}
