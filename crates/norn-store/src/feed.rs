//! The lane-2 read surface: the change feed, and the fetch beside it.
//!
//! [`FeedRead`] is the whole of what a lane-2 engine may ask the store — the
//! two feed drains, the two targeted fetches a fed row is resolved through,
//! the epoch its cursors are valid within, and the write generation a drain
//! records how far it had caught up to. **No write verb crosses this
//! seam.** That is the partition boundary invariant 12 leans on: an engine
//! derives inferred state from committed lane-1 records, and inferred state
//! never becomes a finding, a plan, or a repair input — so the surface the
//! engine holds is one that cannot write any of them. An engine that wants a
//! wider surface has to name [`crate::Request`], which is the reviewable act.
//!
//! Every method here is a delegation: the statements, their `EXPLAIN` bars
//! and their contracts are [`crate::Request`]'s, stated on the methods this
//! borrows.

use crate::StoreError;
use crate::facts::{FeedDocument, FeedTombstone, StoredDocument, StoredFacts};
use crate::path::DocumentPath;
use crate::request::FeedCursor;
use crate::store::Store;

/// A read-only handle over one store, scoped to the lane-2 consumer's
/// questions.
///
/// It borrows the store exclusively — the same discipline every request
/// runs under — so a drain observes one sequence of committed states with
/// no writer of this handle's own making interleaved.
pub struct FeedRead<'a> {
    store: &'a mut Store,
}

impl Store {
    /// The lane-2 read surface over this store.
    pub fn feed_read(&mut self) -> FeedRead<'_> {
        FeedRead { store: self }
    }
}

impl FeedRead<'_> {
    /// The store's epoch — see [`Store::epoch`]. A consumer records this
    /// beside its cursors; a mismatch later means rescan, never seek.
    pub fn epoch(&self) -> &str {
        self.store.epoch()
    }

    /// The last write generation committed to the store — see
    /// [`crate::Request::write_generation`]. A consumer reads it immediately
    /// before the page that completes a feed, so the value it records beside
    /// that completion covers every write the page could present.
    pub fn write_generation(&mut self) -> Result<i64, StoreError> {
        self.store.begin_request().write_generation()
    }

    /// The next page of current document rows in feed order — see
    /// [`crate::Request::changed_documents_after`].
    pub fn changed_documents_after(
        &mut self,
        after: Option<&FeedCursor>,
        limit: usize,
    ) -> Result<Vec<(FeedCursor, FeedDocument)>, StoreError> {
        let page = self
            .store
            .begin_request()
            .changed_documents_after(after, limit);
        #[cfg(feature = "induced-failure")]
        crate::faults::write_if_the_feed_page_is_armed(self.store);
        page
    }

    /// The next page of recorded deaths in feed order — see
    /// [`crate::Request::changed_tombstones_after`].
    pub fn changed_tombstones_after(
        &mut self,
        after: Option<&FeedCursor>,
        limit: usize,
    ) -> Result<Vec<(FeedCursor, FeedTombstone)>, StoreError> {
        let page = self
            .store
            .begin_request()
            .changed_tombstones_after(after, limit);
        #[cfg(feature = "induced-failure")]
        crate::faults::write_if_the_feed_page_is_armed(self.store);
        page
    }

    /// One document's row — see [`crate::Request::stored_document`].
    pub fn stored_document(
        &mut self,
        path: &DocumentPath,
    ) -> Result<Option<StoredDocument>, StoreError> {
        self.store.begin_request().stored_document(path)
    }

    /// One document's row, body and facts — see
    /// [`crate::Request::stored_facts`].
    pub fn stored_facts(&mut self, path: &DocumentPath) -> Result<Option<StoredFacts>, StoreError> {
        self.store.begin_request().stored_facts(path)
    }
}
