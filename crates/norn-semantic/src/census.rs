//! The census: which documents the store holds readable, by body hash, and
//! how many of them the sidecar has embeddings over.
//!
//! **Coverage is counted, never scanned.** A status reports two numbers for
//! the engine: `total`, the documents it would embed, and `embedded`, those
//! whose current content has embeddings here. Both are store facts the drain's
//! own cursors cannot answer part way through a drain, because the rows past a
//! cursor are not yet read. The census is a second, embedding-free reader of
//! the same two feeds with cursors of its own: it keeps one row per readable
//! document, with the body hash the store holds, and the two counts in the
//! sidecar's pinned scalars beside it. Every census row it moves and every
//! vector row the drain moves adjusts the counts by what that one row changed,
//! in the transaction that moves it, so a step's counting cost follows the
//! rows it moved and never the size of the vault.
//!
//! **The counts are exact at the snapshot in which both census feeds came
//! back short.** Each step reads at most one page of each feed, so a census
//! that has a whole vault or a large burst to read catches up over several
//! steps; until it does, the engine answers the counts its last complete
//! reading left, and the step owes another. Whether the recorded counts are
//! whole is recorded beside them, so an engine opened afresh answers the last
//! whole count too. A store lifetime the census was not taken in empties it
//! and clears that last whole count, so a rescan never carries a count across
//! two databases: until the new lifetime is counted whole, there is no count
//! to answer. A sidecar rebuilt from zero starts with an empty census, as it
//! starts with empty cursors.
//!
//! The census lives in the sidecar because the sidecar is the engine's whole
//! state: the store's tables carry nothing a lane-2 engine derives (ADR
//! 0047), and the feed is the one surface the engine reads the store through.

use norn_db::rusqlite::{Connection, OptionalExtension, params};
use norn_embed::Model;
use norn_store::{FeedCursor, FeedRead};

use crate::ddl;
use crate::error::{self, EngineError};

/// How many feed rows one census page reads: the store's own page cap. A
/// census row costs a lookup and a write, so a page of them is a bounded
/// fraction of one drained page of embeddings.
pub(crate) const CENSUS_PAGE: usize = norn_store::MAX_PAGE;

/// How many of the documents the engine would embed have embeddings over
/// their current content.
///
/// Both counts are taken at one store state: the snapshot in which the
/// engine's census last read both feeds to their ends, with the sidecar as
/// the engine's last commit left it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Coverage {
    /// Readable documents whose current body has an embedding under this
    /// engine's model.
    pub embedded: u64,
    /// Readable documents: every document the store holds whose content was
    /// read. A document whose names stand while its content is unreadable is
    /// not among them, because there is nothing to embed.
    pub total: u64,
}

/// The two counts as the sidecar records them, adjusted row by row, and
/// whether they are whole.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Counts {
    pub(crate) embedded: i64,
    pub(crate) total: i64,
    /// The census step that last moved the census read both feeds to their
    /// ends, so the counts are exact at its snapshot, moved since only by
    /// the drain's vector rows. Emptied counts are not whole.
    pub(crate) whole: bool,
}

impl Counts {
    /// The counts as a coverage reading. A count below zero would be a
    /// census that disagrees with its own rows, which the recorded counts'
    /// read refuses as damage, so the conversion clamps rather than wraps.
    pub(crate) fn coverage(self) -> Coverage {
        Coverage {
            embedded: u64::try_from(self.embedded).unwrap_or(0),
            total: u64::try_from(self.total).unwrap_or(0),
        }
    }

    /// Record the counts in `transaction`, beside the rows that moved them.
    pub(crate) fn put(self, transaction: &Connection) -> Result<(), EngineError> {
        norn_db::meta::put_meta(transaction, ddl::meta::CENSUS_TOTAL, self.total)?;
        norn_db::meta::put_meta(transaction, ddl::meta::CENSUS_EMBEDDED, self.embedded)?;
        norn_db::meta::put_meta(transaction, ddl::meta::CENSUS_WHOLE, i64::from(self.whole))?;
        Ok(())
    }

    /// A census row at `path` now holds `body_hash`.
    pub(crate) fn census_upsert(
        &mut self,
        transaction: &Connection,
        model: &Model,
        path: &str,
        body_hash: &str,
    ) -> Result<(), EngineError> {
        let before = census_hash(transaction, path)?;
        let held = vector_hash(transaction, model, path)?;
        transaction
            .execute(
                "INSERT INTO census (path, body_hash) VALUES (?1, ?2)
                 ON CONFLICT(path) DO UPDATE SET body_hash = excluded.body_hash",
                params![path, body_hash],
            )
            .map_err(|error| error::sql("writing a census row", error))?;
        if before.is_none() {
            self.total += 1;
        }
        self.embedded += i64::from(held.as_deref() == Some(body_hash))
            - i64::from(before.is_some() && held == before);
        Ok(())
    }

    /// The document at `path` is no longer readable in the store, or no longer
    /// stands.
    pub(crate) fn census_remove(
        &mut self,
        transaction: &Connection,
        model: &Model,
        path: &str,
    ) -> Result<(), EngineError> {
        let Some(before) = census_hash(transaction, path)? else {
            return Ok(());
        };
        let held = vector_hash(transaction, model, path)?;
        transaction
            .execute("DELETE FROM census WHERE path = ?1", params![path])
            .map_err(|error| error::sql("removing a census row", error))?;
        self.total -= 1;
        self.embedded -= i64::from(held.as_deref() == Some(before.as_str()));
        Ok(())
    }

    /// This engine's vector row at `path` is about to hold `input_hash`, or
    /// none where it is about to be removed. Called before the vector write
    /// it describes, in the same transaction.
    ///
    /// The removals (a reconcile's dead path, an unreadable read's or a
    /// death's retraction) find no census row today: the census reads up to
    /// a page of each feed before the drain's smaller page in the same step,
    /// so it has already removed the path's row. They still call this,
    /// because that holds only while the census reads first and at least as
    /// far as the drain, and a count must not depend on it.
    pub(crate) fn vector_moves(
        &mut self,
        transaction: &Connection,
        model: &Model,
        path: &str,
        input_hash: Option<&str>,
    ) -> Result<(), EngineError> {
        let Some(counted) = census_hash(transaction, path)? else {
            return Ok(());
        };
        let held = vector_hash(transaction, model, path)?;
        self.embedded += i64::from(input_hash == Some(counted.as_str()))
            - i64::from(held.as_deref() == Some(counted.as_str()));
        Ok(())
    }
}

/// What one census step read.
pub(crate) struct Advance {
    /// Both feeds came back short in the snapshot the step read, so the
    /// counts are exact at that snapshot.
    pub(crate) complete: bool,
    /// Feed rows the step read and applied: the census's own work, which
    /// follows the rows that moved in the store and never the vault's size.
    pub(crate) rows: u64,
}

/// The census row at `path`'s body hash, where the census holds one.
fn census_hash(connection: &Connection, path: &str) -> Result<Option<String>, EngineError> {
    connection
        .query_row(
            "SELECT body_hash FROM census WHERE path = ?1",
            params![path],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error::sql("reading a census row", error))
}

/// The input hash of `model`'s vector row at `path`, where one is held.
fn vector_hash(
    connection: &Connection,
    model: &Model,
    path: &str,
) -> Result<Option<String>, EngineError> {
    connection
        .query_row(
            "SELECT input_hash FROM document_vectors
             WHERE path = ?1 AND model_id = ?2 AND model_version = ?3",
            params![path, model.id(), model.version()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error::sql("reading a held input hash", error))
}

/// The feed pages one census step applies, read in the step's snapshot.
pub(crate) struct Pages {
    /// The census was taken in another store lifetime, or in none: its rows
    /// and counts are emptied before the pages apply.
    pub(crate) reset: bool,
    /// Each fed document's position, path and body hash, absent where its
    /// content is unreadable.
    documents: Vec<(FeedCursor, String, Option<String>)>,
    /// Each recorded death's position and path.
    tombstones: Vec<(FeedCursor, String)>,
}

impl Pages {
    /// Read the next page of each feed past the census's own positions.
    /// `recorded_epoch` and the two cursors are what the sidecar holds for
    /// the census; positions recorded in another lifetime are not resumed.
    pub(crate) fn read(
        feed: &mut FeedRead<'_>,
        recorded_epoch: Option<&str>,
        documents_after: Option<FeedCursor>,
        tombstones_after: Option<FeedCursor>,
    ) -> Result<Self, EngineError> {
        let reset = recorded_epoch != Some(feed.epoch());
        let (documents_after, tombstones_after) = if reset {
            (None, None)
        } else {
            (documents_after, tombstones_after)
        };
        let documents = feed
            .changed_documents_after(documents_after.as_ref(), CENSUS_PAGE)?
            .into_iter()
            .map(|(at, document)| (at, document.path.as_str().to_string(), document.body_hash))
            .collect();
        let tombstones = feed
            .changed_tombstones_after(tombstones_after.as_ref(), CENSUS_PAGE)?
            .into_iter()
            .map(|(at, tombstone)| (at, tombstone.path.as_str().to_string()))
            .collect();
        Ok(Pages {
            reset,
            documents,
            tombstones,
        })
    }

    /// Whether applying the pages would change nothing the sidecar holds.
    pub(crate) fn is_empty(&self) -> bool {
        !self.reset && self.documents.is_empty() && self.tombstones.is_empty()
    }

    /// What the step read, without applying it.
    pub(crate) fn advance(&self) -> Advance {
        Advance {
            complete: self.documents.len() < CENSUS_PAGE && self.tombstones.len() < CENSUS_PAGE,
            rows: (self.documents.len() + self.tombstones.len()) as u64,
        }
    }

    /// Apply the pages in `transaction`, moving `counts` row by row, and
    /// record the census's positions, its store lifetime, the counts and
    /// whether they are now whole. A reset empties the counts, which are then
    /// whole only where this one step read both feeds to their ends.
    pub(crate) fn apply(
        &self,
        transaction: &Connection,
        model: &Model,
        store_epoch: &str,
        counts: &mut Counts,
    ) -> Result<(), EngineError> {
        if self.reset {
            transaction
                .execute("DELETE FROM census", [])
                .map_err(|error| error::sql("emptying the census", error))?;
            *counts = Counts::default();
            norn_db::meta::put_meta(transaction, ddl::meta::CENSUS_STORE_EPOCH, store_epoch)?;
            norn_db::meta::put_meta(transaction, ddl::meta::CENSUS_DOCUMENT_CURSOR, "")?;
            norn_db::meta::put_meta(transaction, ddl::meta::CENSUS_TOMBSTONE_CURSOR, "")?;
        }
        for (_, path, body_hash) in &self.documents {
            match body_hash.as_deref() {
                Some(body_hash) => counts.census_upsert(transaction, model, path, body_hash)?,
                None => counts.census_remove(transaction, model, path)?,
            }
        }
        for (_, path) in &self.tombstones {
            counts.census_remove(transaction, model, path)?;
        }
        counts.whole = self.advance().complete;
        if let Some((last, _, _)) = self.documents.last() {
            norn_db::meta::put_meta(
                transaction,
                ddl::meta::CENSUS_DOCUMENT_CURSOR,
                crate::engine::encode_cursor(last),
            )?;
        }
        if let Some((last, _)) = self.tombstones.last() {
            norn_db::meta::put_meta(
                transaction,
                ddl::meta::CENSUS_TOMBSTONE_CURSOR,
                crate::engine::encode_cursor(last),
            )?;
        }
        counts.put(transaction)
    }
}
