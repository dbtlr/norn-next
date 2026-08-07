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
//! same vault entry gets interleaving, not a refusal. The missing half is a
//! per-vault file lock taken outside the store, which is what makes "one writer"
//! a property of the vault rather than of the handle; it is carved and not built
//! (NORN-33).
//!
//! **Reads will run on a separate handle, not on this connection.** That is
//! recorded here because it is the other half of the writer discipline: when the
//! read builders arrive, wire reads run on a dedicated read-only snapshot handle
//! this crate mints from a live store — its own connection, opened inside this
//! crate so the substrate seam holds — and `&mut` stays what a writer takes. A
//! shared borrow of the store itself cannot serve reads, because the store lives
//! inside an attachment that lifecycle jobs hold mutably for their whole
//! duration (ADR 0014). Nothing implements it yet — every request here is
//! `&mut`, and this store still holds exactly one connection.
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

use rusqlite::{Connection, OpenFlags, OptionalExtension, ToSql};

use crate::ddl;
use crate::error::{self, StoreError};
use crate::facts::{LinkFamily, Provenance, TagSource};
use crate::request::Request;

/// How long a connection waits on a lock before reporting the database busy.
///
/// Serialization is the host's job, so contention here means two processes hold
/// the same vault — which the per-vault file lock is what prevents. The timeout
/// is the backstop that turns a race into a wait rather than an error.
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
/// default, so a shipped build carries none of the three hooks these
/// arrangements reach: the out-of-band executor here, the busy-injection a
/// pinned-scalar read checks on every call, and the tear the increment checks
/// between two entries.
#[cfg(feature = "induced-failure")]
#[doc(hidden)]
pub mod induced_failure {
    use super::{Store, StoreError, error, put_meta};
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
    Ok(Attempt::Connected(connection))
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
        transaction
            .execute_batch(&statement)
            .map_err(|error| StoreError::Sql {
                operation: "creating the store schema",
                message: format!(
                    "`{}`: {error}",
                    statement.lines().next().unwrap_or(&statement)
                ),
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

/// End the process where an arrangement asked for a changeset to be torn after
/// this many entries.
///
/// The abort is deliberate rather than a panic: a panic unwinds, and an unwind
/// rolls the transaction back through the driver's own destructor — which is the
/// tidy end, not the one rung 2 has to survive.
#[cfg(feature = "induced-failure")]
pub(crate) fn abort_if_the_changeset_is_torn(applied: u64) {
    if TEAR_CHANGESET_AFTER.get() == Some(applied) {
        std::process::abort();
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
