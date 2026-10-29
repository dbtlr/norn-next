//! The store itself: what one derived database means, over the file and the
//! connection `norn-db` owns.
//!
//! # One writer per store, and the borrow checker is what enforces it
//!
//! A store holds exactly one connection, and every operation runs through a
//! request that borrows the store mutably. That is the whole of "one writer per
//! *store*": on the writer's side there is no pool, no internal lock and no
//! second writer connection to serialize against. Reader pools beside it
//! acquire independent read-only connections under a shared resource budget.
//!
//! It is **not** one writer per database file. Two `Store` values on one path are
//! two connections and two writers, serialized by SQLite's own locking rather
//! than by anything here — which means a host that opens a second store for the
//! same vault entry gets interleaving, not a refusal. The missing half is the
//! maintainer file lock taken outside the store, in the derived directory the
//! store's file sits in, which is what makes "one writer" a property of the
//! derived store rather than of the handle. The host holds that lock for
//! the attachment's lifetime.
//!
//! **Reads belong on a separate handle, not on this connection.** That is
//! recorded here because it is the other half of the writer discipline: a wire
//! read is answered from a dedicated read-only snapshot handle with its own
//! connection, taken from the substrate seam like every other, and `&mut`
//! stays what a writer takes. A shared borrow of the store itself cannot serve
//! reads, because the store lives inside an attachment that lifecycle jobs hold
//! mutably for their whole duration (ADR 0015). [`SnapshotReader`] is that
//! handle's type. [`Store::reader_pool`] creates host-request pools without
//! I/O. [`Store::open_reader`] opens an eager private job reader. Both use
//! read-only source connections and leave the writer exclusively borrowed.
//!
//! # What the substrate owns, and what is decided here
//!
//! The connection, the pragmas the store schema is designed to be read under —
//! write-ahead logging and the foreign keys the cascade depends on among them —
//! the pinned-scalar mechanics, the store epoch, the database file's lifecycle
//! and the open ceremony over all of them are `norn-db`'s. What is decided here
//! is what a reading of those mechanics *means* for derived state: which rung a
//! disagreement is, what the store's own keys say, and whether the file
//! outlives the store.
//!
//! # The database-side heal rung
//!
//! Rung 3 is *discard and rebuild*, and what it means for derived state is
//! decided here. The act is [`norn_db::open`]: it reads the store schema
//! version, the DDL fingerprint, the digest of the schema the database was
//! created holding, and the store epoch, and it rebuilds from zero when any of
//! them disagrees with this build or is absent, or when the file is not a
//! database at all. The rebuild is the whole file — removed, along with the
//! sidecars a journal leaves beside it, and created again from the statement
//! list, which mints a new epoch: see [`Store::epoch`].
//!
//! What this crate hands that ceremony is the statement list, the version it
//! pins, and the store's own pinned keys — the mode, which decides whether the
//! file outlives the handle and is the one reading an open may refuse over, and
//! the path order, the case behaviour the vault root was proven to have, which
//! every row was derived under. An open under another order than the one the
//! store records, or over a store that records none, is a rebuild from zero,
//! reported as the store's own [`RebuildReason::Client`] detail naming what the
//! store records and the order its root proves: see
//! [`Store::path_order`]. The derivation version is the same kind of key: the
//! deriver names the derivation that writes the rows, an open under another
//! version, or over a store that records none, is a rebuild from zero, and the
//! detail names what the store records and the version the open is under: see
//! [`Store::derivation_version`]. Where both keys moved, the one detail names
//! both. What it takes back is [`OpenOutcome`]: the rung the state was at, with
//! a typed [`RebuildReason`] where the answer was a rebuild.
//!
//! **Rung 3 is for damaged state, never for a hostile environment.** A full
//! disk, a revoked permission, a parent directory that cannot be created, a
//! database somebody else holds: each of those is refused and reported, and none
//! of them discards anything. Refusing is the correct resolution when the
//! environment is broken and the stored state is not, because discarding a sound
//! database destroys work to fix nothing. One policy decides which is which —
//! [`norn_db::is_damaged`] — and every read an open performs goes through it.

use std::cell::{Cell, RefCell};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use norn_db::EmittedPlan;
use norn_db::rusqlite::types::Value;
use norn_db::rusqlite::{self, Connection, OptionalExtension, Row, params, params_from_iter};
use norn_db::{Adoption, Database, OpenOutcome, RebuildReason, meta};
use norn_wire::{FindingKind, LinkAddressKind, Severity};

use crate::counters::SnapshotCounters;
use crate::ddl;
use crate::error::{self, StoreError};
use crate::facts::{DerivationVersion, LinkFamily, Provenance, StoredPathOrder, TagSource};
use crate::hash;
use crate::read::{Ran, ReadFilter, ReadStatement, StatementFailure, Stepped};
use crate::request::Request;

mod intake;
pub use intake::{GetBuilder, IntakeBuilder};

#[derive(Clone, Copy, PartialEq, Eq)]
enum GrantPolicy {
    CompleteRequest,
    SourceOnly,
}

/// The read-only snapshot handle wire reads run on.
///
/// Host requests use [`Store::reader_pool`], a zero-I/O handle whose independent
/// connections share a [`crate::ReadBudget`]. A private job can instead use the
/// eager single-connection [`Store::open_reader`].
///
/// Acquisition and establishment are separate because the caller samples trust
/// under its entry gate. [`SnapshotReader::try_take_for`] tries warm capacity
/// without waiting. [`SnapshotReader::acquire`] waits and opens outside that gate
/// under the same deadline. [`ConnectionTurn::establish`] begins the snapshot
/// after the caller has rechecked identity and trust. A turn that establishes
/// nothing returns its resources on drop, including during an unwind.
///
/// Source connections are read-only with `query_only` and a statement authorizer.
/// A reader can never derive or rebuild the source. Open failures report typed
/// damage separately from environmental failure for the host's lifecycle judgment.
///
/// The handle outlives the entry that minted it where a read is still running
/// against it: the entry lets its own hold go at the teardown window, and the
/// read goes on answering from the handle it took. Canonical throwaway teardown
/// can unlink its file. A sealed loading source instead retains actual file
/// ownership through every open connection and established snapshot.
///
/// **The database file this handle reads is not reachable from it.** A handle
/// is what a hold hands out, and a caller holding the file's path needs no
/// hold at all: it can open its own read-only connection over the same
/// database and establish snapshots on it under no adjudication, un-pinned,
/// holding no lease, serialized against nothing and invisible to the read
/// account — the same escape a cloned handle would be. So there is no
/// accessor, and the [`fmt::Debug`] rendering below carries the epoch rather
/// than the path. The absence is pinned:
///
/// ```compile_fail,E0599
/// use std::path::Path;
/// use norn_store::SnapshotReader;
///
/// fn the_file_escapes(reader: &SnapshotReader) -> &Path {
///     reader.path()
/// }
/// ```
pub struct SnapshotReader {
    grant_policy: GrantPolicy,
    intake_file: Option<Arc<norn_db::TemporaryFile>>,
    budget: Option<Arc<crate::ReadBudget>>,
    pool: Option<crate::read_budget::Pool>,
    source: Option<std::path::PathBuf>,
    count_results: Arc<crate::count::CountResults>,
    /// The eager private reader connection, held here between reads and taken
    /// by the read that is answering.
    ///
    /// Absent means a read holds it. A read that finds it absent waits on
    /// [`SnapshotReader::returned`] rather than opening a second connection,
    /// which is the serialization this handle is.
    connection: Mutex<Option<Database>>,
    returned: Condvar,
    /// The database the connection reads, carried beside it because the
    /// identity is asked for while a read holds the connection. The file it
    /// reads is not carried: the connection holds the path it was opened on,
    /// and a second copy here would be one this type had to keep from being
    /// read back out.
    epoch: String,
    /// The case behaviour the rows of the store this handle was minted from
    /// were derived under. A store's order is fixed for the life of the
    /// database it holds — another order is another database — so the handle
    /// carries it from the mint, and every snapshot it establishes reads under
    /// it.
    order: StoredPathOrder,
}

impl fmt::Debug for SnapshotReader {
    /// The epoch and nothing else. The epoch names which database the handle
    /// reads; the path would name where to open a second connection to it,
    /// which is the escape this type has no accessor for either.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotReader")
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

impl SnapshotReader {
    /// Only warm source connections can be acquired while an entry gate is held.
    pub fn try_take_for(
        self: &Arc<Self>,
        need: crate::ReadNeed,
        deadline: Instant,
    ) -> Option<ConnectionTurn> {
        if self.grant_policy == GrantPolicy::SourceOnly && need != crate::ReadNeed::Source {
            return None;
        }
        if let (Some(budget), Some(pool)) = (&self.budget, &self.pool) {
            let grant = budget.try_grant(pool, need)?;
            return Some(self.ready_turn(grant, deadline));
        }
        self.take_standalone()
    }

    fn ready_turn(
        self: &Arc<Self>,
        mut grant: crate::read_budget::Grant,
        deadline: Instant,
    ) -> ConnectionTurn {
        let database = match grant.source.item.take() {
            Some(crate::read_budget::Cached::Source(database)) => database,
            _ => unreachable!("a warm source grant contains a source connection"),
        };
        if let Some(pool) = &self.pool {
            pool.failure(None);
        }
        ConnectionTurn {
            reader: Arc::clone(self),
            database: Some(database),
            grant: Some(grant),
            deadline,
        }
    }

    /// Reserve the complete request before opening any cold connection. Never
    /// called under the lifecycle gate.
    pub fn acquire(
        self: &Arc<Self>,
        need: crate::ReadNeed,
        deadline: Instant,
    ) -> Result<Option<ConnectionTurn>, StoreError> {
        if self.grant_policy == GrantPolicy::SourceOnly && need != crate::ReadNeed::Source {
            return Err(StoreError::Sql {
                operation: "acquiring a loading source",
                condition: None,
                message: "loading sources admit source-only reads".into(),
            });
        }
        let (Some(budget), Some(pool)) = (&self.budget, &self.pool) else {
            return Ok(self.wait_for_the_connection_until(deadline));
        };
        let Some(mut grant) = budget.grant(pool, need, deadline) else {
            return Ok(None);
        };
        if grant.source.item.is_none() {
            let result = (|| {
                let open_ordinary = || {
                    norn_db::open_read_only_until(
                        self.source.as_ref().expect("pooled source"),
                        deadline,
                    )
                    .adopted
                    .map_err(StoreError::from)
                };
                let database = if let Some(file) = &self.intake_file {
                    file.open_snapshot().map_err(|error| match error {
                        norn_db::TemporaryReadError::Failed(error) => StoreError::from(error),
                        norn_db::TemporaryReadError::Lost { path } => StoreError::Lifecycle {
                            operation: "opening sealed loading source",
                            path,
                            message: "the disposable source is missing or replaced".into(),
                        },
                    })?
                } else {
                    open_ordinary()?
                };
                if database.epoch() != self.epoch {
                    return Err(StoreError::Sql {
                        operation: "opening a snapshot reader",
                        condition: None,
                        message: "the source database was replaced while opening a reader".into(),
                    });
                }
                crate::read::register_functions(database.connection())?;
                crate::resolve::register_functions(database.connection())?;
                database.count_statements_begun();
                Ok::<_, StoreError>(database)
            })();
            match result {
                Ok(database) => {
                    budget.opened();
                    grant.source.item = Some(crate::read_budget::Cached::Source(database));
                    pool.failure(None);
                }
                Err(error) => {
                    pool.failure(Some(error.clone()));
                    return Err(error);
                }
            }
        }
        if Instant::now() >= deadline || !pool.is_live() {
            return Ok(None);
        }
        Ok(Some(self.ready_turn(grant, deadline)))
    }

    /// Retire admission without closing files under the caller's entry gate.
    pub fn retire(&self) {
        if let Some(pool) = &self.pool {
            pool.retire();
        }
    }
    /// Close disposable idle resources outside the entry gate.
    pub fn retire_intake(&self) {
        assert!(
            self.intake_file.is_some(),
            "only sealed intake uses this retirement"
        );
        self.retire();
        if let Some(budget) = &self.budget {
            budget.drain();
        }
    }
    pub fn last_open_failure(&self) -> Option<StoreError> {
        self.pool.as_ref()?.last_failure()
    }

    /// Try for a source-only warm grant without waiting or opening a database.
    /// A cold pool returns nothing even when the shared budget has capacity;
    /// [`SnapshotReader::acquire`] can open it outside the caller's gate.
    /// The turn keeps this handle alive through establishment and query work.
    pub fn try_take(self: &Arc<Self>) -> Option<ConnectionTurn> {
        self.try_take_for(
            crate::ReadNeed::Source,
            Instant::now() + std::time::Duration::from_secs(5),
        )
    }

    fn take_standalone(self: &Arc<Self>) -> Option<ConnectionTurn> {
        let mut held = self
            .connection
            .lock()
            .expect("a snapshot reader's connection is poisoned");
        held.take().map(|database| ConnectionTurn {
            reader: Arc::clone(self),
            database: Some(database),
            grant: None,
            deadline: Instant::now() + std::time::Duration::from_secs(5),
        })
    }

    /// Wait for this handle's connection until `deadline`, and take it.
    ///
    /// **The wait ends one of two ways**: the connection comes back, and the
    /// turn is answered; or the deadline passes first, and nothing is. A
    /// deadline already past takes the connection where it is free and
    /// answers nothing where it is not, without waiting. So a wait whose
    /// wake-up never comes — a holding read that never ends, or a teardown
    /// that wakes nobody — is bounded by the caller's deadline and by nothing
    /// else.
    ///
    /// A caller holding a lock the holding read needs in order to end would
    /// wait out its whole deadline here and then be answered nothing, which
    /// is why [`SnapshotReader::try_take`] exists and why this is the spelling
    /// taken with no such lock held.
    pub fn wait_for_the_connection_until(
        self: &Arc<Self>,
        deadline: Instant,
    ) -> Option<ConnectionTurn> {
        if self.pool.is_some() {
            return self
                .acquire(crate::ReadNeed::Source, deadline)
                .ok()
                .flatten();
        }
        let mut held = self
            .connection
            .lock()
            .expect("a snapshot reader's connection is poisoned");
        let database = loop {
            if let Some(database) = held.take() {
                break database;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            (held, _) = self
                .returned
                .wait_timeout(held, remaining)
                .expect("a snapshot reader's connection is poisoned");
        };
        Some(ConnectionTurn {
            reader: Arc::clone(self),
            database: Some(database),
            grant: None,
            deadline,
        })
    }

    /// The database this handle reads, from its creation to its discard.
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// Statements SQLite has begun on the calling thread over every snapshot
    /// reader's connection, over the thread's life.
    ///
    /// **SQLite counts them, not the code beside each statement.** A handle's
    /// connection counts its statements from the moment the handle is minted,
    /// through [`Database::count_statements_begun`], so every statement
    /// SQLite's statement trace reports on it is counted as SQLite begins it:
    /// a snapshot's `BEGIN`, its establishing statement, each statement a read
    /// builder runs on it, the statements FTS5 runs inside a full-text match,
    /// and the `ROLLBACK` that ends it. The trace does not report an
    /// `EXPLAIN`, which compiles its query and does not run it, or the reading
    /// of its schema SQLite runs when it loads the schema again, so neither is
    /// counted. The mint's own statements run before the connection counts and
    /// are not among them; [`ReaderMint::statements`] carries those.
    ///
    /// **The count is the thread's, not the handle's.** A connection runs on
    /// one thread at a time and a turn is one read's, so a caller that holds a
    /// handle's turn and reads this on both sides of a stretch of its own
    /// thread has, in the difference, every statement SQLite began for it in
    /// that stretch. A caller holding a lock across the stretch learns what
    /// ran under its lock from its own two readings, however the statements
    /// were composed and whoever ran them on that thread. Retained-count
    /// scratch connections count here too; [`Snapshot::counters`] separately
    /// names source statements only.
    pub fn statements_run_on_this_thread() -> u64 {
        norn_db::statements_begun_on_this_thread()
    }

    /// Put the connection back in the handle and wake one read waiting for it.
    ///
    /// **Nothing panics while the connection's mutex is held**, here or in
    /// [`SnapshotReader::try_take`] or
    /// [`SnapshotReader::wait_for_the_connection_until`]:
    /// each only moves the connection in or out of its slot, clones an
    /// [`Arc`] and waits on the condition variable. So the mutex is never
    /// poisoned, and the `expect` below never fires — which is what makes it
    /// safe in the drops that call this on an unwinding thread, where a panic
    /// would abort the process.
    fn give_the_connection_back(&self, database: Database) {
        let mut held = self
            .connection
            .lock()
            .expect("a snapshot reader's connection is poisoned");
        *held = Some(database);
        drop(held);
        self.returned.notify_one();
    }
}

impl Drop for SnapshotReader {
    fn drop(&mut self) {
        if self.intake_file.is_some() {
            self.retire_intake();
            return;
        }
        self.retire();
    }
}

/// An eager private reader and the statements its source open ran.
/// Host requests instead use the zero-I/O [`Store::reader_pool`] path.
#[derive(Debug)]
pub struct ReaderMint {
    /// The handle this store's reads run on, or why the store has none.
    pub reader: Result<SnapshotReader, StoreError>,
    /// Statements the mint ran against the database. They are the read-only
    /// open's own — the journal-mode read and the store-epoch read — counted
    /// where each one runs, so a refusal reports the statements that ran
    /// before it.
    pub statements: u64,
}

/// One read's turn on the connection its handle holds.
///
/// It is the connection out of the handle and not yet inside a snapshot, which
/// is the state a caller is in while it decides whether to establish. The
/// connection goes back where the turn drops without establishing — a caller
/// that changed its mind, and an unwind alike — so a handle is never left
/// empty by a read that never happened.
pub struct ConnectionTurn {
    grant: Option<crate::read_budget::Grant>,
    deadline: Instant,
    reader: Arc<SnapshotReader>,
    /// The connection, taken out by [`ConnectionTurn::establish`] so the
    /// snapshot owns the give-back from there on.
    database: Option<Database>,
}

impl fmt::Debug for ConnectionTurn {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectionTurn")
            .field("reader", &self.reader)
            .finish_non_exhaustive()
    }
}

impl ConnectionTurn {
    /// Establish the write-ahead-log snapshot one read answers from, and
    /// sample the store reading it is answered under.
    ///
    /// **Two statements SQLite runs, and one of them reads the database.** A
    /// deferred `BEGIN` opens the transaction and takes no snapshot and reads
    /// no row, so the snapshot is the next statement's, the establishing one,
    /// and that statement is the read of the store's write generation, which
    /// is the reading the answer carries. A caller that runs this where the
    /// published demand is read gets a demand and a snapshot describing one
    /// instant, and pays one read of the database for both.
    ///
    /// The connection is already reserved. Establishment performs no resource
    /// acquisition. Any SQLite busy wait uses the remaining original deadline;
    /// an expired turn refuses before running SQL.
    ///
    /// **The connection stays in the turn until the snapshot takes it**, so
    /// the turn's drop is what gives it back from every way out before that:
    /// the refusal, and an unwind between the `BEGIN` and the handing out. The
    /// drop ends the transaction the `BEGIN` opened before it gives the
    /// connection back. From the handing out on, the connection goes back to
    /// the handle when the [`Snapshot`] is dropped.
    ///
    /// The snapshot carries the case behaviour the handle's store rows were
    /// derived under, which the handle carries from its mint, and a find's
    /// `resolves` part compiles its class under it, so no read detects one and
    /// no caller hands one over. It costs no statement.
    ///
    /// **What it ran is counted by SQLite whichever way it ended.** An
    /// establishment runs the deferred `BEGIN` and the establishing statement,
    /// and one that refused also rolled the transaction back before it gave
    /// the connection up; a caller holding a lock across all of that reads
    /// what SQLite began off [`SnapshotReader::statements_run_on_this_thread`]
    /// on both sides of that lock. A statement refused before SQLite ran it
    /// is not among them. A snapshot's own [`Snapshot::counters`] start from
    /// the establishing statement alone and are that one read's view of its
    /// query work.
    pub fn establish(mut self) -> Result<Snapshot, StoreError> {
        let reader = Arc::clone(&self.reader);
        let mut counters = SnapshotCounters::default();
        let database = self
            .database
            .as_mut()
            .expect("a turn holds the connection until it establishes or drops");
        let (reading, directory_marks_present) =
            establish_on(database, &reader, &mut counters, self.deadline)?;
        Ok(Snapshot {
            order: reader.order,
            reader,
            database: self.database.take(),
            grant: RefCell::new(self.grant.take()),
            deadline: self.deadline,
            reading,
            directory_marks_present,
            relations_loading: false,
            counters: Cell::new(counters),
        })
    }
}

impl Drop for ConnectionTurn {
    /// **The connection goes back with no transaction open on it.** A turn
    /// dropped inside its establishment holds the transaction the `BEGIN`
    /// opened, and a connection given back inside one would refuse the next
    /// read's `BEGIN`. Ending a transaction that is not open runs nothing, so
    /// a turn dropped before it established gives the connection back
    /// without a statement.
    fn drop(&mut self) {
        if let Some(mut database) = self.database.take() {
            let _ = database.close_snapshot();
            if let Some(grant) = self.grant.take() {
                grant.finish(database);
            } else {
                self.reader.give_the_connection_back(database);
            }
        }
    }
}

/// Statements SQLite runs to establish one snapshot that answers: the deferred
/// `BEGIN`, which opens the transaction and reads no row, and the establishing
/// statement, which reads the store's write generation and so takes the
/// snapshot.
///
/// **This is what [`SnapshotReader::statements_run_on_this_thread`] moves by
/// across [`ConnectionTurn::establish`] when it answers**, and so what a
/// caller holding a lock across the establishment reads it ran under that
/// lock. An establishment that refused ran what SQLite began before it
/// refused and the `ROLLBACK` that ended its transaction, which need not be
/// this number. A snapshot's own [`Snapshot::counters`] count the
/// establishing statement alone, because they count the read's query work.
pub const SNAPSHOT_ESTABLISHMENT_STATEMENTS: u64 = 2;

/// Open the snapshot and run the one statement that establishes it.
///
/// The two counts are taken at different moments, because they answer
/// different questions. A snapshot either opened or it did not, so it is
/// counted where the open succeeded. A statement is a cost the caller paid
/// whether or not it answered — a read that waited out the busy timeout and
/// then failed held the connection for that wait — so it is counted as it is
/// run.
fn establish_on(
    database: &mut Database,
    reader: &SnapshotReader,
    counters: &mut SnapshotCounters,
    deadline: Instant,
) -> Result<(StoreReading, bool), StoreError> {
    let check_deadline = || {
        if Instant::now() >= deadline {
            Err(StoreError::Sql {
                operation: "establishing a read snapshot",
                condition: None,
                message: "the read acquisition deadline has elapsed".to_string(),
            })
        } else {
            Ok(())
        }
    };
    check_deadline()?;
    database.bound_busy_waits_until(deadline)?;
    database.open_snapshot()?;
    #[cfg(feature = "induced-failure")]
    crate::faults::unwind_if_the_snapshot_open_is_armed();
    counters.count_snapshot();
    counters.count_statement();
    check_deadline()?;
    database.bound_busy_waits_until(deadline)?;
    #[cfg(feature = "induced-failure")]
    if norn_db::faults::meta_read_is_armed() {
        // Keep the substrate's existing one-shot refusal at this establishing read.
        last_write_generation(database.connection())?;
    }
    // This is still the one establishing read. The presence bit belongs to
    // this snapshot, so link rows need no extra statement when no marks stand.
    let (write_generation, directory_marks_present) = database
        .connection()
        .query_row(
            "SELECT value, EXISTS(SELECT 1 FROM directory_marks WHERE path_key >= '')
         FROM meta WHERE key = ?1",
            params![meta::WRITE_GENERATION],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?)),
        )
        .optional()
        .map_err(|error| crate::error::sql("establishing a read snapshot", error))?
        .ok_or_else(|| StoreError::Damaged {
            what: "the database records no write generation, so no read can say what it read"
                .to_string(),
        })?;
    check_deadline()?;
    Ok((
        StoreReading {
            epoch: reader.epoch.clone(),
            write_generation,
        },
        directory_marks_present,
    ))
}

/// The last write generation committed to the database on `connection`.
///
/// A store records its counter at create, so one that records none is damaged:
/// no reading of it can say what it read.
pub(crate) fn last_write_generation(connection: &Connection) -> Result<i64, StoreError> {
    meta::get_meta::<i64>(connection, meta::WRITE_GENERATION)?.ok_or_else(|| StoreError::Damaged {
        what: "the database records no write generation, so no read can say what it read"
            .to_string(),
    })
}

/// One read's snapshot: the connection it answers on, the reading it was
/// established at, and what establishing it cost.
///
/// Every statement run through this reads the rows the snapshot was
/// established over, so a request answered here is answered from one instant
/// of the database whatever the writer does meanwhile. The connection goes
/// back to the [`SnapshotReader`] when this is dropped, and the next read
/// waiting for it is woken.
///
/// **The handle inside is not reachable from here.** A snapshot is a read's to
/// answer from, not a way to reach the handle it was established on and
/// establish another: the field is private and no accessor hands it out, so
/// the one route to a snapshot is a turn taken from a handle a caller already
/// holds. An accessor added here would let a caller clone the handle out of a
/// snapshot, end the read that adjudicated it, and go on establishing
/// snapshots through the pool that no adjudication ever saw — so
/// the absence is pinned:
///
/// ```compile_fail,E0599
/// use std::sync::Arc;
/// use norn_store::{Snapshot, SnapshotReader};
///
/// fn the_handle_escapes(snapshot: &Snapshot) -> Arc<SnapshotReader> {
///     Arc::clone(snapshot.reader())
/// }
/// ```
pub struct Snapshot {
    pub(crate) relations_loading: bool,
    pub(crate) grant: RefCell<Option<crate::read_budget::Grant>>,
    pub(crate) deadline: Instant,
    reader: Arc<SnapshotReader>,
    /// The connection this snapshot is open on, taken out to be given back to
    /// the reader at the drop that ends the snapshot.
    database: Option<Database>,
    reading: StoreReading,
    /// Whether this snapshot holds any unreadable-directory mark.
    pub(crate) directory_marks_present: bool,
    /// The case behaviour the rows this snapshot reads were derived under, as
    /// the handle it was established on carries it.
    order: StoredPathOrder,
    /// What this snapshot cost, counted through `&self`: a read builder runs
    /// on a shared borrow, so nothing that holds the snapshot lends it out
    /// mutably. The connection already makes a snapshot `!Sync`, so a `Cell`
    /// costs it nothing.
    counters: Cell<SnapshotCounters>,
}

impl fmt::Debug for Snapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Snapshot")
            .field("reading", &self.reading)
            .finish_non_exhaustive()
    }
}

impl Snapshot {
    /// Withhold relation judgments until construction completes its relations step.
    ///
    /// NORN-468 PR C (host early admission) consumes this mark on early holds;
    /// the host does not establish such holds on the canonical store yet.
    #[must_use]
    pub fn relations_loading(mut self) -> Self {
        self.relations_loading = true;
        self
    }

    pub(crate) fn count_results(&self) -> &Arc<crate::count::CountResults> {
        &self.reader.count_results
    }

    /// The store reading this snapshot was established at: which database, and
    /// how far its writes had got.
    pub fn reading(&self) -> &StoreReading {
        &self.reading
    }

    /// The filesystem order proven when this store was opened.
    /// It remains a rebuild input for filesystem identity, heal paging and
    /// query path globs. Resolution uses the same comparison key on every root.
    pub fn path_order(&self) -> StoredPathOrder {
        self.order
    }

    /// What this read's snapshot cost: the snapshot itself, and the statements
    /// run on it and what SQLite counted stepping them.
    pub fn counters(&self) -> SnapshotCounters {
        self.counters.get()
    }

    /// The connection this snapshot's statements run on.
    ///
    /// It is opened with the read-only flag, so every write to the database it
    /// names and to any database attached to it is refused by SQLite before
    /// the statement runs; `query_only` and the statement authorizer stand on
    /// top of that and refuse the pragma, the attach and the temporary object
    /// a statement would otherwise reach around it with.
    ///
    /// **Private to this module, on purpose.** A statement run here and not
    /// through [`Snapshot::run_statement_staged`] or
    /// [`Snapshot::run_statements_merged`] escapes
    /// [`Snapshot::counters`] — a read's cost would no longer be what
    /// [`Snapshot::count_statement`] and [`Snapshot::count_steps`] counted —
    /// so nothing outside this file reaches it: every read builder runs its
    /// statements through [`Snapshot::run_statement`] or
    /// [`Snapshot::run_statement_staged`] or [`Snapshot::run_statements_merged`],
    /// which count as they run, and
    /// [`Snapshot::explained`] reaches [`Snapshot::database`] to explain a
    /// statement already run rather than to run one. The application-defined
    /// functions a statement calls are registered on this connection when its
    /// handle is minted, by [`Store::open_reader`].
    fn connection(&self) -> &Connection {
        self.database().connection()
    }

    /// The handle this snapshot's connection belongs to, which is what takes
    /// a plan of a statement the connection ran.
    ///
    /// Private for the reason [`Snapshot::connection`] is: [`Database`] hands
    /// its own connection back to anything holding it.
    fn database(&self) -> &Database {
        self.database
            .as_ref()
            .expect("a snapshot holds its connection until it is dropped")
    }

    /// The database this snapshot reads, from its creation to its discard.
    pub fn epoch(&self) -> &str {
        self.reading.epoch()
    }

    /// Count one statement run on this snapshot's connection.
    ///
    /// Counted as it is run, whether or not it answered: a statement that
    /// waited out the busy timeout and then failed held the connection for
    /// that wait.
    pub(crate) fn count_statement(&self) {
        let mut counters = self.counters.get();
        counters.count_statement();
        self.counters.set(counters);
    }

    /// Add what SQLite counted stepping one statement run on this snapshot's
    /// connection.
    pub(crate) fn count_steps(&self, stepped: crate::read::Stepped) {
        let mut counters = self.counters.get();
        counters.count_steps(stepped.vm_steps, stepped.full_scan_steps);
        self.counters.set(counters);
    }

    pub(crate) fn explain_run(&self, ran: &Ran) -> Result<EmittedPlan, StoreError> {
        match &ran.external_plan {
            Some(plan) => Ok(plan.clone()),
            None => self
                .database()
                .emitted_plan(ran.sql(), params_from_iter(ran.values().iter()))
                .map_err(StoreError::from),
        }
    }

    /// Each statement in `record`, in the order it ran, as `plan` makes it of
    /// its name, its filters, and the plan SQLite reports for it: the text it
    /// ran, bound to the values it ran with, explained on this snapshot's
    /// read-only connection. An explain is a report about a statement rather
    /// than a run of it, so it is not counted.
    pub(crate) fn explained<P>(
        &self,
        record: Vec<Ran>,
        mut plan: impl FnMut(ReadStatement, Vec<ReadFilter>, EmittedPlan) -> P,
    ) -> Result<Vec<P>, StoreError> {
        record
            .into_iter()
            .map(|ran| {
                let emitted = self.explain_run(&ran)?;
                Ok(plan(ran.statement, ran.filters, emitted))
            })
            .collect()
    }

    /// Run `ran`, counted on this snapshot and recorded at the end of `record`
    /// before it is prepared, and read each row it answers through `read`.
    ///
    /// What is prepared is the text the record holds, bound to the values it holds:
    /// [`Snapshot::explained`] explains the record, so the plan of a
    /// statement is the plan of what ran. Once every row is read, or the
    /// stepping failed, the record takes what SQLite counted stepping it
    /// ([`Stepped`]) and the snapshot adds it to its own counts.
    pub(crate) fn run_statement<T>(
        &self,
        record: &mut Vec<Ran>,
        ran: Ran,
        read: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<Vec<T>> {
        self.run_statement_staged(record, ran, read)
            .map_err(StatementFailure::into_inner)
    }

    /// [`Snapshot::run_statement`], its failure saying whether the statement
    /// failed before it was stepped — preparing its text or binding its
    /// values — or while it was stepped.
    ///
    /// Delegates preparation, stepping and counting to [`Snapshot::stream_statement`].
    pub(crate) fn run_statement_staged<T>(
        &self,
        record: &mut Vec<Ran>,
        ran: Ran,
        mut read: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>, StatementFailure> {
        let mut found = Vec::new();
        match self.stream_statement(record, ran, |row| read(row).map(|value| found.push(value))) {
            Ok(()) => Ok(found),
            Err(crate::read::StreamFailure::Source(problem)) => Err(problem),
            Err(crate::read::StreamFailure::Consumer(problem)) => {
                Err(StatementFailure::Stepping(problem))
            }
        }
    }

    /// Stream one counted source statement without retaining its result set.
    /// Consumer failures remain distinct from source-driver damage. This and
    /// [`Snapshot::run_statements_merged`] are the counted preparation seams
    /// on the source connection; disposable results count their own work.
    pub(crate) fn stream_statement<E>(
        &self,
        record: &mut Vec<Ran>,
        ran: Ran,
        mut consume: impl FnMut(&Row<'_>) -> Result<(), E>,
    ) -> Result<(), crate::read::StreamFailure<E>> {
        use crate::read::StreamFailure;
        self.count_statement();
        record.push(ran);
        let ran = record.last_mut().expect("the statement was just recorded");
        let mut statement = self
            .connection()
            .prepare(ran.sql())
            .map_err(|problem| StreamFailure::Source(StatementFailure::Preparing(problem)))?;
        let result = (|| {
            let mut rows = statement
                .query(params_from_iter(ran.values().iter()))
                .map_err(|problem| StreamFailure::Source(StatementFailure::Preparing(problem)))?;
            while let Some(row) = rows
                .next()
                .map_err(|problem| StreamFailure::Source(StatementFailure::Stepping(problem)))?
            {
                consume(row).map_err(StreamFailure::Consumer)?;
            }
            Ok(())
        })();
        ran.stepped = Stepped::of(&statement);
        self.count_steps(ran.stepped);
        result
    }

    /// Merge path-only streams in answer order, retaining one head per stream.
    ///
    /// Every statement is counted and recorded before preparation. Counters
    /// are collected for all streams after exhaustion or the first failure.
    pub(crate) fn run_statements_merged(
        &self,
        record: &mut Vec<Ran>,
        runs: impl IntoIterator<Item = Ran>,
        mut read: impl FnMut(String),
    ) -> rusqlite::Result<()> {
        let start = record.len();
        let mut statements = Vec::new();
        let result = (|| {
            for ran in runs {
                self.count_statement();
                record.push(ran);
                let ran = record.last().expect("the statement was just recorded");
                statements.push(self.connection().prepare(ran.sql())?);
            }
            let mut streams = statements
                .iter_mut()
                .zip(&record[start..])
                .map(|(statement, ran)| statement.query(params_from_iter(ran.values().iter())))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut heads = BinaryHeap::new();
            for (at, stream) in streams.iter_mut().enumerate() {
                if let Some(row) = stream.next()? {
                    let path: String = row.get(0)?;
                    heads.push(Reverse((crate::read::answer_place(&path), at)));
                }
            }
            while let Some(Reverse(((_, path), at))) = heads.pop() {
                read(path);
                if let Some(row) = streams[at].next()? {
                    let path: String = row.get(0)?;
                    heads.push(Reverse((crate::read::answer_place(&path), at)));
                }
            }
            Ok(())
        })();
        for (ran, statement) in record[start..].iter_mut().zip(&statements) {
            ran.stepped = Stepped::of(statement);
        }
        for ran in &record[start..] {
            self.count_steps(ran.stepped);
        }
        result
    }

    /// Run one yes-or-no probe, which answers exactly one row.
    pub(crate) fn ask(
        &self,
        record: &mut Vec<Ran>,
        probe: Ran,
        operation: &'static str,
    ) -> Result<bool, StoreError> {
        self.run_statement(record, probe, |row| row.get::<_, bool>(0))
            .and_then(|answers| {
                answers
                    .into_iter()
                    .next()
                    .ok_or(rusqlite::Error::QueryReturnedNoRows)
            })
            .map_err(|problem| error::sql(operation, problem))
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        // The connection goes back whichever way the read ended, including an
        // unwind: a handle left empty is an entry no later read reaches.
        if let Some(mut database) = self.database.take() {
            let _ = database.close_snapshot();
            if let Some(grant) = self.grant.get_mut().take() {
                let budget = grant.source.budget();
                grant.finish(database);
                if let Some(budget) = budget {
                    budget.drain();
                }
            } else {
                self.reader.give_the_connection_back(database);
            }
        }
    }
}

/// Which database a read was answered from, and how far its writes had got.
///
/// **A generation orders the writes of one database and the epoch says which
/// database**, so the pair is one reading: a generation compared across epochs
/// names a position in a database that no longer exists. Every answer carries
/// this, because what a read saw is a fact about the state it read rather than
/// one a later call can be asked for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreReading {
    epoch: String,
    write_generation: i64,
}

impl StoreReading {
    /// The reading a snapshot established at `write_generation` in `epoch`.
    ///
    /// The establishment mints its own, and this is the spelling a caller
    /// standing in for one uses: a test double for a reader answers under a
    /// reading the same shape a store's own answers under.
    pub fn of(epoch: impl Into<String>, write_generation: i64) -> Self {
        StoreReading {
            epoch: epoch.into(),
            write_generation,
        }
    }

    /// The database this reading names.
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// The last write generation committed to that database when the snapshot
    /// was established. A read may trail derivation still in flight, and this
    /// is how far it does not.
    pub fn write_generation(&self) -> i64 {
        self.write_generation
    }
}

/// Whether the store's file outlives the store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreMode {
    /// A registered vault's durable derived state. The file survives the
    /// process.
    Durable,
    /// Disposable derivation. Ordinary throwaway stores unlink on close or drop.
    /// Sealed first-index sources instead retain their file through every reader.
    /// A registration's canonical derived state remains durable.
    Throwaway,
}

impl StoreMode {
    const fn as_str(self) -> &'static str {
        match self {
            StoreMode::Durable => "durable",
            StoreMode::Throwaway => "throwaway",
        }
    }

    fn from_str(recorded: &str) -> Option<Self> {
        match recorded {
            "durable" => Some(StoreMode::Durable),
            "throwaway" => Some(StoreMode::Throwaway),
            _ => None,
        }
    }
}

/// One vault's derived state.
#[derive(Debug)]
pub struct Store {
    intake_unsealed: bool,
    pub(crate) metadata_intake: bool,
    count_results: Arc<crate::count::CountResults>,
    pub(crate) database: Database,
    mode: StoreMode,
    order: StoredPathOrder,
    derivation: DerivationVersion,
    outcome: OpenOutcome,
    /// Whether the build that fills this store has not ended: set from the open
    /// outcome, because a create and a rebuild both record the mark and a reuse
    /// found none.
    under_construction: bool,
    pub(crate) link_health_deferred: bool,
    pub(crate) steps: Option<crate::construction::Steps>,
    torn_down: bool,
}

impl Store {
    /// This canonical store's fairness identity, independent of physical loading files.
    pub fn read_group(&self) -> crate::ReadGroup {
        crate::ReadGroup(self.database.path().to_path_buf())
    }

    /// Store-owned disposable namespace beside the canonical database.
    pub fn loading_directory(path: &Path) -> PathBuf {
        let mut name = path.as_os_str().to_os_string();
        name.push(".loading");
        PathBuf::from(name)
    }

    /// Sweep recognized loading orphans under the caller's existing maintainer lock.
    pub fn sweep_loading(path: &Path) -> Result<usize, StoreError> {
        norn_db::sweep_temporary_directory(&Self::loading_directory(path)).map_err(StoreError::from)
    }

    /// Mint a zero-I/O handle. Layer 6 callers share one host-owned budget;
    /// private writer jobs continue to use the eager standalone mint.
    pub fn reader_pool(&self, budget: &Arc<crate::ReadBudget>) -> SnapshotReader {
        self.reader_pool_in_group(budget, &self.read_group())
    }

    fn reader_pool_in_group(
        &self,
        budget: &Arc<crate::ReadBudget>,
        group: &crate::ReadGroup,
    ) -> SnapshotReader {
        let reader = SnapshotReader {
            intake_file: None,
            grant_policy: GrantPolicy::CompleteRequest,
            budget: Some(Arc::clone(budget)),
            pool: Some(budget.pool(group.0.clone(), crate::read_budget::Kind::Source)),
            source: Some(self.database.path().to_path_buf()),
            count_results: Arc::clone(&self.count_results),
            epoch: self.epoch().to_string(),
            order: self.order,
            connection: Mutex::new(None),
            returned: Condvar::new(),
        };
        if self.intake_unsealed {
            reader.retire();
        }
        reader
    }
    /// Open, or create, the durable store at `path` for a vault root proven to
    /// have `order`'s case behaviour, whose rows `derivation` writes.
    ///
    /// The parent directory is prepared if it is missing. A database that is not
    /// the shape this build writes, whose rows were derived under another
    /// order, whose rows another derivation wrote, or whose construction never
    /// finished, is rebuilt from zero, and [`Store::open_outcome`] says whether
    /// that happened and why.
    ///
    /// A store the open created or rebuilt is under construction
    /// ([`Store::under_construction`]): the caller derives the vault into it and
    /// ends the build with [`Store::finish_construction`], and a store whose
    /// build never ended is rebuilt by the next open rather than resumed.
    pub fn open(
        path: impl AsRef<Path>,
        order: StoredPathOrder,
        derivation: DerivationVersion,
    ) -> Result<Self, StoreError> {
        Self::open_in_mode(path.as_ref(), StoreMode::Durable, order, derivation)
    }

    /// Open, or create, a throwaway store at `path`.
    ///
    /// The same open in every respect but one: the file is removed when the
    /// store is closed or dropped, so a test's derived state does not
    /// accumulate on disk.
    ///
    /// Opening one **over a durable store is refused.** The mode is recorded in
    /// the database when it is created, so a throwaway open over a registered
    /// vault's derived state can be told apart from a throwaway open over its own
    /// leftovers — and the alternative is a teardown that deletes a vault's whole
    /// derived state on drop, silently, because a caller passed the wrong path.
    pub fn open_throwaway(
        path: impl AsRef<Path>,
        order: StoredPathOrder,
        derivation: DerivationVersion,
    ) -> Result<Self, StoreError> {
        Self::open_in_mode(path.as_ref(), StoreMode::Throwaway, order, derivation)
    }

    fn open_in_mode(
        path: &Path,
        mode: StoreMode,
        order: StoredPathOrder,
        derivation: DerivationVersion,
    ) -> Result<Self, StoreError> {
        let (connection, outcome) = norn_db::open(
            path,
            &store_schema(),
            &StoreClient {
                mode,
                order,
                derivation,
            },
        )?;
        let database = Database::adopt(connection, path)?;
        refuse_loading_source(database.connection(), database.path())?;
        crate::resolve::register_functions(database.connection())?;
        let under_construction = outcome != OpenOutcome::Reused;
        Ok(Store {
            intake_unsealed: false,
            metadata_intake: false,
            count_results: Arc::new(crate::count::CountResults::for_store(
                crate::CountRetentionPolicy::default(),
                path,
            )),
            database,
            mode,
            order,
            derivation,
            outcome,
            under_construction,
            link_health_deferred: false,
            steps: None,
            torn_down: false,
        })
    }

    /// Configure immutable-count retention before minting any snapshot readers.
    pub fn set_count_retention(
        &mut self,
        policy: crate::count::CountRetentionPolicy,
    ) -> Result<(), StoreError> {
        let results = Arc::get_mut(&mut self.count_results).ok_or_else(|| StoreError::Sql {
            operation: "configuring count retention",
            condition: None,
            message: "configure retention before minting readers".to_string(),
        })?;
        *results = crate::count::CountResults::for_store(policy, self.database.path());
        Ok(())
    }

    /// Called only while the host holds this store's maintainer lock.
    pub fn sweep_count_results(&self) -> Result<usize, StoreError> {
        self.count_results.sweep()
    }

    /// Sweep residue without opening the source; exclusive maintainership is required.
    pub fn sweep_count_residue(path: &Path) -> Result<usize, StoreError> {
        crate::count::CountResults::for_store(crate::CountRetentionPolicy::default(), path).sweep()
    }

    /// List candidate stores; maintainership must precede inspection of their storage.
    pub fn count_residue_stores(root: &Path) -> Result<Vec<PathBuf>, StoreError> {
        norn_db::temporary_store_parents(root).map_err(StoreError::from)
    }

    /// The database file this store is holding.
    pub fn path(&self) -> &Path {
        self.database.path()
    }

    /// The connection this store's requests run their statements on.
    pub(crate) fn connection(&self) -> &Connection {
        self.database.connection()
    }

    /// Whether this store's file outlives it — and so whether closing or dropping
    /// the store removes the database.
    ///
    /// It is the mode the open settled on rather than the one the caller asked
    /// for: the mode is recorded in the database, and a throwaway open over a
    /// durable store is refused rather than adopted, so a registered vault's
    /// derived state cannot be armed for teardown by a process that opened it
    /// casually.
    pub fn mode(&self) -> StoreMode {
        self.mode
    }

    /// Whether the build that fills this store has not ended.
    ///
    /// A store is under construction from the transaction that creates it, so
    /// every open that created or rebuilt the database says so, and one that
    /// reused a finished store does not.
    pub fn under_construction(&self) -> bool {
        self.under_construction
    }

    /// Defer link health until the from-zero build's complete judgment.
    ///
    /// This in-memory deferral is independent of the construction mark: ordinary
    /// stores and disposable early-read sources keep per-changeset judgment.
    /// Repeating it during a build is harmless; a finished store refuses it.
    pub fn defer_link_health(&mut self) -> Result<(), StoreError> {
        if !self.under_construction {
            return Err(StoreError::Lifecycle {
                operation: "deferring link health",
                path: self.path().to_path_buf(),
                message: "a finished store cannot defer link health".into(),
            });
        }
        self.link_health_deferred = true;
        Ok(())
    }

    /// Whether the from-zero build still owes its complete link-health judgment.
    pub fn link_health_deferred(&self) -> bool {
        self.link_health_deferred
    }

    /// End construction after full text is exhausted and link health is judged.
    ///
    /// For a stepped build, one transaction restores any absent deferred index,
    /// installs the full-text triggers,
    /// verifies the recorded schema digest, and clears the construction mark.
    /// Rung 3 verifies integrity before this call. After commit, restore the opened
    /// WAL autocheckpoint and truncate the WAL; a busy checkpoint is evidence,
    /// not a failure. Neither synchronous nor cache size changes.
    ///
    /// Stores whose steps never began only delete the mark, as ordinary oracles
    /// and disposable sources do. Neither epoch nor write generation moves.
    pub fn finish_construction(&mut self) -> Result<crate::ConstructionFinish, StoreError> {
        #[cfg(feature = "induced-failure")]
        let _timing = norn_db::profiling::span("store.step.finish");
        if self.link_health_deferred {
            return Err(crate::construction::refused(
                self,
                "finishing construction",
                "the build still owes its link-health judgment",
            ));
        }
        if self
            .steps
            .as_ref()
            .is_some_and(|steps| steps.step != crate::construction::Step::FullTextComplete)
        {
            return Err(crate::construction::refused(
                self,
                "finishing construction",
                "full text is not exhausted",
            ));
        }
        let transaction = self
            .database
            .immediate_transaction("opening the construction-finish transaction")?;
        if self.steps.is_some() {
            for (name, _) in ddl::construction::DEFERRED {
                let exists: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'index' AND name = ?1)", [name], |r| r.get(0)).map_err(|e| error::sql("checking deferred index", e))?;
                if !exists {
                    transaction
                        .execute_batch(&ddl::construction::statement(name))
                        .map_err(|e| error::sql("restoring deferred index", e))?;
                }
            }
            for statement in ddl::fts::statements()
                .into_iter()
                .filter(|sql| sql.starts_with("CREATE TRIGGER "))
            {
                transaction
                    .execute_batch(&statement)
                    .map_err(|e| error::sql("installing full-text triggers", e))?;
            }
            let recorded: Option<String> = meta::get_meta(&transaction, meta::SCHEMA_DIGEST)?;
            let actual = norn_db::schema_digest(&transaction)
                .map_err(|e| error::sql("checking the finished schema", e))?;
            if recorded.as_deref() != Some(actual.as_str()) {
                return Err(StoreError::Damaged {
                    what: "the finished construction schema differs from its recorded digest"
                        .into(),
                });
            }
        }
        meta::delete_meta(&transaction, ddl::meta::UNDER_CONSTRUCTION)?;
        transaction
            .commit()
            .map_err(|e| error::sql("finishing construction", e))?;
        self.under_construction = false;
        if self.steps.is_none() {
            return Ok(crate::ConstructionFinish::default());
        }
        // The build has finished once the mark is gone, so the steps end before
        // anything below can fail: a finished store never reads as one whose
        // build began and failed.
        let restored = self.restore_construction_settings();
        self.steps = None;
        restored?;
        self.connection()
            .execute_batch(
                "DROP TABLE temp.construction_progress; DROP TABLE temp.construction_spellings",
            )
            .map_err(|e| error::sql("dropping construction progress", e))?;
        let busy: bool = self
            .connection()
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .map_err(|e| error::sql("checkpointing construction", e))?;
        Ok(crate::ConstructionFinish {
            checkpoint_busy: busy,
        })
    }

    /// How the open ended up with this database.
    pub fn open_outcome(&self) -> &OpenOutcome {
        &self.outcome
    }

    /// The case behaviour every row this store holds was derived under: the
    /// order the vault root was proven to have when the store was opened.
    ///
    /// **It is a rebuild input.** Two things the rows hold follow it: document
    /// identity — which spellings are one row, so `Foo.md` and `foo.md` are one
    /// document where the root folds ASCII case and two where it tells them
    /// apart — and the key space a finding's classes are filed in. Neither is
    /// something a later derivation converges, so the store records the order
    /// and an open under another one rebuilds from zero: rows that root could
    /// not have produced are never served under it. A heal's merge is not among
    /// them; it pages rows under its own walk's order. A store that records no
    /// order is rebuilt the same way, since nothing vouches for its rows.
    pub fn path_order(&self) -> StoredPathOrder {
        self.order
    }

    /// The derivation every row this store holds was written by: the version
    /// the store was opened under.
    ///
    /// **It is a rebuild input**, and it is the one no other input sees: a
    /// derivation that writes different rows for the same bytes leaves the DDL
    /// fingerprint and the path order where they were, and an increment derives
    /// a file again only when its bytes move. So the store records the version
    /// and an open under another one rebuilds from zero, and a store that
    /// records none, or records a value no build writes, is rebuilt the same way.
    /// The version is the deriver's; the store knows nothing of what changed.
    pub fn derivation_version(&self) -> DerivationVersion {
        self.derivation
    }

    /// The rebuild a vault root proven to have `proven`'s case behaviour owes
    /// this store: the reason an open under `proven` rebuilds it for, or `None`
    /// where its rows were derived under that order.
    ///
    /// An open judges this for itself. This is the judgment for a store that is
    /// already open when its root's case behaviour is proven again, which is
    /// what a recovery that installs coverage anew does.
    pub fn path_order_moved(&self, proven: StoredPathOrder) -> Option<RebuildReason> {
        path_order_rebuild(
            Some(&Recorded::Text(self.order.as_str().to_string())),
            proven,
        )
        .map(|detail| RebuildReason::Client { detail })
    }

    /// The identity this database carries from creation to discard.
    ///
    /// **Progress recorded against one epoch is not valid in the next.** A write
    /// generation orders the writes of one database; the epoch says which
    /// database those generations belong to. So a consumer that keeps a change-
    /// feed cursor keeps this beside it, and a cursor whose epoch is not the
    /// store's names a position in a database that no longer exists — the answer
    /// there is a rescan from the start of the feed, never a seek.
    ///
    /// It is minted at create and never rewritten, so an epoch that moved is a
    /// database that was discarded and built again. Every route to that is one
    /// act — heal rung 3 at open, [`Store::discard_and_reopen`] for damage found
    /// later — and each of them re-runs create, so a new epoch follows a rebuild
    /// rather than being arranged for it.
    ///
    /// The value is 128 random bits, and it is opaque: it is compared against a
    /// recorded one and never read for what it is made of. Two epochs are equal
    /// when they name one database lifetime, and nothing else is asked of them.
    ///
    /// The layer that records one is the first lane-2 engine, which is the layer
    /// that keeps a change-feed cursor. Nothing in the current call graph reads
    /// this: the only reader of a feed today is the equivalence comparator, and
    /// a comparator holds its positions no further than the drain it took them
    /// in.
    pub fn epoch(&self) -> &str {
        self.database.epoch()
    }

    /// Mint the read-only snapshot handle this store's reads run on.
    ///
    /// **The mint is fallible and it is the only one.** A second connection is
    /// opened over the file this store is holding, with the read-only open
    /// flag — which no statement run on the connection can withdraw, and which
    /// is what refuses every write — and with `query_only` and a statement
    /// authorizer on top of it, so a caller that gets a handle has one that
    /// can read and cannot derive, and a caller that gets an error has a store
    /// whose reads cannot be served rather than a handle that answers nothing.
    /// The open reaches the filesystem, which is why it can fail: the file may
    /// be gone, the environment may refuse it, and a database that is not in
    /// write-ahead logging is refused rather than read under a mode its writer
    /// is not using.
    ///
    /// The application-defined functions the find builder's statements call —
    /// the path-glob match and the ambiguity-ignore test — are registered on the
    /// connection here, before any statement runs on it, so every snapshot the
    /// handle establishes has them. The writer's connection carries the
    /// ambiguity-ignore test too, registered where the store opens, because a
    /// class read runs there as well.
    ///
    /// It is taken from a **live** store, and that is what binds the handle:
    /// the writer holds the file open, so the `-shm` a read-only write-ahead
    /// logging open needs is there to be read. Nothing here concludes a heal
    /// rung — the open bypasses inspection and rebuild entirely — because a
    /// reader reads derived state and never decides what it means.
    ///
    /// **It reports what it ran against the database beside the handle**, and
    /// it reports it whichever way it ended: a caller that holds a lock across
    /// the mint answers for those statements, and a mint that refused held
    /// that lock for the ones it ran before it refused.
    pub fn open_reader(&self) -> ReaderMint {
        if self.intake_unsealed {
            return ReaderMint {
                reader: Err(StoreError::Sql {
                    operation: "opening experimental metadata before sealing",
                    condition: None,
                    message: "seal metadata indexes before minting readers".into(),
                }),
                statements: 0,
            };
        }
        let path = self.database.path().to_path_buf();
        let opened = norn_db::open_read_only(&path);
        let reader = opened
            .adopted
            .map_err(StoreError::from)
            .and_then(|database| {
                crate::read::register_functions(database.connection())?;
                crate::resolve::register_functions(database.connection())?;
                // The connection counts from here, as it becomes a snapshot
                // reader's: the open's statements above are the mint's, and
                // are carried on the mint rather than on the count.
                database.count_statements_begun();
                Ok(database)
            })
            .map(|database| SnapshotReader {
                intake_file: None,
                grant_policy: GrantPolicy::CompleteRequest,
                budget: None,
                pool: None,
                source: None,
                count_results: Arc::clone(&self.count_results),
                epoch: database.epoch().to_string(),
                order: self.order,
                connection: Mutex::new(Some(database)),
                returned: Condvar::new(),
            });
        ReaderMint {
            reader,
            statements: opened.statements,
        }
    }

    /// Open a request. Everything the store does happens inside one, so that
    /// every derivation is attributable to the request that caused it.
    pub fn begin_request(&mut self) -> Request<'_> {
        Request::new(self)
    }

    /// Hold every copy of a finding's rules equal to the one the wire reports.
    ///
    /// A finding's rules are the set its row cites, read back off the set's
    /// spelling; `finding_rules` holds them a second time, one row per rule,
    /// for a validate selecting by rule to seek. Nothing structural keeps the
    /// two equal after the write, so each way they can come apart is damage
    /// here: a spelling that is no set this crate wrote — not JSON, empty, out
    /// of order, named twice or spelled other than canonically — a set held
    /// under another fingerprint than a finding citing it, and a finding whose
    /// rule rows are not exactly the names of its set, which includes rule rows
    /// for a finding citing no set and a set's name with no row. And a set no
    /// finding cites, which `findings_collect_rule_set` deletes with its last
    /// citer, so only a write out of band leaves one standing.
    fn verify_rule_sets(&self) -> Result<(), StoreError> {
        let count = |sql: &str, operation: &'static str| -> Result<i64, StoreError> {
            self.connection()
                .query_row(sql, [], |row| row.get(0))
                .map_err(|error| error::sql(operation, error))
        };
        let unreadable = count(
            "SELECT count(*) FROM rule_sets WHERE json_valid(rules) = 0",
            "checking the rule sets' spellings",
        )?;
        if unreadable != 0 {
            return Err(StoreError::Damaged {
                what: format!("{unreadable} rule sets are spelled as something other than JSON"),
            });
        }
        let walked = self
            .connection()
            .prepare(&format!(
                "SELECT rs.id, {} FROM rule_sets AS rs
                 LEFT JOIN json_each(rs.rules) AS j ORDER BY rs.id",
                crate::rule_set::walked_columns("rs", "j")
            ))
            .and_then(|mut read| {
                read.query_map([], |row| crate::rule_set::walked(row, 0, 1))?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .map_err(|error| error::sql("reading the rule sets", error))?;
        crate::rule_set::owned_sets(walked)?;

        let elsewhere = count(
            "SELECT count(*) FROM findings AS f JOIN rule_sets AS rs ON rs.id = f.rule_set
             WHERE rs.vault_schema_fingerprint IS NOT f.vault_schema_fingerprint",
            "checking the fingerprint a rule set is held under",
        )?;
        if elsewhere != 0 {
            return Err(StoreError::Damaged {
                what: format!(
                    "{elsewhere} findings cite a rule set held under another fingerprint"
                ),
            });
        }

        let unheld = count(
            "SELECT count(*) FROM finding_rules AS fr JOIN findings AS f ON f.id = fr.finding
             WHERE NOT EXISTS (
                 SELECT 1 FROM rule_sets AS rs, json_each(rs.rules) AS j
                 WHERE rs.id = f.rule_set AND j.value = fr.rule
             )",
            "checking the rule rows against their finding's set",
        )?;
        let unrowed = count(
            "SELECT count(*) FROM findings AS f JOIN rule_sets AS rs ON rs.id = f.rule_set,
                 json_each(rs.rules) AS j
             WHERE NOT EXISTS (
                 SELECT 1 FROM finding_rules AS fr WHERE fr.finding = f.id AND fr.rule = j.value
             )",
            "checking a finding's set against its rule rows",
        )?;
        if unheld + unrowed != 0 {
            return Err(StoreError::Damaged {
                what: format!(
                    "{unheld} rule rows name a rule their finding's rule set does not hold, and \
                     {unrowed} rules a finding's set holds have no rule row"
                ),
            });
        }

        let uncited = count(
            "SELECT count(*) FROM rule_sets AS rs
             WHERE NOT EXISTS (SELECT 1 FROM findings AS f WHERE f.rule_set = rs.id)",
            "checking every rule set is cited",
        )?;
        if uncited != 0 {
            return Err(StoreError::Damaged {
                what: format!("{uncited} rule sets stand that no finding cites"),
            });
        }
        Ok(())
    }

    /// Check the database against itself, and report the first way it is not
    /// consistent.
    ///
    /// Ten checks, because a store has ten kinds of consistency to lose:
    /// the pages themselves, the foreign keys that carry cascade deletion, the
    /// full-text index against the column it is an index of, the frontmatter
    /// projection against being JSON at all, the closed vocabularies against
    /// the values a reader will accept, the rule rows against the findings
    /// whose key they copy, the rule sets against the findings citing them
    /// (`verify_rule_sets`: each set's spelling, the fingerprint it
    /// is held under, its names against its citers' rule rows, and a set no
    /// finding cites), the document and tombstone pillars against each
    /// other, each content-quarantined place's hash against the finding it
    /// stands beside, and each document's sub-fingerprints against the
    /// columns they are hashes of. The third is what an external-content FTS5
    /// table can lose without anything else noticing, which is exactly why the
    /// index is maintained by triggers — and it is asked at **rank 1**, which
    /// checks the index against `documents.body` rather than only against
    /// itself. The fourth is the projection's own claim, checked by the JSON1
    /// reader that will be asked to query it. The fifth closes the gap between
    /// "the doctor says healthy" and a read that fails: a value outside a
    /// closed vocabulary is damage the reader reports, so the verification has
    /// to see it too. The sixth is the copy of a finding's key each rule row
    /// carries, which the write takes from the finding's own row and nothing
    /// structural keeps equal after it; a rule row that disagrees would page
    /// its finding under another kind or place. The seventh holds the names
    /// a finding reports, read off its set, equal to the rule rows a validate
    /// selects it by. The eighth is the disjointness the
    /// `tombstones_clear_on_derive` trigger maintains — nothing structural
    /// holds it, so it is checked at rest rather than trusted, the same
    /// ruling the vocabularies get. The ninth is the same ruling over the
    /// trigger `content_quarantines_end_with_their_finding`: a
    /// content-quarantined place's hash stands only beside a content finding
    /// at that place, where no document row stands.
    ///
    /// The tenth is a **recompute at rest**, for the reason the stored suffix
    /// key gets one: a sub-fingerprint is a derived column, and every read that
    /// would notice one drifting from the column it hashes is a read that has
    /// already trusted it. A change-feed consumer triages on these values and
    /// fetches nothing where they match, so a body hash that stopped describing
    /// its body is a document that stops being re-derived and reports nothing
    /// wrong. The check reads one row at a time, so what it costs in memory is
    /// one document rather than the table.
    ///
    /// This is a maintenance act rather than a request: it derives nothing, so
    /// it moves no counter.
    ///
    /// **A failure here is not automatically damage.** The full-text check is a
    /// write, so it fails when the database cannot be written to at all — and a
    /// verdict of `Damaged` is what authorizes discarding the database. Only a
    /// driver error that describes the file's own contents is reported as damage;
    /// everything else is reported as the refused operation it was.
    pub fn verify_integrity(&self) -> Result<(), StoreError> {
        let report: String = self
            .connection()
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(|error| error::sql("checking database integrity", error))?;
        if report != "ok" {
            return Err(StoreError::Damaged { what: report });
        }

        let orphans: i64 = self
            .connection()
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .map_err(|error| error::sql("checking foreign keys", error))?;
        if orphans != 0 {
            return Err(StoreError::Damaged {
                what: format!("{orphans} rows reference a row that is not there"),
            });
        }

        self.connection()
            .execute(
                "INSERT INTO documents_fts(documents_fts, rank) VALUES ('integrity-check', 1)",
                [],
            )
            .map_err(|error| {
                if norn_db::is_damaged(&error) {
                    StoreError::Damaged {
                        what: format!(
                            "the full-text index disagrees with the documents it indexes: {error}"
                        ),
                    }
                } else {
                    error::sql("checking the full-text index", error)
                }
            })?;

        let unreadable: i64 = self
            .connection()
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

        for (table, column, vocabulary) in [
            (
                "links",
                "family",
                quoted(LinkFamily::ALL.iter().map(|value| value.as_str())),
            ),
            (
                "links",
                "address",
                quoted(LinkAddressKind::ALL.iter().map(|value| value.as_str())),
            ),
            (
                "document_tags",
                "source",
                quoted(TagSource::ALL.iter().map(|value| value.as_str())),
            ),
            (
                "tombstones",
                "provenance",
                quoted(Provenance::ALL.iter().map(|value| value.as_str())),
            ),
            (
                "findings",
                "kind",
                quoted(FindingKind::ALL.iter().map(FindingKind::as_str)),
            ),
            (
                "findings",
                "severity",
                quoted(Severity::ALL.iter().map(Severity::as_str)),
            ),
            (
                "finding_rules",
                "kind",
                quoted(FindingKind::ALL.iter().map(FindingKind::as_str)),
            ),
            (
                "finding_rules",
                "severity",
                quoted(Severity::ALL.iter().map(Severity::as_str)),
            ),
        ] {
            let outside: i64 = self
                .connection()
                .query_row(
                    &format!("SELECT count(*) FROM {table} WHERE {column} NOT IN ({vocabulary})"),
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error::sql("checking a closed vocabulary", error))?;
            if outside != 0 {
                return Err(StoreError::Damaged {
                    what: format!(
                        "{outside} rows hold a `{table}.{column}` outside the values this schema \
                         writes"
                    ),
                });
            }
        }

        // A rule row carries its finding's key so a validate by rule seeks it
        // in the findings' order; a copy that disagrees with its finding
        // would page that finding under another kind, severity or place.
        let strayed: i64 = self
            .connection()
            .query_row(
                "SELECT count(*) FROM finding_rules AS fr JOIN findings AS f ON f.id = fr.finding
                 WHERE fr.vault_schema_fingerprint IS NOT f.vault_schema_fingerprint
                    OR fr.kind IS NOT f.kind OR fr.severity IS NOT f.severity
                    OR fr.path IS NOT f.path OR fr.position IS NOT f.position",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error::sql("checking the rules findings cite", error))?;
        if strayed != 0 {
            return Err(StoreError::Damaged {
                what: format!("{strayed} rule rows disagree with the finding they cite a rule of"),
            });
        }
        self.verify_rule_sets()?;

        let undead: i64 = self
            .connection()
            .query_row(
                "SELECT count(*) FROM tombstones JOIN documents USING (path)",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error::sql("checking the pillars are disjoint", error))?;
        if undead != 0 {
            return Err(StoreError::Damaged {
                what: format!("{undead} tombstones stand at a path that holds a document row"),
            });
        }

        let unfounded: i64 = self
            .connection()
            .query_row(
                &format!(
                    "SELECT count(*) FROM content_quarantines
                     WHERE EXISTS (SELECT 1 FROM documents WHERE documents.path = content_quarantines.path)
                        OR NOT EXISTS (
                            SELECT 1 FROM findings
                            WHERE findings.path = content_quarantines.path AND findings.kind = '{}'
                        )",
                    crate::increment::CONTENT_FINDING_KIND.as_str()
                ),
                [],
                |row| row.get(0),
            )
            .map_err(|error| error::sql("checking each quarantined hash's finding", error))?;
        if unfounded != 0 {
            return Err(StoreError::Damaged {
                what: format!(
                    "{unfounded} quarantined hashes stand without their content finding or beside \
                     a document row"
                ),
            });
        }

        self.recompute_the_sub_fingerprints()
    }

    /// Recompute every document's sub-fingerprints from the columns they are
    /// hashes of, and report the first row that disagrees.
    ///
    /// One row at a time, and nothing accumulates: a body is read, hashed and
    /// dropped before the next row is asked for, so the check costs the widest
    /// document rather than the table.
    fn recompute_the_sub_fingerprints(&self) -> Result<(), StoreError> {
        let operation = "recomputing a document's sub-fingerprints";
        let mut statement = self
            .connection()
            .prepare(
                "SELECT path, body, body_hash, frontmatter, frontmatter_projection_hash
                 FROM documents WHERE body IS NOT NULL",
            )
            .map_err(|error| error::sql(operation, error))?;
        let mut rows = statement
            .query([])
            .map_err(|error| error::sql(operation, error))?;
        let read = |row: &rusqlite::Row<'_>| -> rusqlite::Result<RecomputedRow> {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        };
        while let Some(row) = rows.next().map_err(|error| error::sql(operation, error))? {
            let (path, body, body_hash, frontmatter, projection_hash) =
                read(row).map_err(|error| error::sql(operation, error))?;
            if hash::sub_fingerprint(&body) != body_hash {
                return Err(StoreError::Damaged {
                    what: format!("`{path}` records a body hash its stored body does not produce"),
                });
            }
            let agrees = match (&frontmatter, &projection_hash) {
                (None, None) => true,
                (Some(projection), Some(recorded)) => {
                    hash::sub_fingerprint(projection) == *recorded
                }
                // The `CHECK` on the table refuses this pair, so reaching it is
                // damage rather than a shape a write produces.
                (Some(_), None) | (None, Some(_)) => false,
            };
            if !agrees {
                return Err(StoreError::Damaged {
                    what: format!(
                        "`{path}` records a frontmatter projection hash its stored projection does \
                         not produce"
                    ),
                });
            }
        }
        Ok(())
    }

    /// What store schema this database records having been written under.
    ///
    /// The three values every open compares against this build: the pinned
    /// version, the fingerprint of the statement list, and the digest of the
    /// schema the database held when it was created. Open reads those rows for
    /// itself and rebuilds from zero where any of them disagrees, so this
    /// reports what an already-open store settled on rather than deciding it.
    pub fn recorded_store_schema(&self) -> Result<RecordedStoreSchema, StoreError> {
        Ok(RecordedStoreSchema {
            version: norn_db::meta::get_meta(self.connection(), meta::STORE_SCHEMA_VERSION)?,
            ddl_fingerprint: norn_db::meta::get_meta(self.connection(), meta::DDL_FINGERPRINT)?,
            schema_digest: norn_db::meta::get_meta(self.connection(), meta::SCHEMA_DIGEST)?,
        })
    }

    /// The digest of the schema this database actually holds, right now.
    ///
    /// Taken over `sqlite_schema`, so it answers what is there rather than what a
    /// build would have created. Compared against
    /// [`RecordedStoreSchema::schema_digest`] on every open.
    pub fn schema_digest(&self) -> Result<String, StoreError> {
        norn_db::schema_digest(self.connection())
            .map_err(|error| error::sql("digesting the schema", error))
    }

    /// Close the store, tearing a throwaway one down.
    ///
    /// A throwaway store tears itself down when dropped as well; this is the
    /// same teardown with its failures reported rather than swallowed.
    pub fn close(mut self) -> Result<(), StoreError> {
        if self.mode == StoreMode::Throwaway {
            norn_db::remove_database(self.database.path())?;
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
        norn_db::remove_database(self.database.path())?;
        self.torn_down = true;
        Ok(())
    }

    /// Discard the database at `path`, and every sidecar a journal leaves
    /// beside it, without opening it.
    ///
    /// For derived state nothing is going to open again: the vault it was
    /// derived from is no longer served. The caller holds the maintainer lock
    /// over the directory, so no store is open on the file. A database that is
    /// not there is already discarded.
    pub fn discard_at(path: &Path) -> Result<(), StoreError> {
        Ok(norn_db::remove_database(path)?)
    }

    /// Discard the database and open a store on the one that replaces it — heal
    /// rung 3 for damage found after the open.
    ///
    /// The open resolves damage it finds for itself, so this is the spelling
    /// for damage a *later* operation met: a page that read corrupt under a
    /// warm increment, a full-text index that stopped agreeing with the column
    /// it indexes, a value outside a closed vocabulary. None of those are
    /// visible to an open, which reads the store schema and nothing else.
    ///
    /// Consuming the store is what makes the order safe, and the order is
    /// unlink-then-close: [`Store::discard`] removes the file and its sidecars
    /// while this connection is still open, and the connection closes when that
    /// call drops the store it consumed. The unlink is safe under POSIX inode
    /// semantics — the open connection keeps the discarded pages reachable to
    /// itself alone, so a close-time checkpoint writes to an inode nothing can
    /// name and creates no `-wal` or `-shm` at the paths the replacement will
    /// use. The reopen below runs strictly after that close, so the store handed
    /// back is the same path in the same mode with no sidecar of the discarded
    /// database beside it, holding what a fresh create holds: **nothing**.
    /// Everything the discarded database held is derived state, and the caller's
    /// next act is deriving it again from the vault.
    ///
    /// The replacement is a create, so it is under construction until the
    /// caller's build ends it ([`Store::finish_construction`]).
    ///
    /// The store handed back is opened under `order`, which is the case
    /// behaviour its root is proven to have now: a store rebuilt because that
    /// behaviour moved is handed the new one, and one rebuilt for damage is
    /// handed the order it already had. It is opened under the derivation
    /// version this store was, because the deriver's version does not move
    /// while the build that names it runs. Retention configuration survives;
    /// result ownership belongs to the replacement rather than the discarded store.
    pub fn discard_and_reopen(self, order: StoredPathOrder) -> Result<Self, StoreError> {
        let path = self.database.path().to_path_buf();
        let mode = self.mode;
        let derivation = self.derivation;
        let policy = self.count_results.policy();
        self.discard()?;
        let mut replacement = Self::open_in_mode(&path, mode, order, derivation)?;
        replacement.set_count_retention(policy)?;
        Ok(replacement)
    }
}

/// One row the sub-fingerprint recompute reads: the path a failure names, then
/// each hashed column beside the hash it has to produce.
type RecomputedRow = (String, String, String, Option<String>, Option<String>);

/// What a database says about the store schema it was written under.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedStoreSchema {
    pub version: Option<i64>,
    pub ddl_fingerprint: Option<String>,
    pub schema_digest: Option<String>,
}

impl Drop for Store {
    fn drop(&mut self) {
        if self.mode == StoreMode::Throwaway && !self.torn_down {
            // Best effort: a drop has nobody to report to, and the alternative
            // to a silent failure here is a temporary database that outlives
            // every process that could have removed it. `close` is the spelling
            // that reports.
            let _ = norn_db::remove_database(self.database.path());
        }
    }
}

/// The statement list this build writes, and what the ceremony calls it.
///
/// The version is pinned and the ceremony takes the DDL fingerprint over the
/// list, so a DDL edit moves the fingerprint and an open resolves it by
/// rebuilding.
fn store_schema() -> norn_db::Schema {
    norn_db::Schema {
        operations: norn_db::schema_operations!("store schema"),
        version: ddl::STORE_SCHEMA_VERSION,
        statements: ddl::statements(),
    }
}

/// What the open ceremony asks this crate, once it has judged the mechanics.
///
/// Four keys are the store's own: the mode, which decides whether the file
/// outlives the handle, the path order every row was derived under, the
/// derivation version every row was written by, and the write generation every
/// derivation draws its stamp from. The ceremony writes none of them and reads
/// none of them — it hands the create transaction over for them, and hands the
/// database over to be judged by them.
struct StoreClient {
    mode: StoreMode,
    order: StoredPathOrder,
    derivation: DerivationVersion,
}

/// A value one of the store's own `meta` keys holds, as an open reads it back:
/// the text every build writes there, or the SQLite type of a value no build
/// writes.
///
/// A key's reconciliation reads a value of another type the way it reads a
/// spelling it does not recognize, so a row holding one is judged rather than
/// failing the open that reads it.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Recorded {
    Text(String),
    OtherType(&'static str),
}

impl Recorded {
    /// The value `key` holds, or `None` where the row is absent.
    fn read(connection: &Connection, key: &str) -> Result<Option<Recorded>, StoreError> {
        let value = norn_db::meta::get_meta::<Value>(connection, key)?;
        Ok(value.map(|value| match value {
            Value::Text(text) => Recorded::Text(text),
            Value::Null => Recorded::OtherType("null"),
            Value::Integer(_) => Recorded::OtherType("integer"),
            Value::Real(_) => Recorded::OtherType("real"),
            Value::Blob(_) => Recorded::OtherType("blob"),
        }))
    }

    /// The text, where the value is text.
    fn text(&self) -> Option<&str> {
        match self {
            Recorded::Text(text) => Some(text),
            Recorded::OtherType(_) => None,
        }
    }
}

/// Judge the order a store records against the one its root proves: the
/// reason the store owes a rebuild from zero, naming what it records and what
/// the root proves, or `None` where its rows were derived under that order.
///
/// A store that records no order owes the rebuild too: nothing says which
/// order its rows were derived under, so nothing says they are rows the root
/// could have produced. So does a recorded spelling no build writes, and a
/// recorded value that is not text.
///
/// One judgment for both occasions it is taken on: an open over a database
/// the mechanics call usable, and a store already open whose root's case
/// behaviour is proven again.
fn path_order_rebuild(recorded: Option<&Recorded>, proven: StoredPathOrder) -> Option<String> {
    let proven_spelling = proven.as_str();
    match recorded {
        Some(Recorded::Text(recorded))
            if StoredPathOrder::from_recorded(recorded) == Some(proven) =>
        {
            None
        }
        Some(Recorded::Text(recorded)) => Some(format!(
            "the store's rows were derived under the `{recorded}` path order and its root proves \
             `{proven_spelling}`"
        )),
        Some(Recorded::OtherType(kind)) => Some(format!(
            "the store records its path order as a value of type `{kind}`, which no build writes, \
             and its root proves `{proven_spelling}`"
        )),
        None => Some(format!(
            "the store records no path order its rows were derived under, and its root proves \
             `{proven_spelling}`"
        )),
    }
}

/// Judge the derivation version a store records against the one it is opened
/// under: the reason the store owes a rebuild from zero, naming what it records
/// and the version the open is under, or `None` where the same derivation wrote
/// its rows.
///
/// A store that records no version owes the rebuild too: nothing says which
/// derivation wrote its rows. So does a recorded spelling no build writes —
/// which is every text but the version's own decimal digits — and a recorded
/// value that is not text.
fn derivation_rebuild(
    recorded: Option<&Recorded>,
    opened_under: DerivationVersion,
) -> Option<String> {
    match recorded {
        Some(Recorded::Text(recorded)) if *recorded == opened_under.recorded() => None,
        Some(Recorded::Text(recorded)) => Some(format!(
            "the store's rows were written by derivation version `{recorded}` and this build \
             derives under `{opened_under}`"
        )),
        Some(Recorded::OtherType(kind)) => Some(format!(
            "the store records its derivation version as a value of type `{kind}`, which no \
             build writes, and this build derives under `{opened_under}`"
        )),
        None => Some(format!(
            "the store records no derivation version its rows were written by, and this build \
             derives under `{opened_under}`"
        )),
    }
}

impl norn_db::Client for StoreClient {
    type Error = StoreError;

    fn record(&self, transaction: &Connection) -> Result<(), StoreError> {
        norn_db::meta::put_meta(transaction, ddl::meta::STORE_MODE, self.mode.as_str())?;
        norn_db::meta::put_meta(transaction, ddl::meta::PATH_ORDER, self.order.as_str())?;
        norn_db::meta::put_meta(
            transaction,
            ddl::meta::DERIVATION_VERSION,
            self.derivation.recorded().as_str(),
        )?;
        norn_db::meta::put_meta(transaction, ddl::meta::UNDER_CONSTRUCTION, 1_i64)?;
        norn_db::meta::put_meta(transaction, meta::WRITE_GENERATION, 0_i64)?;
        Ok(())
    }

    fn unfinished_build_key(&self) -> Option<&'static str> {
        Some(ddl::meta::UNDER_CONSTRUCTION)
    }

    /// Judge the mode, path order and derivation version.
    ///
    /// Among those keys, the mode goes first because an open can refuse over it,
    /// and a refusal discards nothing: a throwaway open over a durable store
    /// derived under another order is refused, not rebuilt. The two rebuild
    /// inputs are both read, and a store where both moved is rebuilt once for a
    /// reason that names both.
    fn adopt(&self, connection: &Connection, path: &Path) -> Result<Adoption, StoreError> {
        // A loading source must be refused before either mode adoption or rebuilding.
        refuse_loading_source(connection, path)?;
        self.adopt_mode(connection, path)?;
        let details: Vec<String> = [
            construction_unfinished(connection)?,
            self.path_order_moved(connection)?,
            self.derivation_moved(connection)?,
        ]
        .into_iter()
        .flatten()
        .collect();
        Ok(if details.is_empty() {
            Adoption::Keep
        } else {
            Adoption::Rebuild {
                detail: details.join("; "),
            }
        })
    }

    /// The condition an arrangement armed for this database's creation, which
    /// is how the statement list's error path is reached at all: a create
    /// writes over whatever was on disk, so damage cannot be arranged there.
    /// The arm and the surface that arms it are this crate's, because no other
    /// crate reaches a store's database.
    #[cfg(feature = "induced-failure")]
    fn armed_failure(&self, connection: &Connection) -> Option<norn_db::rusqlite::Error> {
        crate::faults::failure_armed_for_the_store_schema(connection)
    }
}

impl StoreClient {
    /// Reconcile the mode a database records with the mode it is being opened
    /// in.
    ///
    /// A throwaway open over a durable store is refused: adopting it would arm
    /// a teardown that deletes a registered vault's whole derived state when
    /// the store drops. **A throwaway open over a `store_mode` row that is
    /// absent or unreadable is refused the same way**, and a value that is not
    /// text is unreadable. Create always records the mode as text, so a
    /// missing or unrecognized row is out-of-band tampering rather than a
    /// database this crate ever produces, and the conservative
    /// reading is the refusal: the alternative is arming delete-on-drop over a
    /// database whose own record does not say it is disposable.
    ///
    /// The other direction is adoption, and it is safe unconditionally: a
    /// durable open deletes nothing, so it takes over a throwaway store's
    /// leftovers or an unrecorded mode exactly as it takes over its own kind.
    /// Disposable derived state is rebuildable and the file is no longer
    /// anybody's to delete.
    ///
    /// **A mode this store cannot adopt is a refusal rather than a rebuild.**
    /// The database is sound and the caller asked for the wrong thing about
    /// it, so nothing is discarded.
    ///
    /// The arms are written out mode by recorded mode rather than folded
    /// behind a wildcard, so every combination of asked-for and recorded mode
    /// is a decision this code states.
    fn adopt_mode(&self, connection: &Connection, path: &Path) -> Result<(), StoreError> {
        let recorded = Recorded::read(connection, ddl::meta::STORE_MODE)?
            .as_ref()
            .and_then(Recorded::text)
            .and_then(StoreMode::from_str);
        match (self.mode, recorded) {
            (StoreMode::Throwaway, Some(StoreMode::Durable)) => Err(StoreError::Lifecycle {
                operation: "opening a throwaway store",
                path: path.to_path_buf(),
                message:
                    "the database is a durable store, and a throwaway store deletes its file when \
                          it closes"
                        .to_string(),
            }),
            (StoreMode::Throwaway, None) => Err(StoreError::Lifecycle {
                operation: "opening a throwaway store",
                path: path.to_path_buf(),
                message: "the database does not record itself as a throwaway store, and a \
                          throwaway store deletes its file when it closes"
                    .to_string(),
            }),
            (StoreMode::Throwaway, Some(StoreMode::Throwaway))
            | (StoreMode::Durable, Some(StoreMode::Throwaway))
            | (StoreMode::Durable, None) => {
                norn_db::meta::put_meta(connection, ddl::meta::STORE_MODE, self.mode.as_str())?;
                Ok(())
            }
            (StoreMode::Durable, Some(StoreMode::Durable)) => Ok(()),
        }
    }

    /// Reconcile the order a database's rows were derived under with the order
    /// its root is proven to have now.
    ///
    /// **A moved order is a rebuild, never a refusal.** Every row is a
    /// projection of the vault, so rows derived under an order the root no
    /// longer proves cost a derivation to replace — and serving them would
    /// answer with spellings merged or split the way this root does not merge
    /// or split them. A recorded spelling no build writes, a recorded value
    /// that is not text, and a database that records no order at all are the
    /// same rebuild: nothing vouches for the order their rows were derived
    /// under.
    fn path_order_moved(&self, connection: &Connection) -> Result<Option<String>, StoreError> {
        let recorded = Recorded::read(connection, ddl::meta::PATH_ORDER)?;
        Ok(path_order_rebuild(recorded.as_ref(), self.order))
    }

    /// Reconcile the derivation a database's rows were written by with the one
    /// this build derives under.
    ///
    /// **A moved derivation is a rebuild, never a refusal**, for the reason a
    /// moved order is: every row is a projection of the vault, and rows another
    /// derivation wrote would answer differently from rows this one writes for
    /// the same bytes, until each file was edited. An absent row, a spelling no
    /// build writes and a value that is not text are the same rebuild.
    fn derivation_moved(&self, connection: &Connection) -> Result<Option<String>, StoreError> {
        let recorded = Recorded::read(connection, ddl::meta::DERIVATION_VERSION)?;
        Ok(derivation_rebuild(recorded.as_ref(), self.derivation))
    }
}

/// A closed vocabulary as an SQL list of quoted literals.
fn quoted<'a>(values: impl Iterator<Item = &'a str>) -> String {
    values
        .map(|value| format!("'{value}'"))
        .collect::<Vec<String>>()
        .join(", ")
}

/// The reason the store owes a rebuild from zero because the build that filled
/// it never ended, or `None` where it carries no mark.
///
/// The mark is judged by being set: any value, of any type, is a build that did
/// not finish. This is the open's one added read of a finished store.
fn construction_unfinished(connection: &Connection) -> Result<Option<String>, StoreError> {
    Ok(
        Recorded::read(connection, ddl::meta::UNDER_CONSTRUCTION)?.map(|_| {
            "the store's construction never finished, so its rows are a partial derivation"
                .to_string()
        }),
    )
}

fn refuse_loading_source(connection: &Connection, path: &Path) -> Result<(), StoreError> {
    if meta::get_meta::<i64>(connection, ddl::meta::LOADING_SOURCE)?.is_some() {
        return Err(StoreError::Lifecycle {
            operation: "opening a sealed loading source",
            path: path.to_path_buf(),
            message: "loading projections require their owned reader and cannot be adopted".into(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use norn_testkit::scratch::Scratch;

    #[test]
    fn rebuilding_preserves_retention_configuration_without_reusing_result_ownership() {
        let scratch = Scratch::new("store-rebuild-count-policy");
        let mut store = Store::open(
            scratch.join("store.sqlite3"),
            StoredPathOrder::Sensitive,
            crate::DerivationVersion::new(1),
        )
        .unwrap();
        let policy = crate::CountRetentionPolicy {
            lifetime: std::time::Duration::from_secs(7),
            max_results: 3,
            max_result_bytes: Some(16384),
            total_bytes: Some(24576),
        };
        store.set_count_retention(policy).unwrap();
        let previous = Arc::clone(&store.count_results);
        let rebuilt = store
            .discard_and_reopen(StoredPathOrder::Sensitive)
            .unwrap();
        let actual = rebuilt.count_results.policy();
        assert_eq!(actual.lifetime, policy.lifetime);
        assert_eq!(actual.max_results, policy.max_results);
        assert_eq!(actual.max_result_bytes, policy.max_result_bytes);
        assert_eq!(actual.total_bytes, policy.total_bytes);
        assert!(!Arc::ptr_eq(&previous, &rebuilt.count_results));
    }

    /// **A reader cannot write, and it is not the writer's connection.** The
    /// open takes neither the write flag nor create, and SQLite reports the
    /// database read-only on this connection: the refusal of a write is the
    /// connection's own rather than a rule a read path has to keep, which is
    /// what makes a warm read's zero derivation structural.
    #[test]
    fn a_snapshot_refuses_a_write_and_reads_beside_the_writer() {
        let scratch = Scratch::new("norn-store-reader-refuses-writes");
        let mut store = Store::open(
            scratch.join("derived").join("store.sqlite3"),
            StoredPathOrder::Sensitive,
            crate::DerivationVersion::new(1),
        )
        .expect("a store opens");

        let reader = Arc::new(
            store
                .open_reader()
                .reader
                .expect("a live store mints a reader"),
        );
        let snapshot = reader
            .try_take()
            .expect("a handle nothing is reading holds its connection")
            .establish()
            .expect("a snapshot");
        assert!(
            snapshot
                .connection()
                .is_readonly(norn_db::rusqlite::MAIN_DB)
                .expect("SQLite reports the mode the database is open in"),
            "the snapshot's connection is open for writing"
        );
        snapshot
            .connection()
            .execute("INSERT INTO meta (key, value) VALUES ('probe', 1)", [])
            .expect_err("a read-only connection refused nothing");

        // The writer's own connection is another connection, and it still
        // writes: the refusal above is the reader's and not the file's.
        drop(snapshot);
        store
            .begin_request()
            .pin_vault_schema(b"version: 1\n", "a-fingerprint")
            .expect("the writer still writes");
    }

    /// A database discarded without being opened is gone with every sidecar
    /// beside it, and discarding one that is not there answers.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: reading the directory back.
    fn a_database_discarded_at_its_path_is_gone_with_its_sidecars() {
        let scratch = Scratch::new("norn-store-discard-at");
        let path = scratch.join("derived").join("store.sqlite3");
        let store = Store::open(
            &path,
            StoredPathOrder::Sensitive,
            crate::DerivationVersion::new(1),
        )
        .expect("a store opens");
        let wal = scratch.join("derived").join("store.sqlite3-wal");
        std::fs::write(&wal, b"").expect("a sidecar beside the database");
        drop(store);

        Store::discard_at(&path).expect("the database is discarded");
        Store::discard_at(&path).expect("an absent database is already discarded");

        assert_eq!(
            std::fs::read_dir(scratch.join("derived")).unwrap().count(),
            0,
            "the database or a sidecar stood"
        );
    }

    /// **The connection is out of the handle only while a read holds it.** A
    /// turn that establishes nothing gives it back where it drops, and a
    /// snapshot gives it back where the read ends, so the next read finds a
    /// handle it can take without waiting.
    #[test]
    fn a_turn_that_establishes_nothing_gives_the_connection_back() {
        let scratch = Scratch::new("norn-store-reader-turn");
        let store = Store::open(
            scratch.join("derived").join("store.sqlite3"),
            StoredPathOrder::Sensitive,
            crate::DerivationVersion::new(1),
        )
        .expect("a store opens");
        let reader = Arc::new(
            store
                .open_reader()
                .reader
                .expect("a live store mints a reader"),
        );

        let turn = reader.try_take().expect("a free handle hands out its turn");
        assert!(
            reader.try_take().is_none(),
            "a handle whose connection a turn holds handed out a second turn"
        );
        drop(turn);

        let snapshot = reader
            .try_take()
            .expect("the dropped turn kept the connection")
            .establish()
            .expect("a snapshot");
        assert!(
            reader.try_take().is_none(),
            "a handle whose connection a snapshot holds handed out a turn"
        );
        drop(snapshot);
        assert!(
            reader.try_take().is_some(),
            "the ended read kept the connection"
        );
    }

    /// The establishing statement is one, and it is the one the reading is
    /// read off: a deferred `BEGIN` takes no snapshot, so the generation read
    /// is what makes the snapshot real.
    #[test]
    fn establishing_a_snapshot_runs_one_statement_and_reports_the_reading() {
        let scratch = Scratch::new("norn-store-reader-establish");
        let store = Store::open(
            scratch.join("derived").join("store.sqlite3"),
            StoredPathOrder::Sensitive,
            crate::DerivationVersion::new(1),
        )
        .expect("a store opens");
        let reader = Arc::new(
            store
                .open_reader()
                .reader
                .expect("a live store mints a reader"),
        );

        let snapshot = reader
            .try_take()
            .expect("a free handle hands out its turn")
            .establish()
            .expect("a snapshot");
        assert_eq!(snapshot.counters().snapshots_opened(), 1);
        assert_eq!(
            snapshot.counters().statements_executed(),
            1,
            "establishing one snapshot ran something other than the one establishing statement"
        );
        assert_eq!(
            snapshot.reading().epoch(),
            store.epoch(),
            "the snapshot names a database its handle was not minted from"
        );
    }

    /// **SQLite counts what a handle's connection runs, on the thread that
    /// runs it.** The mint's statements run before the connection counts, so
    /// minting moves nothing. An establishment is the deferred `BEGIN` and the
    /// establishing statement, a statement a read runs on the snapshot is one
    /// more whether or not anything counted it by hand, and the drop that ends
    /// the snapshot is its `ROLLBACK`. A turn that establishes nothing ran
    /// nothing, and a statement the store's own writing connection runs is not
    /// a reader's: those are the controls, because a count that moved with
    /// every turn or every statement on the thread would say nothing about the
    /// reader.
    #[test]
    fn a_readers_connection_counts_each_statement_sqlite_runs_on_it() {
        let scratch = Scratch::new("norn-store-reader-statements-run");
        let store = Store::open(
            scratch.join("derived").join("store.sqlite3"),
            StoredPathOrder::Sensitive,
            crate::DerivationVersion::new(1),
        )
        .expect("a store opens");
        let run = SnapshotReader::statements_run_on_this_thread;
        let before = run();
        let minted = store.open_reader();
        let reader = Arc::new(minted.reader.expect("a live store mints a reader"));
        assert_eq!(
            (run() - before, minted.statements),
            (0, 2),
            "the mint's statements were counted as the reader's, or the mint carried none"
        );

        drop(reader.try_take().expect("a free handle hands out its turn"));
        assert_eq!(
            run() - before,
            0,
            "a turn that established nothing was counted as a statement"
        );

        let snapshot = reader
            .try_take()
            .expect("the dropped turn gave the connection back")
            .establish()
            .expect("a snapshot");
        let established = SNAPSHOT_ESTABLISHMENT_STATEMENTS;
        assert_eq!(
            (run() - before, established),
            (2, 2),
            "establishing ran something other than the BEGIN and the establishing statement"
        );
        last_write_generation(snapshot.connection()).expect("a generation read");
        assert_eq!(
            run() - before,
            established + 1,
            "a statement run on the snapshot and counted by nothing else is missing"
        );
        last_write_generation(store.database.connection()).expect("the writer reads its own");
        assert_eq!(
            run() - before,
            established + 1,
            "a statement on the store's own connection was counted as the reader's"
        );
        drop(snapshot);
        assert_eq!(
            run() - before,
            established + 2,
            "the ROLLBACK that ends the snapshot is missing"
        );
    }

    /// **An unwind between taking the connection and establishing gives the
    /// connection back.** A turn is the connection out of the handle and not
    /// yet inside a snapshot, so a panic there is the one moment the handle
    /// could be left empty for good — and a handle left empty is an entry
    /// every later read waits on a connection nothing holds for. The turn's
    /// drop runs on the unwinding thread, which is what puts it back.
    #[test]
    fn a_turn_an_unwind_drops_gives_the_connection_back() {
        let scratch = Scratch::new("norn-store-reader-unwind");
        let store = Store::open(
            scratch.join("derived").join("store.sqlite3"),
            StoredPathOrder::Sensitive,
            crate::DerivationVersion::new(1),
        )
        .expect("a store opens");
        let reader = Arc::new(
            store
                .open_reader()
                .reader
                .expect("a live store mints a reader"),
        );

        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _turn = reader.try_take().expect("a free handle hands out its turn");
            panic!("a read that took the connection and never established");
        }));
        assert!(
            unwound.is_err(),
            "the read that was to unwind holding the connection returned instead"
        );

        let snapshot = reader
            .try_take()
            .expect("the unwound turn kept the handle's connection")
            .establish()
            .expect("a snapshot");
        assert_eq!(
            snapshot.counters().snapshots_opened(),
            1,
            "the read after the unwind established no snapshot"
        );
    }

    /// **An unwind between an establishment's `BEGIN` and its snapshot being
    /// handed out gives the connection back, with no transaction open on
    /// it.** The connection is inside a transaction and inside no snapshot
    /// there, so a turn that let it go with the unwind would leave the handle
    /// empty for good, and one that gave it back as it stood would hand the
    /// next read a connection whose `BEGIN` refuses. The read after the unwind
    /// establishing on the same handle is what holds both.
    #[test]
    fn an_establishment_an_unwind_leaves_after_its_begin_gives_the_connection_back() {
        let scratch = Scratch::new("norn-store-reader-unwind-after-begin");
        let store = Store::open(
            scratch.join("derived").join("store.sqlite3"),
            StoredPathOrder::Sensitive,
            crate::DerivationVersion::new(1),
        )
        .expect("a store opens");
        let reader = Arc::new(
            store
                .open_reader()
                .reader
                .expect("a live store mints a reader"),
        );

        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::induced_failure::unwind_after_the_next_snapshot_opens();
            reader
                .try_take()
                .expect("a free handle hands out its turn")
                .establish()
        }));
        assert!(
            unwound.is_err(),
            "the establishment armed to unwind after its BEGIN returned instead"
        );

        let snapshot = reader
            .try_take()
            .expect("the unwound establishment kept the handle's connection")
            .establish()
            .expect("the connection came back with the unwound establishment's transaction open");
        assert_eq!(
            snapshot.counters().snapshots_opened(),
            1,
            "the read after the unwind established no snapshot"
        );
    }

    /// One document written into the store, so a read has rows to answer from.
    fn write_one_document(store: &mut Store, at: &str, body: &str) {
        let facts = crate::facts::DocumentFacts::new(
            crate::path::DocumentPath::new(at).expect("a vault-relative document path"),
            "a-content-hash",
            body,
            body.len() as u64,
        );
        store
            .begin_request()
            .apply_increment(
                crate::increment::IncrementProvenance::Derived,
                [crate::increment::Change::Upsert(facts)],
                &[],
                &crate::fields::ContentModel::none(),
            )
            .expect("a changeset that writes one document");
    }

    /// **A full-text read is judged by running it, because explaining it
    /// cannot judge it.** FTS5 runs `PRAGMA data_version` on the connection
    /// for itself, once for every `MATCH` and once for every `fts5vocab` read,
    /// so a statement authorizer that refuses that pragma refuses every
    /// full-text read this schema is built on — while `EXPLAIN QUERY PLAN`
    /// over the identical statement still returns a clean plan, because
    /// explaining a virtual table never runs it. An `EXPLAIN` bar would
    /// therefore certify a statement no read can execute. This case executes
    /// the match and the vocabulary read and asserts their rows.
    #[test]
    fn a_snapshot_runs_the_full_text_statements_this_schema_is_built_on() {
        const MATCHED: &str = "SELECT documents.path FROM documents_fts
             JOIN documents ON documents.id = documents_fts.rowid
             WHERE documents_fts MATCH ?1";

        let scratch = Scratch::new("norn-store-reader-full-text");
        let mut store = Store::open(
            scratch.join("derived").join("store.sqlite3"),
            StoredPathOrder::Sensitive,
            crate::DerivationVersion::new(1),
        )
        .expect("a store opens");
        write_one_document(&mut store, "notes/first.md", "the interloper walked in");

        let reader = Arc::new(
            store
                .open_reader()
                .reader
                .expect("a live store mints a reader"),
        );
        let snapshot = reader
            .try_take()
            .expect("a handle nothing is reading holds its connection")
            .establish()
            .expect("a snapshot");
        let connection = snapshot.connection();

        let matched: Vec<String> = connection
            .prepare(MATCHED)
            .expect("a full-text match compiles on a snapshot connection")
            .query_map(["interloper"], |row| row.get(0))
            .expect("a full-text match runs on a snapshot connection")
            .collect::<Result<_, _>>()
            .expect("the rows a full-text match answered");
        assert_eq!(
            matched,
            vec!["notes/first.md".to_string()],
            "a full-text match answered rows other than the document whose body carries the term"
        );

        let indexed: Vec<String> = connection
            .prepare("SELECT term FROM documents_fts_vocab ORDER BY term")
            .expect("a vocabulary read compiles on a snapshot connection")
            .query_map([], |row| row.get(0))
            .expect("a vocabulary read runs on a snapshot connection")
            .collect::<Result<_, _>>()
            .expect("the rows a vocabulary read answered");
        assert!(
            indexed.contains(&"interloper".to_string()),
            "the index this snapshot reads holds no term the written body carries: {indexed:?}"
        );

        // The plan for that same statement, which is what an EXPLAIN bar reads
        // and the reason the two assertions above read rows instead.
        let plan = snapshot
            .database()
            .emitted_plan(MATCHED, ["interloper"])
            .expect("the match's plan");
        assert!(
            plan.steps
                .iter()
                .any(|step| step.detail.contains("documents_fts")),
            "the plan of a full-text match names no virtual table: {:?}",
            plan.steps
        );
    }

    /// **The read surface a snapshot connection has is the set of statements
    /// that run on it.** Every shape a read builder composes is executed here
    /// and answers its row: a join, a subquery, a recursive common table
    /// expression, an aggregate with `GROUP BY` and `HAVING`, a window
    /// function, a `UNION`, ordering with a bounded page, the json1 functions
    /// a frontmatter projection is read with, `sqlite_master`, and a bound
    /// parameter. A shape the statement authorizer refuses is a surface this
    /// connection does not have, whatever a plan taken of it reports.
    #[test]
    fn a_snapshot_runs_every_shape_a_read_builder_composes() {
        let scratch = Scratch::new("norn-store-reader-shapes");
        let mut store = Store::open(
            scratch.join("derived").join("store.sqlite3"),
            StoredPathOrder::Sensitive,
            crate::DerivationVersion::new(1),
        )
        .expect("a store opens");
        write_one_document(&mut store, "notes/first.md", "the interloper walked in");

        let reader = Arc::new(
            store
                .open_reader()
                .reader
                .expect("a live store mints a reader"),
        );
        let snapshot = reader
            .try_take()
            .expect("a handle nothing is reading holds its connection")
            .establish()
            .expect("a snapshot");
        let connection = snapshot.connection();

        for (shape, sql) in [
            (
                "a join",
                "SELECT count(*) FROM documents AS outer_row
                 JOIN documents AS inner_row ON inner_row.id = outer_row.id",
            ),
            (
                "a subquery",
                "SELECT count(*) FROM documents WHERE id IN (SELECT id FROM documents)",
            ),
            (
                "a recursive common table expression",
                "WITH RECURSIVE counted(n) AS (
                     SELECT 1 UNION ALL SELECT n + 1 FROM counted WHERE n < 3
                 ) SELECT sum(n) = 6 FROM counted",
            ),
            (
                "an aggregate with GROUP BY and HAVING",
                "SELECT count(*) FROM (
                     SELECT path FROM documents GROUP BY path HAVING count(*) = 1
                 )",
            ),
            (
                "a window function",
                "SELECT count(*) FROM (SELECT row_number() OVER (ORDER BY path) FROM documents)",
            ),
            (
                "a UNION",
                "SELECT count(*) FROM (SELECT path FROM documents UNION SELECT path FROM documents)",
            ),
            (
                "an ordered bounded page",
                "SELECT count(*) FROM (SELECT path FROM documents ORDER BY path LIMIT 1 OFFSET 0)",
            ),
            (
                "the json1 functions",
                "SELECT json_valid(json_object('term', 'interloper'))",
            ),
            (
                "a read of sqlite_master",
                "SELECT count(*) > 0 FROM sqlite_master WHERE name = 'documents_fts'",
            ),
        ] {
            let answered: i64 = connection
                .query_row(sql, [], |row| row.get(0))
                .unwrap_or_else(|error| panic!("{shape} on a snapshot connection: {error}"));
            assert_eq!(answered, 1, "{shape} answered {answered}");
        }

        let bound: i64 = connection
            .query_row(
                "SELECT count(*) FROM documents WHERE path = ?1",
                ["notes/first.md"],
                |row| row.get(0),
            )
            .expect("a bound parameter on a snapshot connection");
        assert_eq!(bound, 1, "a bound parameter matched no row");
    }
}
