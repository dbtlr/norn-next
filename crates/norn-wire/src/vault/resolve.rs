//! `vault resolve`: which registered vault contains a directory.
//!
//! **The client passes its working directory in.** A host answers from the
//! registry it holds and reads no process state of the caller's, so the
//! directory to resolve is a parameter rather than something the host infers
//! about whoever asked.
//!
//! **Containing no registration is an outcome, not a refusal.** A directory
//! outside every registered root is a true answer to the question asked, and a
//! surface renders it as whatever that surface does with an unregistered
//! directory. A directory two registrations over one identity both contain is
//! a refusal — `vault/ambiguous-root`, carrying the names — because it names
//! no one vault to answer about.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::address::VaultRoot;
use crate::status::Registration;

/// What a `vault resolve` request carries.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ResolveParams {
    /// The absolute directory to resolve. A client asking about where it is
    /// running passes its own working directory.
    pub directory: VaultRoot,
}

impl ResolveParams {
    /// A request for the registration containing `directory`.
    pub const fn new(directory: VaultRoot) -> Self {
        ResolveParams { directory }
    }
}

/// What `vault resolve` answers with.
///
/// On the wire a resolution is an object tagged `outcome`:
/// `{"outcome":"registered","registration":{…}}`, `{"outcome":"none"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ResolveReport {
    /// One registration contains the directory.
    #[non_exhaustive]
    Registered {
        /// The registration that contains it.
        registration: Registration,
    },
    /// No registration contains the directory.
    None {},
}

impl ResolveReport {
    /// `registration` contains the directory.
    pub const fn registered(registration: Registration) -> Self {
        ResolveReport::Registered { registration }
    }

    /// No registration contains the directory.
    pub const fn none() -> Self {
        ResolveReport::None {}
    }
}
