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
    /// A config path is a symlink whose target does not exist. This is a
    /// refusal rather than an absence, because the two mean opposite things:
    /// nothing there is a first run, and a link pointing at nothing is a
    /// machine whose state moved or was half removed.
    DanglingSymlink { path: PathBuf },
    /// A file holding secrets is readable by the group or by the world. Read
    /// as a refusal rather than repaired in place: the bytes have already been
    /// exposed for as long as the mode has stood, and tightening the mode
    /// silently would hide that.
    InsecurePermissions { path: PathBuf, mode: u32 },
    /// A token label that is already taken. Labels are how a token is named
    /// for removal, so a second entry under one label is a token nobody can
    /// address.
    DuplicateLabel { label: String },
    /// A vault name outside the grammar. **No bypass**: the name keys a table,
    /// a directory and a URL-ish identifier at once, and a name that is legal
    /// in one of those and not the others has no safe reading.
    IllegalName { name: String, problem: &'static str },
    /// A vault root that is not absolute. A relative root means a different
    /// directory to every process that reads it, and machine-local state is
    /// read by more than one.
    RelativeRoot { path: PathBuf },
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
            ConfigError::InsecurePermissions { path, mode } => write!(
                f,
                "{} holds secrets and its mode is {mode:04o}, which is readable beyond its owner",
                path.display()
            ),
            ConfigError::DuplicateLabel { label } => {
                write!(f, "a token is already labelled `{label}`")
            }
            ConfigError::IllegalName { name, problem } => {
                write!(f, "`{name}` is not a vault name: {problem}")
            }
            ConfigError::RelativeRoot { path } => write!(
                f,
                "`{}` is not an absolute path, and a vault root is",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

/// The refusal for an operating-system error met while `operation` was running
/// against `path`.
pub(crate) fn io(operation: &'static str, path: &Path, error: std::io::Error) -> ConfigError {
    ConfigError::Io {
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
