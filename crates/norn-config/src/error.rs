//! What machine-local state refuses, and how it says so.
//!
//! One error type for the whole crate, and every refusal is a variant rather
//! than a sentence. A caller decides what to do about a corrupt registry, a
//! file this binary is too old to read, and a token file the whole machine can
//! read — three different decisions — so the three are three shapes, and none
//! of them is a string a caller would have to match on.
//!
//! Mapping a refusal onto the wire's structured envelope belongs to whoever
//! serves it. What is here is the engineering fact: which file, and what about
//! it was wrong.

use std::fmt;
use std::path::{Path, PathBuf};

/// A machine-local read or write that did not happen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigError {
    /// The environment does not say where the config directory is. Only the
    /// one env-resolving constructor produces this; every other entry point
    /// takes the directories it works over.
    Environment {
        variable: &'static str,
        problem: &'static str,
    },
    /// A filesystem call failed, named with the path it was made against. A
    /// bare operating-system message says what went wrong and never which
    /// file.
    Io {
        operation: &'static str,
        path: PathBuf,
        message: String,
    },
    /// A write whose rename landed and whose durability did not. The change is
    /// at the path and every reader sees it; whether it survives a power cut
    /// is unknown, because the directory entry the rename made was not
    /// confirmed onto the disk.
    ///
    /// **Retrying is a second mutation, not a repeat of this one.** The first
    /// one is visible: a caller that minted a credential has written it down,
    /// and adding it again refuses as a duplicate. The resolution is to read
    /// the file and act on what it holds.
    MutationUnconfirmed {
        operation: &'static str,
        path: PathBuf,
        message: String,
    },
    /// A file this binary's schema version covers holds something that is not
    /// that schema: bytes that are not TOML, a table where a value belongs, a
    /// value outside a closed vocabulary. The reason is written for a person
    /// who has to go and look at the file.
    Corrupt { path: PathBuf, reason: String },
    /// A file was written by a newer binary. **Refused, never guessed at**: a
    /// reader that dropped the parts it did not understand would write the
    /// file back without them, and the newer binary's state would be gone.
    VersionAhead {
        path: PathBuf,
        found: i64,
        supported: i64,
    },
    /// A config path, or a directory on the way to one, is a symlink whose
    /// target does not exist. This is a refusal rather than an absence,
    /// because the two mean opposite things: nothing there is a first run, and
    /// a link pointing at nothing is a machine whose state moved or was half
    /// removed. The path named is the link, which may be an ancestor of the
    /// file that was asked for.
    DanglingSymlink { path: PathBuf },
    /// A machine-local data file is a symlink that resolves. Refused in both
    /// directions: a read through the link would take bytes from a file
    /// nothing else in this crate governs, and a write would replace the link
    /// with a regular file — so the two directions would disagree about which
    /// file the state is in. These are machine-written files, not dotfile
    /// material.
    SymlinkedFile { path: PathBuf },
    /// A file holding secrets, or the directory holding it, is reachable by
    /// the group or by the world. Read as a refusal rather than repaired in
    /// place: the bytes have already been exposed for as long as the mode has
    /// stood, and tightening the mode silently would hide that.
    InsecurePermissions { path: PathBuf, mode: u32 },
    /// A token label that is already taken. Labels are how a token is named
    /// for removal, so a second entry under one label is a token nobody can
    /// address.
    DuplicateLabel { label: String },
    /// A token with no secret in it. An empty credential matches an empty
    /// presentation, which is every request that carries no credential at all.
    EmptySecret { label: String },
    /// The system clock reads before the Unix epoch, so there is no moment to
    /// stamp a token with. Refused rather than stamped with the epoch: a
    /// fabricated 1970 is a date that reads as true and is not.
    ClockBeforeEpoch,
    /// A vault name outside the grammar. **No bypass**: the name keys a table,
    /// a directory and a URL-ish identifier at once, and a name that is legal
    /// in one of those and not the others has no safe reading.
    ///
    /// The grammar is [`VaultName`](norn_wire::VaultName)'s and the refusal is
    /// that type's; this is the same refusal read as one of this crate's, so a
    /// caller reading a registry file handles one error type rather than two.
    IllegalName { name: String, problem: &'static str },
    /// A token label outside the grammar. Same terms as [`IllegalName`]: the
    /// label keys a table and is typed by a person, and a label that is not a
    /// bare word has no safe reading.
    ///
    /// [`IllegalName`]: ConfigError::IllegalName
    IllegalLabel {
        label: String,
        problem: &'static str,
    },
    /// A path this crate records or builds paths under, in a shape it cannot
    /// record. `subject` names which path it was.
    IllegalPath {
        path: PathBuf,
        subject: &'static str,
        problem: &'static str,
    },
    /// A mutation of a file this thread is already inside a mutation of. The
    /// lock is exclusive and held for the whole read-modify-write, so the
    /// second one would wait on the first for as long as the process lives.
    NestedMutation { path: PathBuf },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Environment { variable, problem } => {
                write!(f, "the environment's `{variable}` {problem}")
            }
            ConfigError::Io {
                operation,
                path,
                message,
            } => write!(f, "{operation} {} failed: {message}", path.display()),
            ConfigError::MutationUnconfirmed {
                operation,
                path,
                message,
            } => write!(
                f,
                "{} holds the change, and {operation} it afterwards failed: {message}. The change \
                 is visible to every reader; whether it survives a power cut is unknown",
                path.display()
            ),
            ConfigError::Corrupt { path, reason } => {
                write!(f, "{} is not readable: {reason}", path.display())
            }
            ConfigError::VersionAhead {
                path,
                found,
                supported,
            } => write!(
                f,
                "{} is at schema version {found} and this build reads {supported}",
                path.display()
            ),
            ConfigError::DanglingSymlink { path } => write!(
                f,
                "{} is a symlink whose target does not exist",
                path.display()
            ),
            ConfigError::SymlinkedFile { path } => write!(
                f,
                "{} is a symlink, and machine-local state is kept in a file this crate writes",
                path.display()
            ),
            ConfigError::InsecurePermissions { path, mode } => write!(
                f,
                "{} guards secrets and its mode is {mode:04o}, which reaches beyond its owner",
                path.display()
            ),
            ConfigError::DuplicateLabel { label } => {
                write!(f, "a token is already labelled `{label}`")
            }
            ConfigError::EmptySecret { label } => {
                write!(f, "the token labelled `{label}` carries no secret")
            }
            ConfigError::ClockBeforeEpoch => {
                write!(f, "this machine's clock reads before the Unix epoch")
            }
            ConfigError::IllegalName { name, problem } => {
                write!(f, "`{name}` is not a vault name: {problem}")
            }
            ConfigError::IllegalLabel { label, problem } => {
                write!(f, "`{label}` is not a token label: {problem}")
            }
            ConfigError::IllegalPath {
                path,
                subject,
                problem,
            } => write!(f, "`{}` is not a {subject}: {problem}", path.display()),
            ConfigError::NestedMutation { path } => write!(
                f,
                "{} is already being changed further up this thread's stack",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<norn_wire::IllegalVaultName> for ConfigError {
    /// A name the grammar refused, read as this crate's refusal. The account
    /// is the grammar's own — the string that was offered and what the grammar
    /// wanted — so nothing about the refusal changes on the way through.
    fn from(refusal: norn_wire::IllegalVaultName) -> Self {
        ConfigError::IllegalName {
            name: refusal.name().to_string(),
            problem: refusal.problem(),
        }
    }
}

/// The refusal for an operating-system error met while `operation` was running
/// against `path`.
pub(crate) fn io(operation: &'static str, path: &Path, error: std::io::Error) -> ConfigError {
    ConfigError::Io {
        operation,
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

/// The refusal for an operating-system error met after the rename that made a
/// mutation visible.
pub(crate) fn unconfirmed(
    operation: &'static str,
    path: &Path,
    error: std::io::Error,
) -> ConfigError {
    ConfigError::MutationUnconfirmed {
        operation,
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

/// The refusal for a file whose contents are not the schema it claims.
pub(crate) fn corrupt(path: &Path, reason: impl Into<String>) -> ConfigError {
    ConfigError::Corrupt {
        path: path.to_path_buf(),
        reason: reason.into(),
    }
}
