//! The store itself: one database file, one connection, and the lifecycle of
//! both.
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
//! connection, opened inside this crate so the substrate seam holds, and `&mut`
//! stays what a writer takes. A shared borrow of the store itself cannot serve
//! reads, because the store lives inside an attachment that lifecycle jobs hold
//! mutably for their whole duration (ADR 0015). [`SnapshotReader`] is that
//! handle's type and the whole of what stands here: [`Store`] offers no mint,
//! every request in this crate is `&mut` against the one connection, and no
//! caller anywhere holds a reader.
//!
//! # Write-ahead logging, foreign keys, and why they are set in one place
//!
//! Both are **per-connection** settings in SQLite rather than properties of the
//! file — foreign keys are off by default in every new connection — and the
//! cascade that carries wholesale row replacement depends on them being on. One
//! function opens every connection this crate ever holds, so there is no
//! reading of the schema under settings the schema was not designed for.
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
//! # The database-side heal rung
//!
//! Rung 3 is *discard and rebuild*, and this is where it lives. An open reads
//! three things out of `meta` — the store schema version, the DDL fingerprint,
//! and the digest of the schema the database was created holding — and rebuilds
//! from zero when any of them disagrees with this build, or when the file is not
//! a database at all. The rebuild is the whole file: it is removed, along with
//! the sidecars a journal leaves beside it, and created again from the statement
//! list.
//!
//! The third read is what the first two cannot answer. A fingerprint is compared
//! against a value the database reports about *itself*, so a dropped index, table
//! or trigger leaves it matching — and a dropped `documents_path` forks every
//! path into duplicate rows. The schema digest is taken over `sqlite_schema`, so
//! it is a statement about what is actually there.
//!
//! **Rung 3 is for damaged state, never for a hostile environment.** A full
//! disk, a revoked permission, a parent directory that cannot be created, a
//! database somebody else holds: each of those is refused and reported, and none
//! of them discards anything. Refusing is the correct resolution when the
//! environment is broken and the stored state is not, because discarding a sound
//! database destroys work to fix nothing. One policy decides which is which —
//! [`error::is_damaged`] — and every read an open performs goes through it.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use norn_wire::{FindingKind, Severity};
use rusqlite::{Connection, OpenFlags, OptionalExtension, ToSql};

use crate::ddl;
use crate::error::{self, StoreError};
use crate::facts::{LinkFamily, Provenance, TagSource};
use crate::request::Request;

/// How long a connection waits on a lock before reporting the database busy.
///
/// Serialization is the host's job, so contention here means two processes have
/// the same derived store open — which the maintainer file lock is what
/// prevents. Two processes holding one *vault* it neither prevents nor needs to:
/// a vault is multi-writer, and two derived stores over it are two files. The
/// timeout is the backstop that turns a race into a wait rather than an error.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The flags every connection is opened with.
///
/// Named rather than defaulted so that `SQLITE_OPEN_URI` is *absent*: a path is a
/// filesystem path and never a URI with parameters in it.
const OPEN_FLAGS: OpenFlags = OpenFlags::SQLITE_OPEN_READ_WRITE
    .union(OpenFlags::SQLITE_OPEN_CREATE)
    .union(OpenFlags::SQLITE_OPEN_NO_MUTEX);

/// The two names SQLite reads as something other than a file, whatever the URI
/// flag says: the in-memory database, and the anonymous temporary one.
const NOT_A_FILE: &[&str] = &[":memory:", ""];

/// How many compiled statements a connection keeps.
///
/// Compiling SQL is a material share of what a small changeset costs, and the
/// increment prepares thirteen statements — the document write, one discard per
/// fact table, one insert per fact table, the document delete, the tombstone
/// write and the two findings discards — every time it runs. The cache is what
/// makes that a per-connection cost rather than a per-changeset one, so the
/// capacity is set above the widest write path's count rather than left at the
/// driver's default of sixteen, which the next statement to join that path would
/// take the store over.
const PREPARED_STATEMENT_CACHE: usize = 32;

/// The read-only snapshot handle wire reads run on.
///
/// The type is uninhabited and it is the whole of what stands here: [`Store`]
/// offers no mint, no value of this type exists, and no code path answers a
/// read from a snapshot. What the type carries is the shape a consumer holds a
/// reader through — one the entry keeps beside its store and lets go of before
/// that store closes — so the seam is settled before the connection behind it
/// is.
///
/// The discipline the mint must satisfy when it arrives is the one this shape
/// was carved for. A reader is made from a live [`Store`], because minting from
/// a live store is what binds the handle's lifetime and what guarantees the
/// usable `-shm` a read-only WAL open requires. Its connection is opened
/// read-only with `query_only` set, so a reader answers from the last committed
/// increment, never blocks the writer, and derives nothing.
pub enum SnapshotReader {}

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
    /// a schema that carries no `meta` table to ask, or a schema that no longer
    /// holds what the statement list created.
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
        refuse_a_name_that_is_not_a_file(path)?;
        prepare_parent(path)?;
        let (connection, outcome) = match connect(path)? {
            Attempt::Connected(connection) => match inspect(&connection)? {
                Verdict::Fresh => {
                    create(&connection, mode)?;
                    (connection, OpenOutcome::Created)
                }
                Verdict::Usable => {
                    adopt_mode(&connection, path, mode)?;
                    (connection, OpenOutcome::Reused)
                }
                Verdict::Rebuild(reason) => {
                    drop(connection);
                    (rebuild(path, mode)?, OpenOutcome::RebuiltFromZero(reason))
                }
            },
            Attempt::Rebuild(reason) => {
                (rebuild(path, mode)?, OpenOutcome::RebuiltFromZero(reason))
            }
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

    /// Open a request. Everything the store does happens inside one, so that
    /// every derivation is attributable to the request that caused it.
    pub fn begin_request(&mut self) -> Request<'_> {
        Request::new(self)
    }

    /// Check the database against itself, and report the first way it is not
    /// consistent.
    ///
    /// Five checks, because a store has five kinds of consistency to lose: the
    /// pages themselves, the foreign keys that carry cascade deletion, the
    /// full-text index against the column it is an index of, the frontmatter
    /// projection against being JSON at all, and the closed vocabularies against
    /// the values a reader will accept. The third is what an external-content
    /// FTS5 table can lose without anything else noticing, which is exactly why
    /// the index is maintained by triggers — and it is asked at **rank 1**, which
    /// checks the index against `documents.body` rather than only against itself.
    /// The fourth is the projection's own claim, checked by the JSON1 reader that
    /// will be asked to query it. The fifth closes the gap between "the doctor
    /// says healthy" and a read that fails: a value outside a closed vocabulary
    /// is damage the reader reports, so the verification has to see it too.
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
                what: format!("{orphans} rows reference a row that is not there"),
            });
        }

        self.connection
            .execute(
                "INSERT INTO documents_fts(documents_fts, rank) VALUES ('integrity-check', 1)",
                [],
            )
            .map_err(|error| {
                if error::is_damaged(&error) {
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
                .connection
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
            version: get_meta(&self.connection, ddl::meta::STORE_SCHEMA_VERSION)?,
            ddl_fingerprint: get_meta(&self.connection, ddl::meta::DDL_FINGERPRINT)?,
            schema_digest: get_meta(&self.connection, ddl::meta::SCHEMA_DIGEST)?,
        })
    }

    /// The digest of the schema this database actually holds, right now.
    ///
    /// Taken over `sqlite_schema`, so it answers what is there rather than what a
    /// build would have created. Compared against
    /// [`RecordedStoreSchema::schema_digest`] on every open.
    pub fn schema_digest(&self) -> Result<String, StoreError> {
        schema_digest(&self.connection).map_err(|error| error::sql("digesting the schema", error))
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
        let path = self.path.clone();
        let mode = self.mode;
        self.discard()?;
        Self::open_in_mode(&path, mode)
    }
}

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
            let _ = remove_database(&self.path);
        }
    }
}

/// The arrangements a suite reaches past the store's own guarantees to make.
///
/// Every function here puts the database, or the connection to it, in a state no
/// store operation produces, and each of them exists because a rung, a refusal or
/// a verification cannot be reached from outside otherwise — no other crate may
/// open a connection, so the only way to arrange either is through the crate that
/// owns the connection. **Nothing in a product path calls any of this.** The
/// module is hidden from the documentation and every name in it says what it
/// does.
///
/// The whole module lives behind the `induced-failure` feature, off by
/// default, so a shipped build carries none of the hooks these arrangements
/// reach: the out-of-band executor here, the busy-injection a pinned-scalar
/// read checks on every call, the two tears the increment checks — between two
/// entries and at a changeset's own boundaries — the page cap an open applies,
/// and the damage the store schema's statement list meets.
///
/// **Two of the arrangements are per-thread and the rest are per-process**, and
/// the split is not incidental. A tear armed and met on one thread is the
/// store's own suite arranging its own call. An arrangement a *host* has to
/// meet is armed by a suite thread and fires on a worker thread the host owns,
/// so nothing thread-local would ever be read; those are process-wide, and the
/// process either does not survive them or clears them by name.
#[cfg(feature = "induced-failure")]
#[doc(hidden)]
pub mod induced_failure {
    use std::sync::atomic::Ordering;

    use super::{
        CHANGESETS_COMMITTED, DISARMED, PAGE_CAP, Store, StoreError, TEAR_AFTER_COMMIT,
        TEAR_AT_CHUNK_BOUNDARY, error, put_meta,
    };
    use crate::ddl;

    /// Record a store schema pair this build did not produce.
    ///
    /// The next open reads it, disagrees and rebuilds from zero — which is
    /// precisely the resolution the pair exists to trigger, and the only way to
    /// reach rung 3's fingerprint branch from outside. It is also what a
    /// migration will need once version 1 has frozen as the migratable baseline:
    /// a migration has to record the shape it migrated *to*.
    pub fn record_store_schema_out_of_band(
        store: &mut Store,
        version: i64,
        fingerprint: &str,
    ) -> Result<(), StoreError> {
        let transaction = store
            .connection
            .transaction()
            .map_err(|error| error::sql("opening the store schema transaction", error))?;
        put_meta(&transaction, ddl::meta::STORE_SCHEMA_VERSION, version)?;
        put_meta(&transaction, ddl::meta::DDL_FINGERPRINT, fingerprint)?;
        transaction
            .commit()
            .map_err(|error| error::sql("recording the store schema", error))
    }

    /// Run SQL against the store's own connection.
    ///
    /// Two kinds of arrangement, and both are the whole reason the fence exists.
    ///
    /// **Damage at rest**: dropping a trigger the full-text index depends on,
    /// editing a body behind that index's back, dropping the unique index that
    /// keeps paths unique, writing a value outside a closed vocabulary. All of it
    /// is state no store operation produces, and all of it is state a verification
    /// or an open has to be able to catch.
    ///
    /// **A condition on the connection**: `PRAGMA query_only`, which is what a
    /// revoked permission or a read-only mount looks like from inside a statement.
    /// That is not damage and the store must not answer it as damage — a verdict
    /// of `Damaged` authorizes discarding the database — so arranging it is how
    /// the distinction gets tested at all. `PRAGMA cache_size` is the other one:
    /// a page cache smaller than an open transaction's dirty pages is what makes
    /// that transaction spill into the write-ahead log, so an atomicity case can
    /// be about the file rather than about one process's memory.
    pub fn execute_out_of_band(store: &mut Store, sql: &str) -> Result<(), StoreError> {
        store
            .connection
            .execute_batch(sql)
            .map_err(|error| error::sql("running SQL out of band", error))
    }

    /// Make the next read of a pinned `meta` scalar fail as though the database
    /// were held by somebody else.
    ///
    /// A busy database is the shape of an environment failure that an open has to
    /// refuse rather than resolve: rebuilding from zero in response would destroy
    /// a sound database to fix nothing. Nothing else can arrange one
    /// deterministically — the store's timeout turns real contention into a wait.
    /// The arrangement is per-thread and one-shot.
    pub fn fail_next_meta_read_as_busy() {
        super::NEXT_META_READ_FAILS.set(true);
    }

    /// Kill this process partway through the next changeset, once `entries` of
    /// its entries have been applied.
    ///
    /// **A process killed mid-increment is a rung-2 injection, and it cannot be
    /// arranged from outside.** The abort happens with the transaction open and
    /// nothing committed — no unwinding, no rollback, no destructor — which is
    /// what a `SIGKILL` between two entries looks like to the database, and the
    /// only way to put a store's file in that state deliberately. The
    /// arrangement is per-thread, and the process does not survive it, so there
    /// is nothing to clear.
    pub fn abort_after_changeset_entries(entries: u64) {
        super::TEAR_CHANGESET_AFTER.set(Some(entries));
    }

    /// Kill this process the moment `changesets` of them have committed, with
    /// the findings recorded beside the last one still unwritten.
    ///
    /// **One flush is a changeset plus the findings recorded after it, each in
    /// its own transaction**, and this is the window between the two. What it
    /// leaves is the increment landed with nothing beside it saying why — a
    /// tombstone where a quarantined path had a row, a degraded row asserting a
    /// frontmatter nothing read — which is the state the rows themselves are
    /// required to demand their own re-derivation from.
    ///
    /// Process-wide, because the flush this tears is a host worker's.
    pub fn abort_after_committing_changesets(changesets: u64) {
        TEAR_AFTER_COMMIT.store(changesets, Ordering::SeqCst);
    }

    /// Kill this process at the first statement of the changeset after
    /// `changesets` of them have completed whole.
    ///
    /// **A heal-scale increment is chunked into separately atomic changesets**,
    /// and this is the boundary between two of them: the chunk before it
    /// committed and recorded its findings, the chunk after it has not opened a
    /// transaction. What it leaves is every chunk that landed and no part of
    /// the one that had not begun — each generation whole, the vault's coverage
    /// short by whatever the walk had not reached.
    ///
    /// It is a different point from
    /// [`abort_after_committing_changesets`], which stops inside one flush, and
    /// from [`abort_after_changeset_entries`], which stops inside one
    /// transaction. Process-wide, for the same reason.
    pub fn abort_at_the_chunk_boundary(changesets: u64) {
        TEAR_AT_CHUNK_BOUNDARY.store(changesets, Ordering::SeqCst);
    }

    /// How many changesets have committed in this process so far.
    ///
    /// What a tear is armed against, so a case that has to tear the changeset
    /// carrying a particular document can read where the count stands rather
    /// than assume it.
    pub fn changesets_committed() -> u64 {
        CHANGESETS_COMMITTED.load(Ordering::SeqCst)
    }

    /// Hold every store this process opens from here on to `pages` pages.
    ///
    /// **This is a full disk, met by the engine rather than described to it.**
    /// Past the cap, every statement that has to grow the file reports
    /// `SQLITE_FULL` — the code a disk with no room left produces — so an
    /// increment refuses through the store's own error typing and everything
    /// downstream of it classifies a real condition rather than a fabricated
    /// one. It is not damage, and the store must not answer it as damage.
    ///
    /// The cap is read when a connection is opened, so it reaches a store a
    /// host opens for itself. Cap at the count a database already holds and it
    /// cannot grow at all; [`uncap_the_pages`] clears it, which is what a
    /// recovery case does before demanding the vault again.
    pub fn cap_the_pages(pages: u64) {
        PAGE_CAP.store(pages, Ordering::SeqCst);
    }

    /// Let every store this process opens from here on grow again.
    pub fn uncap_the_pages() {
        PAGE_CAP.store(0, Ordering::SeqCst);
    }

    /// How many pages this database is holding.
    ///
    /// What [`cap_the_pages`] is armed against: a cap is only a full disk when
    /// it is the count the file already stands at, and a number guessed from a
    /// file size is a number that leaves the case passing for the wrong reason.
    pub fn page_count(store: &mut Store) -> Result<u64, StoreError> {
        store
            .connection
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .map_err(|error| error::sql("reading the page count", error))
    }

    /// Make the next store schema written at `database` meet a corrupt
    /// database partway through its statement list.
    ///
    /// **A database that cannot be created is the damage no rung resolves.**
    /// Rung 3's answer to damage is to discard the file and write the store
    /// schema again, so a creation that is itself damaged is the state above
    /// the top of the ladder — and the required behavior there is to give the
    /// entry back rather than to climb again.
    ///
    /// The arrangement is the error the statement meets, and nothing else: it
    /// is handed to the same `sql_at_statement` a real driver error goes to, so
    /// the typing, the message and the routing under it are the production
    /// ones. Corrupting the file on disk first reaches nothing — a create
    /// writes over whatever was there — which is why the condition is armed
    /// here rather than arranged on disk.
    ///
    /// **It is armed at one file rather than at the next creation anywhere.**
    /// A suite runs its cases on threads of one process, so an arrangement
    /// keyed on "next" is an arrangement whichever case happens to create a
    /// store next takes. One-shot at the file it names.
    pub fn corrupt_the_next_store_creation_at(database: &std::path::Path) {
        arm_the_store_schema(database, rusqlite::ffi::SQLITE_CORRUPT);
    }

    /// Make the next store schema written at `database` meet a **refusal**
    /// partway through its statement list: a read-only database, which says
    /// nothing about the file's contents.
    ///
    /// The pair with [`corrupt_the_next_store_creation_at`] is what holds the
    /// two halves of the creation's error handling apart. Damage is typed the
    /// same whichever statement met it, so the damage arm above says nothing
    /// about *which* statement failed. This arm does: a refusal carries the
    /// statement it was met at, and a creation that stopped naming it is a
    /// creation nobody can debug from the message it reports.
    pub fn refuse_the_next_store_creation_at(database: &std::path::Path) {
        arm_the_store_schema(database, rusqlite::ffi::SQLITE_READONLY);
    }

    fn arm_the_store_schema(database: &std::path::Path, code: i32) {
        *super::ARMED_STORE_SCHEMA
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some((database.to_path_buf(), code));
    }

    /// Disarm every process-wide arrangement above.
    ///
    /// The per-thread ones are absent by design: nothing survives the abort
    /// they arm. These are here because a suite in one process arms a cap,
    /// watches a request refuse under it, and then has to watch the same
    /// request converge without it.
    pub fn disarm() {
        PAGE_CAP.store(0, Ordering::SeqCst);
        *super::ARMED_STORE_SCHEMA
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        TEAR_AFTER_COMMIT.store(DISARMED, Ordering::SeqCst);
        TEAR_AT_CHUNK_BOUNDARY.store(DISARMED, Ordering::SeqCst);
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

/// Refuse a name SQLite would read as something other than the file it spells.
///
/// `:memory:` and the empty name are special to SQLite whatever the open flags
/// say, and neither is a file this crate's lifecycle operations could remove or
/// rebuild. A caller that passed one made a mistake about what a store is, which
/// is a refusal — never a verdict about stored state.
fn refuse_a_name_that_is_not_a_file(path: &Path) -> Result<(), StoreError> {
    let spelled = path.to_string_lossy();
    if NOT_A_FILE.contains(&spelled.as_ref()) {
        return Err(StoreError::Lifecycle {
            operation: "opening the database",
            path: path.to_path_buf(),
            message: "a store is a file, and this names a database that is not one".to_string(),
        });
    }
    Ok(())
}

/// Open a connection and put it in the state the store schema is designed for.
///
/// Every connection this crate holds comes from here. The journal mode is read
/// back rather than assumed, because a database that refuses write-ahead logging
/// is not a database this store can keep its promises on.
#[allow(clippy::disallowed_methods)] // The substrate seam: this is the one place a SQLite connection is opened.
fn connect(path: &Path) -> Result<Attempt, StoreError> {
    let connection =
        Connection::open_with_flags(path, OPEN_FLAGS).map_err(|error| StoreError::Lifecycle {
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
    #[cfg(feature = "induced-failure")]
    cap_the_pages(&connection)?;
    Ok(Attempt::Connected(connection))
}

/// Hold this connection to the page count an arrangement capped it at.
///
/// The cap is what a database on a disk with no room left meets, and it is met
/// by the engine rather than by a fabricated error: past the limit every
/// statement that has to grow the file reports `SQLITE_FULL`, which is the code
/// a real full disk produces and which every reader downstream classifies
/// exactly as it classifies one. A build without the feature never reads the
/// cap.
#[cfg(feature = "induced-failure")]
fn cap_the_pages(connection: &Connection) -> Result<(), StoreError> {
    let pages = PAGE_CAP.load(std::sync::atomic::Ordering::Relaxed);
    if pages == 0 {
        return Ok(());
    }
    connection
        .pragma_update(None, "max_page_count", pages)
        .map_err(|error| error::sql("capping the page count", error))
}

/// Read the store schema this database carries, and decide whether it is the
/// one this build writes.
///
/// Every read here fails the same way: through [`rebuild_or_fail`], so one policy
/// decides whether a driver error describes a damaged file or a broken
/// environment. That matters most for the `meta` reads — a busy database, a
/// revoked permission or an I/O error on one of them is not evidence that the
/// stored state is wrong.
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

    let version: Option<i64> = match read_meta(connection, ddl::meta::STORE_SCHEMA_VERSION) {
        Ok(version) => version,
        Err(error) => return rebuild_or_fail(error).map(Verdict::Rebuild),
    };
    if version != Some(ddl::STORE_SCHEMA_VERSION) {
        return Ok(Verdict::Rebuild(RebuildReason::StoreSchemaVersion {
            expected: ddl::STORE_SCHEMA_VERSION,
            found: version,
        }));
    }

    let found: Option<String> = match read_meta(connection, ddl::meta::DDL_FINGERPRINT) {
        Ok(found) => found,
        Err(error) => return rebuild_or_fail(error).map(Verdict::Rebuild),
    };
    if found.as_deref() != Some(expected_fingerprint.as_str()) {
        return Ok(Verdict::Rebuild(RebuildReason::DdlFingerprint {
            expected: expected_fingerprint,
            found,
        }));
    }

    let recorded: Option<String> = match read_meta(connection, ddl::meta::SCHEMA_DIGEST) {
        Ok(recorded) => recorded,
        Err(error) => return rebuild_or_fail(error).map(Verdict::Rebuild),
    };
    let holds = match schema_digest(connection) {
        Ok(digest) => digest,
        Err(error) => return rebuild_or_fail(error).map(Verdict::Rebuild),
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
    Ok(Verdict::Usable)
}

/// The digest of the schema a database actually holds.
///
/// Every object, in a fixed order, by kind, name and the statement that created
/// it. A dropped index, table or trigger changes it; so does one redefined
/// behind the store's back. `sql` is null for an index SQLite created for a
/// constraint, and the empty string stands in for it.
fn schema_digest(connection: &Connection) -> rusqlite::Result<String> {
    let mut statement = connection
        .prepare("SELECT type, name, ifnull(sql, '') FROM sqlite_schema ORDER BY 1, 2")?;
    let mut parts: Vec<String> = Vec::new();
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        parts.push(row.get(0)?);
        parts.push(row.get(1)?);
        parts.push(row.get(2)?);
    }
    Ok(ddl::digest(parts.iter().map(String::as_str)))
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

/// Remove the database and create it again from the statement list.
fn rebuild(path: &Path, mode: StoreMode) -> Result<Connection, StoreError> {
    remove_database(path)?;
    match connect(path)? {
        Attempt::Connected(connection) => {
            create(&connection, mode)?;
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
/// inspect, and the pinned values written at the end are what say the list
/// finished. The schema digest is taken after the statements ran, so it is a
/// reading of the database rather than a second reading of the list.
fn create(connection: &Connection, mode: StoreMode) -> Result<(), StoreError> {
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| error::sql("opening the store schema transaction", error))?;
    for statement in ddl::statements() {
        #[cfg(feature = "induced-failure")]
        let armed = failure_armed_for_the_store_schema(connection);
        #[cfg(not(feature = "induced-failure"))]
        let armed: Option<rusqlite::Error> = None;
        let ran = match armed {
            Some(error) => Err(error),
            None => transaction.execute_batch(&statement),
        };
        ran.map_err(|error| {
            error::sql_at_statement("creating the store schema", &statement, error)
        })?;
    }
    let digest =
        schema_digest(&transaction).map_err(|error| error::sql("digesting the schema", error))?;
    put_meta(
        &transaction,
        ddl::meta::STORE_SCHEMA_VERSION,
        ddl::STORE_SCHEMA_VERSION,
    )?;
    put_meta(&transaction, ddl::meta::DDL_FINGERPRINT, ddl::fingerprint())?;
    put_meta(&transaction, ddl::meta::SCHEMA_DIGEST, digest)?;
    put_meta(&transaction, ddl::meta::STORE_MODE, mode.as_str())?;
    put_meta(&transaction, ddl::meta::WRITE_GENERATION, 0_i64)?;
    transaction
        .commit()
        .map_err(|error| error::sql("committing the store schema", error))
}

/// Reconcile the mode a database records with the mode it is being opened in.
///
/// A throwaway open over a durable store is refused: adopting it would arm a
/// teardown that deletes a registered vault's whole derived state when the store
/// drops. **A throwaway open over a `store_mode` row that is absent or
/// unreadable is refused the same way.** Create always records the mode, so a
/// missing or unrecognized row is out-of-band tampering rather than a database
/// this crate ever produces, and the conservative reading is the refusal: the
/// alternative is arming delete-on-drop over a database whose own record does
/// not say it is disposable.
///
/// The other direction is adoption, and it is safe unconditionally: a durable
/// open deletes nothing, so it takes over a throwaway store's leftovers or an
/// unrecorded mode exactly as it takes over its own kind. Disposable derived
/// state is rebuildable and the file is no longer anybody's to delete.
///
/// The arms are written out explicitly, mode by recorded mode, rather than
/// folded behind a wildcard — a wildcard is how the throwaway-and-absent case
/// went unnoticed the first time.
fn adopt_mode(connection: &Connection, path: &Path, mode: StoreMode) -> Result<(), StoreError> {
    let recorded = get_meta::<String>(connection, ddl::meta::STORE_MODE)?
        .as_deref()
        .and_then(StoreMode::from_str);
    match (mode, recorded) {
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
            message: "the database does not record itself as a throwaway store, and a throwaway \
                      store deletes its file when it closes"
                .to_string(),
        }),
        (StoreMode::Throwaway, Some(StoreMode::Throwaway)) => {
            put_meta(connection, ddl::meta::STORE_MODE, mode.as_str())
        }
        (StoreMode::Durable, Some(StoreMode::Durable)) => Ok(()),
        (StoreMode::Durable, Some(StoreMode::Throwaway)) | (StoreMode::Durable, None) => {
            put_meta(connection, ddl::meta::STORE_MODE, mode.as_str())
        }
    }
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
    read_meta(connection, key).map_err(|error| error::sql("reading a pinned value", error))
}

/// [`get_meta`] with the driver's own error.
///
/// The open path needs the error rather than a message: whether a failed `meta`
/// read means the database is damaged or the environment is broken decides
/// whether a sound database is discarded, and that decision is one policy over
/// driver error codes.
fn read_meta<T: rusqlite::types::FromSql>(
    connection: &Connection,
    key: &str,
) -> rusqlite::Result<Option<T>> {
    #[cfg(feature = "induced-failure")]
    if NEXT_META_READ_FAILS.replace(false) {
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

#[cfg(feature = "induced-failure")]
std::thread_local! {
    /// Whether the next pinned-scalar read reports the database busy. Set only
    /// by [`induced_failure::fail_next_meta_read_as_busy`], and cleared by the
    /// read it fails.
    static NEXT_META_READ_FAILS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };

    /// How many entries a changeset applies before this process aborts. Set
    /// only by [`induced_failure::abort_after_changeset_entries`]; nothing
    /// clears it, because nothing runs after the abort it arms.
    static TEAR_CHANGESET_AFTER: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
}

/// The seam the changeset tears record themselves under.
#[cfg(feature = "induced-failure")]
const INCREMENT_SEAM: &str = "norn-store/increment";

/// The value the process-wide tears hold while nothing is armed. A count no run
/// reaches, so the comparison is one load and a mismatch.
#[cfg(feature = "induced-failure")]
const DISARMED: u64 = u64::MAX;

/// How many changesets have committed in this process.
#[cfg(feature = "induced-failure")]
static CHANGESETS_COMMITTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The committed-changeset count this process ends at, with the findings beside
/// that changeset unwritten.
#[cfg(feature = "induced-failure")]
static TEAR_AFTER_COMMIT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(DISARMED);

/// The completed-flush count after which this process ends at the next
/// changeset's first statement.
#[cfg(feature = "induced-failure")]
static TEAR_AT_CHUNK_BOUNDARY: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(DISARMED);

/// The page count every connection opened from here on is held to, or zero for
/// a database nothing caps.
#[cfg(feature = "induced-failure")]
static PAGE_CAP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The database whose next store schema meets a driver error, and the code it
/// meets — or nothing, which is what every ordinary process holds.
#[cfg(feature = "induced-failure")]
static ARMED_STORE_SCHEMA: std::sync::Mutex<Option<(PathBuf, i32)>> = std::sync::Mutex::new(None);

/// End the process where an arrangement asked for a changeset to be torn after
/// this many entries.
///
/// The abort is deliberate rather than a panic: a panic unwinds, and an unwind
/// rolls the transaction back through the driver's own destructor — which is the
/// tidy end, not the one rung 2 has to survive. Every abort below is the same
/// act for the same reason.
#[cfg(feature = "induced-failure")]
pub(crate) fn abort_if_the_changeset_is_torn(applied: u64) {
    if TEAR_CHANGESET_AFTER.get() == Some(applied) {
        record_arm(INCREMENT_SEAM, "boundary=entries");
        std::process::abort();
    }
}

/// End the process at a changeset's first statement, where the flushes before
/// it are as many as an arrangement asked to leave standing.
#[cfg(feature = "induced-failure")]
pub(crate) fn abort_if_the_chunk_boundary_is_torn() {
    use std::sync::atomic::Ordering;
    let committed = CHANGESETS_COMMITTED.load(Ordering::SeqCst);
    if TEAR_AT_CHUNK_BOUNDARY.load(Ordering::SeqCst) == committed {
        record_arm(
            INCREMENT_SEAM,
            &format!("boundary=chunk changesets={committed}"),
        );
        std::process::abort();
    }
}

/// Count a changeset that committed, and end the process where the findings
/// beside it are what an arrangement asked to lose.
#[cfg(feature = "induced-failure")]
pub(crate) fn note_the_changeset_committed() {
    use std::sync::atomic::Ordering;
    let committed = CHANGESETS_COMMITTED.fetch_add(1, Ordering::SeqCst) + 1;
    if TEAR_AFTER_COMMIT.load(Ordering::SeqCst) == committed {
        record_arm(
            INCREMENT_SEAM,
            &format!("boundary=after-commit changesets={committed}"),
        );
        std::process::abort();
    }
}

/// The error the store schema's statement list meets where an arrangement armed
/// one, or nothing where it did not.
///
/// One-shot, and matched against the database being created: the arm is taken
/// by the first statement written at the file it names, so a rung that creates a
/// database, fails, and is asked to create another one meets the condition once
/// rather than forever — and a case running beside it on another thread meets it
/// never.
#[cfg(feature = "induced-failure")]
fn failure_armed_for_the_store_schema(connection: &Connection) -> Option<rusqlite::Error> {
    let mut armed = ARMED_STORE_SCHEMA
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (database, code) = armed.as_ref()?;
    if !names_one_database(Path::new(connection.path()?), database) {
        return None;
    }
    let code = *code;
    *armed = None;
    drop(armed);
    record_arm("norn-store/create", &format!("code={code}"));
    Some(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(code),
        Some("the store schema met an armed condition".to_string()),
    ))
}

/// Whether two spellings name one database file.
///
/// The path SQLite reports back is the one it opened, and a platform may
/// resolve that differently from the one the caller handed over — on macOS a
/// temporary directory under `/var` is reported under `/private/var`. So the
/// comparison is over the components from the file name back, with the root
/// dropped: one spelling being a suffix of the other is what says they are the
/// same file, and the scratch trees a suite arms over carry a serial in their
/// names, so no two of them share one.
#[cfg(feature = "induced-failure")]
fn names_one_database(reported: &Path, armed: &Path) -> bool {
    fn relative(path: &Path) -> PathBuf {
        path.components()
            .filter(|component| {
                !matches!(
                    component,
                    std::path::Component::RootDir | std::path::Component::Prefix(_)
                )
            })
            .collect()
    }
    let reported = relative(reported);
    let armed = relative(armed);
    reported.ends_with(&armed) || armed.ends_with(&reported)
}

/// Append one record saying which arm fired, where a harness asked for one.
///
/// The process an arm fires in is usually about to end, so this opens, writes,
/// syncs and closes rather than buffering: a record still in memory when the
/// abort lands is a record the parent never reads. A harness that named no file
/// gets no record and pays one environment read for it.
#[cfg(feature = "induced-failure")]
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The arm's own record file, which is not a database.
fn record_arm(seam: &str, fields: &str) {
    use std::io::Write as _;
    static HITS: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    let Some(path) = HITS
        .get_or_init(|| std::env::var_os("NORN_STORE_ARM_HITS").map(std::path::PathBuf::from))
        .as_ref()
    else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "seam={seam} {fields}");
        let _ = file.sync_all();
    }
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

/// A closed vocabulary as an SQL list of quoted literals.
fn quoted<'a>(values: impl Iterator<Item = &'a str>) -> String {
    values
        .map(|value| format!("'{value}'"))
        .collect::<Vec<String>>()
        .join(", ")
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

/// Remove the database and every sidecar a journal leaves beside it.
///
/// All four, because a rebuilt database beside a stale journal is a database
/// with somebody else's committed pages in it. `-wal` and `-shm` are write-ahead
/// logging's, and `-journal` is the rollback journal's: a store opens in WAL
/// mode, but a file this store did not write may carry one, and rung 3 is
/// reached for exactly those files. A file that is already gone is not a
/// failure.
#[allow(clippy::disallowed_methods)] // Removing the derived database at heal rung 3, and tearing a throwaway store down.
fn remove_database(path: &Path) -> Result<(), StoreError> {
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
            let verdict = rebuild_or_fail(failure(code)).expect("a rebuild reason");
            assert!(matches!(verdict, RebuildReason::Damaged { .. }), "{code}");
        }
        for code in [
            rusqlite::ffi::SQLITE_BUSY,
            rusqlite::ffi::SQLITE_READONLY,
            rusqlite::ffi::SQLITE_IOERR,
            rusqlite::ffi::SQLITE_LOCKED,
            rusqlite::ffi::SQLITE_PERM,
        ] {
            let error = rebuild_or_fail(failure(code))
                .expect_err("an environment failure is reported, never resolved by a rebuild");
            assert!(matches!(error, StoreError::Sql { .. }), "{code}: {error:?}");
        }
    }
}
