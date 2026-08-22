//! Making a store operation fail without the environment cooperating.
//!
//! Three of this crate's contract claims are about conditions a test cannot
//! arrange: a disk with no room left under an increment, a process that ends
//! between two entries of a changeset, and a store schema whose statement list
//! meets a driver error. All three are real, all three are why the code around
//! them has the shape it has, and none of them is reachable by writing rows
//! into a temporary database.
//!
//! So the seam lives here. [`induced_failure`] is the arming surface a suite
//! calls, and the rest of this file is what the store's own paths read: the
//! thread-local and process-wide arms, the two abort points the increment
//! checks, and the condition a store schema's statement list meets. **Two
//! arrangements are met at the driver seam rather than here** — the page cap an
//! open applies, and the busy a pinned-scalar read reports — so the arms behind
//! them are `norn-db`'s and the surface below forwards to them.
//!
//! **The seam is deliberately small.** It names *where* a store operation can be
//! made to fail, and *how* — a busy database, a full disk, a corrupt or refusing
//! creation, or the end of the process — and never what the store does next,
//! which is the code under test.
//!
//! # The whole file is behind a feature
//!
//! `induced-failure` gates the module declaration in `lib.rs` and forwards to
//! `norn-db`'s feature of the same name, so a shipped build carries none of this
//! and none of the reads into it: the call sites in `store.rs`, `increment.rs`
//! and the substrate are gated on the same feature and compile to nothing
//! without it. No other crate reaches a store's database, so a store's own crate
//! is the only place either an arrangement or its arming surface can live.
//!
//! # Reaching it from outside
//!
//! This crate is its own dev-dependency with the feature on, which is what lets
//! the integration suite in `tests/` reach [`induced_failure`] as an external
//! crate. A host suite reaches it the same way, through its own dev-dependency
//! on this crate with the feature named — which is what a rung whose required
//! outcome is "this process does not survive here" needs, since the arm is read
//! by the process that dies and the assertion is made by the one that spawned
//! it.

use std::path::{Path, PathBuf};

use norn_db::rusqlite::{self, Connection};

use crate::error::{self, StoreError};
use crate::store::Store;

/// The arrangements a suite reaches past the store's own guarantees to make.
///
/// Every function here puts the database, or the connection to it, in a state no
/// store operation produces, and each of them exists because a rung, a refusal or
/// a verification cannot be reached from outside otherwise — no other crate
/// reaches the derived database, so the only way to arrange either is through
/// the crate that owns it. **Nothing in a product path calls any of this.** The
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
#[doc(hidden)]
pub mod induced_failure {
    use std::sync::atomic::Ordering;

    use super::{
        CHANGESETS_COMMITTED, DISARMED, Store, StoreError, TEAR_AFTER_COMMIT,
        TEAR_AT_CHUNK_BOUNDARY, error, rusqlite,
    };

    /// The environment variable naming the file fired arms record themselves in.
    ///
    /// A harness that names no file gets no record, so a suite asserting over an
    /// arm's absence has to point this at a live file to be asserting anything.
    pub const ARM_HITS: &str = "NORN_STORE_ARM_HITS";

    /// The seam the changeset tears record themselves under.
    pub const INCREMENT_SEAM: &str = "norn-store/increment";

    /// The seam the store schema's armed condition records itself under.
    ///
    /// Crate-private where [`INCREMENT_SEAM`] is public, because the two arms
    /// are judged by different processes. A changeset tear ends the process it
    /// fires in, so the only evidence a parent has that the arm fired rather
    /// than being deleted is the record — which is why the seam it is written
    /// under is a name another crate's suite reads. A store schema refused at a
    /// statement returns the refusal to its caller in the same process, and the
    /// case that arms it asserts on that error. The record is written for a
    /// reader that has to attribute an arm without a return value, and the
    /// export arrives with the first such reader.
    pub(crate) const CREATE_SEAM: &str = "norn-store/create";

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
            .database
            .deferred_transaction("opening the store schema transaction")?;
        norn_db::meta::put_meta(&transaction, norn_db::meta::STORE_SCHEMA_VERSION, version)?;
        norn_db::meta::put_meta(&transaction, norn_db::meta::DDL_FINGERPRINT, fingerprint)?;
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
            .connection()
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
        norn_db::faults::fail_next_meta_read_as_busy();
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
        norn_db::faults::set_page_cap(pages);
    }

    /// Let every store this process opens from here on grow again.
    pub fn uncap_the_pages() {
        norn_db::faults::set_page_cap(0);
    }

    /// How many pages this database is holding.
    ///
    /// What [`cap_the_pages`] is armed against: a cap is only a full disk when
    /// it is the count the file already stands at, and a number guessed from a
    /// file size is a number that leaves the case passing for the wrong reason.
    pub fn page_count(store: &mut Store) -> Result<u64, StoreError> {
        store
            .connection()
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

    /// Disarm every process-wide arrangement above: the page cap, the store
    /// schema's armed condition, and the two tears.
    ///
    /// The per-thread ones are absent by design: nothing survives the abort
    /// they arm. The committed-changeset count is absent for a different
    /// reason — it is a counter rather than an arm, it is what a tear is armed
    /// *against*, and a run that reset it would move the boundary every arm
    /// still standing in this process names. These are here because a suite in
    /// one process arms a cap, watches a request refuse under it, and then has
    /// to watch the same request converge without it.
    pub fn disarm() {
        norn_db::faults::set_page_cap(0);
        *super::ARMED_STORE_SCHEMA
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        TEAR_AFTER_COMMIT.store(DISARMED, Ordering::SeqCst);
        TEAR_AT_CHUNK_BOUNDARY.store(DISARMED, Ordering::SeqCst);
    }
}

std::thread_local! {
    /// How many entries a changeset applies before this process aborts. Set
    /// only by [`induced_failure::abort_after_changeset_entries`]; nothing
    /// clears it, because nothing runs after the abort it arms.
    static TEAR_CHANGESET_AFTER: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
}

/// The value the process-wide tears hold while nothing is armed. A count no run
/// reaches, so the comparison is one load and a mismatch.
const DISARMED: u64 = u64::MAX;

/// How many changesets have committed in this process.
static CHANGESETS_COMMITTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The committed-changeset count this process ends at, with the findings beside
/// that changeset unwritten.
static TEAR_AFTER_COMMIT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(DISARMED);

/// The completed-flush count after which this process ends at the next
/// changeset's first statement.
static TEAR_AT_CHUNK_BOUNDARY: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(DISARMED);

/// The database whose next store schema meets a driver error, and the code it
/// meets — or nothing, which is what every ordinary process holds.
static ARMED_STORE_SCHEMA: std::sync::Mutex<Option<(PathBuf, i32)>> = std::sync::Mutex::new(None);

/// End the process where an arrangement asked for a changeset to be torn after
/// this many entries.
///
/// The abort is deliberate rather than a panic: a panic unwinds, and an unwind
/// rolls the transaction back through the driver's own destructor — which is the
/// tidy end, not the one rung 2 has to survive. Every abort below is the same
/// act for the same reason.
pub(crate) fn abort_if_the_changeset_is_torn(applied: u64) {
    if TEAR_CHANGESET_AFTER.get() == Some(applied) {
        record_arm(induced_failure::INCREMENT_SEAM, "boundary=entries");
        std::process::abort();
    }
}

/// End the process at a changeset's first statement, where the flushes before
/// it are as many as an arrangement asked to leave standing.
pub(crate) fn abort_if_the_chunk_boundary_is_torn() {
    use std::sync::atomic::Ordering;
    let committed = CHANGESETS_COMMITTED.load(Ordering::SeqCst);
    if TEAR_AT_CHUNK_BOUNDARY.load(Ordering::SeqCst) == committed {
        record_arm(
            induced_failure::INCREMENT_SEAM,
            &format!("boundary=chunk changesets={committed}"),
        );
        std::process::abort();
    }
}

/// Count a changeset that committed, and end the process where the findings
/// beside it are what an arrangement asked to lose.
pub(crate) fn note_the_changeset_committed() {
    use std::sync::atomic::Ordering;
    let committed = CHANGESETS_COMMITTED.fetch_add(1, Ordering::SeqCst) + 1;
    if TEAR_AFTER_COMMIT.load(Ordering::SeqCst) == committed {
        record_arm(
            induced_failure::INCREMENT_SEAM,
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
pub(crate) fn failure_armed_for_the_store_schema(
    connection: &Connection,
) -> Option<rusqlite::Error> {
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
    record_arm(induced_failure::CREATE_SEAM, &format!("code={code}"));
    Some(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(code),
        Some("the store schema met an armed condition".to_string()),
    ))
}

/// Whether two spellings name one database file.
///
/// The path SQLite reports back is the one it opened, and a platform may
/// prefix that with components the caller never wrote — on macOS a temporary
/// directory under `/var` is reported under `/private/var`. So the comparison
/// is over the components from the file name back, with the root dropped, and
/// in one direction only: the reported spelling ends with the armed one. That
/// is the constraint the platform imposes, and it is the whole of it — an
/// armed path that merely ends with the reported one names a different file,
/// and matching it would arm a database nobody asked for. The scratch trees a
/// suite arms over carry a serial in their names, so no two of them share a
/// tail.
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
    relative(reported).ends_with(relative(armed))
}

/// Append one record saying which arm fired, where a harness asked for one.
///
/// The process an arm fires in is usually about to end, so this opens, writes,
/// syncs and closes rather than buffering: a record still in memory when the
/// abort lands is a record the parent never reads. A harness that named no file
/// gets no record and pays one environment read for it.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The arm's own record file, which is not a database.
fn record_arm(seam: &str, fields: &str) {
    use std::io::Write as _;
    static HITS: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    let Some(path) = HITS
        .get_or_init(|| std::env::var_os(induced_failure::ARM_HITS).map(std::path::PathBuf::from))
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
