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
//!
//! **The one judgment that reaches a caller is damaged or not.** Which driver
//! codes describe the file's own contents is a fact about SQLite, and SQLite is
//! the substrate's to own — a host reading it off a message would be matching
//! on the driver through a keyhole. So `norn-db` types damage at the driver
//! seam, every refusal it hands back arrives here as a [`StoreError`],
//! [`StoreError::damage`] is how a caller reads that verdict, and the
//! resolution the verdict authorizes — the database discarded and derived
//! again — stays the host's decision to make.

use std::fmt;
use std::path::PathBuf;

use norn_db::{DbError, rusqlite};

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
    /// verification reports, and what any operation reports when the driver
    /// says the file's own contents are wrong; discarding and rebuilding is the
    /// resolution, and the host decides when to reach for it.
    Damaged { what: String },
    /// A bounded shape was handed more than it holds. The bound is the
    /// contract, so exceeding it is refused rather than truncated: a silently
    /// truncated payload is indistinguishable from a complete one.
    Bound {
        what: &'static str,
        limit: usize,
        given: usize,
    },
    /// One entry of a changeset was refused, named by where it sits and what it
    /// is about. A streaming heal hands over tens of thousands of entries and
    /// fails on whichever one is pathological, so the refusal that reaches the
    /// caller has to say which — the operation alone reads the same for entry
    /// one and entry fifty thousand.
    Entry {
        /// The entry's position in the changeset, counting from zero.
        index: usize,
        path: String,
        problem: Box<StoreError>,
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
            StoreError::Bound { what, limit, given } => {
                write!(f, "{what} holds at most {limit}, and {given} were given")
            }
            StoreError::Entry {
                index,
                path,
                problem,
            } => write!(f, "changeset entry {index}, `{path}`: {problem}"),
        }
    }
}

impl StoreError {
    /// What the store says is damaged, where this refusal is a statement about
    /// damaged state rather than about anything else.
    ///
    /// A changeset entry's refusal is the entry's own, wrapped in the position
    /// it sat at, so the answer is read through [`StoreError::Entry`] rather
    /// than off the outermost shape: an increment that met a corrupt page on
    /// entry fifty thousand met the same damage a read of the same page does,
    /// and a caller matching only the outer variant would resolve one by
    /// rebuilding and the other by trying again.
    ///
    /// This is the whole of what a caller outside this crate needs to reach the
    /// database-side heal rung, and it is why no driver code crosses the API:
    /// the policy deciding which driver failures describe the file's own
    /// contents lives here, beside the operations that meet them.
    pub fn damage(&self) -> Option<&str> {
        match self {
            StoreError::Damaged { what } => Some(what),
            StoreError::Entry { problem, .. } => problem.damage(),
            StoreError::Path { .. }
            | StoreError::Sql { .. }
            | StoreError::Lifecycle { .. }
            | StoreError::Bound { .. } => None,
        }
    }
}

impl std::error::Error for StoreError {}

/// The substrate's three refusal shapes are three of this crate's own, and the
/// conversion is the whole of the mapping: the driver-seam judgment about
/// damage was already taken, and nothing here re-decides it.
impl From<DbError> for StoreError {
    fn from(error: DbError) -> Self {
        match error {
            DbError::Sql { operation, message } => StoreError::Sql { operation, message },
            DbError::Lifecycle {
                operation,
                path,
                message,
            } => StoreError::Lifecycle {
                operation,
                path,
                message,
            },
            DbError::Damaged { what } => StoreError::Damaged { what },
        }
    }
}

/// The refusal for a changeset entry, naming which entry it was.
pub(crate) fn in_entry(
    index: usize,
    path: &crate::path::DocumentPath,
    problem: StoreError,
) -> StoreError {
    StoreError::Entry {
        index,
        path: path.as_str().to_string(),
        problem: Box::new(problem),
    }
}

/// The refusal for a driver error met while `operation` was running.
///
/// **Every driver error this crate reports passes through here**, and the
/// damage typing under it is `norn-db`'s: a code describing the file's own
/// contents is [`StoreError::Damaged`] whichever operation met it — a read, a
/// write, an increment, a verification — because a corrupt page is the same
/// fact about the same file at all of them. Reporting it as the refused
/// operation would leave every caller to re-derive the distinction from a
/// message, and a caller that cannot tell damaged state from a broken
/// environment resolves both the same way: by trying again against a database
/// that will never answer.
pub(crate) fn sql(operation: &'static str, error: rusqlite::Error) -> StoreError {
    norn_db::sql(operation, error).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(code: i32) -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)
    }

    /// The codes that describe the file's own contents are damage whichever
    /// operation met them, and every other code is the refused operation it
    /// was. The extended code is in the list because it is the one a full-text
    /// index that stopped agreeing with its column reports — the shape of
    /// damage a warm write meets rather than an open.
    #[test]
    fn only_a_code_about_the_file_s_contents_is_typed_as_damage() {
        for code in [
            rusqlite::ffi::SQLITE_CORRUPT,
            rusqlite::ffi::SQLITE_CORRUPT | (1 << 8),
            rusqlite::ffi::SQLITE_NOTADB,
        ] {
            let error = sql("writing a changeset", failure(code));
            assert!(error.damage().is_some(), "{code}: {error:?}");
        }
        for code in [
            rusqlite::ffi::SQLITE_BUSY,
            rusqlite::ffi::SQLITE_READONLY,
            rusqlite::ffi::SQLITE_IOERR,
            rusqlite::ffi::SQLITE_LOCKED,
            rusqlite::ffi::SQLITE_PERM,
            rusqlite::ffi::SQLITE_FULL,
            rusqlite::ffi::SQLITE_ERROR,
            rusqlite::ffi::SQLITE_CONSTRAINT,
        ] {
            let error = sql("writing a changeset", failure(code));
            assert_eq!(error.damage(), None, "{code}: {error:?}");
            assert!(matches!(error, StoreError::Sql { .. }), "{code}: {error:?}");
        }
    }

    /// A changeset entry's refusal is the entry's own, and the position it sat
    /// at does not change what it says: an increment that met a corrupt page on
    /// one entry met damage, not a refused write.
    #[test]
    fn damage_inside_a_changeset_entry_is_read_through_the_entry() {
        let inner = sql(
            "writing a changeset",
            failure(rusqlite::ffi::SQLITE_CORRUPT),
        );
        let wrapped = StoreError::Entry {
            index: 49_999,
            path: "notes/note.md".to_string(),
            problem: Box::new(inner),
        };
        assert!(wrapped.damage().is_some(), "{wrapped:?}");

        let sound = StoreError::Entry {
            index: 0,
            path: "notes/note.md".to_string(),
            problem: Box::new(StoreError::Bound {
                what: "a changeset",
                limit: 1,
                given: 2,
            }),
        };
        assert_eq!(sound.damage(), None, "{sound:?}");
    }
}
