//! What a reload met, said in one vocabulary.
//!
//! **Every reload outcome is a fact about the vault**, not about the host: a
//! reload re-reads the vault's own control files and applies what they state,
//! so what it can report is that a file did not read, did not parse or did not
//! apply, that the environment refused the work, or that the derived state
//! under it is not usable. That is why the codes these failures are filed
//! under are `vault/` and not `host/`.
//!
//! **Which file and which boundary are typed, and the account is prose.** A
//! client branches on the file and the stage — a schema that does not parse is
//! a different thing to show a person from a config that does not read — and
//! the sentence beside them is the reader's own account, which nothing
//! matches on.
//!
//! **A control file's refusal is one shape, named once.** The three parts move
//! together and are carried in four places — inside a [`ReloadFailure`], as a
//! drift that could not be read, as a status's last failure, and as an
//! attention reason — so [`ControlFileFailure`] is the type, and the three
//! places that carry nothing wider carry it alone rather than a
//! [`ReloadFailure`] whose other variants they can never hold.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::MaintainerIdentity;

/// Which of a vault's control files a reload was reading.
///
/// On the wire a file is the flat string itself: `"schema"`, `"config"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ControlFile {
    /// The vault schema.
    Schema,
    /// The vault config.
    Config,
}

/// Which boundary of a reload refused the candidate.
///
/// On the wire a stage is the flat string itself: `"read"`, `"parse"`,
/// `"apply"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReloadStage {
    /// The file's bytes could not be taken.
    Read,
    /// The bytes were taken and are not the document the file must hold.
    Parse,
    /// The document parsed and could not be put into service.
    Apply,
}

/// One of a vault's control files refusing at a boundary.
///
/// On the wire a control-file failure is the three fields themselves:
/// `{"file":"schema","stage":"parse","detail":"…"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ControlFileFailure {
    /// Which file it was.
    pub file: ControlFile,
    /// Which boundary refused it.
    pub stage: ReloadStage,
    /// The refusal in words, for a person reading a message or a log.
    pub detail: String,
}

impl ControlFileFailure {
    /// The control `file` refused at `stage`, described by `detail`.
    pub fn new(file: ControlFile, stage: ReloadStage, detail: impl Into<String>) -> Self {
        ControlFileFailure {
            file,
            stage,
            detail: detail.into(),
        }
    }
}

/// Why a reload did not leave the vault serving what its control files state.
///
/// On the wire a failure is an object tagged `kind`:
/// `{"kind":"control_file","failure":{"file":"schema","stage":"parse","detail":"…"}}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReloadFailure {
    /// One of the vault's control files refused at a boundary.
    #[non_exhaustive]
    ControlFile {
        /// Which file refused, where, and in words.
        failure: ControlFileFailure,
    },
    /// The environment refused the work: the disk is full, or a path stopped
    /// being readable. The stored state is sound and the environment is not.
    #[non_exhaustive]
    Environmental {
        /// The refusal in words, for a person reading a message or a log.
        detail: String,
    },
    /// The derived state is damaged, so there is nothing under the vault to
    /// apply a reloaded schema to until it is built again.
    #[non_exhaustive]
    StoreDamaged {
        /// The damage in words, for a person reading a message or a log.
        detail: String,
    },
    /// Change detection over the vault ended while the reload ran, so what
    /// changed from here on would not be reported.
    #[non_exhaustive]
    WatcherTerminal {
        /// The loss in words, for a person reading a message or a log.
        detail: String,
    },
    /// Sole maintainership of this vault's derived state was lost while the
    /// reload ran, so nothing may be written under it.
    LostMaintainership {},
    /// Another process holds sole maintainership of this vault's derived
    /// state.
    #[non_exhaustive]
    MaintainerContended {
        /// The incumbent maintainer's diagnostic identity.
        incumbent: MaintainerIdentity,
    },
    /// This vault is attached under terms that hold no reload, so there is
    /// nothing to re-read and nothing to apply.
    Unsupported {},
}

impl ReloadFailure {
    /// A control file refused, as `failure` accounts for it.
    pub const fn control_file(failure: ControlFileFailure) -> Self {
        ReloadFailure::ControlFile { failure }
    }

    /// The environment refused the work, described by `detail`.
    pub fn environmental(detail: impl Into<String>) -> Self {
        ReloadFailure::Environmental {
            detail: detail.into(),
        }
    }

    /// The derived state is damaged, described by `detail`.
    pub fn store_damaged(detail: impl Into<String>) -> Self {
        ReloadFailure::StoreDamaged {
            detail: detail.into(),
        }
    }

    /// Change detection ended, described by `detail`.
    pub fn watcher_terminal(detail: impl Into<String>) -> Self {
        ReloadFailure::WatcherTerminal {
            detail: detail.into(),
        }
    }

    /// Sole maintainership was lost while the reload ran.
    pub const fn lost_maintainership() -> Self {
        ReloadFailure::LostMaintainership {}
    }

    /// Another process holds maintainership, as far as its diagnostic says.
    pub const fn maintainer_contended(incumbent: MaintainerIdentity) -> Self {
        ReloadFailure::MaintainerContended { incumbent }
    }

    /// This attachment holds no reload.
    pub const fn unsupported() -> Self {
        ReloadFailure::Unsupported {}
    }
}
