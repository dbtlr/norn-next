//! `vault reload`: re-read one vault's control files and apply what changed.
//!
//! **A reload is what activates a control file.** No watcher event ever does,
//! so a vault whose schema or config has been edited goes on serving the ones
//! it has until a reload is asked for.
//!
//! **A dry run validates and activates nothing.** It reads the authored
//! control files, judges them the way a reload does, and leaves the vault
//! serving what it was serving; `activated` is what says which of the two
//! happened.
//!
//! **A reload carries a vault address, as every vault-scope request does.** A
//! root addresses a throwaway attach, which holds no control files to
//! re-read, so the host refuses one here the way it refuses one for a read.
//!
//! The refusals are the ones a reload answers with: `host/unknown-vault` for a
//! name the registry does not hold, `host/unsupported-attach-mode` for an
//! address that names a root, `host/entry-not-ready` for an entry holding
//! nothing to reload yet, `host/entry-untrusted` for an entry whose derived
//! state cannot be trusted, `vault/reload-busy` for a vault that is serving
//! and already has something working over it — a warm job among them — and
//! `vault/reload-failed`, carrying what the reload met, for every outcome of a
//! reload that ran, its `unsupported` kind among them for an attachment that
//! holds no reload at all.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::address::VaultAddress;
use crate::status::Fingerprints;

/// What a reload decided about the vault's schema.
///
/// The decision is read off the candidate schema against the active one. The
/// config is taken into service either way, so an outcome says nothing about
/// whether the config changed.
///
/// On the wire an outcome is the flat string itself: `"config_only"`,
/// `"schema_changed"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReloadOutcome {
    /// The candidate schema is the schema the vault already serves, so what
    /// is derived under it stands and the candidate config was taken into
    /// service beside it, changed or not.
    ConfigOnly,
    /// The candidate schema is not the schema the vault was serving, so it was
    /// pinned and what is derived under it was derived again.
    SchemaChanged,
}

/// What a `vault reload` request carries.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ReloadParams {
    /// The vault to reload. An address naming a root is refused
    /// `host/unsupported-attach-mode`: a throwaway attach holds no control
    /// files to re-read.
    pub vault: VaultAddress,
    /// Whether to validate the authored control files without putting them
    /// into service. `false` applies what the authored control files changed;
    /// `true` reads and judges them and leaves the vault serving what it was
    /// serving.
    pub dry_run: bool,
}

impl ReloadParams {
    /// A request to reload `vault` and apply what changed.
    pub const fn new(vault: VaultAddress) -> Self {
        ReloadParams {
            vault,
            dry_run: false,
        }
    }

    /// The request validating without activating.
    #[must_use]
    pub const fn dry_run(mut self) -> Self {
        self.dry_run = true;
        self
    }
}

/// What `vault reload` answers with.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ReloadReport {
    /// What the reload decided about the vault's schema.
    pub outcome: ReloadOutcome,
    /// The fingerprints the candidate was read at. They are the vault's active
    /// fingerprints where the reload activated, and the authored ones it
    /// validated where it did not.
    pub fingerprints: Fingerprints,
    /// Whether the candidate was put into service. A dry run reports `false`.
    pub activated: bool,
}

impl ReloadReport {
    /// A reload that met `outcome` at `fingerprints` and activated it.
    pub const fn new(outcome: ReloadOutcome, fingerprints: Fingerprints) -> Self {
        ReloadReport {
            outcome,
            fingerprints,
            activated: true,
        }
    }

    /// The report of a dry run: what it validated, and nothing activated.
    #[must_use]
    pub const fn validated(mut self) -> Self {
        self.activated = false;
        self
    }
}
