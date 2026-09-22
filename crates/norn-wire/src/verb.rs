//! What a request asks for, and what it is answered from.
//!
//! **A scope is what a request reaches, not who wrote it.** The three scopes
//! partition every request by the state that answers it: one vault's entry,
//! the serving set and the registry, or the installation itself. The host
//! reads a request's scope to know which door it comes in by.
//!
//! **Addressing is the verb's; scope is the request's.** A verb says whether a
//! vault address is carried — [`Addressing`] — and one verb, `vault_status`,
//! says an address *may* be. `vault status` naming a vault reports where that
//! vault's entry stands; `vault status` naming none reports the serving set,
//! which is a registry request under one registry entry rather than two verbs
//! sharing a name. [`Addressing::scope_with`] is where the two meet: the verb
//! says what may be carried, the request says what was, and the scope follows
//! from the pair.
//!
//! **`Installation` is in the vocabulary before a verb carries it.** Layer 6
//! lands the installation verbs — self-update, service install — into this
//! same registry, and they are the requests that name no vault, carry no
//! answer reading, and are permitted to reach the external network. The scope
//! is spelled here so the partition a surface reads is the whole partition
//! rather than the part that happens to be inhabited, and so the verbs that
//! arrive extend the list rather than the vocabulary under it.
//!
//! **A verb's addressing is answered where the verbs are written.** The match
//! carries no wildcard, so a verb minted without an addressing does not
//! compile: the question is settled once, beside the list, rather than fallen
//! through at whichever surface dispatches one.

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

/// Whether a verb's request carries a vault address.
///
/// On the wire an addressing is the flat string itself: `"required"`,
/// `"none"`, `"optional"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Addressing {
    /// The request carries a vault address, and is answered from that vault's
    /// entry.
    Required,
    /// The request carries no vault address, and is answered from the serving
    /// set and the registry.
    None,
    /// The request may carry a vault address. With one it is answered from
    /// that vault's entry; without one it is answered from the serving set and
    /// the registry.
    Optional,
}

impl Addressing {
    /// Every addressing the vocabulary holds, in declaration order.
    pub const ALL: [Addressing; 3] = [Addressing::Required, Addressing::None, Addressing::Optional];

    /// The addressing as the string it is on the wire.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Addressing::Required => "required",
            Addressing::None => "none",
            Addressing::Optional => "optional",
        }
    }

    /// What a request under this addressing is answered from, given whether it
    /// carries a vault address.
    ///
    /// The match carries no wildcard, so an addressing minted without an
    /// answer here does not compile. `address_present` decides the scope of an
    /// optional addressing and is ignored by the other two, which have already
    /// settled the question: a surface that hands `Required` no address, or
    /// `None` one, has built a request the verb does not describe, and the
    /// scope this reports is the verb's rather than a reading of that mistake.
    pub const fn scope_with(&self, address_present: bool) -> RequestScope {
        match self {
            Addressing::Required => RequestScope::Vault,
            Addressing::None => RequestScope::Registry,
            Addressing::Optional => {
                if address_present {
                    RequestScope::Vault
                } else {
                    RequestScope::Registry
                }
            }
        }
    }
}

impl fmt::Display for Addressing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A string that spells no addressing the vocabulary holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnknownAddressing;

impl fmt::Display for UnknownAddressing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the string spells no addressing")
    }
}

impl std::error::Error for UnknownAddressing {}

impl TryFrom<&str> for Addressing {
    type Error = UnknownAddressing;

    fn try_from(string: &str) -> Result<Self, UnknownAddressing> {
        Self::ALL
            .into_iter()
            .find(|addressing| addressing.as_str() == string)
            .ok_or(UnknownAddressing)
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
    /// Which registered vault contains a directory.
    VaultResolve,
    /// Where one vault's entry stands and what it is serving from, or, naming
    /// no vault, where every entry this installation serves stands.
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

    /// Whether this verb's request carries a vault address.
    ///
    /// The match carries no wildcard: a verb minted without an addressing does
    /// not compile, because a surface deciding whether to demand a vault
    /// address reads the answer from here, and the scope a host routes by
    /// follows from it through [`Addressing::scope_with`].
    pub const fn addressing(&self) -> Addressing {
        match self {
            // A vault address is carried, and the request is answered from
            // that vault's entry under a hold of it.
            Verb::Find
            | Verb::Search
            | Verb::Get
            | Verb::Count
            | Verb::Validate
            | Verb::Describe
            | Verb::VaultReload => Addressing::Required,
            // No vault address is carried; the request is answered from the
            // serving set and the registry, naming no entry to be held.
            Verb::VaultRegister
            | Verb::VaultUnregister
            | Verb::VaultList
            | Verb::VaultSet
            | Verb::VaultResolve
            | Verb::DoctorRegistry => Addressing::None,
            // One vault's standing, or every entry's. Naming a vault reports
            // that entry; naming none reports the serving set.
            Verb::VaultStatus => Addressing::Optional,
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
