//! One database file, one connection, and the lifecycle of both.
//!
//! # Write-ahead logging, foreign keys, and why they are set in one place
//!
//! Both are **per-connection** settings in SQLite rather than properties of the
//! file — foreign keys are off by default in every new connection — and the
//! cascade a client's wholesale row replacement depends on needs them on. One
//! function opens every connection this workspace ever holds, so there is no
//! reading of a schema under settings the schema was not designed for.
//!
//! The open flags are named rather than defaulted, and the one that is left out
//! is the point: **URI filenames are off**. With them on, a path is a
//! mini-language — `file:...?mode=memory` opens a database that is not the file
//! the caller named, and the file-lifecycle operations here would then remove a
//! path nothing was ever written to and report success. A path handed to this
//! crate is a filesystem path, all of it, and the two spellings SQLite treats
//! specially whatever the flags say (`:memory:` and the empty name) are refused
//! as caller errors.
//!
//! # The epoch, and why it is read once
//!
//! [`mint_an_epoch`] draws 128 random bits through the connection a create
//! already holds, and the value goes into `meta` inside the create's own
//! transaction. [`Database::adopt`] reads it back at open and holds it, because
//! it is never rewritten: the value read at open is the value for as long as
//! the handle is open, and a consumer that compares its recorded epoch against
//! the database's before every read should not pay a `meta` query to do it.
//! A database that records none is damage — it was written by something else,
//! and every consumer's record of progress is keyed by the reading.
//!
//! # What this module refuses to decide
//!
//! Whether a database's recorded shape is the one a build writes, and what a
//! disagreement means, are the client's. This module opens, mints, hands back
//! and removes; the verdicts are read one layer up.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};

use crate::error::{self, DbError};
use crate::meta;

/// How long a connection waits on a lock before reporting the database busy.
///
/// Serialization is the caller's job, so contention here means two processes
/// have one database open. The timeout is the backstop that turns a race into
/// a wait rather than an error.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The flags every connection is opened with.
///
/// Named rather than defaulted so that `SQLITE_OPEN_URI` is *absent*: a path is
/// a filesystem path and never a URI with parameters in it.
const OPEN_FLAGS: OpenFlags = OpenFlags::SQLITE_OPEN_READ_WRITE
    .union(OpenFlags::SQLITE_OPEN_CREATE)
    .union(OpenFlags::SQLITE_OPEN_NO_MUTEX);

/// The two names SQLite reads as something other than a file, whatever the URI
/// flag says: the in-memory database, and the anonymous temporary one.
const NOT_A_FILE: &[&str] = &[":memory:", ""];

/// How many compiled statements a connection keeps.
///
/// Compiling SQL is a material share of what a small write costs, and a
/// client's widest write path re-prepares its whole statement set every time it
/// runs. The cache is what makes that a per-connection cost rather than a
/// per-write one, so the capacity is set above the widest path any client has —
/// a changeset that writes a row per table takes more than the driver's default
/// of sixteen — rather than left where the next statement to join one would
/// take that client over.
const PREPARED_STATEMENT_CACHE: usize = 32;

/// What connecting to a path produced.
pub enum Attempt {
    /// The connection is open and in the state a schema is designed to be read
    /// under.
    Connected(Connection),
    /// The file is there and is not a database this build can read. The detail
    /// is what the driver said, for a rebuild to report.
    Unreadable { detail: String },
}

/// An open database, bound to the file it came from and the epoch it records.
#[derive(Debug)]
pub struct Database {
    connection: Connection,
    path: PathBuf,
    epoch: String,
}

impl Database {
    /// Bind an open connection to the file it was opened on, reading the epoch
    /// the database records.
    ///
    /// **A database that records no epoch is damage.** Create mints one, so a
    /// file without one was written by something else — and it is what every
    /// consumer's record of progress is keyed by, so adopting it absent would
    /// let a cursor into a discarded database read as a position in this one.
    pub fn adopt(connection: Connection, path: &Path) -> Result<Self, DbError> {
        let epoch = meta::get_meta::<String>(&connection, meta::STORE_EPOCH)?.ok_or_else(|| {
            DbError::Damaged {
                what: "the database records no store epoch, so nothing it holds can be progressed \
                       against"
                    .to_string(),
            }
        })?;
        Ok(Database {
            connection,
            path: path.to_path_buf(),
            epoch,
        })
    }

    /// The connection every statement runs on. A client composes its own SQL
    /// and runs it here; opening is the act it may not perform for itself.
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// The database file this handle is holding.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The identity this database carries from creation to discard, read at
    /// open and never rewritten.
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// Open a transaction that takes the write lock at `BEGIN`.
    ///
    /// **Every changeset a client commits runs in one of these**, and one
    /// spelling of the discipline is what keeps that true. A schema creation
    /// runs before this handle exists, in one transaction on the connection
    /// the create act still holds, and is not a changeset. A deferred
    /// transaction takes the lock at its first write, so two writers that both
    /// read first can each hold a read lock and deadlock on the upgrade; an
    /// immediate one either takes the lock or reports the database busy, which
    /// the busy timeout turns into a wait. `operation` names what the write is,
    /// because a refusal to begin says which lock was held and never which
    /// write wanted it.
    ///
    /// [`Database::deferred_transaction`] is the sibling a read snapshot takes.
    /// No shipped write takes that one; the induced-failure out-of-band
    /// arrangement is the deliberate exception, writing through it to put a
    /// database in states no store operation produces.
    pub fn immediate_transaction(
        &mut self,
        operation: &'static str,
    ) -> Result<Transaction<'_>, DbError> {
        self.connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error::sql(operation, error))
    }

    /// Open a transaction that takes no lock at `BEGIN` — `BEGIN DEFERRED`.
    ///
    /// **This is what a multi-statement read runs in**, so that every statement
    /// in it answers from one write-ahead-log snapshot rather than from
    /// whatever is committed when each of them runs. A read takes no write
    /// lock, so the upgrade deadlock [`Database::immediate_transaction`] exists
    /// to rule out is not reachable from here; a write inside one is, which is
    /// why the two spellings are separate and named for the lock they take.
    ///
    /// The behavior is pinned at the call rather than read from the
    /// connection's default, which is a value SQLite lets anything holding the
    /// connection mutably change, and the mutable borrow rules out a second
    /// live transaction opened through either spelling at compile time.
    /// `operation` names what the read is, for the same reason the write's
    /// does.
    pub fn deferred_transaction(
        &mut self,
        operation: &'static str,
    ) -> Result<Transaction<'_>, DbError> {
        self.connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(|error| error::sql(operation, error))
    }
}

/// Refuse a name SQLite would read as something other than the file it spells.
///
/// `:memory:` and the empty name are special to SQLite whatever the open flags
/// say, and neither is a file the lifecycle operations here could remove or
/// rebuild. A caller that passed one made a mistake about what a database is,
/// which is a refusal — never a verdict about stored state.
///
/// **Both entry points that take a path call this for themselves** — [`connect`]
/// and [`remove_database`] — so the refusal is the substrate's rather than a
/// step each client remembers to run first. A client that forgot one would
/// otherwise open an in-memory database and be told it succeeded. `operation`
/// is the caller's own, because the two of them refuse the same name for the
/// same reason and report different acts.
fn refuse_a_name_that_is_not_a_file(operation: &'static str, path: &Path) -> Result<(), DbError> {
    let spelled = path.to_string_lossy();
    if NOT_A_FILE.contains(&spelled.as_ref()) {
        return Err(DbError::Lifecycle {
            operation,
            path: path.to_path_buf(),
            message: "a database is a file, and this names one that is not".to_string(),
        });
    }
    Ok(())
}

/// Open a connection and put it in the state a schema is designed to be read
/// under.
///
/// Every connection this workspace holds comes from here. The journal mode is
/// read back rather than assumed, because a database that refuses write-ahead
/// logging is not a database a caller can keep its promises on.
///
/// A name SQLite reads as something other than a file — `:memory:` and the
/// empty name — is refused here rather than opened, so a client that never
/// checks one for itself cannot be told an in-memory database is its file.
#[allow(clippy::disallowed_methods)] // The substrate seam: this is the one place a SQLite connection is opened.
pub fn connect(path: &Path) -> Result<Attempt, DbError> {
    refuse_a_name_that_is_not_a_file("opening the database", path)?;
    let connection =
        Connection::open_with_flags(path, OPEN_FLAGS).map_err(|error| DbError::Lifecycle {
            operation: "opening the database",
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| error::sql("setting the busy timeout", error))?;
    connection.set_prepared_statement_cache_capacity(PREPARED_STATEMENT_CACHE);

    let journal: String =
        match connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0)) {
            Ok(mode) => mode,
            Err(error) => {
                return error::damage_or_fail("reading the store schema", error)
                    .map(|detail| Attempt::Unreadable { detail });
            }
        };
    if !journal.eq_ignore_ascii_case("wal") {
        return Err(DbError::Damaged {
            what: format!("the database refused write-ahead logging and reports `{journal}`"),
        });
    }
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(|error| error::sql("setting the synchronous mode", error))?;
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(|error| error::sql("turning foreign keys on", error))?;
    #[cfg(feature = "induced-failure")]
    crate::faults::cap_the_pages(&connection)?;
    Ok(Attempt::Connected(connection))
}

/// Mint an epoch for a database being created.
///
/// **128 random bits, drawn through the connection the create already holds.**
/// Two epochs are equal when they name one database lifetime, so what the value
/// has to carry is identity and nothing else: a clock reading has a platform's
/// resolution to argue about and a process id is reused, while randomness this
/// wide makes a collision a thing nobody has to reason about.
///
/// It states no fact about the database and is never parsed back. What a reader
/// does with it is compare it against one it recorded.
pub fn mint_an_epoch(connection: &Connection) -> Result<String, DbError> {
    connection
        .query_row("SELECT hex(randomblob(16))", [], |row| row.get(0))
        .map_err(|error| error::sql("minting a store epoch", error))
}

/// Prepare the directory a database file sits in.
///
/// A failure here is environmental — a permission, a path that is a file — so
/// it is reported and nothing is discarded.
#[allow(clippy::disallowed_methods)] // The database's own directory; this crate owns the file lifecycle.
pub fn prepare_parent(path: &Path) -> Result<(), DbError> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    std::fs::create_dir_all(parent).map_err(|error| DbError::Lifecycle {
        operation: "preparing the database directory",
        path: parent.to_path_buf(),
        message: error.to_string(),
    })
}

/// Remove the database and every sidecar a journal leaves beside it.
///
/// All four, because a rebuilt database beside a stale journal is a database
/// with somebody else's committed pages in it. `-wal` and `-shm` are write-ahead
/// logging's, and `-journal` is the rollback journal's: a database opened here
/// is in WAL mode, but a file this crate did not write may carry one, and a
/// rebuild is reached for exactly those files. A file that is already gone is
/// not a failure.
///
/// A name SQLite reads as something other than a file is refused here too,
/// before anything is removed: nothing on disk answers to `:memory:` or to the
/// empty name, so removing four files that were never there and reporting
/// success would tell a caller a database it can still read is gone.
#[allow(clippy::disallowed_methods)] // Discarding a database, and tearing a disposable one down.
pub fn remove_database(path: &Path) -> Result<(), DbError> {
    refuse_a_name_that_is_not_a_file("removing the database", path)?;
    for candidate in [
        path.to_path_buf(),
        sidecar(path, "-wal"),
        sidecar(path, "-shm"),
        sidecar(path, "-journal"),
    ] {
        match std::fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(DbError::Lifecycle {
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
