//! `init`: write a starter schema for a registered vault that declares none.
//!
//! **An init plans only what is missing and never overwrites.** It names a
//! registered vault and states its mode, as every write does, and the host
//! answers one of three outcomes ([`InitReport`]): where the vault reads its
//! schema from the default `.norn/schema.yaml` and nothing stands there, the
//! starter schema is planned — previewed, or applied and taken into service —
//! as one `write_control_file` through the one planner and the one applier;
//! where a schema stands there, the vault is already set up and nothing is
//! planned; and where the registration reads its schema from a
//! `schema_source`, inside the vault or out, the schema lives elsewhere, and
//! nothing is planned either. Rewriting a schema that stands is `vault
//! migrate`'s, not init's.
//!
//! **Why init has a report of its own.** Two of its outcomes plan nothing, so
//! an [`ApplyReport`] cannot carry them: "already set up" and "lives
//! elsewhere" are answers rather than refusals, and the second names the
//! source a caller is sent to. The scaffolded outcome carries the apply's own
//! report whole, so a caller reads a planned or landed starter exactly as it
//! reads any write.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::address::{SchemaSource, VaultAddress};
use crate::apply::{ApplyMode, ApplyReport};
use crate::document::DocumentPath;

/// What an `init` request carries.
///
/// On the wire: `{"vault":{"by":"name","name":"notes"},"mode":"preview"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct InitParams {
    /// The registered vault set up, addressed by name.
    pub vault: VaultAddress,
    /// Whether to preview the starter schema or apply it. There is no
    /// default.
    pub mode: ApplyMode,
}

impl InitParams {
    /// A request to `mode` the setting up of `vault`.
    pub const fn new(vault: VaultAddress, mode: ApplyMode) -> Self {
        InitParams { vault, mode }
    }
}

/// What an `init` answers with.
///
/// On the wire a report is an object tagged `outcome`:
/// `{"outcome":"scaffolded","report":{"outcome":"previewed",…}}`,
/// `{"outcome":"already_set_up","schema":".norn/schema.yaml"}`,
/// `{"outcome":"schema_elsewhere","source":"/shared/schema.yaml"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
#[non_exhaustive]
pub enum InitReport {
    /// The starter schema was planned or written: a preview's plan and
    /// forecast, or an apply's landing, after which the vault was reloaded
    /// under it.
    #[non_exhaustive]
    Scaffolded {
        /// The apply's own report, boxed since a plan it carries is far larger
        /// than either other outcome.
        report: Box<ApplyReport>,
    },
    /// A schema already stands where the vault reads it: nothing was planned
    /// and nothing written.
    #[non_exhaustive]
    AlreadySetUp {
        /// Where the schema stands, vault-relative.
        schema: DocumentPath,
    },
    /// The registration reads its schema from a `schema_source`, which init
    /// does not write: nothing was planned and nothing written.
    #[non_exhaustive]
    SchemaElsewhere {
        /// Where the vault's schema is read from.
        source: SchemaSource,
    },
}

impl InitReport {
    /// The starter schema planned or written, as `report` says.
    pub fn scaffolded(report: ApplyReport) -> Self {
        InitReport::Scaffolded {
            report: Box::new(report),
        }
    }

    /// A schema already stands at `schema`.
    pub const fn already_set_up(schema: DocumentPath) -> Self {
        InitReport::AlreadySetUp { schema }
    }

    /// The vault's schema is read from `source`.
    pub const fn schema_elsewhere(source: SchemaSource) -> Self {
        InitReport::SchemaElsewhere { source }
    }
}
