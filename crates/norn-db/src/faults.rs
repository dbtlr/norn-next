//! Making a driver-seam condition happen without the environment cooperating.
//!
//! Two conditions a suite cannot arrange for itself are met inside this crate
//! rather than above it: a disk with no room left under a write, and a database
//! somebody else holds when a pinned scalar is read. Both are real, both are
//! why the code that meets them has the shape it has, and neither is reachable
//! by writing rows into a temporary database.
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

use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::Connection;

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
