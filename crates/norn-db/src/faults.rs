//! Making a driver-seam condition happen without the environment cooperating.
//!
//! Two conditions a suite cannot arrange for itself are met inside this crate
//! rather than above it: a disk with no room left under a write, and a database
//! somebody else holds when a pinned scalar is read. Both are real, both are
//! why the code that meets them has the shape it has, and neither is reachable
//! by writing rows into a temporary database.
//!
//! One record is kept here for the same reason: the text of every statement
//! SQLite begins, which only the connection's statement trace sees whole.
//!
//! The arming surface belongs to the client, because a suite arms an
//! arrangement in the vocabulary of what it is testing. What lives here is
//! narrower: the arms themselves, and the two reads the substrate's own paths
//! make into them — an open applying the cap, and a pinned-scalar read
//! checking the busy.
//!
//! # The whole module is behind a feature
//!
//! `induced-failure` gates the module declaration in `lib.rs`, so a shipped
//! build carries none of this and none of the reads into it: the call sites in
//! `database.rs` and `meta.rs` are gated on the same feature and compile to
//! nothing without it. A client forwards its own feature of the same name to
//! this crate's, so the arm and the code that meets it are in the build
//! together or in neither.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use rusqlite::Connection;
use rusqlite::trace::{TraceEvent, TraceEventCodes};

use crate::error::{self, DbError};

/// The page count every connection opened from here on is held to, or zero for
/// a database nothing caps.
static PAGE_CAP: AtomicU64 = AtomicU64::new(0);

std::thread_local! {
    /// Whether the next pinned-scalar read reports the database busy. Set only
    /// by [`fail_next_meta_read_as_busy`], and cleared by the read it fails.
    pub(crate) static NEXT_META_READ_FAILS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Hold every database opened in this process from here on to `pages` pages.
///
/// **This is a full disk, met by the engine rather than described to it.**
/// Past the cap, every statement that has to grow the file reports
/// `SQLITE_FULL` — the code a disk with no room left produces — so a write
/// refuses through the ordinary error typing and everything downstream of it
/// classifies a real condition rather than a fabricated one. It is not damage,
/// and nothing may answer it as damage.
///
/// The cap is read when a connection is opened, so it reaches a database a
/// process opens for itself. Process-wide: an arrangement a caller has to meet
/// on a thread it does not own would never be read as a thread-local.
pub fn set_page_cap(pages: u64) {
    PAGE_CAP.store(pages, Ordering::SeqCst);
}

/// Make the next read of a pinned `meta` scalar fail as though the database
/// were held by somebody else.
///
/// A busy database is the shape of an environment failure an open has to
/// refuse rather than resolve: rebuilding from zero in response would destroy a
/// sound database to fix nothing. Nothing else can arrange one
/// deterministically — the busy timeout turns real contention into a wait. The
/// arrangement is per-thread and one-shot.
pub fn fail_next_meta_read_as_busy() {
    NEXT_META_READ_FAILS.set(true);
}

/// Take the busy arm back off the calling thread, consumed or not.
///
/// The read the arm fails is what normally clears it. A case that armed one
/// and never read would otherwise leave it standing for whatever opens next
/// on this thread, so a disarm sweeps it rather than trusting every case to
/// have consumed what it armed.
pub fn clear_the_meta_read_arm() {
    NEXT_META_READ_FAILS.set(false);
}

/// Hold this connection to the page count an arrangement capped it at.
///
/// A build without the feature never reads the cap.
pub(crate) fn cap_the_pages(connection: &Connection) -> Result<(), DbError> {
    let pages = PAGE_CAP.load(Ordering::Relaxed);
    if pages == 0 {
        return Ok(());
    }
    connection
        .pragma_update(None, "max_page_count", pages)
        .map_err(|error| error::sql("capping the page count", error))
}

/// Whether the statements SQLite begins are recorded: on every connection
/// opened while this stands, and on every connection that counts its statements
/// (`Database::count_statements_begun`), whenever it was opened. Raised by [`record_statements_begun`] and lowered by
/// [`stop_recording_statements`].
static RECORDING_STATEMENTS: AtomicBool = AtomicBool::new(false);

/// The text of each statement the recording connections began, in the order
/// SQLite began them, as it was prepared, its parameters unbound. A statement
/// run inside another — a trigger's, or one a virtual table runs for itself —
/// is reported behind `-- `, and the start of a trigger's program as
/// `-- TRIGGER <name>`.
static STATEMENTS_RECORDED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Record the text of every statement SQLite begins on every connection this
/// process opens from here on, until [`stop_recording_statements`]. A
/// connection opened earlier is recorded as well where it counts its
/// statements, because the counting trace records through the same arm; any
/// other connection opened earlier is not.
///
/// **This is a census of what ran, taken by the engine.** A plan bar states
/// how one statement reaches its rows; whether every statement an operation
/// runs is one some bar states is a fact only the statement trace holds,
/// because it sees each statement whoever composed it — a virtual table's own
/// statements and transaction control included. The arm is read when a
/// connection is opened, as the page cap is, and it is process-wide, because
/// the statements a host runs are begun on threads the suite does not own.
///
/// The trace names no connection, so the record holds whatever this process
/// ran: a suite asserting over it runs alone in its process.
pub fn record_statements_begun() {
    RECORDING_STATEMENTS.store(true, Ordering::SeqCst);
}

/// Stop recording and forget what was recorded. A connection opened while
/// the record stood keeps its trace, which records nothing once this returns.
///
/// The arm is lowered and the record emptied under the lock a recording
/// statement takes to check the arm and push, so a statement racing the stop
/// is either in the record this empties or dropped; none is left standing for
/// the next recording.
pub fn stop_recording_statements() {
    let mut record = STATEMENTS_RECORDED
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    RECORDING_STATEMENTS.store(false, Ordering::SeqCst);
    record.clear();
}

/// The text of every statement recorded since the record was last taken, in
/// the order SQLite began them.
pub fn take_recorded_statements() -> Vec<String> {
    std::mem::take(
        &mut *STATEMENTS_RECORDED
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
    )
}

/// Trace this connection's statements into the record, where one is being
/// kept. A build without the feature never reads the arm.
pub(crate) fn trace_if_recording(connection: &Connection) {
    if RECORDING_STATEMENTS.load(Ordering::SeqCst) {
        connection.trace_v2(TraceEventCodes::SQLITE_TRACE_STMT, Some(record_one));
    }
}

/// The statement trace [`trace_if_recording`] installs.
fn record_one(event: TraceEvent<'_>) {
    if let TraceEvent::Stmt(_, sql) = event {
        record_the_statement_begun(sql);
    }
}

/// Add the statement SQLite just began to the record, where one is being kept.
///
/// A connection that counts its statements installs a trace of its own in
/// place of [`trace_if_recording`]'s, and that trace records through here, so
/// a counting connection is recorded as every other one is.
pub(crate) fn record_the_statement_begun(sql: &str) {
    let mut record = STATEMENTS_RECORDED
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if RECORDING_STATEMENTS.load(Ordering::SeqCst) {
        record.push(sql.to_string());
    }
}
