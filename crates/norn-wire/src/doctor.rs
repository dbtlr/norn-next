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
//! its trust state, and a roll-up names one vault per advisory it carries that
//! [wants attention](crate::Advisory::wants_attention) — among them a
//! vault-local shadow fallback the vault does not ignore.
//!
//! **The engine slot and the delivered section sit together**, as they do on
//! a vault status: a config that enables an engine which is not standing is
//! those two readings disagreeing, and a reading that carried one of them
//! could not say so.
//!
//! **A verb that asks for nothing still has a params type**, so `doctor` is
//! spelled by a params type and a report type as every other verb is.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::error::{NameSet, ReasonCode};
use crate::name::VaultName;
use crate::reading::EngineSection;
use crate::status::{Attention, EngineStatus, RollUp};

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
        /// Every registered name that reaches the root, at least two of them,
        /// ascending and each named once.
        aliases: NameSet,
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
    /// The registrations `aliases` all reach one root.
    ///
    /// The floor is the set's: a caller that holds one has names a duplicate
    /// root can be spelled with, so there is nothing left for this to refuse.
    pub fn duplicate_root(aliases: NameSet) -> Self {
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

/// A problem list naming no problem.
///
/// A sound registry and a registry with problems are two answers, and an empty
/// problem list is not how "sound" is spelled: a reading that names nothing
/// wrong is `sound`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NoProblems;

impl fmt::Display for NoProblems {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a registry with problems names at least one; a sound registry is `sound` rather than an empty list")
    }
}

impl std::error::Error for NoProblems {}

/// Whether the registry itself is in order.
///
/// On the wire a sanity reading is an object tagged `state`:
/// `{"state":"sound"}`, `{"state":"problems","problems":[…]}`. The problem
/// list is not empty: a reading naming no problem is refused rather than read.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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

    /// These `problems` stand over the registry, or the reason the list names
    /// no reading at all.
    pub fn problems(
        problems: impl IntoIterator<Item = RegistryProblem>,
    ) -> Result<Self, NoProblems> {
        let problems: Vec<RegistryProblem> = problems.into_iter().collect();
        if problems.is_empty() {
            return Err(NoProblems);
        }
        Ok(RegistrySanity::Problems { problems })
    }

    /// Whether a problem this reading names is the cause `attention` names:
    /// a park on a duplicate root, for a name a duplicate root here names.
    fn owns(&self, attention: &Attention) -> bool {
        let RegistrySanity::Problems { problems } = self else {
            return false;
        };
        let Attention::Parked { name, code } = attention else {
            return false;
        };
        *code == ReasonCode::HostDuplicateRoot
            && problems.iter().any(|problem| match problem {
                RegistryProblem::DuplicateRoot { aliases } => aliases.names().contains(name),
                RegistryProblem::RootUnreadable { .. } | RegistryProblem::RootMissing { .. } => {
                    false
                }
            })
    }
}

impl JsonSchema for RegistrySanity {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("RegistrySanity")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::RegistrySanity")
    }

    /// The two branches a derive would describe, with the floor the reader
    /// keeps advertised as `minItems`. A derive says an array of problems with
    /// no members at all, so a surface validating against it would pass a
    /// reading this crate refuses to read.
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let problem = generator.subschema_for::<RegistryProblem>();
        json_schema!({
            "description": "Whether the registry itself is in order.\n\nOn the wire a sanity reading is an object tagged `state`:\n`{\"state\":\"sound\"}`, `{\"state\":\"problems\",\"problems\":[…]}`. The problem\nlist is not empty: a reading naming no problem is refused rather than read.",
            "oneOf": [
                {
                    "type": "object",
                    "description": "Every registration names a root of its own, and every root is there and\nreadable.",
                    "properties": {
                        "state": {
                            "type": "string",
                            "const": "sound",
                        },
                    },
                    "required": ["state"],
                },
                {
                    "type": "object",
                    "description": "These are what is wrong.",
                    "properties": {
                        "state": {
                            "type": "string",
                            "const": "problems",
                        },
                        "problems": {
                            "type": "array",
                            "description": "What is wrong, at least one.",
                            "items": problem,
                            "minItems": 1,
                        },
                    },
                    "required": ["state", "problems"],
                },
            ],
        })
    }
}

/// The sanity reading as it arrives, before the problem list is checked for
/// naming a problem at all. The tag and the field names are the reading's own,
/// so the bytes a reader accepts are the bytes a writer produces.
#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum RegistrySanityFields {
    Sound {},
    Problems { problems: Vec<RegistryProblem> },
}

impl<'de> Deserialize<'de> for RegistrySanity {
    /// A reading arrives as the branch it names and is read back through the
    /// same grammar the constructors hold: a problem list naming no problem is
    /// no reading, so it refuses the read rather than landing as a second
    /// spelling of `sound`.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match RegistrySanityFields::deserialize(deserializer)? {
            RegistrySanityFields::Sound {} => Ok(RegistrySanity::sound()),
            RegistrySanityFields::Problems { problems } => {
                RegistrySanity::problems(problems).map_err(D::Error::custom)
            }
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
    /// What every entry this installation serves adds up to, less the
    /// attention a registry problem here names the cause of.
    pub roll_up: RollUp,
    /// Whether the registry itself is in order.
    pub registry: RegistrySanity,
    /// What each vault's engine is doing. A producer emits them ascending by
    /// name, and a reader accepts whatever order they arrive in.
    pub engines: Vec<EngineHealth>,
}

impl DoctorRegistryReport {
    /// The registry reading: `roll_up` over the entries, `registry` sanity,
    /// and the health of each `engines` entry, ascending by name. The field
    /// says a producer emits the engines in name order, so the constructor is
    /// what makes this one a producer that does.
    ///
    /// **One cause is named once.** A duplicate root is a registry problem,
    /// so the park it raises on each name the problem names is left out of
    /// the roll-up's attention; a duplicate-root park on a name no problem
    /// here names is kept. The counts are the roll-up's own.
    pub fn new(
        roll_up: RollUp,
        registry: RegistrySanity,
        engines: impl IntoIterator<Item = EngineHealth>,
    ) -> Self {
        let mut engines: Vec<EngineHealth> = engines.into_iter().collect();
        engines.sort_by(|left, right| left.name.cmp(&right.name));
        let roll_up = roll_up.retaining_attention(|attention| !registry.owns(attention));
        DoctorRegistryReport {
            roll_up,
            registry,
            engines,
        }
    }
}
