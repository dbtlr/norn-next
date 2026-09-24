//! One database file, one connection, and the lifecycle of both.
//!
//! # Write-ahead logging, foreign keys, and why they are set in one place
//!
//! Both are **per-connection** settings in SQLite rather than properties of the
//! file — foreign keys are off by default in every new connection — and the
//! cascade a client's wholesale row replacement depends on needs them on. Two
//! functions open every connection this workspace ever holds — [`connect`] for
//! the one writer of a database, [`connect_read_only`] for a reader beside it —
//! so there is no reading of a schema under settings the schema was not
//! designed for.
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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};

use crate::error::{self, DbError};
use crate::meta;
use crate::plan::ColumnRead;

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

/// The flags a read-only connection is opened with.
///
/// Create is absent as well as write: a read-only open answers for a database
/// that is already there, so a path that names no file is a refusal rather
/// than an empty database nothing wrote. `SQLITE_OPEN_URI` is absent for the
/// reason it is absent above.
const READ_ONLY_FLAGS: OpenFlags =
    OpenFlags::SQLITE_OPEN_READ_ONLY.union(OpenFlags::SQLITE_OPEN_NO_MUTEX);

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
    /// Raised while this handle runs its own transaction control, and down
    /// everywhere else. A sealed read-only connection reads it through its
    /// authorizer; a writable one has no authorizer and is unaffected by it.
    /// See [`arm_the_snapshot_control`].
    snapshot_control: Arc<AtomicBool>,
    /// Whether the connection is read-only, and so sealed by the authorizer
    /// [`arm_the_snapshot_control`] installs. A writable connection has none.
    sealed: bool,
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
        Self::adopt_counting(connection, path, &mut 0)
    }

    /// [`Database::adopt`], reporting what it ran against the database.
    ///
    /// The count is taken beside the epoch read and before it is run, for the
    /// reason [`connect_read_only_counting`] gives.
    fn adopt_counting(
        connection: Connection,
        path: &Path,
        statements: &mut u64,
    ) -> Result<Self, DbError> {
        *statements += 1;
        let epoch = meta::get_meta::<String>(&connection, meta::STORE_EPOCH)?.ok_or_else(|| {
            DbError::Damaged {
                what: "the database records no store epoch, so nothing it holds can be progressed \
                       against"
                    .to_string(),
            }
        })?;
        let snapshot_control = Arc::new(AtomicBool::new(false));
        let sealed = connection
            .is_readonly(rusqlite::MAIN_DB)
            .map_err(|error| error::sql("reading the mode the database is open in", error))?;
        if sealed {
            arm_the_snapshot_control(&connection, Arc::clone(&snapshot_control))?;
        }
        Ok(Database {
            connection,
            path: path.to_path_buf(),
            epoch,
            snapshot_control,
            sealed,
        })
    }

    /// The connection every statement runs on. A client composes its own SQL
    /// and runs it here; opening is the act it may not perform for itself.
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Run `prepare`, reporting every column a statement it prepares reads.
    ///
    /// **SQLite's authorizer is what reports a read**, once per column a
    /// statement reads, while the statement is prepared, wherever in the
    /// statement the read stands. A connection has one authorizer, so the one
    /// installed for the length of `prepare` records each read and hands the
    /// verdict to the connection's own: a sealed connection's seal, the
    /// handle's transaction control included, judges every statement exactly
    /// as it does outside, and a writable connection, which has no
    /// authorizer, is refused nothing. The connection's own authorizer is
    /// reinstated whichever way `prepare` ended.
    ///
    /// Installing an authorizer expires the connection's prepared statements,
    /// so each is prepared again the next time it runs.
    pub(crate) fn recording_reads<'a, T>(
        &'a self,
        prepare: impl FnOnce(&'a Connection) -> Result<T, DbError>,
    ) -> Result<(T, BTreeSet<ColumnRead>), DbError> {
        let recorded = Arc::new(Mutex::new(BTreeSet::new()));
        let sink = Arc::clone(&recorded);
        let seal = self.sealed.then(|| Arc::clone(&self.snapshot_control));
        self.connection
            .authorizer(Some(move |context: AuthContext<'_>| {
                if let AuthAction::Read {
                    table_name,
                    column_name,
                } = context.action
                {
                    sink.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(ColumnRead {
                            table: table_name.to_string(),
                            column: column_name.to_string(),
                        });
                }
                match &seal {
                    Some(control) => sealed_verdict(control, context),
                    None => Authorization::Allow,
                }
            }))
            .map_err(|error| error::sql("recording the columns a statement reads", error))?;
        let prepared = prepare(&self.connection);
        let reinstated = if self.sealed {
            arm_the_snapshot_control(&self.connection, Arc::clone(&self.snapshot_control))
        } else {
            self.connection
                .authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
                .map_err(|error| error::sql("removing the read recorder", error))
        };
        let value = prepared?;
        reinstated?;
        let reads = std::mem::take(&mut *recorded.lock().unwrap_or_else(PoisonError::into_inner));
        Ok((value, reads))
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

    /// Open the deferred transaction a snapshot read answers from, without
    /// borrowing the handle for its length.
    ///
    /// [`Database::deferred_transaction`] is the same `BEGIN DEFERRED` and is
    /// the spelling a read that ends inside one statement scope takes; this is
    /// the spelling a read that **outlives its caller's stack frame** takes —
    /// a snapshot established under one lock and read under none. The mutable
    /// borrow is what rules out a second live transaction either way: a
    /// handle in a snapshot is a handle nothing else can begin a transaction
    /// on, and beginning one over a transaction that is already open is
    /// refused here rather than reported by the driver.
    ///
    /// **A deferred `BEGIN` takes no snapshot.** The transaction is open when
    /// this returns and the first statement run on the connection is what
    /// establishes the write-ahead-log snapshot every later statement reads
    /// from, which is why the caller runs one and why where it runs it is a
    /// contract rather than an ordering detail.
    pub fn open_snapshot(&mut self) -> Result<(), DbError> {
        if !self.connection.is_autocommit() {
            return Err(DbError::Lifecycle {
                operation: "opening a read snapshot",
                path: self.path.clone(),
                message: "a transaction is already open on this handle".to_string(),
            });
        }
        let _control = SnapshotControl::raise(&self.snapshot_control);
        self.connection
            .execute_batch("BEGIN DEFERRED")
            .map_err(|error| error::sql("opening a read snapshot", error))
    }

    /// End the snapshot [`Database::open_snapshot`] opened, by rolling it
    /// back.
    ///
    /// A snapshot writes nothing, so rolling back and committing leave the
    /// database in the same state and the rollback is the one that says so. A
    /// handle with no transaction open ends nothing and reports success: the
    /// call is what a reader runs on its way out, and a reader that already
    /// closed its snapshot is in the state this leaves it in.
    pub fn close_snapshot(&mut self) -> Result<(), DbError> {
        if self.connection.is_autocommit() {
            return Ok(());
        }
        let _control = SnapshotControl::raise(&self.snapshot_control);
        self.connection
            .execute_batch("ROLLBACK")
            .map_err(|error| error::sql("ending a read snapshot", error))
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

/// Open a **read-only** connection to a database that is already there.
///
/// This is the shape a snapshot reader is opened in, beside the writer its
/// client holds.
///
/// **The read-only open flag is what refuses every write.** It is a property
/// of the connection that no statement run on the connection can withdraw, and
/// it covers the main database and every database attached to it: a write to
/// either is refused by SQLite before the statement runs. That is the
/// guarantee a caller composing statements here may rely on.
///
/// Two settings stand on top of it, and neither is that guarantee. `query_only`
/// is a pragma, and a pragma is defeasible: `PRAGMA query_only = 0` on this
/// connection would succeed on its own. What it buys is a second refusal for
/// statements the file mode does not reach — a write to a temporary table, for
/// one — stated at the connection rather than left to the read paths. The
/// authorizer is what makes the pair stand: it refuses every action but
/// reading rows, planning them, the transaction control a snapshot runs on,
/// and the one read-only pragma FTS5 issues for itself, so every setting
/// `PRAGMA`, `ATTACH`, and temporary-object creation are refused at statement
/// preparation and the connection cannot disarm itself. The authorizer states
/// which pragma passes and why it is not a pragma opening.
///
/// Write-ahead logging is **read back rather than set** — setting the journal
/// mode is a write — so a database in any other mode is refused here instead
/// of being read under settings its writer is not using. The refusal is an
/// environmental one: the mode is a fact about how the file is being used
/// rather than about the contents of its pages, and discarding a sound
/// database over it would destroy work to fix nothing.
///
/// A path that names no database is a refusal: without the create flag there
/// is nothing to open, and a reader that created an empty database would
/// answer every read with no rows rather than saying the file is gone.
pub fn connect_read_only(path: &Path) -> Result<Connection, DbError> {
    connect_read_only_counting(path, &mut 0)
}

/// [`connect_read_only`], reporting what it ran against the database.
///
/// The count is taken beside each statement and before it is run, so a
/// statement that waited out the busy timeout and then failed is one of these:
/// the caller paid for it. `statements` is added to rather than assigned, so a
/// caller counting a whole open passes one tally through every step of it.
#[allow(clippy::disallowed_methods)] // The substrate seam: this is the one place a SQLite connection is opened.
fn connect_read_only_counting(path: &Path, statements: &mut u64) -> Result<Connection, DbError> {
    refuse_a_name_that_is_not_a_file("opening the database read-only", path)?;
    let connection =
        Connection::open_with_flags(path, READ_ONLY_FLAGS).map_err(|error| DbError::Lifecycle {
            operation: "opening the database read-only",
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| error::sql("setting the busy timeout", error))?;
    connection.set_prepared_statement_cache_capacity(PREPARED_STATEMENT_CACHE);

    *statements += 1;
    let journal: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .map_err(|error| error::sql("reading the journal mode", error))?;
    if !journal.eq_ignore_ascii_case("wal") {
        return Err(DbError::Lifecycle {
            operation: "opening the database read-only",
            path: path.to_path_buf(),
            message: format!(
                "a read-only handle needs write-ahead logging and the database reports `{journal}`"
            ),
        });
    }
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(|error| error::sql("turning foreign keys on", error))?;
    connection
        .pragma_update(None, "query_only", true)
        .map_err(|error| error::sql("making the connection read-only", error))?;
    // Last, because it refuses the pragmas above: everything this function had
    // to set is set before the connection stops accepting settings at all.
    connection
        .authorizer(Some(refuse_everything_but_reading))
        .map_err(|error| error::sql("sealing the read-only connection", error))?;
    Ok(connection)
}

/// A read-only open of a database, and what that open ran against it.
///
/// **The statements are reported whichever way the open ended.** A caller that
/// holds a lock across the open waits for a statement that took the busy
/// timeout and then failed exactly as it waits for one that answered, so a
/// report only a successful open made would leave the expensive failures
/// unaccounted.
#[derive(Debug)]
pub struct ReadOnlyOpen {
    /// The database this open bound to its file, or why there is none.
    pub adopted: Result<Database, DbError>,
    /// Statements this open ran against the database: the journal-mode read
    /// that checks the file is in write-ahead logging, and the store-epoch
    /// read that binds the connection to the file it was opened on. The
    /// settings the open applies read no rows and are not among them, and a
    /// refusal reports the statements that ran before it.
    pub statements: u64,
}

/// Open a read-only connection over a database and bind it to its file,
/// reporting what the open ran against that database.
///
/// This is the spelling a caller takes where the cost of the open is part of
/// what it answers for — a mint under a lock every other holder of the thing
/// being minted for waits behind. [`connect_read_only`] and
/// [`Database::adopt`] are the same two acts for a caller that answers for
/// neither.
pub fn open_read_only(path: &Path) -> ReadOnlyOpen {
    let mut statements = 0;
    let adopted = connect_read_only_counting(path, &mut statements)
        .and_then(|connection| Database::adopt_counting(connection, path, &mut statements));
    ReadOnlyOpen {
        adopted,
        statements,
    }
}

/// Permission for one act of transaction control on a sealed read-only
/// connection, withdrawn where it drops.
///
/// The permission covers the one statement the handle runs under it. That
/// statement runs on a connection the caller borrows mutably, so nothing else
/// can be running inside the window, and the withdrawal is a destructor so an
/// unwind out of the statement closes it too.
struct SnapshotControl<'a>(&'a AtomicBool);

impl<'a> SnapshotControl<'a> {
    fn raise(control: &'a AtomicBool) -> Self {
        control.store(true, Ordering::SeqCst);
        SnapshotControl(control)
    }
}

impl Drop for SnapshotControl<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Bind a sealed read-only connection's transaction control to the handle that
/// owns it.
///
/// **A request is answered from one snapshot, and this is what makes that
/// structural.** The authorizer below denies transaction control outright, so
/// the only `BEGIN`, `COMMIT` or `ROLLBACK` that reaches such a connection is
/// one [`Database::open_snapshot`] or [`Database::close_snapshot`] issues:
/// those two raise `control` around their own statement and lower it again,
/// and it is down everywhere else. Without it a caller composing SQL over a
/// snapshot's connection could run `ROLLBACK`, end the snapshot the read was
/// adjudicated for, and go on reading whatever is committed next — while the
/// reading that read carries still named the generation it started at.
///
/// A writable connection has no authorizer, so nothing consults the flag on
/// one and a writer's transactions are unaffected.
fn arm_the_snapshot_control(
    connection: &Connection,
    control: Arc<AtomicBool>,
) -> Result<(), DbError> {
    connection
        .authorizer(Some(move |context: AuthContext<'_>| {
            sealed_verdict(&control, context)
        }))
        .map_err(|error| error::sql("binding the snapshot control to its handle", error))
}

/// What a sealed connection's authorizer answers: transaction control while
/// `control` is raised, and otherwise [`refuse_everything_but_reading`].
fn sealed_verdict(control: &AtomicBool, context: AuthContext<'_>) -> Authorization {
    if matches!(context.action, AuthAction::Transaction { .. }) && control.load(Ordering::SeqCst) {
        return Authorization::Allow;
    }
    refuse_everything_but_reading(context)
}

/// The one thing a read-only connection may do: read rows.
///
/// **Deny by default.** The allowed set is reading a column, the `SELECT` that
/// reads it, the functions a predicate applies, and the one pragma named
/// below. Everything else is refused at statement preparation — the writes the
/// file mode already refuses, and, past those, every setting `PRAGMA` so the
/// connection cannot relax its own settings, `ATTACH` so it cannot reach a
/// database that is writable, and temporary objects so there is no writable
/// table inside the connection either.
///
/// **Transaction control is denied here**, so the snapshot a read is
/// adjudicated for cannot be ended by a statement composed over its
/// connection. The handle's own `BEGIN` and `ROLLBACK` are what
/// [`arm_the_snapshot_control`] admits, for the length of one statement each.
///
/// **The one pragma is `data_version`, asked with no value.** It reports
/// whether another connection has committed to this file since this one last
/// looked; it sets nothing and it takes no value. It is also the pragma FTS5
/// runs on the connection for itself, once for every `MATCH` and every
/// `fts5vocab` read, so refusing it does not narrow a read surface — it
/// removes one. On a connection that refuses it every full-text statement
/// fails with an authorization refusal while `EXPLAIN QUERY PLAN` over that
/// same statement still returns a clean plan, because explaining a virtual
/// table never runs it. The entry is that one name with that one shape, so it
/// opens no pragma surface: `PRAGMA query_only = 0`, `PRAGMA foreign_keys =
/// OFF` and every other setting are refused exactly as they were.
///
/// **The table-valued pragma form is a pragma here, and is refused.** `SELECT
/// ... FROM pragma_table_info('documents')` reaches this as the pragma it
/// spells rather than as a read of a table, so it is denied with the rest.
/// Nothing a read answers needs it: a read answers about a client's rows, and
/// the schema facts a read reports are rows of the client's own meta table and
/// of `sqlite_master`, both of which this connection reads.
///
/// A new action SQLite gains arrives as an action this refuses, which is the
/// direction an allow-list is chosen for: a read builder that needs one is
/// refused loudly here rather than served through a hole nobody added.
fn refuse_everything_but_reading(context: AuthContext<'_>) -> Authorization {
    match context.action {
        AuthAction::Read { .. }
        | AuthAction::Select
        | AuthAction::Function { .. }
        | AuthAction::Recursive
        | AuthAction::Pragma {
            pragma_name: "data_version",
            pragma_value: None,
        } => Authorization::Allow,
        _ => Authorization::Deny,
    }
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
