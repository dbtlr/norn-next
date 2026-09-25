//! `vault register`: take a vault into the durable serving set under a name.
//!
//! **The request is the registration.** What a register carries is the four
//! fields a registry entry holds, and what it answers with is the entry as
//! admitted beside what that entry publishes once it is in. A caller reads the
//! second to know whether the vault it just registered is serving yet.
//!
//! The refusals are the host's: `host/already-served` where the name is
//! already registered, `host/duplicate-root` where another registration
//! already reaches that root, `host/registry-unwritable` where the registry
//! file could not be read or replaced, and `host/entry-untrusted` where the root
//! itself could not be read — carrying the environmental-refusal reason,
//! which is the rendering the registry recheck gives such a root.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::status::{Published, Registration};

/// What a `vault register` request carries.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct RegisterParams {
    /// The registration to admit.
    pub registration: Registration,
}

impl RegisterParams {
    /// A request to admit `registration`.
    pub const fn new(registration: Registration) -> Self {
        RegisterParams { registration }
    }
}

/// What `vault register` answers with: the entry as admitted, and what it
/// publishes after insertion.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct RegisterReport {
    /// The registration as admitted.
    pub registration: Registration,
    /// What the new entry publishes.
    pub published: Published,
}

impl RegisterReport {
    /// `registration` was admitted, and its entry publishes `published`.
    pub const fn new(registration: Registration, published: Published) -> Self {
        RegisterReport {
            registration,
            published,
        }
    }
}
