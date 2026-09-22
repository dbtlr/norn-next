//! `vault status`: where one vault's entry stands, or where every one of them
//! does.
//!
//! **Naming a vault and naming none are one verb.** The two answers are the
//! same reading at two widths, so they are two shapes of one report rather
//! than two verbs sharing a name. Which one comes back follows from whether
//! the request named a vault, which is what
//! [`Addressing::Optional`](crate::Addressing::Optional) says about this verb.
//!
//! **A status names a registration, not an address.** A root addresses a
//! throwaway attach, which has no lifecycle to observe.
//!
//! **A status reads the published demand and creates none.** A parked entry
//! reports its park, an unattached entry reports that it is unattached, and
//! neither is a refusal: asking where a vault stands never attaches one. The
//! one refusal is `host/unknown-vault`, for a name the registry does not hold.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::name::VaultName;
use crate::status::{RollUp, VaultStatus};

/// What a `vault status` request carries.
#[derive(Clone, Debug, Deserialize, Default, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct StatusParams {
    /// The registration to report on. `null` reports the roll-up over every
    /// entry this installation serves, which is the default. A root addresses
    /// a throwaway attach, which has no lifecycle to observe, so this is a
    /// name rather than a vault address.
    pub vault: Option<VaultName>,
}

impl StatusParams {
    /// A request for the roll-up over every entry.
    pub const fn new() -> Self {
        StatusParams { vault: None }
    }

    /// The request reporting on `vault` alone.
    #[must_use]
    pub fn with_vault(mut self, vault: VaultName) -> Self {
        self.vault = Some(vault);
        self
    }
}

/// What `vault status` answers with.
///
/// On the wire a status report is an object tagged `shape`:
/// `{"shape":"vault","status":{…}}`,
/// `{"shape":"roll_up","roll_up":{…},"vaults":[…]}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
#[non_exhaustive]
pub enum StatusReport {
    /// The request named a vault, and this is where that entry stands.
    #[non_exhaustive]
    Vault {
        /// Where the entry stands.
        ///
        /// Held behind one indirection, which is invisible on the wire: one
        /// vault's standing is several times the width of the roll-up beside
        /// it, and the box prices that width in the shape that carries it
        /// rather than in every value of this type.
        status: Box<VaultStatus>,
    },
    /// The request named no vault, and this is where every entry stands.
    #[non_exhaustive]
    RollUp {
        /// What the entries add up to.
        roll_up: RollUp,
        /// Where each entry stands, ascending by name.
        vaults: Vec<VaultStatus>,
    },
}

impl StatusReport {
    /// One entry's standing.
    pub fn vault(status: VaultStatus) -> Self {
        StatusReport::Vault {
            status: Box::new(status),
        }
    }

    /// Every entry's standing, with the roll-up `vaults` add up to.
    pub fn roll_up(vaults: Vec<VaultStatus>) -> Self {
        StatusReport::RollUp {
            roll_up: RollUp::of(&vaults),
            vaults,
        }
    }
}
