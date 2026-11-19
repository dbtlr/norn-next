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
//! **`Installation` is in the vocabulary before a verb carries it.** Phase 6.3
//! lands token commands; phase 6.4 lands self-update, service install,
//! completions and manpage. They extend this same registry and are the
//! requests that name no vault, carry no
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
    /// Preview or apply a plan: operations, or a resolved plan.
    Apply,
    /// Change the frontmatter of one document, or of every document a
    /// predicate list matches.
    Set,
    /// Change the text of one document.
    Edit,
    /// Create one document at a path.
    New,
    /// Move one document, or every document a folder holds, rewriting the
    /// links naming what moves so they name it where it lands, and saying of
    /// each link left as written why.
    Move,
    /// Remove one document, saying what becomes of the links naming it.
    Delete,
    /// Respell every wikilink naming one document — or every broken one
    /// naming one place no document stands at — to name another, each in
    /// its own form, and
    /// saying of each wikilink left as written why.
    RewriteWikilink,
    /// Plan the fixes a batch of a vault's findings calls for, previewed or
    /// applied as one plan.
    Repair,
    /// Write a starter schema for a registered vault that declares none.
    Init,
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
    /// Bring one vault's control files up to the versions this build reads.
    VaultMigrate,
    /// The findings standing over the registry itself.
    DoctorRegistry,
}

/// What applying a verb changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerbEffect {
    /// Observes state without changing it.
    Reads,
    /// Changes vault documents.
    Writes { destructive: bool },
    /// Changes registration or control state.
    Administers { destructive: bool },
}

impl Verb {
    /// The effect of applying this verb, independent of its execution pool.
    pub const fn effect(&self) -> VerbEffect {
        match self {
            Self::Find => VerbEffect::Reads,
            Self::Search => VerbEffect::Reads,
            Self::Get => VerbEffect::Reads,
            Self::Count => VerbEffect::Reads,
            Self::Validate => VerbEffect::Reads,
            Self::Describe => VerbEffect::Reads,
            Self::Apply => VerbEffect::Writes { destructive: true },
            Self::Set => VerbEffect::Writes { destructive: true },
            Self::Edit => VerbEffect::Writes { destructive: true },
            Self::New => VerbEffect::Writes { destructive: false },
            Self::Move => VerbEffect::Writes { destructive: true },
            Self::Delete => VerbEffect::Writes { destructive: true },
            Self::RewriteWikilink => VerbEffect::Writes { destructive: true },
            Self::Repair => VerbEffect::Writes { destructive: true },
            Self::Init => VerbEffect::Administers { destructive: false },
            Self::VaultRegister => VerbEffect::Administers { destructive: false },
            Self::VaultUnregister => VerbEffect::Administers { destructive: true },
            Self::VaultList => VerbEffect::Reads,
            Self::VaultSet => VerbEffect::Administers { destructive: true },
            Self::VaultResolve => VerbEffect::Reads,
            Self::VaultStatus => VerbEffect::Reads,
            Self::VaultReload => VerbEffect::Administers { destructive: false },
            Self::VaultMigrate => VerbEffect::Administers { destructive: true },
            Self::DoctorRegistry => VerbEffect::Reads,
        }
    }

    /// Whether the params require an apply or preview mode.
    pub const fn takes_mode(&self) -> bool {
        match self {
            Self::Find => false,
            Self::Search => false,
            Self::Get => false,
            Self::Count => false,
            Self::Validate => false,
            Self::Describe => false,
            Self::Apply => true,
            Self::Set => true,
            Self::Edit => true,
            Self::New => true,
            Self::Move => true,
            Self::Delete => true,
            Self::RewriteWikilink => true,
            Self::Repair => true,
            Self::Init => true,
            Self::VaultRegister => false,
            Self::VaultUnregister => false,
            Self::VaultList => false,
            Self::VaultSet => false,
            Self::VaultResolve => false,
            Self::VaultStatus => false,
            Self::VaultReload => false,
            Self::VaultMigrate => true,
            Self::DoctorRegistry => false,
        }
    }

    /// One sentence describing the request.
    pub const fn summary(&self) -> &'static str {
        match self {
            Self::Find => "Documents matching a conjunction of predicates, in a stated order.",
            Self::Search => "Ranked hits for a query, answered from the vault's search ladder.",
            Self::Get => "One document, addressed by a resolution target.",
            Self::Count => "How many documents match, grouped by a key where one is asked for.",
            Self::Validate => "The findings standing over a vault.",
            Self::Describe => "What a vault declares about itself and what its documents carry.",
            Self::Apply => "Preview or apply a plan: operations, or a resolved plan.",
            Self::Set => {
                "Change the frontmatter of one document, or of every document a predicate list matches."
            }
            Self::Edit => "Change the text of one document.",
            Self::New => "Create one document at a path.",
            Self::Move => {
                "Move one document, or every document a folder holds, rewriting the links naming what moves so they name it where it lands, and saying of each link left as written why."
            }
            Self::Delete => "Remove one document, saying what becomes of the links naming it.",
            Self::RewriteWikilink => {
                "Respell every wikilink naming one document — or every broken one naming one place no document stands at — to name another, each in its own form, and saying of each wikilink left as written why."
            }
            Self::Repair => {
                "Plan the fixes a batch of a vault's findings calls for, previewed or applied as one plan."
            }
            Self::Init => "Write a starter schema for a registered vault that declares none.",
            Self::VaultRegister => "Register a vault under a name.",
            Self::VaultUnregister => "Stop serving a registered name and remove its registration.",
            Self::VaultList => "Every registration this installation holds.",
            Self::VaultSet => "Change a field of one registration.",
            Self::VaultResolve => "Which registered vault contains a directory.",
            Self::VaultStatus => {
                "Where one vault's entry stands and what it is serving from, or, naming no vault, where every entry this installation serves stands."
            }
            Self::VaultReload => "Re-read one vault's control files and apply what changed.",
            Self::VaultMigrate => {
                "Bring one vault's control files up to the versions this build reads."
            }
            Self::DoctorRegistry => "The findings standing over the registry itself.",
        }
    }

    /// The schema of the params parsed by the native wire route.
    pub fn params_schema(&self) -> schemars::Schema {
        match self {
            Self::Find => schemars::schema_for!(crate::FindParams),
            Self::Search => schemars::schema_for!(crate::SearchParams),
            Self::Get => schemars::schema_for!(crate::GetParams),
            Self::Count => schemars::schema_for!(crate::CountParams),
            Self::Validate => schemars::schema_for!(crate::ValidateParams),
            Self::Describe => schemars::schema_for!(crate::DescribeParams),
            Self::Apply => schemars::schema_for!(crate::ApplyParams),
            Self::Set => schemars::schema_for!(crate::SetParams),
            Self::Edit => schemars::schema_for!(crate::EditParams),
            Self::New => schemars::schema_for!(crate::NewParams),
            Self::Move => schemars::schema_for!(crate::MoveParams),
            Self::Delete => schemars::schema_for!(crate::DeleteParams),
            Self::RewriteWikilink => schemars::schema_for!(crate::RewriteWikilinkParams),
            Self::Repair => schemars::schema_for!(crate::RepairParams),
            Self::Init => schemars::schema_for!(crate::InitParams),
            Self::VaultRegister => schemars::schema_for!(crate::RegisterParams),
            Self::VaultUnregister => schemars::schema_for!(crate::UnregisterParams),
            Self::VaultList => schemars::schema_for!(crate::ListParams),
            Self::VaultSet => schemars::schema_for!(crate::VaultSetParams),
            Self::VaultResolve => schemars::schema_for!(crate::ResolveParams),
            Self::VaultStatus => schemars::schema_for!(crate::StatusParams),
            Self::VaultReload => schemars::schema_for!(crate::ReloadParams),
            Self::VaultMigrate => schemars::schema_for!(crate::MigrateParams),
            Self::DoctorRegistry => schemars::schema_for!(crate::DoctorRegistryParams),
        }
    }

    /// Every verb the registry holds, in declaration order.
    ///
    /// Reading a verb back and enumerating the registry both walk this list,
    /// so a variant absent here is unreadable and unadvertisable — the schema
    /// suite holds this list equal to the enum itself.
    pub const ALL: [Verb; 24] = [
        Verb::Find,
        Verb::Search,
        Verb::Get,
        Verb::Count,
        Verb::Validate,
        Verb::Describe,
        Verb::Apply,
        Verb::Set,
        Verb::Edit,
        Verb::New,
        Verb::Move,
        Verb::Delete,
        Verb::RewriteWikilink,
        Verb::Repair,
        Verb::Init,
        Verb::VaultRegister,
        Verb::VaultUnregister,
        Verb::VaultList,
        Verb::VaultSet,
        Verb::VaultResolve,
        Verb::VaultStatus,
        Verb::VaultReload,
        Verb::VaultMigrate,
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
            Verb::Apply => "apply",
            Verb::Set => "set",
            Verb::Edit => "edit",
            Verb::New => "new",
            Verb::Move => "move",
            Verb::Delete => "delete",
            Verb::RewriteWikilink => "rewrite_wikilink",
            Verb::Repair => "repair",
            Verb::Init => "init",
            Verb::VaultRegister => "vault_register",
            Verb::VaultUnregister => "vault_unregister",
            Verb::VaultList => "vault_list",
            Verb::VaultSet => "vault_set",
            Verb::VaultResolve => "vault_resolve",
            Verb::VaultStatus => "vault_status",
            Verb::VaultReload => "vault_reload",
            Verb::VaultMigrate => "vault_migrate",
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
            // that vault's entry under a hold of it. An apply carries its
            // vault as the address its plan names; a write verb carries it
            // as its own.
            Verb::Find
            | Verb::Search
            | Verb::Get
            | Verb::Count
            | Verb::Validate
            | Verb::Describe
            | Verb::Apply
            | Verb::Set
            | Verb::Edit
            | Verb::New
            | Verb::Move
            | Verb::Delete
            | Verb::RewriteWikilink
            | Verb::Repair
            | Verb::Init
            | Verb::VaultReload
            | Verb::VaultMigrate => Addressing::Required,
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
