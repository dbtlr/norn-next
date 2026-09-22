//! `vault unregister`: stop serving a registered name and remove its
//! registration.
//!
//! **What is discarded is derived state.** The vault's own documents are never
//! touched, and neither is the maintainer lock file: a lock file is how two
//! processes agree who maintains a vault's derived state, so unlinking one
//! would let a second process take maintainership of state a first is still
//! holding. What `state_discarded` reports is the derived database, its
//! sidecars and the shadow home.
//!
//! The refusals are `host/unknown-vault` where no such registration exists,
//! `host/entry-held` where the entry is in use, `host/maintainer-contended`
//! where another process maintains the derived state, and a parked entry's own
//! code.

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
    /// it, which is the default.
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
    /// Whether the vault's derived database, its sidecars and its shadow home
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
