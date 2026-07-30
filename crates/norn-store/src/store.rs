//! The store itself: one database file, one connection, and the lifecycle of
//! both.
//!
//! # One connection, and the writer is the borrow checker
//!
//! A store holds exactly one connection, and every operation runs through a
//! request that borrows the store mutably. That is the whole of "one writer per
//! entry": there is no pool here, no internal lock, and no second connection to
//! serialize against. A host that serves one vault entry from several threads
//! serializes at the application level, which is where it already decides what
//! a request is.
//!
//! # Write-ahead logging, foreign keys, and why they are set in one place
//!
//! Both are **per-connection** settings in SQLite rather than properties of the
//! file — foreign keys are off by default in every new connection — and the
//! cascade that carries wholesale row replacement depends on them being on. One
//! function opens every connection this crate ever holds, so there is no
//! reading of the schema under settings the schema was not designed for.
//!
//! # The database-side heal rung
//!
//! Rung 3 is *discard and rebuild*, and this is where it lives. An open reads
//! the store schema version and the DDL fingerprint out of `meta`, and rebuilds
//! from zero when either disagrees with this build or when the file is not a
//! database at all. The rebuild is the whole file: it is removed, along with the
//! write-ahead log and shared-memory sidecars, and created again from the
//! statement list.
//!
//! **Rung 3 is for damaged state, never for a hostile environment.** A full
//! disk, a revoked permission, a parent directory that cannot be created: each
//! of those is refused and reported, and none of them discards anything.
//! Refusing is the correct resolution when the environment is broken and the
//! stored state is not, because discarding a sound database destroys work to fix
//! nothing.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, ToSql};

use crate::ddl;
use crate::error::{self, StoreError};
use crate::request::Request;

/// How long a connection waits on a lock before reporting the database busy.
///
/// Serialization is the host's job, so contention here means two processes hold
/// the same vault — which the per-vault file lock is what prevents. The timeout
/// is the backstop that turns a race into a wait rather than an error.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Whether the store's file outlives the store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreMode {
    /// A registered vault's durable derived state. The file survives the
    /// process.
    Durable,
    /// Disposable derivation over a store that is torn down when it is closed
    /// or dropped. This is what an unregistered root gets: derived state with no
    /// promise of being there next time.
    Throwaway,
}

/// How an open ended up with the database it handed back.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpenOutcome {
    /// There was no database, so one was created.
    Created,
    /// The database was the shape this build writes, and was opened as it stood.
    Reused,
    /// The database was not usable and was rebuilt from zero — heal rung 3.
    RebuiltFromZero(RebuildReason),
}

/// Why a database was discarded and rebuilt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RebuildReason {
    /// The DDL fingerprint differs from this build's, which pre-release means
    /// the DDL was edited. It consumes no version number: the store schema
    /// version is pinned, and a development-time shape change is resolved by
    /// rebuilding rather than by minting a version.
    DdlFingerprint {
        expected: String,
        found: Option<String>,
    },
    /// The store schema version is not the one this build pins.
    StoreSchemaVersion { expected: i64, found: Option<i64> },
    /// The file is not a store this build wrote: not a database, corrupt pages,
    /// or a schema that carries no `meta` table to ask.
    Damaged { detail: String },
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
                    "the store schema version is {found} and this build pins {expected}"
                ),
                None => write!(f, "the store schema version is absent"),
            },
            RebuildReason::Damaged { detail } => write!(f, "{detail}"),
        }
    }
}

/// One vault's derived state.
#[derive(Debug)]
pub struct Store {
    pub(crate) connection: Connection,
    path: PathBuf,
    mode: StoreMode,
    outcome: OpenOutcome,
    torn_down: bool,
}

impl Store {
    /// Open, or create, the durable store at `path`.
    ///
    /// The parent directory is prepared if it is missing. A database that is not
    /// the shape this build writes is rebuilt from zero, and
    /// [`Store::open_outcome`] says whether that happened and why.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_in_mode(path.as_ref(), StoreMode::Durable)
    }

    /// Open, or create, a throwaway store at `path`.
    ///
    /// The same open in every respect but one: the file is removed when the
    /// store is closed or dropped, so an unregistered root's derived state does
    /// not accumulate on disk.
    pub fn open_throwaway(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_in_mode(path.as_ref(), StoreMode::Throwaway)
    }

    fn open_in_mode(path: &Path, mode: StoreMode) -> Result<Self, StoreError> {
        prepare_parent(path)?;
        let (connection, outcome) = match connect(path)? {
            Attempt::Connected(connection) => match inspect(&connection)? {
                Verdict::Fresh => {
                    create(&connection)?;
                    (connection, OpenOutcome::Created)
                }
                Verdict::Usable => (connection, OpenOutcome::Reused),
                Verdict::Rebuild(reason) => {
                    drop(connection);
                    (rebuild(path)?, OpenOutcome::RebuiltFromZero(reason))
                }
            },
            Attempt::Rebuild(reason) => (rebuild(path)?, OpenOutcome::RebuiltFromZero(reason)),
        };
        Ok(Store {
            connection,
            path: path.to_path_buf(),
            mode,
            outcome,
            torn_down: false,
        })
    }

    /// The database file this store is holding.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn mode(&self) -> StoreMode {
        self.mode
    }

    /// How the open ended up with this database.
    pub fn open_outcome(&self) -> &OpenOutcome {
        &self.outcome
    }

    /// Open a request. Everything the store does happens inside one, so that
    /// every derivation is attributable to the request that caused it.
    pub fn begin_request(&mut self) -> Request<'_> {
        Request::new(self)
    }

    /// Check the database against itself, and report the first way it is not
    /// consistent.
    ///
    /// Four checks, because a store has four kinds of consistency to lose: the
    /// pages themselves, the foreign keys that carry cascade deletion, the
    /// full-text index against the column it is an index of, and the
    /// frontmatter projection against being JSON at all. The third is what an
    /// external-content FTS5 table can lose without anything else noticing,
    /// which is exactly why the index is maintained by triggers; the fourth is
    /// the projection's own claim, checked by the JSON1 reader that will be
    /// asked to query it.
    ///
    /// This is a maintenance act rather than a request: it derives nothing, so
    /// it moves no counter.
    pub fn verify_integrity(&self) -> Result<(), StoreError> {
        let report: String = self
            .connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(|error| error::sql("checking database integrity", error))?;
        if report != "ok" {
            return Err(StoreError::Damaged { what: report });
        }

        let orphans: i64 = self
            .connection
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .map_err(|error| error::sql("checking foreign keys", error))?;
        if orphans != 0 {
            return Err(StoreError::Damaged {
                what: format!("{orphans} rows reference a document that is not there"),
            });
        }

        self.connection
            .execute_batch("INSERT INTO documents_fts(documents_fts) VALUES ('integrity-check')")
            .map_err(|error| StoreError::Damaged {
                what: format!(
                    "the full-text index disagrees with the documents it indexes: {error}"
                ),
            })?;

        let unreadable: i64 = self
            .connection
            .query_row(
                "SELECT count(*) FROM documents
                 WHERE frontmatter IS NOT NULL AND json_valid(frontmatter) = 0",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error::sql("checking the frontmatter projection", error))?;
        if unreadable != 0 {
            return Err(StoreError::Damaged {
                what: format!(
                    "{unreadable} documents carry a frontmatter projection that is not JSON"
                ),
            });
        }
        Ok(())
    }

    /// What store schema this database records having been written under.
    ///
    /// The pair an open compares against this build. A doctor reports it; the
    /// open path reads it before it trusts any other table.
    pub fn recorded_store_schema(&self) -> Result<(Option<i64>, Option<String>), StoreError> {
        Ok((
            get_meta(&self.connection, ddl::meta::STORE_SCHEMA_VERSION)?,
            get_meta(&self.connection, ddl::meta::DDL_FINGERPRINT)?,
        ))
    }

    /// Record the store schema this database is to be understood as carrying.
    ///
    /// Creating a database writes this pair, and it is what the next open
    /// compares against the build that performs it. Two other callers are
    /// expected, and both need it to take its values rather than assume them:
    /// a migration, once version 1 has frozen as the migratable baseline, has
    /// to record the shape it migrated *to*; and the induced-failure suite has
    /// to arrange a database whose recorded shape is not this build's, which is
    /// the only way to reach rung 3's fingerprint trigger from outside — no
    /// other crate may open a connection to arrange one.
    ///
    /// Recording a pair this build did not produce is not a corruption of
    /// anything: the next open reads it, disagrees, and rebuilds from zero,
    /// which is precisely the resolution the pair exists to trigger.
    pub fn record_store_schema(
        &mut self,
        version: i64,
        fingerprint: &str,
    ) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| error::sql("opening the store schema transaction", error))?;
        put_meta(&transaction, ddl::meta::STORE_SCHEMA_VERSION, version)?;
        put_meta(&transaction, ddl::meta::DDL_FINGERPRINT, fingerprint)?;
        transaction
            .commit()
            .map_err(|error| error::sql("recording the store schema", error))
    }

    /// Close the store, tearing a throwaway one down.
    ///
    /// A throwaway store tears itself down when dropped as well; this is the
    /// same teardown with its failures reported rather than swallowed.
    pub fn close(mut self) -> Result<(), StoreError> {
        if self.mode == StoreMode::Throwaway {
            remove_database(&self.path)?;
            self.torn_down = true;
        }
        Ok(())
    }

    /// Discard the database entirely — heal rung 3, reached deliberately.
    ///
    /// The store is consumed, the file and its sidecars are removed, and the
    /// caller opens again to get a database built from the statement list. The
    /// open path reaches this by itself for a store schema that disagrees with
    /// this build; this is the entry point for damage found later, which the
    /// lower rungs cannot resolve.
    pub fn discard(mut self) -> Result<(), StoreError> {
        remove_database(&self.path)?;
        self.torn_down = true;
        Ok(())
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if self.mode == StoreMode::Throwaway && !self.torn_down {
            // Best effort: a drop has nobody to report to, and the alternative
            // to a silent failure here is a temporary database that outlives
            // every process that could have removed it. `close` is the spelling
            // that reports.
            let _ = remove_database(&self.path);
        }
    }
}

/// What connecting to a path produced.
enum Attempt {
    Connected(Connection),
    /// The file is there and is not a database this build can read.
    Rebuild(RebuildReason),
}

/// What inspecting a connected database concluded.
enum Verdict {
    /// No schema at all: a new file, or an empty one.
    Fresh,
    /// The shape this build writes.
    Usable,
    Rebuild(RebuildReason),
}

/// Open a connection and put it in the state the store schema is designed for.
///
/// Every connection this crate holds comes from here. The journal mode is read
/// back rather than assumed, because a database that refuses write-ahead logging
/// is not a database this store can keep its promises on.
fn connect(path: &Path) -> Result<Attempt, StoreError> {
    let connection = Connection::open(path).map_err(|error| StoreError::Lifecycle {
        operation: "opening the database",
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| error::sql("setting the busy timeout", error))?;

    let journal: String =
        match connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0)) {
            Ok(mode) => mode,
            Err(error) => return rebuild_or_fail(error).map(Attempt::Rebuild),
        };
    if !journal.eq_ignore_ascii_case("wal") {
        return Err(StoreError::Damaged {
            what: format!("the database refused write-ahead logging and reports `{journal}`"),
        });
    }
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(|error| error::sql("setting the synchronous mode", error))?;
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(|error| error::sql("turning foreign keys on", error))?;
    Ok(Attempt::Connected(connection))
}

/// Read the store schema this database carries, and decide whether it is the
/// one this build writes.
fn inspect(connection: &Connection) -> Result<Verdict, StoreError> {
    let objects: i64 =
        match connection.query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0)) {
            Ok(count) => count,
            Err(error) => return rebuild_or_fail(error).map(Verdict::Rebuild),
        };
    if objects == 0 {
        return Ok(Verdict::Fresh);
    }

    let expected_fingerprint = ddl::fingerprint();
    let has_meta: i64 = match connection.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name = 'meta'",
        [],
        |row| row.get(0),
    ) {
        Ok(count) => count,
        Err(error) => return rebuild_or_fail(error).map(Verdict::Rebuild),
    };
    if has_meta == 0 {
        return Ok(Verdict::Rebuild(RebuildReason::Damaged {
            detail: "the database holds tables and no `meta` table, so it is not a store this \
                     build wrote"
                .to_string(),
        }));
    }

    let version: Option<i64> = match get_meta(connection, ddl::meta::STORE_SCHEMA_VERSION) {
        Ok(version) => version,
        Err(error) => return rebuild_or_fail_store(error).map(Verdict::Rebuild),
    };
    if version != Some(ddl::STORE_SCHEMA_VERSION) {
        return Ok(Verdict::Rebuild(RebuildReason::StoreSchemaVersion {
            expected: ddl::STORE_SCHEMA_VERSION,
            found: version,
        }));
    }

    let found: Option<String> = match get_meta(connection, ddl::meta::DDL_FINGERPRINT) {
        Ok(found) => found,
        Err(error) => return rebuild_or_fail_store(error).map(Verdict::Rebuild),
    };
    if found.as_deref() != Some(expected_fingerprint.as_str()) {
        return Ok(Verdict::Rebuild(RebuildReason::DdlFingerprint {
            expected: expected_fingerprint,
            found,
        }));
    }
    Ok(Verdict::Usable)
}

/// Whether a driver error met while reading the store schema says the database
/// is not one this build wrote, or says the environment is broken.
///
/// A value in `meta` that will not convert counts as the former: the column's
/// contents are part of the shape, so text where an integer belongs means the
/// file was written by something else.
fn rebuild_or_fail(error: rusqlite::Error) -> Result<RebuildReason, StoreError> {
    let unreadable = matches!(
        error,
        rusqlite::Error::InvalidColumnType(..)
            | rusqlite::Error::FromSqlConversionFailure(..)
            | rusqlite::Error::IntegralValueOutOfRange(..)
    );
    if error::is_damaged(&error) || unreadable {
        Ok(RebuildReason::Damaged {
            detail: error.to_string(),
        })
    } else {
        Err(error::sql("reading the store schema", error))
    }
}

/// [`rebuild_or_fail`] for an error that has already been mapped onto the
/// store's own error type, which is what the `meta` readers hand back.
fn rebuild_or_fail_store(error: StoreError) -> Result<RebuildReason, StoreError> {
    match &error {
        StoreError::Sql { message, .. } => Ok(RebuildReason::Damaged {
            detail: message.clone(),
        }),
        _ => Err(error),
    }
}

/// Remove the database and create it again from the statement list.
fn rebuild(path: &Path) -> Result<Connection, StoreError> {
    remove_database(path)?;
    match connect(path)? {
        Attempt::Connected(connection) => {
            create(&connection)?;
            Ok(connection)
        }
        Attempt::Rebuild(reason) => Err(StoreError::Damaged {
            what: format!("a database rebuilt from zero is still not readable: {reason}"),
        }),
    }
}

/// Run the whole statement list, and record what was run.
///
/// One transaction: a half-created store schema is a store schema nothing can
/// inspect, and the fingerprint written at the end is what says the list
/// finished.
fn create(connection: &Connection) -> Result<(), StoreError> {
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| error::sql("opening the store schema transaction", error))?;
    for statement in ddl::statements() {
        transaction
            .execute_batch(statement)
            .map_err(|error| StoreError::Sql {
                operation: "creating the store schema",
                message: format!(
                    "`{}`: {error}",
                    statement.lines().next().unwrap_or(statement)
                ),
            })?;
    }
    put_meta(
        &transaction,
        ddl::meta::STORE_SCHEMA_VERSION,
        ddl::STORE_SCHEMA_VERSION,
    )?;
    put_meta(&transaction, ddl::meta::DDL_FINGERPRINT, ddl::fingerprint())?;
    put_meta(&transaction, ddl::meta::WRITE_GENERATION, 0_i64)?;
    transaction
        .commit()
        .map_err(|error| error::sql("committing the store schema", error))
}

/// Write one pinned scalar.
pub(crate) fn put_meta(
    connection: &Connection,
    key: &str,
    value: impl ToSql,
) -> Result<(), StoreError> {
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
pub(crate) fn get_meta<T: rusqlite::types::FromSql>(
    connection: &Connection,
    key: &str,
) -> Result<Option<T>, StoreError> {
    connection
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            rusqlite::params![key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error::sql("reading a pinned value", error))
}

/// The next write generation, taken inside whatever transaction is open.
///
/// Taking it is the same act as recording it, so the counter cannot be read by
/// one write and used by another: the update and the read are one statement.
pub(crate) fn next_generation(connection: &Connection) -> Result<i64, StoreError> {
    connection
        .query_row(
            "UPDATE meta SET value = value + 1 WHERE key = ?1 RETURNING value",
            rusqlite::params![ddl::meta::WRITE_GENERATION],
            |row| row.get(0),
        )
        .map_err(|error| error::sql("taking the next write generation", error))
}

/// Prepare the directory the database file sits in.
///
/// A failure here is environmental — a permission, a path that is a file — so it
/// is reported and nothing is discarded.
#[allow(clippy::disallowed_methods)] // The derived database's own directory; the store owns its file lifecycle.
fn prepare_parent(path: &Path) -> Result<(), StoreError> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    std::fs::create_dir_all(parent).map_err(|error| StoreError::Lifecycle {
        operation: "preparing the database directory",
        path: parent.to_path_buf(),
        message: error.to_string(),
    })
}

/// Remove the database and the sidecars write-ahead logging leaves beside it.
///
/// All three, because a rebuilt database beside a stale write-ahead log is a
/// database with somebody else's committed pages in it. A file that is already
/// gone is not a failure.
#[allow(clippy::disallowed_methods)] // Removing the derived database at heal rung 3, and tearing a throwaway store down.
fn remove_database(path: &Path) -> Result<(), StoreError> {
    for candidate in [
        path.to_path_buf(),
        sidecar(path, "-wal"),
        sidecar(path, "-shm"),
    ] {
        match std::fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(StoreError::Lifecycle {
                    operation: "removing the database",
                    path: candidate,
                    message: error.to_string(),
                });
            }
        }
    }
    Ok(())
}

/// The path SQLite writes a sidecar to: the database's own path with a suffix
/// appended to the file name.
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}
