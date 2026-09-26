//! The store itself: what one derived database means, over the file and the
//! connection `norn-db` owns.
//!
//! # One writer per store, and the borrow checker is what enforces it
//!
//! A store holds exactly one connection, and every operation runs through a
//! request that borrows the store mutably. That is the whole of "one writer per
//! *store*": on the writer's side there is no pool, no internal lock and no
//! second connection to serialize against. The reader this module mints beside
//! it is a second connection with a lock of its own, and it writes nothing —
//! see below.
//!
//! It is **not** one writer per database file. Two `Store` values on one path are
//! two connections and two writers, serialized by SQLite's own locking rather
//! than by anything here — which means a host that opens a second store for the
//! same vault entry gets interleaving, not a refusal. The missing half is the
//! maintainer file lock taken outside the store, in the derived directory the
//! store's file sits in, which is what makes "one writer" a property of the
//! derived store rather than of the handle; it is carved and not built
//! (NORN-33).
//!
//! **Reads belong on a separate handle, not on this connection.** That is
//! recorded here because it is the other half of the writer discipline: a wire
//! read is answered from a dedicated read-only snapshot handle with its own
//! connection, taken from the substrate seam like every other, and `&mut`
//! stays what a writer takes. A shared borrow of the store itself cannot serve
//! reads, because the store lives inside an attachment that lifecycle jobs hold
//! mutably for their whole duration (ADR 0015). [`SnapshotReader`] is that
//! handle's type and [`Store::open_reader`] is its one mint: every request in
//! this crate is `&mut` against the one connection, and a reader is a second
//! connection over the same file that is opened read-only and so can only
//! read.
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

use std::cell::Cell;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};

use norn_db::rusqlite::types::Value;
use norn_db::rusqlite::{self, Connection};
use norn_db::{Adoption, Database, OpenOutcome, RebuildReason, meta};
use norn_wire::{FindingKind, Severity};

use crate::counters::SnapshotCounters;
use crate::ddl;
use crate::error::{self, StoreError};
use crate::facts::{DerivationVersion, LinkFamily, Provenance, StoredPathOrder, TagSource};
use crate::hash;
use crate::request::Request;

/// The read-only snapshot handle wire reads run on.
///
/// One handle per attached entry, and one connection inside it: a read takes
/// that connection for the length of its snapshot and gives it back when the
/// snapshot ends, so **concurrent reads of one entry serialize against each
/// other here** rather than opening a connection each.
///
/// **Taking the connection and establishing on it are two acts**, because the
/// caller holds a lock across the second and may not hold it across the first:
/// [`SnapshotReader::try_take`] answers at once with the connection or with
/// nothing, and [`SnapshotReader::wait_for_the_connection`] waits for it.
/// Either way what comes back is a [`ConnectionTurn`], and
/// [`ConnectionTurn::establish`] is the one way a snapshot is made. A turn
/// that establishes nothing gives the connection back when it drops, an
/// unwind included.
///
/// A handle is made from a live [`Store`] by [`Store::open_reader`], which is
/// what binds its lifetime: it reads the file that store is holding, and the
/// usable `-shm` a read-only write-ahead-log open needs is there because the
/// writer has it open. The connection comes from the substrate seam opened
/// **read-only** — the open flag, which no statement run on the connection can
/// withdraw, is what refuses every write, and `query_only` and the statement
/// authorizer stand on top of it — so no rung is concluded through a reader,
/// and a warm read's zero derivation counters are structural rather than a
/// rule the read paths keep.
///
/// The handle outlives the entry that minted it where a read is still running
/// against it: the entry lets its own hold go at the teardown window, and the
/// read goes on answering from the handle it took. Nothing promises the
/// database file behind it survives that teardown.
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
    /// The one connection reads answer from, held here between reads and taken
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
    /// Take this handle's connection where no read holds it. **Never waits.**
    ///
    /// This is the spelling a caller holding a lock takes: the answer is the
    /// turn or nothing, and nothing is another read holding the connection
    /// rather than a fault.
    ///
    /// The handle is taken by [`Arc`] because the turn keeps it, and the
    /// snapshot after it keeps it too: a read runs after the caller that
    /// established it has let its own hold go.
    pub fn try_take(self: &Arc<Self>) -> Option<ConnectionTurn> {
        let mut held = self
            .connection
            .lock()
            .expect("a snapshot reader's connection is poisoned");
        held.take().map(|database| ConnectionTurn {
            reader: Arc::clone(self),
            database: Some(database),
        })
    }

    /// Wait for this handle's connection, and take it.
    ///
    /// **It blocks until the read holding the connection ends.** A caller
    /// holding a lock the holding read needs in order to end would deadlock
    /// here, which is why [`SnapshotReader::try_take`] exists and why this is
    /// the spelling taken with no such lock held.
    pub fn wait_for_the_connection(self: &Arc<Self>) -> ConnectionTurn {
        let mut held = self
            .connection
            .lock()
            .expect("a snapshot reader's connection is poisoned");
        let database = loop {
            if let Some(database) = held.take() {
                break database;
            }
            held = self
                .returned
                .wait(held)
                .expect("a snapshot reader's connection is poisoned");
        };
        ConnectionTurn {
            reader: Arc::clone(self),
            database: Some(database),
        }
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
    /// through [`Database::count_statements_begun`], so every statement run on
    /// it is counted as SQLite begins it: a snapshot's `BEGIN`, its
    /// establishing statement, each statement a read builder runs on it, and
    /// the `ROLLBACK` that ends it. The mint's own statements run before the
    /// connection counts and are not among them; [`ReaderMint::statements`]
    /// carries those.
    ///
    /// **The count is the thread's, not the handle's.** A connection runs on
    /// one thread at a time and a turn is one read's, so a caller that holds a
    /// handle's turn and reads this on both sides of a stretch of its own
    /// thread has, in the difference, every statement SQLite began for it in
    /// that stretch. A caller holding a lock across the stretch learns what
    /// ran under its lock from its own two readings, however the statements
    /// were composed and whoever ran them on that thread.
    pub fn statements_run_on_this_thread() -> u64 {
        norn_db::statements_begun_on_this_thread()
    }

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

/// A minted read handle, and what minting it ran against the database.
///
/// The mint is a second open of the file a live store is holding, and it is
/// paid for under whatever lock its caller took: the count is what that caller
/// reports about the lock it held. It is carried on both answers because both
/// cost the same lock — a refusal that waited out a busy timeout and then
/// failed cost what a handle cost.
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
    /// **This waits for nothing.** The connection is already this turn's, so
    /// the act is the transaction and the statement and no acquisition, which
    /// is what makes it safe to run under a caller's lock.
    ///
    /// The connection goes back to the handle when the [`Snapshot`] is
    /// dropped, or here where the establishment refuses.
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
        let mut database = self
            .database
            .take()
            .expect("a turn holds the connection until it establishes or drops");
        let reader = Arc::clone(&self.reader);
        let mut counters = SnapshotCounters::default();
        let established = establish_on(&mut database, &reader, &mut counters);
        match established {
            Ok(reading) => Ok(Snapshot {
                order: reader.order,
                reader,
                database: Some(database),
                reading,
                counters: Cell::new(counters),
            }),
            Err(error) => {
                let _ = database.close_snapshot();
                reader.give_the_connection_back(database);
                Err(error)
            }
        }
    }
}

impl Drop for ConnectionTurn {
    fn drop(&mut self) {
        if let Some(database) = self.database.take() {
            self.reader.give_the_connection_back(database);
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
) -> Result<StoreReading, StoreError> {
    database.open_snapshot()?;
    counters.count_snapshot();
    counters.count_statement();
    let write_generation = last_write_generation(database.connection())?;
    Ok(StoreReading {
        epoch: reader.epoch.clone(),
        write_generation,
    })
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
/// snapshots on the entry's one connection that no adjudication ever saw — so
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
    reader: Arc<SnapshotReader>,
    /// The connection this snapshot is open on, taken out to be given back to
    /// the reader at the drop that ends the snapshot.
    database: Option<Database>,
    reading: StoreReading,
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
    /// The store reading this snapshot was established at: which database, and
    /// how far its writes had got.
    pub fn reading(&self) -> &StoreReading {
        &self.reading
    }

    /// The case behaviour the rows this snapshot reads were derived under.
    ///
    /// A find's `resolves` part is the one reader of it: the order selects the
    /// suffix key the resolution probes and the case its ignore globs match
    /// under. No other read builder consults it: a find's, a count's and a
    /// validate's path parts match bytes and their pages order paths the same
    /// way on every root.
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
    /// The find builder runs its statements here, and each one it runs is
    /// counted on [`Snapshot::counters`] beside the establishing one through
    /// [`Snapshot::count_statement`]. The application-defined functions those
    /// statements call are registered on this connection when its handle is
    /// minted, by [`Store::open_reader`].
    pub(crate) fn connection(&self) -> &Connection {
        self.database().connection()
    }

    /// The handle this snapshot's connection belongs to, which is what takes
    /// a plan of a statement the connection ran.
    pub(crate) fn database(&self) -> &Database {
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
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        // The connection goes back whichever way the read ended, including an
        // unwind: a handle left empty is an entry no later read reaches.
        if let Some(mut database) = self.database.take() {
            let _ = database.close_snapshot();
            self.reader.give_the_connection_back(database);
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
    /// Disposable derivation over a store that is torn down when it is closed
    /// or dropped. This is what an unregistered root gets: derived state with no
    /// promise of being there next time.
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
    pub(crate) database: Database,
    mode: StoreMode,
    order: StoredPathOrder,
    derivation: DerivationVersion,
    outcome: OpenOutcome,
    torn_down: bool,
}

impl Store {
    /// Open, or create, the durable store at `path` for a vault root proven to
    /// have `order`'s case behaviour, whose rows `derivation` writes.
    ///
    /// The parent directory is prepared if it is missing. A database that is not
    /// the shape this build writes, whose rows were derived under another
    /// order, or whose rows another derivation wrote, is rebuilt from zero, and
    /// [`Store::open_outcome`] says whether that happened and why.
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
    /// store is closed or dropped, so an unregistered root's derived state does
    /// not accumulate on disk.
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
        crate::resolve::register_functions(database.connection())?;
        Ok(Store {
            database,
            mode,
            order,
            derivation,
            outcome,
            torn_down: false,
        })
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

    /// Check the database against itself, and report the first way it is not
    /// consistent.
    ///
    /// Seven checks, because a store has seven kinds of consistency to lose:
    /// the pages themselves, the foreign keys that carry cascade deletion, the
    /// full-text index against the column it is an index of, the frontmatter
    /// projection against being JSON at all, the closed vocabularies against
    /// the values a reader will accept, the document and tombstone pillars
    /// against each other, and each document's sub-fingerprints against the
    /// columns they are hashes of. The third is what an external-content FTS5
    /// table can lose without anything else noticing, which is exactly why the
    /// index is maintained by triggers — and it is asked at **rank 1**, which
    /// checks the index against `documents.body` rather than only against
    /// itself. The fourth is the projection's own claim, checked by the JSON1
    /// reader that will be asked to query it. The fifth closes the gap between
    /// "the doctor says healthy" and a read that fails: a value outside a
    /// closed vocabulary is damage the reader reports, so the verification has
    /// to see it too. The sixth is the disjointness the
    /// `tombstones_clear_on_derive` trigger maintains — nothing structural
    /// holds it, so it is checked at rest rather than trusted, the same ruling
    /// the vocabularies get.
    ///
    /// The seventh is a **recompute at rest**, for the reason the stored suffix
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
                 FROM documents",
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
    /// The store handed back is opened under `order`, which is the case
    /// behaviour its root is proven to have now: a store rebuilt because that
    /// behaviour moved is handed the new one, and one rebuilt for damage is
    /// handed the order it already had. It is opened under the derivation
    /// version this store was, because the deriver's version does not move
    /// while the build that names it runs.
    pub fn discard_and_reopen(self, order: StoredPathOrder) -> Result<Self, StoreError> {
        let path = self.database.path().to_path_buf();
        let mode = self.mode;
        let derivation = self.derivation;
        self.discard()?;
        Self::open_in_mode(&path, mode, order, derivation)
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
        norn_db::meta::put_meta(transaction, meta::WRITE_GENERATION, 0_i64)?;
        Ok(())
    }

    /// Take the verdict over the store's own keys: the mode, then the path
    /// order and the derivation version.
    ///
    /// The mode goes first because it is the one reading an open refuses over,
    /// and a refusal discards nothing: a throwaway open over a durable store
    /// derived under another order is refused, not rebuilt. The two rebuild
    /// inputs are both read, and a store where both moved is rebuilt once for a
    /// reason that names both.
    fn adopt(&self, connection: &Connection, path: &Path) -> Result<Adoption, StoreError> {
        self.adopt_mode(connection, path)?;
        let details: Vec<String> = [
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

#[cfg(test)]
mod tests {
    use super::*;
    use norn_testkit::scratch::Scratch;

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
