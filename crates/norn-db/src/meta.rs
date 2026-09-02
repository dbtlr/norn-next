//! `meta` — the small pinned scalars an open reads before it trusts anything
//! else.
//!
//! A key/value table, because it is the one table whose shape may never
//! change: the DDL fingerprint is read out of it in order to decide whether
//! the rest of the database is the shape this build writes, so a
//! reader of `meta` cannot rely on any other table's columns being what it
//! expects. Adding a pinned scalar is a new key rather than a new column.
//!
//! `value` is declared `BLOB`, which in SQLite means the column has no
//! affinity and every value keeps the type it was written with: the
//! fingerprint reads back as text, a generation as an integer, and a client's
//! own pinned bytes as the bytes they were read from.
//!
//! # Two kinds of key, and this crate owns one of them
//!
//! **The mechanics keys are here, and the open ceremony owns four of them.**
//! [`crate::open`] writes [`STORE_SCHEMA_VERSION`], [`DDL_FINGERPRINT`],
//! [`SCHEMA_DIGEST`] and [`STORE_EPOCH`] inside the create transaction, and
//! reads all four back at every later open to decide whether the file is a
//! database this build wrote. Every database derived over this crate carries
//! the same four, which is what lets a second client's database be read by the
//! same reasoning as the first's.
//!
//! Two readings sit outside that ceremony and are the substrate's own:
//! [`crate::Database::adopt`] reads [`STORE_EPOCH`] to bind a connection to the
//! lifetime it belongs to, and [`next_generation`] writes [`WRITE_GENERATION`],
//! the counter a client seeds among its own keys where its rows carry
//! generations.
//!
//! **Every other key belongs to the client that writes it.** A key naming what
//! a database was derived under, what it holds a projection of, or whether its
//! file outlives the handle, is the client's own vocabulary: it spells the key,
//! it decides what the value means, and it writes it inside the create
//! transaction through [`crate::Client::record`] and reads it at
//! [`crate::Client::adopt`], through [`put_meta`] and [`get_meta`] like any
//! other pinned scalar.

use rusqlite::{Connection, OptionalExtension, ToSql};

use crate::error::{self, DbError};

/// The statements the `meta` table is made of, for a client to run first in
/// its own statement list: an open reads `meta` to decide whether the rest of
/// the database is trustworthy, so nothing may precede it.
pub fn statements() -> Vec<String> {
    STATEMENTS
        .iter()
        .map(|statement| (*statement).to_string())
        .collect()
}

const STATEMENTS: &[&str] = &["CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value BLOB
) WITHOUT ROWID"];

/// The store schema version this database was created at.
pub const STORE_SCHEMA_VERSION: &str = "store_schema_version";

/// The digest of the statement list this database was created from.
pub const DDL_FINGERPRINT: &str = "ddl_fingerprint";

/// The digest of the schema this database held when it was created.
pub const SCHEMA_DIGEST: &str = "schema_digest";

/// The global write sequence a derived row draws its stamp from.
///
/// It is a **global write sequence**, not a per-derivation one: every
/// derivation in the database draws from it, so two rows anywhere are
/// comparable, and a single derivation touching two tables stamps both with
/// the same number. **Generation orders; a timestamp only informs** — a clock
/// can move backwards and a generation cannot.
///
/// A pinned scalar is not a derivation and takes none: [`put_meta`] writes the
/// value it was handed, so a key a client reconciles at open moves no counter
/// and orders nothing.
pub const WRITE_GENERATION: &str = "write_generation";

/// The identity this database carries from creation to discard.
///
/// A generation orders writes **inside** one database; the epoch says which
/// database those generations belong to, so a consumer that recorded progress
/// against one epoch and reads another knows its position names nothing and
/// rescans. It is minted at create and never rewritten, which is what makes a
/// rebuild from zero a new epoch.
pub const STORE_EPOCH: &str = "store_epoch";

/// Write one pinned scalar.
pub fn put_meta(connection: &Connection, key: &str, value: impl ToSql) -> Result<(), DbError> {
    connection
        .execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )
        .map_err(|error| error::sql("writing a pinned value", error))?;
    Ok(())
}

/// Read one pinned scalar, or `None` where the key is not set.
pub fn get_meta<T: rusqlite::types::FromSql>(
    connection: &Connection,
    key: &str,
) -> Result<Option<T>, DbError> {
    read_meta(connection, key).map_err(|error| error::sql("reading a pinned value", error))
}

/// [`get_meta`] with the driver's own error.
///
/// An open path needs the error rather than a message: whether a failed `meta`
/// read means the database is damaged or the environment is broken decides
/// whether a sound database is discarded, and that decision is one policy over
/// driver error codes — [`crate::damage_or_fail`].
pub fn read_meta<T: rusqlite::types::FromSql>(
    connection: &Connection,
    key: &str,
) -> rusqlite::Result<Option<T>> {
    #[cfg(feature = "induced-failure")]
    if crate::faults::NEXT_META_READ_FAILS.replace(false) {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("database is locked".to_string()),
        ));
    }
    connection
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            rusqlite::params![key],
            |row| row.get(0),
        )
        .optional()
}

/// The next write generation, taken inside whatever transaction is open.
///
/// Taking it is the same act as recording it, so the counter cannot be read by
/// one write and used by another: the update and the read are one statement.
pub fn next_generation(connection: &Connection) -> Result<i64, DbError> {
    connection
        .query_row(
            "UPDATE meta SET value = value + 1 WHERE key = ?1 RETURNING value",
            rusqlite::params![WRITE_GENERATION],
            |row| row.get(0),
        )
        .map_err(|error| error::sql("taking the next write generation", error))
}
