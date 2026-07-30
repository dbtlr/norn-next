//! What the store refuses, and how it says so.
//!
//! One error type, and **no driver type in it**. A `rusqlite::Error` in a
//! public signature would put the substrate in this crate's API, which is the
//! opposite of the seam it exists to be: a caller would then be able to match
//! on SQLite result codes, and the crate that owns the driver would have
//! published it. What crosses is the operation that was refused and what the
//! driver said, as text.
//!
//! Mapping a refusal onto the wire's structured envelope is the host's, not
//! this crate's. A store error is an engineering fact about a database
//! operation; which of those facts a caller is owed, and under which reason
//! code, is an orchestration decision.

use std::fmt;
use std::path::PathBuf;

/// A store operation that did not happen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoreError {
    /// A path handed to the store is not a vault-root-relative, normalized
    /// document path. Normalization is the filesystem seam's, so this is a
    /// caller that skipped it rather than a vault that is unusual.
    Path { path: String, problem: &'static str },
    /// The driver refused an operation. `operation` names what the store was
    /// doing, because a bare driver message says which constraint failed and
    /// never which write.
    Sql {
        operation: &'static str,
        message: String,
    },
    /// A file-lifecycle step the driver does not cover failed: preparing the
    /// database's parent directory, or removing the database. **An
    /// environmental failure, not damaged state** — nothing is discarded in
    /// response to one.
    Lifecycle {
        operation: &'static str,
        path: PathBuf,
        message: String,
    },
    /// The database is damaged in a way a reader can state. This is what a
    /// verification reports; discarding and rebuilding is the resolution, and
    /// the host decides when to reach for it.
    Damaged { what: String },
    /// An operation names a document the store does not hold.
    UnknownDocument { path: String },
    /// A bounded shape was handed more than it holds. The bound is the
    /// contract, so exceeding it is refused rather than truncated: a silently
    /// truncated payload is indistinguishable from a complete one.
    Bound {
        what: &'static str,
        limit: usize,
        given: usize,
    },
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Path { path, problem } => {
                write!(f, "`{path}` is not a document path: {problem}")
            }
            StoreError::Sql { operation, message } => write!(f, "{operation} failed: {message}"),
            StoreError::Lifecycle {
                operation,
                path,
                message,
            } => write!(f, "{operation} on {} failed: {message}", path.display()),
            StoreError::Damaged { what } => write!(f, "the store is damaged: {what}"),
            StoreError::UnknownDocument { path } => {
                write!(f, "no document is stored at `{path}`")
            }
            StoreError::Bound { what, limit, given } => {
                write!(f, "{what} holds at most {limit}, and {given} were given")
            }
        }
    }
}

impl std::error::Error for StoreError {}

/// The refusal for a driver error met while `operation` was running.
pub(crate) fn sql(operation: &'static str, error: rusqlite::Error) -> StoreError {
    StoreError::Sql {
        operation,
        message: error.to_string(),
    }
}

/// Whether a driver error says the *database* is damaged, as opposed to saying
/// the environment is.
///
/// The distinction decides whether rebuilding from zero is the answer.
/// Discarding a sound database because a disk was full or a permission was
/// revoked would destroy work to fix nothing, so only the codes that describe
/// the file's own contents qualify: it is not a database, or its pages are
/// corrupt.
///
/// Two codes deliberately do **not** qualify. `SQLITE_BUSY` and `SQLITE_LOCKED`
/// say somebody else holds the database, and `SQLITE_SCHEMA` says the schema
/// moved under a prepared statement — each of them describes a moment rather
/// than the file, and each of them is resolved by trying again.
pub(crate) fn is_damaged(error: &rusqlite::Error) -> bool {
    use rusqlite::ErrorCode::{DatabaseCorrupt, NotADatabase};
    match error {
        rusqlite::Error::SqliteFailure(failure, _) => {
            matches!(failure.code, DatabaseCorrupt | NotADatabase)
        }
        _ => false,
    }
}
