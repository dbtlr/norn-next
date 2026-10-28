//! In-memory from-zero steps. The persistent mark makes interrupted work disposable.

use norn_db::rusqlite::Connection;

use crate::{OpenOutcome, Store, StoreError, error};

/// Evidence from the checkpoint after construction's closing commit.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConstructionFinish {
    /// A reader prevented WAL truncation. Construction still finished successfully.
    pub checkpoint_busy: bool,
}

#[derive(Debug)]
pub(crate) struct Steps {
    pub(crate) step: Step,
    pub(crate) saved_autocheckpoint: i64,
    pub(crate) fts_cursor: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Documents,
    FullText,
    FullTextComplete,
}

impl Store {
    /// Begin from-zero steps on an empty, freshly created store with link health deferred.
    ///
    /// Drops the three full-text triggers atomically, then raises WAL autocheckpoint
    /// to 10,000 pages. The opened setting and progress live only in memory: a crash
    /// leaves the construction mark, so the next open rebuilds instead of resuming.
    /// A second call is refused: a build that began and failed leaves a partial
    /// build's rows and full text, so a retry discards the store and builds from
    /// an empty one ([`Self::construction_began`]).
    pub fn begin_steps(&mut self) -> Result<(), StoreError> {
        if !self.under_construction()
            || matches!(self.open_outcome(), OpenOutcome::Reused)
            || !self.link_health_deferred()
            || self.steps.is_some()
        {
            return Err(refused(
                self,
                "beginning construction steps",
                "steps require an empty fresh store with deferred link health",
            ));
        }
        let saved_autocheckpoint = pragma(self.connection(), "wal_autocheckpoint")?;
        let path = self.path().to_path_buf();
        let transaction = self
            .database
            .immediate_transaction("beginning construction steps")?;
        let occupied: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM documents) OR EXISTS(SELECT 1 FROM vault_files)
                    OR EXISTS(SELECT 1 FROM tombstones)",
                [],
                |row| row.get(0),
            )
            .map_err(|e| error::sql("checking construction admission", e))?;
        if occupied {
            return Err(StoreError::Lifecycle {
                operation: "beginning construction steps",
                path,
                message: "construction inputs must be empty".into(),
            });
        }
        transaction
            .execute_batch(
                "DROP TRIGGER documents_fts_insert;
             DROP TRIGGER documents_fts_delete;
             DROP TRIGGER documents_fts_update",
            )
            .map_err(|e| error::sql("deferring full-text triggers", e))?;
        transaction
            .commit()
            .map_err(|e| error::sql("beginning construction steps", e))?;
        self.steps = Some(Steps {
            step: Step::Documents,
            saved_autocheckpoint,
            fts_cursor: 0,
        });
        self.connection()
            .pragma_update(None, "wal_autocheckpoint", 10_000)
            .map_err(|e| error::sql("raising construction autocheckpoint", e))?;
        Ok(())
    }

    /// Whether a from-zero build began its steps on this store and has not
    /// finished. Such a store is never built over again: its rows and full-text
    /// index are a partial build's, so the next from-zero build over it is owed
    /// to rung 3, which discards it and builds from an empty store.
    pub fn construction_began(&self) -> bool {
        self.steps.is_some()
    }

    /// Put back the connection settings a from-zero build raised, after the
    /// build failed and the store stays open until it is discarded. The steps
    /// stay begun ([`Self::construction_began`]). Idempotent; a store whose
    /// steps never began is untouched.
    pub fn restore_construction_settings(&mut self) -> Result<(), StoreError> {
        let Some(steps) = self.steps.as_ref() else {
            return Ok(());
        };
        self.connection()
            .pragma_update(None, "wal_autocheckpoint", steps.saved_autocheckpoint)
            .map_err(|e| error::sql("restoring construction autocheckpoint", e))
    }
}

fn pragma(connection: &Connection, name: &str) -> Result<i64, StoreError> {
    connection
        .pragma_query_value(None, name, |row| row.get(0))
        .map_err(|e| error::sql("reading construction pragmas", e))
}

pub(crate) fn refused(store: &Store, operation: &'static str, message: &str) -> StoreError {
    StoreError::Lifecycle {
        operation,
        path: store.path().to_path_buf(),
        message: message.into(),
    }
}

pub(crate) const FTS_LAST_SQL: &str = "SELECT coalesce(max(id), ?1) FROM
    (SELECT id FROM documents WHERE id > ?1 ORDER BY id LIMIT ?2)";
pub(crate) const FTS_BATCH_SQL: &str = "INSERT INTO documents_fts(rowid, body)
    SELECT id, body FROM documents WHERE id > ?1 ORDER BY id LIMIT ?2";
pub(crate) const FTS_REMAINING_SQL: &str = "SELECT EXISTS(SELECT 1 FROM documents WHERE id > ?1)";

pub(crate) fn full_text_batch(store: &mut Store, limit: usize) -> Result<(u64, bool), StoreError> {
    #[cfg(feature = "induced-failure")]
    let _timing = norn_db::profiling::span("store.step.full_text");
    if limit == 0 || limit > 2_048 {
        return Err(StoreError::Bound {
            what: "a full-text batch (nonempty)",
            limit: 2_048,
            given: limit,
        });
    }
    let Some(steps) = store.steps.as_ref() else {
        return Err(refused(
            store,
            "building full text",
            "construction steps have not begun",
        ));
    };
    if store.link_health_deferred() {
        return Err(refused(
            store,
            "building full text",
            "the build still owes its link-health judgment",
        ));
    }
    if steps.step == Step::FullTextComplete {
        return Ok((0, true));
    }
    let cursor = steps.fts_cursor;
    let transaction = store
        .database
        .immediate_transaction("opening a full-text batch")?;
    let last: i64 = transaction
        .query_row(FTS_LAST_SQL, (cursor, limit as i64), |row| row.get(0))
        .map_err(|e| error::sql("seeking a full-text batch", e))?;
    // NULL bodies must participate too: the insert trigger writes an empty docsize
    // row for each names-only document.
    let rows = transaction
        .execute(FTS_BATCH_SQL, (cursor, limit as i64))
        .map_err(|e| error::sql("indexing a full-text batch", e))?;
    let more: bool = transaction
        .query_row(FTS_REMAINING_SQL, [last], |row| row.get(0))
        .map_err(|e| error::sql("checking remaining full text", e))?;
    transaction
        .commit()
        .map_err(|e| error::sql("committing a full-text batch", e))?;
    let steps = store.steps.as_mut().expect("steps began");
    steps.fts_cursor = last;
    steps.step = if more {
        Step::FullText
    } else {
        Step::FullTextComplete
    };
    Ok((rows as u64, !more))
}
