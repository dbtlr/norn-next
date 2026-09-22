//! What a request asks for, and what it is answered from.
//!
//! **A scope is what a verb reaches, not who wrote it.** The three scopes
//! partition every request by the state that answers it: one vault's entry,
//! the serving set and the registry, or the installation itself. A surface
//! reads a verb's scope to know whether it must be handed a vault address at
//! all, and the host reads it to know which door the request comes in by.
//!
//! **`Installation` is in the vocabulary before a verb carries it.** Layer 6
//! lands the installation verbs — self-update, service install — into this
//! same registry, and they are the requests that name no vault, carry no
//! answer reading, and are permitted to reach the external network. The scope
//! is spelled here so the partition a surface reads is the whole partition
//! rather than the part that happens to be inhabited, and so the verbs that
//! arrive extend the list rather than the vocabulary under it.
//!
//! **A verb's scope is answered where the verbs are written.** The match
//! carries no wildcard, so a verb minted without a scope does not compile:
//! the question is settled once, beside the list, rather than fallen through
//! at whichever surface dispatches one.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What a request is answered from.
///
/// On the wire a scope is the flat string itself: `"vault"`, `"registry"`,
/// `"installation"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RequestScope {
    /// The request names one vault address and is answered from that vault's
    /// entry.
    Vault,
    /// The request names no vault and is answered from the serving set and the
    /// registry.
    Registry,
    /// The request acts on the installation itself. It names no vault, carries
    /// no answer reading, and is the only scope permitted to reach the
    /// external network.
    Installation,
}

impl RequestScope {
    /// Every scope the vocabulary holds, in declaration order.
    pub const ALL: [RequestScope; 3] = [
        RequestScope::Vault,
        RequestScope::Registry,
        RequestScope::Installation,
    ];

    /// The scope as the string it is on the wire.
    pub const fn as_str(&self) -> &'static str {
        match self {
            RequestScope::Vault => "vault",
            RequestScope::Registry => "registry",
            RequestScope::Installation => "installation",
        }
    }
}

impl fmt::Display for RequestScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A string that spells no request scope the vocabulary holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnknownRequestScope;

impl fmt::Display for UnknownRequestScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the string spells no request scope")
    }
}

impl std::error::Error for UnknownRequestScope {}

impl TryFrom<&str> for RequestScope {
    type Error = UnknownRequestScope;

    fn try_from(string: &str) -> Result<Self, UnknownRequestScope> {
        Self::ALL
            .into_iter()
            .find(|scope| scope.as_str() == string)
            .ok_or(UnknownRequestScope)
    }
}

/// What a request asks the host to do.
///
/// On the wire a verb is the flat string itself: `"find"`, `"vault_reload"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Verb {
    /// Documents matching a conjunction of predicates, in a stated order.
    Find,
    /// Ranked hits for a query, answered from the vault's search ladder.
    Search,
    /// One document, addressed by a resolution target.
    Get,
    /// How many documents match, grouped by a key where one is asked for.
    Count,
    /// The findings standing over a vault.
    Validate,
    /// What a vault declares about itself and what its documents carry.
    Describe,
    /// Register a vault under a name.
    VaultRegister,
    /// Stop serving a registered name and remove its registration.
    VaultUnregister,
    /// Every registration this installation holds.
    VaultList,
    /// Change a field of one registration.
    VaultSet,
    /// Which registered vault a root or a name resolves to.
    VaultResolve,
    /// Where one vault's entry stands and what it is serving from.
    VaultStatus,
    /// Re-read one vault's control files and apply what changed.
    VaultReload,
    /// The findings standing over the registry itself.
    DoctorRegistry,
}

impl Verb {
    /// Every verb the registry holds, in declaration order.
    ///
    /// Reading a verb back and enumerating the registry both walk this list,
    /// so a variant absent here is unreadable and unadvertisable — the schema
    /// suite holds this list equal to the enum itself.
    pub const ALL: [Verb; 14] = [
        Verb::Find,
        Verb::Search,
        Verb::Get,
        Verb::Count,
        Verb::Validate,
        Verb::Describe,
        Verb::VaultRegister,
        Verb::VaultUnregister,
        Verb::VaultList,
        Verb::VaultSet,
        Verb::VaultResolve,
        Verb::VaultStatus,
        Verb::VaultReload,
        Verb::DoctorRegistry,
    ];

    /// The verb as the string it is on the wire.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Verb::Find => "find",
            Verb::Search => "search",
            Verb::Get => "get",
            Verb::Count => "count",
            Verb::Validate => "validate",
            Verb::Describe => "describe",
            Verb::VaultRegister => "vault_register",
            Verb::VaultUnregister => "vault_unregister",
            Verb::VaultList => "vault_list",
            Verb::VaultSet => "vault_set",
            Verb::VaultResolve => "vault_resolve",
            Verb::VaultStatus => "vault_status",
            Verb::VaultReload => "vault_reload",
            Verb::DoctorRegistry => "doctor_registry",
        }
    }

    /// What this verb is answered from.
    ///
    /// The match carries no wildcard: a verb minted without a scope does not
    /// compile, because a surface deciding whether to demand a vault address
    /// and a host deciding which door to route through both read the answer
    /// from here.
    pub const fn scope(&self) -> RequestScope {
        match self {
            // Answered from one vault's entry, under a hold of that entry.
            Verb::Find
            | Verb::Search
            | Verb::Get
            | Verb::Count
            | Verb::Validate
            | Verb::Describe
            | Verb::VaultStatus
            | Verb::VaultReload => RequestScope::Vault,
            // Answered from the serving set and the registry, naming no vault
            // entry to be held.
            Verb::VaultRegister
            | Verb::VaultUnregister
            | Verb::VaultList
            | Verb::VaultSet
            | Verb::VaultResolve
            | Verb::DoctorRegistry => RequestScope::Registry,
        }
    }
}

impl fmt::Display for Verb {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A string that spells no verb the registry holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnknownVerb;

impl fmt::Display for UnknownVerb {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the string spells no verb")
    }
}

impl std::error::Error for UnknownVerb {}

impl TryFrom<&str> for Verb {
    type Error = UnknownVerb;

    /// The verb a wire string names, found by walking [`Verb::ALL`] against
    /// the strings [`Verb::as_str`] hands out: reading a verb back is the
    /// inverse of writing it.
    fn try_from(string: &str) -> Result<Self, UnknownVerb> {
        Self::ALL
            .into_iter()
            .find(|verb| verb.as_str() == string)
            .ok_or(UnknownVerb)
    }
}
