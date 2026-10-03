//! `vault migrate`: bring a registered vault's control files up to the
//! versions this build reads.
//!
//! **A migration is a plan over the vault's control files.** It names a
//! registered vault and states its mode, as every write does. The host reads
//! the vault's schema, wherever its registration reads it, and the vault
//! config, and walks each up its migration ladder one step per version, each
//! step a text transform that edits only what it changes. Where a file is
//! behind, its rewrite is one `write_control_file`, and the rewrites of both
//! files are one plan through the one planner and the one applier: previewed,
//! or applied and taken into service by a reload. Where every file is at the
//! version this build reads, nothing is planned and the vault is already
//! current. A missing file is never created: a schema or config that is not
//! there has nothing to migrate.
//!
//! **Why a migration has a report of its own.** Its already-current outcome
//! plans nothing, so an [`ApplyReport`] cannot carry it. The migrated outcome
//! carries the apply's own report whole, so a caller reads a planned or
//! landed migration exactly as it reads any write, and beside a landing the
//! refusal of the reload that followed it, where that reload refused: a write
//! that landed is reported as landed whatever the reload answered.
//!
//! **What refuses a migration is the file, named.** A control file whose
//! version cannot be read, one at a version ahead of this build, one no step
//! migrates from, a rewrite that would lose a comment of the file it
//! rewrites, and a file another writer changed while the migration ran are
//! each refused `vault/migration-refused`, naming the file and why
//! ([`MigrationRefusal`]). Nothing is planned or written for any of them.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::address::VaultAddress;
use crate::apply::{ApplyMode, ApplyReport};
use crate::error::ErrorEnvelope;

/// What a `vault migrate` request carries.
///
/// On the wire: `{"vault":{"by":"name","name":"notes"},"mode":"preview"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct MigrateParams {
    /// The registered vault migrated, addressed by name.
    pub vault: VaultAddress,
    /// Whether to preview the migration or apply it. There is no default.
    pub mode: ApplyMode,
}

impl MigrateParams {
    /// A request to `mode` the migration of `vault`.
    pub const fn new(vault: VaultAddress, mode: ApplyMode) -> Self {
        MigrateParams { vault, mode }
    }
}

/// What a `vault migrate` answers with.
///
/// On the wire a report is an object tagged `outcome`:
/// `{"outcome":"migrated","report":{"outcome":"previewed",…},"reload_refused":null}`,
/// `{"outcome":"already_current"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
#[non_exhaustive]
pub enum MigrateReport {
    /// The control files behind this build were rewritten, or their rewrite
    /// planned: a preview's plan and forecast, or an apply's landing, after
    /// which the vault was reloaded under what landed.
    #[non_exhaustive]
    Migrated {
        /// The apply's own report, boxed since a plan it carries is far larger
        /// than the other outcome.
        report: Box<ApplyReport>,
        /// The refusal of the reload that followed a landing, where that
        /// reload refused: the rewritten files stand on disk and the vault
        /// goes on serving what it served, reporting a reload pending. `null`
        /// where nothing landed or the vault reloaded under it.
        reload_refused: Option<ErrorEnvelope>,
    },
    /// Every control file the vault holds is at the version this build reads:
    /// nothing was planned and nothing written.
    AlreadyCurrent,
}

impl MigrateReport {
    /// The migration planned or written, as `report` says.
    pub fn migrated(report: ApplyReport) -> Self {
        MigrateReport::Migrated {
            report: Box::new(report),
            reload_refused: None,
        }
    }

    /// The migration written, as `report` says, and the reload after it
    /// refused with `refusal`.
    pub fn migrated_reload_refused(report: ApplyReport, refusal: ErrorEnvelope) -> Self {
        MigrateReport::Migrated {
            report: Box::new(report),
            reload_refused: Some(refusal),
        }
    }

    /// Every control file already current.
    pub const fn already_current() -> Self {
        MigrateReport::AlreadyCurrent
    }
}

/// Why one control file was not migrated.
///
/// On the wire a refusal is an object tagged `reason`:
/// `{"reason":"comment_lost","comment":"# keep me"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
#[non_exhaustive]
pub enum MigrationRefusal {
    /// The file's version cannot be read: it is not text, not in its
    /// format, or carries no version its format spells.
    #[non_exhaustive]
    Unreadable {
        /// Why, in words, for a person reading a message or a log.
        detail: String,
    },
    /// The file is at a version ahead of the one this build reads, which a
    /// later build wrote.
    #[non_exhaustive]
    VersionAhead {
        /// The version the file states.
        found: i64,
        /// The version this build reads.
        current: i64,
    },
    /// The file is behind the version this build reads, at a version no step
    /// of its ladder migrates from.
    #[non_exhaustive]
    NoStep {
        /// The version the file states.
        found: i64,
        /// The version this build reads.
        current: i64,
    },
    /// The rewrite would not hold a comment the file holds. A comment is the
    /// file author's and no step may drop one, so the rewrite is refused
    /// rather than the comment lost.
    #[non_exhaustive]
    CommentLost {
        /// The comment, from its `#` to the end of its line.
        comment: String,
    },
    /// Another writer changed the file while the migration ran, so its
    /// rewrite was composed from bytes the file no longer holds. Migrating
    /// again composes it from what the file holds now.
    Changed,
}

impl MigrationRefusal {
    /// The file's version cannot be read, for the reason `detail` gives.
    pub fn unreadable(detail: impl Into<String>) -> Self {
        MigrationRefusal::Unreadable {
            detail: detail.into(),
        }
    }

    /// The file is at `found`, ahead of `current`.
    pub const fn version_ahead(found: i64, current: i64) -> Self {
        MigrationRefusal::VersionAhead { found, current }
    }

    /// The file is at `found`, behind `current`, and no step migrates from
    /// `found`.
    pub const fn no_step(found: i64, current: i64) -> Self {
        MigrationRefusal::NoStep { found, current }
    }

    /// The rewrite would lose `comment`.
    pub fn comment_lost(comment: impl Into<String>) -> Self {
        MigrationRefusal::CommentLost {
            comment: comment.into(),
        }
    }

    /// The file changed while the migration ran.
    pub const fn changed() -> Self {
        MigrationRefusal::Changed
    }
}
