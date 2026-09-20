//! The store itself: what one derived database means, over the file and the
//! connection `norn-db` owns.
//!
//! # One writer per store, and the borrow checker is what enforces it
//!
//! A store holds exactly one connection, and every operation runs through a
//! request that borrows the store mutably. That is the whole of "one writer per
//! *store*": there is no pool here, no internal lock, and no second connection
//! to serialize against.
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
//! connection over the same file that can only read.
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
//! pins, and the store's own pinned key — the mode, which decides whether the
//! file outlives the handle and is the one reading an open may refuse over.
//! What it takes back is [`OpenOutcome`]: the rung the state was at, with a
//! typed [`RebuildReason`] where the answer was a rebuild.
//!
//! **Rung 3 is for damaged state, never for a hostile environment.** A full
//! disk, a revoked permission, a parent directory that cannot be created, a
//! database somebody else holds: each of those is refused and reported, and none
//! of them discards anything. Refusing is the correct resolution when the
//! environment is broken and the stored state is not, because discarding a sound
//! database destroys work to fix nothing. One policy decides which is which —
//! [`norn_db::is_damaged`] — and every read an open performs goes through it.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use norn_db::rusqlite::{self, Connection};
use norn_db::{Adoption, Database, OpenOutcome, meta};
use norn_wire::{FindingKind, Severity};

use crate::counters::SnapshotCounters;
use crate::ddl;
use crate::error::{self, StoreError};
use crate::facts::{LinkFamily, Provenance, TagSource};
use crate::hash;
use crate::request::Request;

/// The read-only snapshot handle wire reads run on.
///
/// One handle per attached entry, and one connection inside it: a read takes
/// that connection for the length of its snapshot and gives it back when the
/// snapshot ends, so **concurrent reads of one entry serialize against each
/// other here** rather than opening a connection each. The wait that
/// serialization costs is counted rather than assumed — [`Snapshot::waits`] —
/// because measured contention is what mints more handles through this same
/// seam.
///
/// A handle is made from a live [`Store`] by [`Store::open_reader`], which is
/// what binds its lifetime: it reads the file that store is holding, and the
/// usable `-shm` a read-only write-ahead-log open needs is there because the
/// writer has it open. The connection comes from the substrate seam opened
/// read-only with `query_only` set, so **derivation is impossible on it by
/// construction** — no rung is concluded through a reader, and a warm read's
/// zero derivation counters are structural rather than a rule the read paths
/// keep.
///
/// The handle outlives the entry that minted it where a read is still running
/// against it: the entry lets its own hold go at the teardown window, and the
/// read goes on answering from the handle it took. Nothing promises the
/// database file behind it survives that teardown.
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
    /// identity is asked for while a read holds the connection.
    epoch: String,
    path: PathBuf,
}

impl fmt::Debug for SnapshotReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotReader")
            .field("path", &self.path)
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

impl SnapshotReader {
    /// Establish the write-ahead-log snapshot one read answers from, and
    /// sample the store reading it is answered under.
    ///
    /// **One statement against the database, and it is the establishing one.**
    /// A deferred `BEGIN` takes no snapshot, so the snapshot is the first
    /// statement's — and that statement is the read of the store's write
    /// generation, which is the reading the answer carries. A caller that runs
    /// this where the trust label is read gets a label and a snapshot
    /// describing one instant, and pays one statement for both.
    ///
    /// The connection is taken here and given back when the [`Snapshot`] is
    /// dropped. A read that finds another read holding it waits, and the wait
    /// is reported on the snapshot it eventually establishes.
    ///
    /// The handle is taken by [`Arc`] because the snapshot holds it: a read
    /// runs after the caller that established it has let its own hold go.
    pub fn establish(self: Arc<Self>) -> Result<Snapshot, StoreError> {
        let (mut database, waits) = self.take_the_connection();
        let mut counters = SnapshotCounters::default();
        let reading = establish_on(&mut database, &self.epoch, &mut counters);
        match reading {
            Ok(reading) => Ok(Snapshot {
                reader: self,
                database: Some(database),
                reading,
                counters,
                waits,
            }),
            Err(error) => {
                let _ = database.close_snapshot();
                self.give_the_connection_back(database);
                Err(error)
            }
        }
    }

    /// The database file this handle reads.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The database this handle reads, from its creation to its discard.
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// Take the one connection, waiting for the read that holds it, and report
    /// how many times this read waited.
    fn take_the_connection(&self) -> (Database, u64) {
        let mut held = self
            .connection
            .lock()
            .expect("a snapshot reader's connection is poisoned");
        let mut waits = 0;
        loop {
            if let Some(database) = held.take() {
                return (database, waits);
            }
            waits += 1;
            held = self
                .returned
                .wait(held)
                .expect("a snapshot reader's connection is poisoned");
        }
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

/// Open the snapshot and run the one statement that establishes it.
fn establish_on(
    database: &mut Database,
    epoch: &str,
    counters: &mut SnapshotCounters,
) -> Result<StoreReading, StoreError> {
    database.open_snapshot()?;
    counters.count_snapshot();
    let write_generation =
        norn_db::meta::get_meta::<i64>(database.connection(), norn_db::meta::WRITE_GENERATION)?
            .ok_or_else(|| StoreError::Damaged {
                what: "the database records no write generation, so no read can say what it read"
                    .to_string(),
            })?;
    counters.count_statement();
    Ok(StoreReading {
        epoch: epoch.to_string(),
        write_generation,
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
pub struct Snapshot {
    reader: Arc<SnapshotReader>,
    /// The connection this snapshot is open on, taken out to be given back to
    /// the reader at the drop that ends the snapshot.
    database: Option<Database>,
    reading: StoreReading,
    counters: SnapshotCounters,
    waits: u64,
}

impl fmt::Debug for Snapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Snapshot")
            .field("reading", &self.reading)
            .field("waits", &self.waits)
            .finish_non_exhaustive()
    }
}

impl Snapshot {
    /// The store reading this snapshot was established at: which database, and
    /// how far its writes had got.
    pub fn reading(&self) -> &StoreReading {
        &self.reading
    }

    /// The handle this snapshot is open on.
    pub fn reader(&self) -> &Arc<SnapshotReader> {
        &self.reader
    }

    /// What this read's snapshot cost: the snapshot itself, and the statements
    /// run on it.
    pub fn counters(&self) -> SnapshotCounters {
        self.counters
    }

    /// How many times establishing this snapshot waited for the one connection
    /// its reader holds. Nonzero is the contention concurrent reads of one
    /// entry pay, measured rather than assumed.
    pub fn waits(&self) -> u64 {
        self.waits
    }

    /// The connection this snapshot's statements run on.
    ///
    /// It is `query_only`, so a statement composed here derives nothing. The
    /// read builders are the layer that runs statements on it — each of them
    /// counted on [`Snapshot::counters`] beside the establishing one — and the
    /// current call graph reaches it from this crate's own cases alone, which
    /// is where the refusal a `query_only` connection makes is stated.
    #[allow(dead_code)] // Reached by the read builders; covered by this module's own cases.
    pub(crate) fn connection(&self) -> &Connection {
        self.database
            .as_ref()
            .expect("a snapshot holds its connection until it is dropped")
            .connection()
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
    ///
    /// Opening one **over a durable store is refused.** The mode is recorded in
    /// the database when it is created, so a throwaway open over a registered
    /// vault's derived state can be told apart from a throwaway open over its own
    /// leftovers — and the alternative is a teardown that deletes a vault's whole
    /// derived state on drop, silently, because a caller passed the wrong path.
    pub fn open_throwaway(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_in_mode(path.as_ref(), StoreMode::Throwaway)
    }

    fn open_in_mode(path: &Path, mode: StoreMode) -> Result<Self, StoreError> {
        let (connection, outcome) = norn_db::open(path, &store_schema(), &StoreClient { mode })?;
        Ok(Store {
            database: Database::adopt(connection, path)?,
            mode,
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
    /// opened over the file this store is holding, read-only and `query_only`,
    /// so a caller that gets a handle has one that can read and cannot derive,
    /// and a caller that gets an error has a store whose reads cannot be
    /// served rather than a handle that answers nothing. The open reaches the
    /// filesystem, which is why it can fail: the file may be gone, the
    /// environment may refuse it, and a database that is not in write-ahead
    /// logging is refused rather than read under a mode its writer is not
    /// using.
    ///
    /// It is taken from a **live** store, and that is what binds the handle:
    /// the writer holds the file open, so the `-shm` a read-only write-ahead
    /// logging open needs is there to be read. Nothing here concludes a heal
    /// rung — the open bypasses inspection and rebuild entirely — because a
    /// reader reads derived state and never decides what it means.
    pub fn open_reader(&self) -> Result<SnapshotReader, StoreError> {
        let path = self.database.path().to_path_buf();
        let connection = norn_db::connect_read_only(&path)?;
        let database = Database::adopt(connection, &path)?;
        Ok(SnapshotReader {
            epoch: database.epoch().to_string(),
            path,
            connection: Mutex::new(Some(database)),
            returned: Condvar::new(),
        })
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
    pub fn discard_and_reopen(self) -> Result<Self, StoreError> {
        let path = self.database.path().to_path_buf();
        let mode = self.mode;
        self.discard()?;
        Self::open_in_mode(&path, mode)
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
/// Two keys are the store's own: the mode, which decides whether the file
/// outlives the handle, and the write generation every derivation draws its
/// stamp from. The ceremony writes neither and reads neither — it hands the
/// create transaction over for them, and hands the database over to be
/// judged by them.
struct StoreClient {
    mode: StoreMode,
}

impl norn_db::Client for StoreClient {
    type Error = StoreError;

    fn record(&self, transaction: &Connection) -> Result<(), StoreError> {
        norn_db::meta::put_meta(transaction, ddl::meta::STORE_MODE, self.mode.as_str())?;
        norn_db::meta::put_meta(transaction, meta::WRITE_GENERATION, 0_i64)?;
        Ok(())
    }

    /// Reconcile the mode a database records with the mode it is being opened
    /// in.
    ///
    /// A throwaway open over a durable store is refused: adopting it would arm
    /// a teardown that deletes a registered vault's whole derived state when
    /// the store drops. **A throwaway open over a `store_mode` row that is
    /// absent or unreadable is refused the same way.** Create always records
    /// the mode, so a missing or unrecognized row is out-of-band tampering
    /// rather than a database this crate ever produces, and the conservative
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
    fn adopt(&self, connection: &Connection, path: &Path) -> Result<Adoption, StoreError> {
        let recorded = norn_db::meta::get_meta::<String>(connection, ddl::meta::STORE_MODE)?
            .as_deref()
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
                Ok(Adoption::Keep)
            }
            (StoreMode::Durable, Some(StoreMode::Durable)) => Ok(Adoption::Keep),
        }
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
    /// open takes neither the write flag nor create and sets `query_only` over
    /// them, so a statement that would derive is refused by the connection
    /// rather than by a rule a read path has to keep — which is what makes a
    /// warm read's zero derivation structural.
    #[test]
    fn a_snapshot_refuses_a_write_and_reads_beside_the_writer() {
        let scratch = Scratch::new("norn-store-reader-refuses-writes");
        let mut store =
            Store::open(scratch.join("derived").join("store.sqlite3")).expect("a store opens");

        let reader = Arc::new(store.open_reader().expect("a live store mints a reader"));
        let snapshot = Arc::clone(&reader).establish().expect("a snapshot");
        let refused = snapshot
            .connection()
            .execute("INSERT INTO meta (key, value) VALUES ('probe', 1)", [])
            .expect_err("a read-only connection refused nothing");
        assert!(
            refused.to_string().contains("readonly") || refused.to_string().contains("read-only"),
            "a refused write is reported as something other than the connection being read-only: \
             {refused}"
        );

        // The writer's own connection is another connection, and it still
        // writes: the refusal above is the reader's and not the file's.
        drop(snapshot);
        store
            .begin_request()
            .pin_vault_schema(b"version: 1\n", "a-fingerprint")
            .expect("the writer still writes");
    }
}
