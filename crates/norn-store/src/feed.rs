//! The lane-2 read surface: the change feed, and the fetch beside it.
//!
//! [`FeedRead`] is the whole of what a lane-2 engine may ask the store — the
//! two feed drains, the two targeted fetches a fed row is resolved through,
//! the epoch its cursors are valid within, the write generation a drain
//! records how far it had caught up to, and the death floor its cursors are
//! valid above ([ADR 0052]). **No write verb crosses this
//! seam.** That is the partition boundary invariant 12 leans on: an engine
//! derives inferred state from committed lane-1 records, and inferred state
//! never becomes a finding, a plan, or a repair input — so the surface the
//! engine holds is one that cannot write any of them. An engine that wants a
//! wider surface has to name [`crate::Request`], which is the reviewable act.
//!
//! A feed read runs over one of two sources, and the surface is the same over
//! both:
//!
//! - **The store's writer**, borrowed exclusively through [`Store::feed_read`].
//!   This is what a caller holding the store runs a whole drain over: the
//!   suites, and the equivalence comparator.
//! - **A [`FeedSource`]**, a read-only connection of the source's own minted
//!   from the live store. This is what the host's background lane reads
//!   through, so a drain never holds the store's writer and never waits for
//!   it. Each [`FeedSource::page`] is one write-ahead-log snapshot, which
//!   never blocks the writer.
//!
//! Every method here is a delegation: the statements, their `EXPLAIN` bars
//! and their contracts are [`crate::Request`]'s, stated on the methods this
//! borrows.
//!
//! # A cursor is valid within one epoch and above the death floor
//!
//! The death prune removes deaths no reader needs, and records the highest
//! generation it removed as the death floor ([`FeedRead::death_floor`]). A
//! consumer whose read-through generation stands below the floor may have
//! deaths it never read gone from the feed, so it reconciles against current
//! state rather than seeking forward, as it does after an epoch change. Over a
//! page the floor is read inside the page's own snapshot, beside the epoch the
//! source checked when its connection opened, so it describes the instant
//! every row the page reads does.
//!
//! [ADR 0052]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0052-a-death-is-kept-only-while-lane-2-or-a-broken-link-needs-it.md

use std::path::PathBuf;

use norn_db::Database;

use crate::StoreError;
use crate::facts::{FeedDocument, FeedTombstone, StoredDocument, StoredFacts};
use crate::path::DocumentPath;
use crate::request::{FeedCursor, ReadWork, Request};
use crate::store::{Store, StoreReading};

/// A read-only handle over one store, scoped to the lane-2 consumer's
/// questions.
///
/// Over the writer it borrows the store exclusively — the same discipline
/// every request runs under — so a drain observes one sequence of committed
/// states with no writer of this handle's own making interleaved. Over a
/// [`FeedSource`] it is one page's snapshot, so everything read through it
/// describes one instant of the store, and its drop ends the snapshot.
pub struct FeedRead<'a> {
    source: Source<'a>,
}

enum Source<'a> {
    Writer(&'a mut Store),
    Page {
        database: &'a mut Database,
        epoch: &'a str,
        read_work: ReadWork,
    },
}

impl Store {
    /// The lane-2 read surface over this store.
    pub fn feed_read(&mut self) -> FeedRead<'_> {
        FeedRead {
            source: Source::Writer(self),
        }
    }

    /// Mint the feed source this store lifetime's lane-2 consumer reads
    /// through.
    ///
    /// **The mint opens no database and runs no SQL**, as a reader pool's mint
    /// does, so it can run under a lock every holder of the store waits
    /// behind: the host mints it in the gate hold that commits an engine's
    /// delivery beside this store lifetime. The connection opens at the first
    /// [`FeedSource::page`], read-only. The source is bound to this store's
    /// epoch, and holds nothing of the store itself, so the writer is never
    /// borrowed by it.
    pub fn feed_source(&self) -> FeedSource {
        FeedSource {
            path: self.database.path().to_path_buf(),
            epoch: self.epoch().to_string(),
            connection: None,
        }
    }
}

/// A read-only connection over one store lifetime, opened lazily, that a
/// lane-2 consumer reads the feed through one page at a time.
///
/// It is outside the request read budget: it is one connection per consumer,
/// owned by whoever minted it, and it closes when it is dropped. **An
/// establishment against another store lifetime is refused**, so a source
/// that outlived the store it was minted from — the file discarded and built
/// again at the same path — reads nothing from the new one.
///
/// The database file it reads is not reachable from it, for the reason
/// [`crate::SnapshotReader`] gives: a caller holding the path could open a
/// connection no adjudication saw.
pub struct FeedSource {
    path: PathBuf,
    epoch: String,
    connection: Option<Database>,
}

impl std::fmt::Debug for FeedSource {
    /// The epoch and whether the connection is open; never the path.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FeedSource")
            .field("epoch", &self.epoch)
            .field("open", &self.connection.is_some())
            .finish_non_exhaustive()
    }
}

impl FeedSource {
    /// The store lifetime this source was minted from.
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// Open the next page: one write-ahead-log snapshot every read through
    /// the handle shares, ended when the handle is dropped.
    ///
    /// The connection is opened here the first time, read-only, and checked
    /// against the epoch the source was minted for. The file gone, the
    /// environment refusing the open, and another store lifetime at the path
    /// are refusals of the environment, never damage. Damage the open or the
    /// snapshot meets is [`StoreError::Damaged`], as it is for a reader's
    /// open, so the consumer's caller judges it as damage met in this
    /// source's lifetime. A connection that refused or met damage is not
    /// kept, so the next page opens again.
    pub fn page(&mut self) -> Result<FeedRead<'_>, StoreError> {
        if self.connection.is_none() {
            let database = norn_db::open_read_only(&self.path)
                .adopted
                .map_err(StoreError::from)?;
            if database.epoch() != self.epoch {
                return Err(StoreError::Sql {
                    operation: "opening a feed source",
                    condition: None,
                    message: "the store at the source's path is another lifetime than the \
                              source was minted from"
                        .to_string(),
                });
            }
            self.connection = Some(database);
        }
        let opened = self
            .connection
            .as_mut()
            .expect("the connection was opened above")
            .open_snapshot();
        if let Err(refused) = opened {
            self.connection = None;
            return Err(StoreError::from(refused));
        }
        let database = self
            .connection
            .as_mut()
            .expect("the connection was opened above");
        Ok(FeedRead {
            source: Source::Page {
                database,
                epoch: &self.epoch,
                read_work: ReadWork::default(),
            },
        })
    }
}

impl Drop for FeedRead<'_> {
    /// A page's snapshot ends with the page, on every way out of it.
    fn drop(&mut self) {
        if let Source::Page { database, .. } = &mut self.source {
            let _ = database.close_snapshot();
        }
    }
}

impl FeedRead<'_> {
    /// The store's epoch — see [`Store::epoch`]. A consumer records this
    /// beside its cursors; a mismatch later means rescan, never seek.
    pub fn epoch(&self) -> &str {
        match &self.source {
            Source::Writer(store) => store.epoch(),
            Source::Page { epoch, .. } => epoch,
        }
    }

    /// The last write generation committed to the store — see
    /// [`crate::Request::write_generation`]. A consumer reads it immediately
    /// before the page that completes a feed, so the value it records beside
    /// that completion covers every write the page could present.
    pub fn write_generation(&mut self) -> Result<i64, StoreError> {
        match &mut self.source {
            Source::Writer(store) => store.begin_request().write_generation(),
            Source::Page { database, .. } => {
                crate::store::last_write_generation(database.connection())
            }
        }
    }

    /// The death floor — see [`crate::Request::death_floor`]: the highest
    /// generation of any death the death prune has removed, or `None` where
    /// it has removed none. Over a page it is read in the page's own
    /// snapshot. A consumer checks it before each page against the
    /// generation it has read through, and reconciles where it stands below.
    pub fn death_floor(&mut self) -> Result<Option<i64>, StoreError> {
        match &mut self.source {
            Source::Writer(store) => store.begin_request().death_floor(),
            Source::Page { database, .. } => crate::store::death_floor(database.connection()),
        }
    }

    /// The store reading this handle reads under: its epoch, and the write
    /// generation and the death floor read together in one statement, so
    /// over a page both describe the page's own snapshot.
    pub fn store_reading(&mut self) -> Result<StoreReading, StoreError> {
        let (write_generation, death_floor) = match &mut self.source {
            Source::Writer(store) => crate::store::generation_and_floor(store.connection())?,
            Source::Page { database, .. } => {
                crate::store::generation_and_floor(database.connection())?
            }
        };
        Ok(StoreReading::of(self.epoch().to_string(), write_generation)
            .with_death_floor(death_floor))
    }

    /// The next page of current document rows in feed order — see
    /// [`crate::Request::changed_documents_after`].
    pub fn changed_documents_after(
        &mut self,
        after: Option<&FeedCursor>,
        limit: usize,
    ) -> Result<Vec<(FeedCursor, FeedDocument)>, StoreError> {
        match &mut self.source {
            Source::Writer(store) => {
                let page = store.begin_request().changed_documents_after(after, limit);
                #[cfg(feature = "induced-failure")]
                crate::faults::write_if_the_feed_page_is_armed(store);
                page
            }
            Source::Page {
                database,
                read_work,
                ..
            } => Request::changed_documents_on(database.connection(), read_work, after, limit),
        }
    }

    /// The next page of recorded deaths in feed order — see
    /// [`crate::Request::changed_tombstones_after`].
    pub fn changed_tombstones_after(
        &mut self,
        after: Option<&FeedCursor>,
        limit: usize,
    ) -> Result<Vec<(FeedCursor, FeedTombstone)>, StoreError> {
        match &mut self.source {
            Source::Writer(store) => {
                let page = store.begin_request().changed_tombstones_after(after, limit);
                #[cfg(feature = "induced-failure")]
                crate::faults::write_if_the_feed_page_is_armed(store);
                page
            }
            Source::Page {
                database,
                read_work,
                ..
            } => Request::changed_tombstones_on(database.connection(), read_work, after, limit),
        }
    }

    /// One document's row — see [`crate::Request::stored_document`].
    pub fn stored_document(
        &mut self,
        path: &DocumentPath,
    ) -> Result<Option<StoredDocument>, StoreError> {
        match &mut self.source {
            Source::Writer(store) => store.begin_request().stored_document(path),
            Source::Page { database, .. } => {
                Request::stored_document_on(database.connection(), path)
            }
        }
    }

    /// One document's row, body and facts — see
    /// [`crate::Request::stored_facts`]. Over a page they are read inside the
    /// page's own snapshot, so they describe the instant the feed rows do.
    pub fn stored_facts(&mut self, path: &DocumentPath) -> Result<Option<StoredFacts>, StoreError> {
        match &mut self.source {
            Source::Writer(store) => store.begin_request().stored_facts(path),
            Source::Page {
                database,
                read_work,
                ..
            } => Request::stored_facts_on(database.connection(), read_work, path),
        }
    }
}
