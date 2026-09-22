//! `doctor`'s registry half: the findings standing over the registry itself.
//!
//! **`doctor` never mutates and never takes a vault.** It reads what this
//! installation holds and reports it; nothing it does changes a registration,
//! an entry or a vault.
//!
//! **Only the registry half crosses this seam.** `doctor` also reports on the
//! installation — the binary, the service, the machine-local directories —
//! and that half is an installation request, which Layer 6 lands. What is
//! spelled here is the half answered from the serving set and the registry, so
//! a surface that renders both renders two reports rather than one report with
//! halves that arrive at different layers.
//!
//! **Sanity is a sum, not a list that is usually empty.** A sound registry and
//! a registry with problems are two answers a surface renders differently, and
//! an empty problem list is not how "sound" is spelled.
//!
//! **The roll-up's attention reasons include what each vault's advisories
//! raise.** An [`Advisory`](crate::Advisory) is something about a vault's
//! serving worth telling an operator that is neither a refusal nor a move of
//! its trust state, and a roll-up names one vault per advisory it carries.
//!
//! **The engine slot and the delivered section sit together**, as they do on
//! a vault status: a config that enables an engine which is not standing is
//! those two readings disagreeing, and a reading that carried one of them
//! could not say so.
//!
//! **A verb that asks for nothing still has a params type**, so `doctor` is
//! spelled by a params type and a report type as every other verb is.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::name::VaultName;
use crate::reading::EngineSection;
use crate::status::{EngineStatus, RollUp};

/// What a `doctor` request carries: nothing.
///
/// `doctor` reads the whole installation, so there is nothing to narrow it to
/// and no vault to name.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct DoctorRegistryParams {}

impl DoctorRegistryParams {
    /// A request for the findings standing over the registry.
    pub const fn new() -> Self {
        DoctorRegistryParams {}
    }
}

/// Something about the registry that is wrong.
///
/// On the wire a problem is an object tagged `problem`:
/// `{"problem":"root_missing","name":"notes"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "problem", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RegistryProblem {
    /// More than one registration reaches one root, so each of them maintains
    /// its own derived state over the same documents.
    #[non_exhaustive]
    DuplicateRoot {
        /// Every registered name that reaches the root, ascending.
        aliases: Vec<VaultName>,
    },
    /// A registration's root is there and could not be read.
    #[non_exhaustive]
    RootUnreadable {
        /// The registration.
        name: VaultName,
        /// What refused, in words, for a person reading a message or a log.
        /// Clients never match on it.
        detail: String,
    },
    /// A registration's root is not there at all.
    #[non_exhaustive]
    RootMissing {
        /// The registration.
        name: VaultName,
    },
}

impl RegistryProblem {
    /// The registrations `aliases` all reach one root, in name order and
    /// each named once.
    ///
    /// The field says the aliases are ascending, so the constructor is what
    /// makes them so: a caller that walked a registry in some other order
    /// hands the same problem across whichever order it walked in.
    pub fn duplicate_root(aliases: impl IntoIterator<Item = VaultName>) -> Self {
        let mut aliases: Vec<VaultName> = aliases.into_iter().collect();
        aliases.sort();
        aliases.dedup();
        RegistryProblem::DuplicateRoot { aliases }
    }

    /// The root of `name` could not be read, for `detail`.
    pub fn root_unreadable(name: VaultName, detail: impl Into<String>) -> Self {
        RegistryProblem::RootUnreadable {
            name,
            detail: detail.into(),
        }
    }

    /// The root of `name` is not there.
    pub const fn root_missing(name: VaultName) -> Self {
        RegistryProblem::RootMissing { name }
    }
}

/// Whether the registry itself is in order.
///
/// On the wire a sanity reading is an object tagged `state`:
/// `{"state":"sound"}`, `{"state":"problems","problems":[…]}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RegistrySanity {
    /// Every registration names a root of its own, and every root is there and
    /// readable.
    Sound {},
    /// These are what is wrong.
    #[non_exhaustive]
    Problems {
        /// What is wrong, at least one.
        problems: Vec<RegistryProblem>,
    },
}

impl RegistrySanity {
    /// Nothing is wrong with the registry.
    pub const fn sound() -> Self {
        RegistrySanity::Sound {}
    }

    /// These `problems` stand over the registry.
    pub fn problems(problems: impl IntoIterator<Item = RegistryProblem>) -> Self {
        RegistrySanity::Problems {
            problems: problems.into_iter().collect(),
        }
    }
}

/// What one vault's engine is doing, and what the host was delivered as that
/// vault's engine section.
///
/// A config that enables an engine which is not standing is the two readings
/// disagreeing, and a reading that carried one of them could not say so.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct EngineHealth {
    /// The vault.
    pub name: VaultName,
    /// What the host was delivered as this vault's engine section.
    pub section: EngineSection,
    /// What this vault's engine slot is doing.
    pub engine: EngineStatus,
}

impl EngineHealth {
    /// The vault `name`, delivered `section`, whose engine slot is `engine`.
    pub const fn new(name: VaultName, section: EngineSection, engine: EngineStatus) -> Self {
        EngineHealth {
            name,
            section,
            engine,
        }
    }
}

/// What `doctor`'s registry half answers with.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct DoctorRegistryReport {
    /// What every entry this installation serves adds up to.
    pub roll_up: RollUp,
    /// Whether the registry itself is in order.
    pub registry: RegistrySanity,
    /// What each vault's engine is doing, ascending by name.
    pub engines: Vec<EngineHealth>,
}

impl DoctorRegistryReport {
    /// The registry reading: `roll_up` over the entries, `registry` sanity,
    /// and the health of each `engines` entry, ascending by name. The field
    /// says the engines are in name order, so the constructor is what makes
    /// them so.
    pub fn new(
        roll_up: RollUp,
        registry: RegistrySanity,
        engines: impl IntoIterator<Item = EngineHealth>,
    ) -> Self {
        let mut engines: Vec<EngineHealth> = engines.into_iter().collect();
        engines.sort_by(|left, right| left.name.cmp(&right.name));
        DoctorRegistryReport {
            roll_up,
            registry,
            engines,
        }
    }
}
