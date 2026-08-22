//! What the substrate refuses, and the one judgment it makes about a refusal.
//!
//! Three shapes, because a database operation fails in three ways that call
//! for different resolutions: the driver refused it, a file-lifecycle step the
//! driver does not cover failed, or the database's own contents are wrong.
//!
//! **The judgment is damaged or not.** Which driver codes describe the file's
//! own contents is a fact about SQLite, and SQLite is this crate's to own — a
//! client reading it off a message would be matching on the driver through a
//! keyhole. So [`sql`] types damage as [`DbError::Damaged`] wherever it is met,
//! and the resolution the verdict authorizes — the database discarded and
//! derived again — stays the client's decision to make.
//!
//! A client maps these onto its own vocabulary. The driver type crosses no
//! public signature above this crate, which is what keeps SQLite result codes
//! out of every API but this one.

use std::fmt;
use std::path::PathBuf;

/// A database operation that did not happen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DbError {
    /// The driver refused an operation. `operation` names what was being done,
    /// because a bare driver message says which constraint failed and never
    /// which write.
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
    /// The database is damaged in a way a reader can state. Discarding and
    /// rebuilding is the resolution, and the client decides when to reach for
    /// it.
    Damaged { what: String },
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::Sql { operation, message } => write!(f, "{operation} failed: {message}"),
            DbError::Lifecycle {
                operation,
                path,
                message,
            } => write!(f, "{operation} on {} failed: {message}", path.display()),
            DbError::Damaged { what } => write!(f, "the database is damaged: {what}"),
        }
    }
}

impl std::error::Error for DbError {}

/// The refusal for a driver error met while `operation` was running.
///
/// **Every driver error this crate reports passes through here, and this is
/// where damage is typed.** A code describing the file's own contents is
/// [`DbError::Damaged`] whichever operation met it — a read, a write, an
/// increment, a verification — because a corrupt page is the same fact about
/// the same file at all of them. Reporting it as the refused operation would
/// leave every caller to re-derive the distinction from a message, and a
/// caller that cannot tell damaged state from a broken environment resolves
/// both the same way: by trying again against a database that will never
/// answer.
pub fn sql(operation: &'static str, error: rusqlite::Error) -> DbError {
    if is_damaged(&error) {
        return DbError::Damaged {
            what: format!("{operation} met a database that is not readable: {error}"),
        };
    }
    DbError::Sql {
        operation,
        message: error.to_string(),
    }
}

/// The refusal for a driver error met while running one statement out of a
/// list, naming the statement it was met at.
///
/// The damage decision is [`sql`]'s and is taken first: a code describing the
/// file's own contents is damage whichever statement met it, so naming the
/// statement is a detail added to a refusal rather than a second policy about
/// what a refusal means.
pub fn sql_at_statement(
    operation: &'static str,
    statement: &str,
    error: rusqlite::Error,
) -> DbError {
    match sql(operation, error) {
        DbError::Sql { operation, message } => DbError::Sql {
            operation,
            message: format!(
                "`{}`: {message}",
                statement.lines().next().unwrap_or(statement)
            ),
        },
        damaged => damaged,
    }
}

/// Whether a driver error says the *database* is damaged, as opposed to saying
/// the environment is.
///
/// One policy, read at two places: an open, which resolves damage by
/// rebuilding before it hands a database back, and [`sql`], which types damage
/// met by every operation afterwards. The distinction decides whether
/// rebuilding from zero is the answer.
/// Discarding a sound database because a disk was full or a permission was
/// revoked would destroy work to fix nothing, so only the codes that describe
/// the file's own contents qualify: it is not a database, or its pages are
/// corrupt.
///
/// Two codes deliberately do **not** qualify. `SQLITE_BUSY` and `SQLITE_LOCKED`
/// say somebody else holds the database, and `SQLITE_SCHEMA` says the schema
/// moved under a prepared statement — each of them describes a moment rather
/// than the file, and each of them is resolved by trying again.
pub fn is_damaged(error: &rusqlite::Error) -> bool {
    use rusqlite::ErrorCode::{DatabaseCorrupt, NotADatabase};
    match error {
        rusqlite::Error::SqliteFailure(failure, _) => {
            matches!(failure.code, DatabaseCorrupt | NotADatabase)
        }
        _ => false,
    }
}

/// Whether a driver error met while reading a database's pinned state says the
/// file is not one this build wrote — reported as the detail a rebuild names —
/// or says the environment is broken, which is a refusal.
///
/// A value in `meta` that will not convert counts as the former: the column's
/// contents are part of the shape, so text where an integer belongs means the
/// file was written by something else.
pub fn damage_or_fail(operation: &'static str, error: rusqlite::Error) -> Result<String, DbError> {
    let unreadable = matches!(
        error,
        rusqlite::Error::InvalidColumnType(..)
            | rusqlite::Error::FromSqlConversionFailure(..)
            | rusqlite::Error::IntegralValueOutOfRange(..)
    );
    if is_damaged(&error) || unreadable {
        Ok(error.to_string())
    } else {
        Err(sql(operation, error))
    }
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
            assert!(
                matches!(error, DbError::Damaged { .. }),
                "{code}: {error:?}"
            );
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
            assert!(matches!(error, DbError::Sql { .. }), "{code}: {error:?}");
        }
    }

    /// A value the pinned-scalar column cannot produce is part of the shape,
    /// so it reads as a file this build did not write rather than as a broken
    /// environment.
    #[test]
    fn a_value_that_will_not_convert_reads_as_a_file_this_build_did_not_write() {
        let detail = damage_or_fail(
            "reading the store schema",
            rusqlite::Error::IntegralValueOutOfRange(0, i64::MAX),
        )
        .expect("a rebuild detail");
        assert!(!detail.is_empty());

        let refused = damage_or_fail(
            "reading the store schema",
            failure(rusqlite::ffi::SQLITE_BUSY),
        )
        .expect_err("an environment failure is reported, never resolved by a rebuild");
        assert!(matches!(refused, DbError::Sql { .. }), "{refused:?}");
    }
}
