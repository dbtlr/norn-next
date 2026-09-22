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
//! directory.
//!
//! **The most specific containing root answers.** Registered roots nest, so a
//! directory is often inside more than one of them; the answer is the
//! registration whose root is the longest of those the directory is under,
//! which is the vault a person working in that directory is working in. Two
//! registrations over one identity are not a most-specific pair at all —
//! neither root is under the other — so that is the refusal
//! `vault/ambiguous-root`, carrying the names, because it names no one vault
//! to answer about.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::address::Directory;
use crate::status::Registration;

/// What a `vault resolve` request carries.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ResolveParams {
    /// The absolute directory to resolve. A client asking about where it is
    /// running passes its own working directory, which sits anywhere under a
    /// registered root or under none.
    pub directory: Directory,
}

impl ResolveParams {
    /// A request for the registration containing `directory`.
    pub const fn new(directory: Directory) -> Self {
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
    /// A registration contains the directory.
    #[non_exhaustive]
    Registered {
        /// The registration that contains it. Where registered roots nest,
        /// this is the one whose root is the most specific of those the
        /// directory is under.
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
