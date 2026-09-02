//! The open ceremony: connect, judge what is there, and create, adopt or
//! rebuild it.
//!
//! Every database derived over this crate is opened the same way, and the way
//! is here. A client hands over the statement list it owns and the version it
//! pins; what comes back is a connection and [`OpenOutcome`] — created,
//! reused, or rebuilt from zero with a typed [`RebuildReason`]. The DDL
//! fingerprint is taken from the list here rather than handed in beside it.
//!
//! # Two verdicts, each taken where it can be taken
//!
//! **The mechanics verdict is this crate's.** Five readings answer whether the
//! file is a database this build wrote at all: it holds a `meta` table, the
//! schema version is the pinned one, the DDL fingerprint is this statement
//! list's, the schema digest is the one create recorded, and it carries an
//! epoch. Those are facts about the machinery every client shares,
//! so one reading of them serves every client and every client's disagreement
//! reads as the same typed reason.
//!
//! The digest is what the fingerprint cannot answer. A fingerprint is compared
//! against a value the database reports about *itself*, so a dropped index,
//! table or trigger leaves it matching — and an index a client's writes depend
//! on for uniqueness leaves duplicate rows behind when it goes. The schema
//! digest is taken over `sqlite_schema`, so it is a statement about what is
//! actually there.
//!
//! The epoch is not about shape at all. Create mints one, so a database that
//! agrees about every part of its shape and records none was written by
//! something else — and it is what every consumer's record of progress is keyed
//! by, so an open that adopted it would let a cursor into a discarded database
//! read as a position in this one.
//!
//! **The verdict over a client's own keys is the client's**, taken through
//! [`Client::adopt`] once the mechanics call the database usable. Whether a
//! file outlives its handle, which model a projection was computed under: what
//! those keys mean is not knowable here, so the client answers keep, rebuild
//! with a detail, or refuse.
//!
//! # Damage is resolved; a broken environment is refused
//!
//! Every reading an open performs goes through [`crate::damage_or_fail`], so a
//! busy database, a revoked permission or an I/O error is reported as the
//! refused operation it was rather than resolved by discarding a sound
//! database. Only a file whose own contents are wrong is a rebuild.
//!
//! # A rebuild is the whole file
//!
//! The database is removed along with the sidecars a journal leaves beside it,
//! connected again, and created from the statement list — which mints a new
//! epoch, because create is what mints one. Progress a consumer recorded
//! against the old epoch names a database that no longer exists.

use std::fmt;
use std::path::Path;

use rusqlite::Connection;

use crate::database::{Attempt, connect, mint_an_epoch, prepare_parent, remove_database};
use crate::error::{self, DbError};
use crate::meta;
use crate::schema::schema_digest;

/// The five acts a ceremony can be refused at, each named for one schema.
///
/// A refusal names the act it refused, because a bare driver message says
/// which constraint failed and never which write. The labels are `&'static
/// str` for the same reason [`DbError::Sql`] carries one: an operation is
/// named from a fixed vocabulary rather than built out of data.
///
/// **[`crate::schema_operations`] is the only spelling.** The fields are
/// private and the macro builds all five from one noun, so a set cannot name
/// five different things or leave one act labelled for another schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Operations {
    /// Reading one of the pinned scalars the mechanics verdict is taken over.
    pub(crate) reading: &'static str,
    /// Opening the transaction the whole statement list runs inside.
    pub(crate) opening_the_transaction: &'static str,
    /// Running one statement of the list.
    pub(crate) creating: &'static str,
    /// Digesting the schema the statements left behind.
    pub(crate) digesting: &'static str,
    /// Committing the schema the list created.
    pub(crate) committing: &'static str,
}

impl Operations {
    /// The constructor [`crate::schema_operations`] expands to.
    ///
    /// It is public because the macro expands in the client's crate, and it is
    /// hidden because the macro is the spelling a client uses: five labels
    /// passed by hand can name five different nouns.
    #[doc(hidden)]
    pub const fn spelled(
        reading: &'static str,
        opening_the_transaction: &'static str,
        creating: &'static str,
        digesting: &'static str,
        committing: &'static str,
    ) -> Self {
        Operations {
            reading,
            opening_the_transaction,
            creating,
            digesting,
            committing,
        }
    }
}

/// The operation labels of one schema, spelled from the noun that names it.
///
/// `norn_db::schema_operations!("store schema")` reads as "creating the store
/// schema" at the statement that failed, "committing the store schema" at the
/// commit, and so on. One noun spells all five, and it is the only way to make
/// an [`Operations`], so no two of them can drift.
#[macro_export]
macro_rules! schema_operations {
    ($noun:literal) => {
        $crate::Operations::spelled(
            concat!("reading the ", $noun),
            concat!("opening the ", $noun, " transaction"),
            concat!("creating the ", $noun),
            concat!("digesting the ", $noun),
            concat!("committing the ", $noun),
        )
    };
}

/// What a client hands the ceremony: the schema it writes.
pub struct Schema {
    /// What a refusal met while running this schema's ceremony names.
    pub operations: Operations,
    /// The version this build pins, recorded under
    /// [`meta::STORE_SCHEMA_VERSION`] and compared at every open.
    pub version: i64,
    /// The statement list, run in order inside one transaction.
    pub statements: Vec<String>,
}

/// The DDL fingerprint of a statement list: what a create records under
/// [`meta::DDL_FINGERPRINT`] and what every later open compares against.
///
/// It is taken from the list rather than handed in beside it, so the recorded
/// value and the statements that produced the database cannot say different
/// things.
fn fingerprint(schema: &Schema) -> String {
    crate::schema::digest(schema.statements.iter().map(String::as_str))
}

/// Why a database was discarded and rebuilt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RebuildReason {
    /// The DDL fingerprint differs from this build's, which pre-release means
    /// the DDL was edited. It consumes no version number: the schema version
    /// is pinned, and a development-time shape change is resolved by rebuilding
    /// rather than by minting a version.
    DdlFingerprint {
        expected: String,
        found: Option<String>,
    },
    /// The schema version recorded under [`meta::STORE_SCHEMA_VERSION`] is not
    /// the one this build pins.
    StoreSchemaVersion { expected: i64, found: Option<i64> },
    /// The file is not a database this build wrote: not a database at all,
    /// corrupt pages, a schema that carries no `meta` table to ask, or a
    /// schema that no longer holds what the statement list created.
    Damaged { detail: String },
    /// The client judged the database it was offered, and asked for a fresh
    /// one. The detail is the client's own: the mechanics agreed about every
    /// part of the shape.
    Client { detail: String },
}

impl fmt::Display for RebuildReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RebuildReason::DdlFingerprint { expected, found } => write!(
                f,
                "the DDL fingerprint is {} and this build writes {expected}",
                found.as_deref().unwrap_or("absent")
            ),
            RebuildReason::StoreSchemaVersion { expected, found } => match found {
                Some(found) => write!(
                    f,
                    "the schema version is {found} and this build pins {expected}"
                ),
                None => write!(f, "the schema version is absent"),
            },
            RebuildReason::Damaged { detail } => write!(f, "{detail}"),
            RebuildReason::Client { detail } => write!(f, "{detail}"),
        }
    }
}

/// How an open ended up with the database it handed back.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpenOutcome {
    /// There was no database, so one was created.
    Created,
    /// The database was the shape this build writes, and was opened as it
    /// stood.
    Reused,
    /// The database was not usable and was rebuilt from zero.
    RebuiltFromZero(RebuildReason),
}

/// What a client makes of the database the mechanics called usable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Adoption {
    /// Take it as it stands, with everything in it.
    Keep,
    /// Remove it and create from zero. The detail says what the client found.
    Rebuild { detail: String },
}

/// The crate that owns what a statement list means.
///
/// The ceremony calls back into it twice: once inside the create transaction,
/// to record the keys the client pins, and once after the mechanics verdict,
/// to take the verdict over those keys.
pub trait Client {
    /// The refusal this client reports. Every substrate refusal crosses into
    /// it, so a client maps the driver seam onto its own vocabulary once.
    type Error: From<DbError>;

    /// Write the client's own pinned scalars, inside the create transaction:
    /// the mechanics keys are already written, and the commit that says the
    /// list finished has not run.
    fn record(&self, transaction: &Connection) -> Result<(), Self::Error>;

    /// Judge the client's own keys in a database the mechanics call usable —
    /// keep it, rebuild it, or refuse it.
    fn adopt(&self, connection: &Connection, path: &Path) -> Result<Adoption, Self::Error>;

    /// The error a statement of the list meets where an arrangement armed one.
    ///
    /// A creation that cannot be made to fail is a creation whose error path
    /// is never exercised, and arranging one on disk reaches nothing: a create
    /// writes over whatever was there. The arm and the surface that arms it
    /// belong to the client, because a suite arms in the vocabulary of what it
    /// is testing. A client that arms nothing answers `None`.
    fn armed_failure(&self, _connection: &Connection) -> Option<rusqlite::Error> {
        None
    }
}

/// Open, or create, the database at `path` under `schema`.
///
/// The parent directory is prepared if it is missing. A database that is not
/// the shape this build writes, or that the client declines to adopt, is
/// rebuilt from zero, and the outcome says whether that happened and why.
pub fn open<C: Client>(
    path: &Path,
    schema: &Schema,
    client: &C,
) -> Result<(Connection, OpenOutcome), C::Error> {
    prepare_parent(path)?;
    match connect(path)? {
        Attempt::Connected(connection) => match inspect(&connection, schema)? {
            Verdict::Fresh => {
                create(&connection, schema, client)?;
                Ok((connection, OpenOutcome::Created))
            }
            Verdict::Usable => match client.adopt(&connection, path)? {
                Adoption::Keep => Ok((connection, OpenOutcome::Reused)),
                Adoption::Rebuild { detail } => {
                    drop(connection);
                    Ok((
                        rebuild(path, schema, client)?,
                        OpenOutcome::RebuiltFromZero(RebuildReason::Client { detail }),
                    ))
                }
            },
            Verdict::Rebuild(reason) => {
                drop(connection);
                Ok((
                    rebuild(path, schema, client)?,
                    OpenOutcome::RebuiltFromZero(reason),
                ))
            }
        },
        Attempt::Unreadable { detail } => Ok((
            rebuild(path, schema, client)?,
            OpenOutcome::RebuiltFromZero(RebuildReason::Damaged { detail }),
        )),
    }
}

/// Remove the database and create it again from the statement list.
///
/// [`open`] reaches this for itself where the file it met is not one this
/// build wrote. It is public for the other route to the same act: a client
/// discarding a database deliberately, over damage an open cannot see.
pub fn rebuild<C: Client>(
    path: &Path,
    schema: &Schema,
    client: &C,
) -> Result<Connection, C::Error> {
    remove_database(path)?;
    match connect(path)? {
        Attempt::Connected(connection) => {
            create(&connection, schema, client)?;
            Ok(connection)
        }
        Attempt::Unreadable { detail } => Err(C::Error::from(DbError::Damaged {
            what: format!("a database rebuilt from zero is still not readable: {detail}"),
        })),
    }
}

/// What inspecting a connected database concluded.
enum Verdict {
    /// No schema at all: a new file, or an empty one.
    Fresh,
    /// The shape this build writes.
    Usable,
    Rebuild(RebuildReason),
}

/// Read what this database records about the schema it holds, and decide
/// whether it is the one this build writes.
///
/// Every read here fails the same way: through [`rebuild_or_fail`], so one
/// policy decides whether a driver error describes a damaged file or a broken
/// environment. That matters most for the `meta` reads — a busy database, a
/// revoked permission or an I/O error on one of them is not evidence that the
/// stored state is wrong.
fn inspect(connection: &Connection, schema: &Schema) -> Result<Verdict, DbError> {
    let reading = schema.operations.reading;
    let objects: i64 =
        match connection.query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0)) {
            Ok(count) => count,
            Err(error) => return rebuild_or_fail(reading, error).map(Verdict::Rebuild),
        };
    if objects == 0 {
        return Ok(Verdict::Fresh);
    }

    let has_meta: i64 = match connection.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name = 'meta'",
        [],
        |row| row.get(0),
    ) {
        Ok(count) => count,
        Err(error) => return rebuild_or_fail(reading, error).map(Verdict::Rebuild),
    };
    if has_meta == 0 {
        return Ok(Verdict::Rebuild(RebuildReason::Damaged {
            detail: "the database holds tables and no `meta` table, so it is not a database \
                     this build wrote"
                .to_string(),
        }));
    }

    let version: Option<i64> = match meta::read_meta(connection, meta::STORE_SCHEMA_VERSION) {
        Ok(version) => version,
        Err(error) => return rebuild_or_fail(reading, error).map(Verdict::Rebuild),
    };
    if version != Some(schema.version) {
        return Ok(Verdict::Rebuild(RebuildReason::StoreSchemaVersion {
            expected: schema.version,
            found: version,
        }));
    }

    let found: Option<String> = match meta::read_meta(connection, meta::DDL_FINGERPRINT) {
        Ok(found) => found,
        Err(error) => return rebuild_or_fail(reading, error).map(Verdict::Rebuild),
    };
    let expected = fingerprint(schema);
    if found.as_deref() != Some(expected.as_str()) {
        return Ok(Verdict::Rebuild(RebuildReason::DdlFingerprint { expected, found }));
    }

    let recorded: Option<String> = match meta::read_meta(connection, meta::SCHEMA_DIGEST) {
        Ok(recorded) => recorded,
        Err(error) => return rebuild_or_fail(reading, error).map(Verdict::Rebuild),
    };
    let holds = match schema_digest(connection) {
        Ok(digest) => digest,
        Err(error) => return rebuild_or_fail(reading, error).map(Verdict::Rebuild),
    };
    if recorded.as_deref() != Some(holds.as_str()) {
        return Ok(Verdict::Rebuild(RebuildReason::Damaged {
            detail: format!(
                "the database holds a schema digesting {holds} and it was created holding {}, so \
                 an object it was created with has been changed or removed",
                recorded.as_deref().unwrap_or("nothing recorded")
            ),
        }));
    }

    // The epoch is the last gate because it is the one the three above cannot
    // ask. Create writes it, so a database that agrees with this build about
    // every part of its shape and records none was written by something
    // else — and it is the reading a consumer's whole record of progress is
    // keyed by, so an open that handed it back absent would let a cursor from
    // a discarded database read as a position in this one.
    let epoch: Option<String> = match meta::read_meta(connection, meta::STORE_EPOCH) {
        Ok(epoch) => epoch,
        Err(error) => return rebuild_or_fail(reading, error).map(Verdict::Rebuild),
    };
    if epoch.is_none() {
        return Ok(Verdict::Rebuild(RebuildReason::Damaged {
            detail: "the database records no store epoch, so it is not a database this build \
                     wrote"
                .to_string(),
        }));
    }
    Ok(Verdict::Usable)
}

/// Which rung a driver error met while reading a database's pinned state
/// belongs to: a rebuild reason where the file's own contents are wrong, or
/// the refused operation the environment made of it.
fn rebuild_or_fail(
    operation: &'static str,
    error: rusqlite::Error,
) -> Result<RebuildReason, DbError> {
    error::damage_or_fail(operation, error).map(|detail| RebuildReason::Damaged { detail })
}

/// Run the whole statement list, and record what was run.
///
/// One transaction: a half-created schema is a schema nothing can inspect, and
/// the pinned values written at the end are what say the list finished. The
/// schema digest is taken after the statements ran, so it is a reading of the
/// database rather than a second reading of the list.
fn create<C: Client>(connection: &Connection, schema: &Schema, client: &C) -> Result<(), C::Error> {
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| error::sql(schema.operations.opening_the_transaction, error))?;
    for statement in &schema.statements {
        let ran = match client.armed_failure(connection) {
            Some(error) => Err(error),
            None => transaction.execute_batch(statement),
        };
        ran.map_err(|error| error::sql_at_statement(schema.operations.creating, statement, error))?;
    }
    let digest = schema_digest(&transaction)
        .map_err(|error| error::sql(schema.operations.digesting, error))?;
    meta::put_meta(&transaction, meta::STORE_SCHEMA_VERSION, schema.version)?;
    meta::put_meta(
        &transaction,
        meta::DDL_FINGERPRINT,
        fingerprint(schema).as_str(),
    )?;
    meta::put_meta(&transaction, meta::SCHEMA_DIGEST, digest)?;
    client.record(&transaction)?;
    meta::put_meta(
        &transaction,
        meta::STORE_EPOCH,
        mint_an_epoch(&transaction)?,
    )?;
    transaction
        .commit()
        .map_err(|error| error::sql(schema.operations.committing, error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(code: i32) -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)
    }

    /// The damage policy an open reads every value through. Only a code about
    /// the file's own contents authorizes discarding it.
    #[test]
    fn only_a_corrupt_file_is_a_reason_to_rebuild() {
        for code in [
            rusqlite::ffi::SQLITE_CORRUPT,
            // The extended code FTS5 reports a failed integrity check as.
            rusqlite::ffi::SQLITE_CORRUPT | (1 << 8),
            rusqlite::ffi::SQLITE_NOTADB,
        ] {
            let verdict =
                rebuild_or_fail("reading the store schema", failure(code)).expect("a reason");
            assert!(matches!(verdict, RebuildReason::Damaged { .. }), "{code}");
        }
        for code in [
            rusqlite::ffi::SQLITE_BUSY,
            rusqlite::ffi::SQLITE_READONLY,
            rusqlite::ffi::SQLITE_IOERR,
            rusqlite::ffi::SQLITE_LOCKED,
            rusqlite::ffi::SQLITE_PERM,
        ] {
            let error = rebuild_or_fail("reading the store schema", failure(code))
                .expect_err("an environment failure is reported, never resolved by a rebuild");
            assert!(matches!(error, DbError::Sql { .. }), "{code}: {error:?}");
        }
    }

    /// The typed reasons are what a caller reports, so each of them says what
    /// disagreed and what this build holds.
    #[test]
    fn every_reason_names_what_disagreed() {
        assert_eq!(
            RebuildReason::DdlFingerprint {
                expected: "abc".to_string(),
                found: None,
            }
            .to_string(),
            "the DDL fingerprint is absent and this build writes abc"
        );
        assert_eq!(
            RebuildReason::StoreSchemaVersion {
                expected: 1,
                found: Some(7),
            }
            .to_string(),
            "the schema version is 7 and this build pins 1"
        );
        assert_eq!(
            RebuildReason::Client {
                detail: "the model moved".to_string(),
            }
            .to_string(),
            "the model moved"
        );
    }
}
