//! `vault status`: where one vault's entry stands, or where every one of them
//! does.
//!
//! **Naming a vault and naming none are two readings, not two widths of one.**
//! Naming a vault reports that entry's standing; naming none reports the
//! roll-up — the counts and the attention reasons — over every entry. They are
//! two shapes of one report rather than two verbs sharing a name, and which
//! one comes back follows from whether the request named a vault, which is
//! what [`Addressing::Optional`](crate::Addressing::Optional) says about this
//! verb. A client that wants one vault's own standing asks for that vault by
//! name.
//!
//! **A status carries a vault address, as every vault-scope request does.** A
//! root addresses a throwaway attach, which has no lifecycle to observe, so
//! the host refuses one here the way it refuses one for a read.
//!
//! **One vault's standing is held behind an indirection in the report.** It is
//! several times the width of the roll-up beside it, and the box prices that
//! width in the shape that carries it rather than in every value of the
//! report type. The indirection is invisible on the wire.
//!
//! **A status reads the published demand and creates none.** A parked entry
//! reports its park, an unattached entry reports that it is unattached, and
//! neither is a refusal: asking where a vault stands never attaches one. The
//! refusals are `host/unknown-vault`, for a name the registry does not hold,
//! and `host/unsupported-attach-mode`, for an address that names a root.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::address::VaultAddress;
use crate::status::{RollUp, VaultStatus};

/// What a `vault status` request carries.
#[derive(Clone, Debug, Deserialize, Default, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct StatusParams {
    /// The vault to report on. A vault named here reports that entry's
    /// standing; `null` reports the roll-up over every entry this
    /// installation serves. An address naming a root is refused
    /// `host/unsupported-attach-mode`: a throwaway attach has no lifecycle to
    /// observe.
    pub vault: Option<VaultAddress>,
}

impl StatusParams {
    /// A request for the roll-up over every entry.
    pub const fn new() -> Self {
        StatusParams { vault: None }
    }

    /// The request reporting on `vault` alone.
    #[must_use]
    pub fn with_vault(mut self, vault: VaultAddress) -> Self {
        self.vault = Some(vault);
        self
    }
}

/// What `vault status` answers with.
///
/// On the wire a status report is an object tagged `shape`:
/// `{"shape":"vault","status":{…}}`,
/// `{"shape":"roll_up","roll_up":{…}}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
#[non_exhaustive]
pub enum StatusReport {
    /// The request named a vault, and this is where that entry stands.
    #[non_exhaustive]
    Vault {
        /// Where the entry stands.
        status: Box<VaultStatus>,
    },
    /// The request named no vault, and this is what every entry adds up to.
    #[non_exhaustive]
    RollUp {
        /// What the entries add up to: the counts, and every vault that wants
        /// attention. Where one entry stands is what naming that vault
        /// reports.
        roll_up: RollUp,
    },
}

impl StatusReport {
    /// One entry's standing.
    pub fn vault(status: VaultStatus) -> Self {
        StatusReport::Vault {
            status: Box::new(status),
        }
    }

    /// What every entry adds up to, as `roll_up` counted them.
    pub const fn roll_up(roll_up: RollUp) -> Self {
        StatusReport::RollUp { roll_up }
    }
}
