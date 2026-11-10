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
//! whole, and the last whole count while they are not, are recorded beside
//! them, so an engine opened afresh answers the last whole count too. A store epoch the census was not taken in empties it and
//! clears that last whole count, so a rescan never carries a count across two
//! databases: until the new epoch is counted whole, there is no count to
//! answer. A sidecar rebuilt from zero starts with an empty census, as it
//! starts with empty cursors.
//!
//! **A census that has not read through the death floor counts again**
//! ([`Pages`]). The database is the same, so it keeps its last whole counts
//! and a status answers them until the new count is whole. A count that
//! reaches the end of both feeds records its completion, the write generation
//! it read before that final page, and the census's read-through generation
//! counts that record, as the drain's counts its tombstone watermark.
//!
//! The census lives in the sidecar because the sidecar is the engine's whole
//! state: the store's tables carry nothing a lane-2 engine derives (ADR
//! 0052), and the feed is the one surface the engine reads the store through.

use norn_db::rusqlite::{Connection, OptionalExtension, params};
use norn_embed::Model;
use norn_store::{FeedCursor, FeedRead, StoreReading};

use crate::ddl;
use crate::error::{self, EngineError};
use crate::progress::{self, ReadThroughGeneration};

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
    /// The counts as they stood when they last stopped being whole, which is
    /// what a status answers while the census catches up; `None` where the
    /// counts have not yet been whole in the census's store epoch.
    pub(crate) last_whole: Option<Coverage>,
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

    /// The counts a status answers: these where they are whole, else the
    /// last whole ones.
    pub(crate) fn answer(self) -> Option<Coverage> {
        if self.whole {
            Some(self.coverage())
        } else {
            self.last_whole
        }
    }

    /// Record the counts in `transaction`, beside the rows that moved them.
    pub(crate) fn put(self, transaction: &Connection) -> Result<(), EngineError> {
        norn_db::meta::put_meta(transaction, ddl::meta::CENSUS_TOTAL, self.total)?;
        norn_db::meta::put_meta(transaction, ddl::meta::CENSUS_EMBEDDED, self.embedded)?;
        norn_db::meta::put_meta(transaction, ddl::meta::CENSUS_WHOLE, i64::from(self.whole))?;
        let (total, embedded) = (
            ddl::meta::CENSUS_LAST_WHOLE_TOTAL,
            ddl::meta::CENSUS_LAST_WHOLE_EMBEDDED,
        );
        match self.last_whole {
            Some(last) => {
                norn_db::meta::put_meta(transaction, total, clamp(last.total))?;
                norn_db::meta::put_meta(transaction, embedded, clamp(last.embedded))?;
            }
            None => {
                norn_db::meta::delete_meta(transaction, total)?;
                norn_db::meta::delete_meta(transaction, embedded)?;
            }
        }
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

/// A count as the sidecar's integer column records it. A count comes from a
/// recorded count that read at or above zero, so it fits; the clamp only
/// keeps the conversion total.
fn clamp(count: u64) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

/// What one census step read.
pub(crate) struct Advance {
    /// Both feeds came back short in the snapshot the step read, so the
    /// counts are exact at that snapshot, and the count completed there: a
    /// recount that reached the end below the floor counts again, which is
    /// not complete and owes another step.
    pub(crate) complete: bool,
    /// Feed rows the step read and applied: the census's own work, which
    /// follows the rows that moved in the store and never the vault's size.
    pub(crate) rows: u64,
    /// The step began a recount below the death floor.
    pub(crate) recount_begun: bool,
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

/// What the census records of its own count, read before each step.
pub(crate) struct Position {
    /// The store epoch the census was taken in, `None` before its first step.
    pub(crate) epoch: Option<String>,
    /// The census's document feed position.
    pub(crate) documents_after: Option<FeedCursor>,
    /// The census's tombstone feed position.
    pub(crate) tombstones_after: Option<FeedCursor>,
    /// The G0 of the recount that runs, where one does.
    pub(crate) recount: Option<i64>,
    /// The completion last recorded, as the sidecar spells it.
    pub(crate) completion: Option<String>,
}

/// How a census step begins.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Begin {
    /// It continues the count its positions describe.
    Continue,
    /// The census was taken in another store epoch, or in none: its rows,
    /// its counts and its last whole counts are emptied, because no count is
    /// carried from one database to another.
    Epoch,
    /// The census has not read through the death floor: it empties its rows
    /// and counts again over the same database, keeping its last whole
    /// counts.
    Recount,
}

/// The feed pages one census step applies, read in the step's snapshot, and
/// what the step does around them.
///
/// **A census below the floor counts again, and never starts that count over
/// because of the floor** ([ADR 0052]). The recount records the write
/// generation its first page reads, its G0, and the census's read-through
/// generation as its records stood, which a restart resumes with: while a
/// recount runs its death cursor raises none, because a death recorded after
/// its first page may be removed before the cursor reaches it. A recount that
/// reaches the end of both feeds where the final page's floor stands at or
/// below its G0 records its completion. One whose final page reads a floor
/// above its G0 records its G0 alone and counts again: it empties the rows it
/// counted, and the next step, below the floor, begins a recount with a G0 of
/// its own.
///
/// **A count outside a recount records its completion at the end of both
/// feeds**: the write generation read before that final page, beside the
/// store epoch. The floor of that page was checked before it against what the
/// census had read through, so no death it had not read was removed.
///
/// [ADR 0052]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0052-a-death-is-kept-only-while-lane-2-or-a-broken-link-needs-it.md
pub(crate) struct Pages {
    begin: Begin,
    /// The recount's G0 where the step runs inside one, its first page
    /// included.
    recount: Option<i64>,
    /// The census's read-through generation before this step, stored with a
    /// recount the step begins.
    read_through: Option<ReadThroughGeneration>,
    /// The reading of the step's snapshot: its epoch, the write generation
    /// and the death floor.
    reading: StoreReading,
    /// The completion recorded before the step, as the sidecar spells it.
    completion: Option<String>,
    /// Each fed document's position, path and body hash, absent where its
    /// content is unreadable.
    documents: Vec<(FeedCursor, String, Option<String>)>,
    /// Each recorded death's position and path.
    tombstones: Vec<(FeedCursor, String)>,
}

impl Pages {
    /// Decide how the step begins, and read the next page of each feed past
    /// the census's own positions, or from the feeds' starts where the step
    /// empties the census. `reading` is the store reading of `feed`'s
    /// snapshot; `read_through` is the census's read-through generation as
    /// its records stand.
    pub(crate) fn read(
        feed: &mut FeedRead<'_>,
        reading: &StoreReading,
        position: Position,
        read_through: Option<&ReadThroughGeneration>,
    ) -> Result<Self, EngineError> {
        let store_epoch = reading.epoch();
        // A census taken in another epoch has read nothing in this one, so
        // below a floor the count over the new epoch is a recount from its
        // first page.
        let below = progress::below(
            progress::in_epoch(read_through, store_epoch),
            reading.death_floor(),
        );
        let (begin, recount) = if position.epoch.as_deref() != Some(store_epoch) {
            (Begin::Epoch, below.then_some(reading.write_generation()))
        } else if let Some(g0) = position.recount {
            (Begin::Continue, Some(g0))
        } else if below {
            (Begin::Recount, Some(reading.write_generation()))
        } else {
            (Begin::Continue, None)
        };
        let (documents_after, tombstones_after) = match begin {
            Begin::Continue => (position.documents_after, position.tombstones_after),
            Begin::Epoch | Begin::Recount => (None, None),
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
            begin,
            recount,
            read_through: read_through.cloned(),
            reading: reading.clone(),
            completion: position.completion,
            documents,
            tombstones,
        })
    }

    /// What the step read, without applying it.
    pub(crate) fn advance(&self) -> Advance {
        Advance {
            complete: self.reads_to_end() && !self.counts_again(),
            rows: (self.documents.len() + self.tombstones.len()) as u64,
            recount_begun: self.begin != Begin::Continue && self.recount.is_some(),
        }
    }

    /// Both feeds came back short in the step's snapshot.
    fn reads_to_end(&self) -> bool {
        self.documents.len() < CENSUS_PAGE && self.tombstones.len() < CENSUS_PAGE
    }

    /// The step reaches the end of a recount whose G0 the floor has passed:
    /// a death recorded after its first page may be gone unread, so applying
    /// it empties the counts to count again, which owes another step.
    fn counts_again(&self) -> bool {
        self.reads_to_end()
            && self
                .recount
                .is_some_and(|g0| progress::below(Some(g0), self.reading.death_floor()))
    }

    /// The completion this step records where it completes a count.
    fn completion_record(&self) -> String {
        progress::encode_generation_in(self.reading.write_generation(), self.reading.epoch())
    }

    /// Whether applying the step would change anything the sidecar holds:
    /// a step over a quiescent store whose counts are whole commits nothing.
    pub(crate) fn changes_anything(&self, counts: &Counts) -> bool {
        let complete = self.reads_to_end();
        self.begin != Begin::Continue
            || !self.documents.is_empty()
            || !self.tombstones.is_empty()
            || complete != counts.whole
            || (complete
                && (self.recount.is_some()
                    || self.completion.as_deref() != Some(self.completion_record().as_str())))
    }

    /// Apply the step in `transaction`, moving `counts` row by row, and
    /// record the census's positions, its store epoch, the counts, whether
    /// they are now whole, and the recount and completion records. An epoch
    /// reset empties the counts and clears the last whole count, which
    /// describes another database; a recount empties the counts and keeps
    /// it. A step that leaves whole counts, part way or by emptying them,
    /// records them as the last whole count, so an engine opened while the
    /// census catches up answers it too.
    pub(crate) fn apply(
        &self,
        transaction: &Connection,
        model: &Model,
        counts: &mut Counts,
    ) -> Result<(), EngineError> {
        let complete = self.reads_to_end();
        let begins = self.begin != Begin::Continue;
        let last_whole = match self.begin {
            Begin::Epoch => None,
            _ if counts.whole && (begins || !complete) => Some(counts.coverage()),
            _ => counts.last_whole,
        };
        if begins {
            empty(transaction, counts)?;
            if self.begin == Begin::Epoch {
                norn_db::meta::put_meta(
                    transaction,
                    ddl::meta::CENSUS_STORE_EPOCH,
                    self.reading.epoch(),
                )?;
            }
            match self.recount {
                Some(g0) => {
                    norn_db::meta::put_meta(transaction, ddl::meta::CENSUS_RECOUNT_G0, g0)?;
                    norn_db::meta::put_meta(
                        transaction,
                        ddl::meta::CENSUS_RECOUNT_READ_THROUGH,
                        progress::encode_read_through(self.read_through.as_ref()),
                    )?;
                }
                None => end_recount(transaction)?,
            }
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
        let mut whole = complete;
        if complete {
            let mut completes = true;
            if let Some(g0) = self.recount {
                norn_db::meta::put_meta(
                    transaction,
                    ddl::meta::CENSUS_RECOUNTED,
                    progress::encode_generation_in(g0, self.reading.epoch()),
                )?;
                end_recount(transaction)?;
                // The floor passed G0 while the recount ran: a death recorded
                // after its first page may be gone unread, so it counts again,
                // keeping the last whole counts and nothing of this count.
                if self.counts_again() {
                    empty(transaction, counts)?;
                    completes = false;
                    whole = false;
                }
            }
            if completes {
                norn_db::meta::put_meta(
                    transaction,
                    ddl::meta::CENSUS_COMPLETION,
                    self.completion_record(),
                )?;
            }
        }
        counts.whole = whole;
        counts.last_whole = last_whole;
        counts.put(transaction)
    }
}

/// Empty the census's rows and counts and move its positions to the feeds'
/// starts. What the counts last were whole is the caller's to keep.
fn empty(transaction: &Connection, counts: &mut Counts) -> Result<(), EngineError> {
    transaction
        .execute("DELETE FROM census", [])
        .map_err(|error| error::sql("emptying the census", error))?;
    *counts = Counts::default();
    norn_db::meta::put_meta(transaction, ddl::meta::CENSUS_DOCUMENT_CURSOR, "")?;
    norn_db::meta::put_meta(transaction, ddl::meta::CENSUS_TOMBSTONE_CURSOR, "")?;
    Ok(())
}

/// Remove the running recount's records.
fn end_recount(transaction: &Connection) -> Result<(), EngineError> {
    norn_db::meta::delete_meta(transaction, ddl::meta::CENSUS_RECOUNT_G0)?;
    norn_db::meta::delete_meta(transaction, ddl::meta::CENSUS_RECOUNT_READ_THROUGH)?;
    Ok(())
}
