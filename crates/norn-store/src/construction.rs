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
    pub(crate) group_open: bool,
    group_guard: bool,
    group_documents: usize,
    /// Construction transactions committed, every closing path counted.
    group_commits: u64,
}

/// Construction completeness boundaries, held in memory rather than persisted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Provisional names and final vault files.
    Names,
    /// Stored bytes, fields and frontmatter facts.
    Content,
    /// Body facts derived from stored bytes.
    Structure,
    /// One complete link-health judgment.
    Relations,
    /// Full-text batches are in progress.
    FullText,
    /// All bodies are indexed; the closing transaction may finish construction.
    FullTextComplete,
}

impl Store {
    /// Begin from-zero steps on an empty, freshly created store with link health deferred.
    ///
    /// Creates TEMP progress and deferred-spelling tables and drops the three
    /// full-text triggers and deferred secondary indexes atomically; the indexes
    /// construction maintains row by row (the `KEPT` list beside `DEFERRED`)
    /// stay.
    /// Then raises WAL autocheckpoint to 10,000 pages. The opened setting and progress live only in memory: a crash
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
             DROP TRIGGER documents_fts_update;
             CREATE TEMP TABLE construction_progress(document INTEGER PRIMARY KEY, stage INTEGER NOT NULL, body_line INTEGER, body_column INTEGER, frontmatter_links INTEGER, frontmatter_tags INTEGER);
             CREATE INDEX temp.construction_progress_stage ON construction_progress(stage, document);
             CREATE TEMP TABLE construction_requeue(document INTEGER PRIMARY KEY);
             CREATE TEMP TABLE construction_spellings(path BLOB PRIMARY KEY) WITHOUT ROWID",
            )
            .map_err(|e| error::sql("deferring full-text triggers", e))?;
        for (name, _) in crate::ddl::construction::DEFERRED {
            transaction
                .execute_batch(&format!("DROP INDEX {name}"))
                .map_err(|e| error::sql("deferring construction indexes", e))?;
        }
        transaction
            .commit()
            .map_err(|e| error::sql("beginning construction steps", e))?;
        self.steps = Some(Steps {
            step: Step::Names,
            saved_autocheckpoint,
            fts_cursor: 0,
            group_open: false,
            group_guard: false,
            group_documents: 0,
            group_commits: 0,
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
    if !matches!(
        steps.step,
        Step::Relations | Step::FullText | Step::FullTextComplete
    ) {
        return Err(refused(
            store,
            "building full text",
            "document steps are incomplete",
        ));
    }
    if store.link_health_deferred() {
        return Err(refused(
            store,
            "building full text",
            "the build still owes its link-health judgment",
        ));
    }
    if steps.step == Step::FullTextComplete {
        #[cfg(feature = "induced-failure")]
        drop(_timing);
        store.build_step_indexes(Step::FullTextComplete)?;
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
    #[cfg(feature = "induced-failure")]
    drop(_timing);
    if !more {
        store.build_step_indexes(Step::FullTextComplete)?;
    }
    Ok((rows as u64, !more))
}

/// One document's content, derived without scanning its body.
#[derive(Debug)]
pub struct ContentWrite {
    pub facts: crate::DocumentFacts,
    /// The first body's position in the original file.
    pub origin: crate::Span,
}

/// Stored bytes and frontmatter counts for one structure scan. The link count is
/// the ordinal offset; frontmatter links need not be loaded a second time.
#[derive(Debug)]
pub struct StructureInput {
    pub facts: crate::DocumentFacts,
    pub origin: crate::Span,
    pub frontmatter_links: usize,
    pub frontmatter_tags: usize,
}

/// A construction transaction spanning host chunks, with no savepoints.
/// Dropping an uncommitted group rolls back all its chunks. Rare unreadable
/// increments close the group before using the ordinary increment path; the
/// next content write opens a new transaction under this same guard.
pub struct StepGroup<'a> {
    store: &'a mut Store,
}

impl StepGroup<'_> {
    /// Open an attribution scope within this transaction group.
    pub fn begin_request(&mut self) -> crate::Request<'_> {
        self.store.begin_request()
    }

    /// Borrow the store for the host's existing finding queue and derivation.
    pub fn store(&mut self) -> &mut Store {
        self.store
    }

    /// Commit the last group, if a rare increment has not already closed it.
    pub fn commit(self) -> Result<(), StoreError> {
        close_group(self.store)?;
        self.store.steps.as_mut().expect("steps").group_guard = false;
        Ok(())
    }
}

impl Drop for StepGroup<'_> {
    fn drop(&mut self) {
        if self.store.steps.as_ref().is_some_and(|s| s.group_open) {
            let _ = self.store.connection().execute_batch("ROLLBACK");
        }
        if let Some(steps) = self.store.steps.as_mut() {
            steps.group_open = false;
            steps.group_guard = false;
        }
    }
}

impl Store {
    /// The current in-memory step; absent on ordinary stores and after finish.
    pub fn construction_step(&self) -> Option<Step> {
        self.steps.as_ref().map(|s| s.step)
    }

    /// Advance an exhausted step. Progress remains in TEMP until construction
    /// finishes; it is never a resume record for an interrupted build. Each
    /// secondary index due at this boundary is created in its own transaction
    /// before the next step can admit readers.
    pub fn finish_step(&mut self, step: Step) -> Result<(), StoreError> {
        if self.construction_step() != Some(step)
            || self.steps.as_ref().is_some_and(|s| s.group_guard)
        {
            return Err(refused(
                self,
                "ending a construction step",
                "step is not current or a group is open",
            ));
        }
        let (stage, next) = match step {
            Step::Names => (0, Step::Content),
            Step::Content => (1, Step::Structure),
            Step::Structure => (2, Step::Relations),
            _ => {
                return Err(refused(
                    self,
                    "ending a construction step",
                    "not a document step",
                ));
            }
        };
        let remaining: bool = self
            .connection()
            .query_row(EXHAUSTED_SQL, [stage], |r| r.get(0))
            .map_err(|e| error::sql("checking step exhaustion", e))?;
        if remaining {
            return Err(refused(
                self,
                "ending a construction step",
                "documents remain in this step",
            ));
        }
        self.build_step_indexes(step)?;
        self.steps.as_mut().expect("steps").step = next;
        Ok(())
    }

    /// Begin a group of at most 2,048 documents. Host chunks remain separately
    /// bounded; a chunk failure fails the build rather than rolling back a savepoint.
    pub fn step_group(&mut self) -> Result<StepGroup<'_>, StoreError> {
        if !matches!(
            self.construction_step(),
            Some(Step::Content | Step::Structure)
        ) || self.steps.as_ref().is_some_and(|s| s.group_guard)
        {
            return Err(refused(
                self,
                "opening a construction group",
                "content or structure required",
            ));
        }
        self.steps.as_mut().expect("steps").group_guard = true;
        if let Err(error) = ensure_group(self, 0) {
            self.steps.as_mut().expect("steps").group_guard = false;
            return Err(error);
        }
        Ok(StepGroup { store: self })
    }

    /// Commit the open group's chunks now and keep its guard, so the next
    /// write opens a fresh transaction under it; whether a transaction was
    /// open to commit (a rare increment may have closed it already). A
    /// pulled-ahead request's reader takes its snapshot after this commit, so
    /// it sees its document. Outside a group this is refused, like every
    /// construction write.
    pub fn commit_step_group(&mut self) -> Result<bool, StoreError> {
        if !self.steps.as_ref().is_some_and(|s| s.group_guard) {
            return Err(refused(
                self,
                "committing a construction group",
                "a group guard is required",
            ));
        }
        let open = self.steps.as_ref().is_some_and(|s| s.group_open);
        close_group(self)?;
        Ok(open)
    }

    /// How many more documents the open group admits before its
    /// 2,048-document bound; zero outside construction.
    pub fn construction_group_room(&self) -> usize {
        self.steps
            .as_ref()
            .map_or(0, |s| 2048usize.saturating_sub(s.group_documents))
    }

    /// Construction transactions committed so far, including those a rare
    /// ordinary increment closed; zero outside construction.
    pub fn construction_group_commits(&self) -> u64 {
        self.steps.as_ref().map_or(0, |s| s.group_commits)
    }
}

/// Commit before a rare ordinary increment, leaving the guard responsible for
/// the next group it opens and for rollback on failure.
pub(crate) fn close_group(store: &mut Store) -> Result<(), StoreError> {
    if store.steps.as_ref().is_some_and(|s| s.group_open) {
        store
            .connection()
            .execute_batch("COMMIT")
            .map_err(|e| error::sql("committing a construction group", e))?;
        let steps = store.steps.as_mut().expect("steps");
        steps.group_open = false;
        steps.group_documents = 0;
        steps.group_commits += 1;
        #[cfg(feature = "induced-failure")]
        crate::faults::note_the_changeset_committed();
    }
    Ok(())
}

/// Open lazily after a rare increment and admit only a bounded host chunk.
fn ensure_group(store: &mut Store, documents: usize) -> Result<(), StoreError> {
    let steps = store.steps.as_ref().ok_or_else(|| {
        refused(
            store,
            "writing a construction chunk",
            "steps have not begun",
        )
    })?;
    if !steps.group_guard {
        return Err(refused(
            store,
            "writing a construction chunk",
            "a group guard is required",
        ));
    }
    if documents > crate::MAX_PAGE || steps.group_documents + documents > 2048 {
        return Err(StoreError::Bound {
            what: "construction group documents",
            limit: 2048,
            given: steps.group_documents + documents,
        });
    }
    if !steps.group_open {
        store
            .connection()
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| error::sql("opening a construction group", e))?;
        store.steps.as_mut().expect("steps").group_open = true;
    }
    store.steps.as_mut().expect("steps").group_documents += documents;
    Ok(())
}

impl crate::Request<'_> {
    /// Read the next bounded set of names owing content. The vault pass selects
    /// by stage; NORN-468 pull-ahead supplies a requested target's candidates
    /// ([`Self::target_candidates`]) to the same content writer instead, and
    /// NORN-469 PR B requeue will supply individual paths there too.
    pub fn content_page(&mut self, limit: usize) -> Result<Vec<crate::DocumentPath>, StoreError> {
        check_limit(limit)?;
        Self::read_all_on(
            self.store.connection(),
            &self.read_work,
            CONTENT_PAGE_SQL,
            [limit as i64],
            |row| {
                let path: String = row.get(0)?;
                Ok(crate::DocumentPath::new(&path))
            },
            "reading a content page",
        )
    }

    /// Select only touched documents still owing this construction's current step.
    /// The small TEMP table drives the lookup, never the vault's document queue.
    pub fn requeued_page(&mut self, limit: usize) -> Result<Vec<crate::DocumentPath>, StoreError> {
        check_limit(limit)?;
        let stage = if self.store.construction_step() == Some(Step::Structure) { 2 } else { 1 };
        Self::read_all_on(self.store.connection(), &self.read_work,
            "SELECT d.path FROM temp.construction_requeue r JOIN temp.construction_progress p ON p.document = r.document JOIN documents d ON d.id = r.document WHERE p.stage <= ?1 ORDER BY p.stage, r.document LIMIT ?2",
            (stage, limit as i64), |row| { let path: String = row.get(0)?; Ok(crate::DocumentPath::new(&path)) },
            "reading requeued documents")
    }

    /// Read one chunk's stored bodies. Frontmatter tags are loaded for just
    /// the document about to be scanned, through [`Self::load_structure_tags`].
    pub fn structure_page(&mut self, limit: usize) -> Result<Vec<StructureInput>, StoreError> {
        check_limit(limit)?;
        Self::read_all_on(
            self.store.connection(),
            &self.read_work,
            STRUCTURE_PAGE_SQL,
            [limit as i64],
            stored_structure,
            "reading a structure page",
        )
    }

    /// Fetch one arbitrary document owing structure, without walking the queue.
    /// NORN-468 pull-ahead selects a requested target's resolved document here,
    /// and NORN-469 PR B requeue will select single paths too; the ordinary pass
    /// keeps stage selection in [`Self::structure_page`].
    pub fn structure_document(
        &mut self,
        path: &crate::DocumentPath,
    ) -> Result<Option<StructureInput>, StoreError> {
        let mut rows = Self::read_all_on(
            self.store.connection(),
            &self.read_work,
            STRUCTURE_DOCUMENT_SQL,
            [path.as_str()],
            stored_structure,
            "reading one structure input",
        )?;
        Ok(rows.pop())
    }

    /// Load one document's frontmatter tags immediately before its scan. A
    /// chunk retains stored bodies, never another document's mapped facts.
    pub fn load_structure_tags(&mut self, input: &mut StructureInput) -> Result<(), StoreError> {
        if let Some(id) = content_id(self.store.connection(), &input.facts.path, 2)? {
            input.facts.tags = Self::read_all_on(
                self.store.connection(),
                &self.read_work,
                FRONTMATTER_TAGS_SQL,
                [id],
                crate::request::stored_tag,
                "reading frontmatter tags",
            )?;
            debug_assert_eq!(input.facts.tags.len(), input.frontmatter_tags);
        }
        Ok(())
    }

    /// Fill content once for an arbitrary set of at most one host chunk. This
    /// is also NORN-468's pull-ahead carrier and NORN-469's requeue carrier:
    /// their path selection uses this same writer, with no second derivation
    /// path. Its findings pass a default directory set, which is safe only
    /// while link health stays deferred to step 4.
    pub fn write_content(
        &mut self,
        rows: &[ContentWrite],
        findings: &[crate::DerivedFinding<'_>],
        declared: &crate::ContentModel,
    ) -> Result<usize, StoreError> {
        #[cfg(feature = "induced-failure")]
        let _timing = norn_db::profiling::span("store.step.content");
        require_document_step(self.store)?;
        crate::increment::check_derived_findings(findings)?;
        ensure_group(self.store, rows.len())?;
        let connection = self.store.connection();
        let pinned: Option<String> =
            norn_db::meta::get_meta(connection, crate::ddl::meta::VAULT_SCHEMA_FINGERPRINT)?;
        crate::increment::refuse_a_declaration_the_store_does_not_pin(declared, pinned.as_deref())?;
        let stamp = norn_db::meta::next_generation(connection)?;
        let mut statements = crate::increment::Statements::prepare(connection)?;
        let mut skipped = std::collections::BTreeSet::new();
        for ContentWrite { facts, origin } in rows {
            crate::increment::refuse_a_document_that_does_not_add_up(facts)?;
            crate::increment::refuse_typed_values_the_pin_does_not_derive(
                pinned.as_deref(),
                facts,
            )?;
            let projection = facts
                .frontmatter()
                .map(crate::json::canonical_json)
                .transpose()?;
            let id = content_id(connection, &facts.path, 1)?;
            let Some(id) = id else {
                skipped.insert(facts.path.as_str());
                continue;
            };
            connection
                .execute(
                    CONTENT_WRITE_SQL,
                    norn_db::rusqlite::params![
                        facts.content_hash,
                        facts.byte_length,
                        facts.body,
                        crate::hash::sub_fingerprint(&facts.body),
                        facts.body_offset,
                        projection,
                        projection.as_deref().map(crate::hash::sub_fingerprint),
                        facts.frontmatter_diagnostic_count,
                        stamp,
                        crate::request::unix_seconds(),
                        id
                    ],
                )
                .map_err(|e| error::sql("writing construction content", e))?;
            crate::increment::insert_fact_rows(
                &mut statements,
                id,
                facts,
                crate::increment::FactPart::Frontmatter,
                0,
                0,
            )?;
            debug_assert!(facts.links.iter().all(|link| link.field.is_some()));
            debug_assert!(
                facts
                    .tags
                    .iter()
                    .all(|tag| tag.source == crate::TagSource::Frontmatter)
            );
            connection
                .execute(
                    CONTENT_PROGRESS_SQL,
                    norn_db::rusqlite::params![
                        id,
                        origin.line,
                        origin.column,
                        facts.links.len(),
                        facts.tags.len()
                    ],
                )
                .map_err(|e| error::sql("advancing content progress", e))?;
            self.counters
                .add(crate::counters::Counter::ContentRowsWritten, 1);
            self.counters.add(
                crate::counters::Counter::LinkRowsWritten,
                facts.links.len() as u64,
            );
            self.counters.add(
                crate::counters::Counter::TagRowsWritten,
                facts.tags.len() as u64,
            );
            self.counters.add(
                crate::counters::Counter::FieldRowsWritten,
                facts.fields().rows().len() as u64,
            );
            self.counters.add(
                crate::counters::Counter::FrontmatterProjections,
                u64::from(projection.is_some()),
            );
        }
        let findings = owed_findings(findings, &skipped);
        let discarded = crate::increment::write_derived_findings(
            connection,
            &findings,
            &mut Default::default(),
        )?;
        self.counters
            .add(crate::counters::Counter::FindingsDiscarded, discarded);
        self.counters.add(
            crate::counters::Counter::FindingsWritten,
            findings.len() as u64,
        );
        Ok(skipped.len())
    }

    /// Withdraw a vanished or byte-quarantined provisional row and its queue
    /// entry. It never represented a readable document, so it owns no death.
    pub fn withdraw_content(&mut self, path: &crate::DocumentPath) -> Result<(), StoreError> {
        require_document_step(self.store)?;
        ensure_group(self.store, 1)?;
        let connection = self.store.connection();
        if let Some(id) = content_id(connection, path, 1)? {
            connection
                .execute(WITHDRAW_PROGRESS_SQL, [id])
                .map_err(|e| error::sql("withdrawing content progress", e))?;
            connection
                .execute(WITHDRAW_SQL, [id])
                .map_err(|e| error::sql("withdrawing a provisional document", e))?;
        }
        Ok(())
    }

    /// Append structure once, with body ordinals after the frontmatter counts
    /// recorded by content. The vault pass and NORN-468 pull-ahead share this
    /// arbitrary-set seam, and NORN-469 PR B requeue will too; pull-ahead may call
    /// it during the content step for a requested document.
    pub fn write_structure(
        &mut self,
        rows: &[StructureInput],
        findings: &[crate::DerivedFinding<'_>],
        declared: &crate::ContentModel,
    ) -> Result<usize, StoreError> {
        #[cfg(feature = "induced-failure")]
        let _timing = norn_db::profiling::span("store.step.structure");
        if !matches!(
            self.store.construction_step(),
            Some(Step::Content | Step::Structure)
        ) {
            return Err(refused(
                self.store,
                "writing construction structure",
                "content or structure required",
            ));
        }
        crate::increment::check_derived_findings(findings)?;
        ensure_group(self.store, rows.len())?;
        let connection = self.store.connection();
        let pinned: Option<String> =
            norn_db::meta::get_meta(connection, crate::ddl::meta::VAULT_SCHEMA_FINGERPRINT)?;
        crate::increment::refuse_a_declaration_the_store_does_not_pin(declared, pinned.as_deref())?;
        let mut statements = crate::increment::Statements::prepare(connection)?;
        let mut skipped = std::collections::BTreeSet::new();
        for input in rows {
            let Some(id) = content_id(connection, &input.facts.path, 2)? else {
                skipped.insert(input.facts.path.as_str());
                continue;
            };
            let (links, tags): (usize, usize) = connection
                .query_row(ORDINALS_SQL, [id], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| error::sql("reading ordinal offsets", e))?;
            debug_assert_eq!(
                (links, tags),
                (input.frontmatter_links, input.frontmatter_tags)
            );
            debug_assert_eq!(
                input
                    .facts
                    .tags
                    .iter()
                    .filter(|t| t.source == crate::TagSource::Frontmatter)
                    .count(),
                tags
            );
            crate::increment::insert_fact_rows(
                &mut statements,
                id,
                &input.facts,
                crate::increment::FactPart::Body,
                links,
                tags,
            )?;
            connection
                .execute(STRUCTURE_PROGRESS_SQL, [id])
                .map_err(|e| error::sql("advancing structure progress", e))?;
            self.counters
                .add(crate::counters::Counter::StructureRowsWritten, 1);
            self.counters.add(
                crate::counters::Counter::LinkRowsWritten,
                input
                    .facts
                    .links
                    .iter()
                    .filter(|l| l.field.is_none())
                    .count() as u64,
            );
            self.counters.add(
                crate::counters::Counter::TagRowsWritten,
                input
                    .facts
                    .tags
                    .iter()
                    .filter(|t| t.source == crate::TagSource::Body)
                    .count() as u64,
            );
            self.counters.add(
                crate::counters::Counter::HeadingRowsWritten,
                input.facts.headings.len() as u64,
            );
            self.counters.add(
                crate::counters::Counter::BlockRowsWritten,
                input.facts.blocks.len() as u64,
            );
        }
        let findings = owed_findings(findings, &skipped);
        let discarded = crate::increment::write_derived_findings(
            connection,
            &findings,
            &mut Default::default(),
        )?;
        self.counters
            .add(crate::counters::Counter::FindingsDiscarded, discarded);
        self.counters.add(
            crate::counters::Counter::FindingsWritten,
            findings.len() as u64,
        );
        Ok(skipped.len())
    }
}

/// The findings a step writer files beside its rows: none about a document it
/// skipped because that document no longer owes the step, whose own write has
/// already filed them. A finding about a path the call passed no row for (a
/// withdrawn, byte-quarantined document's content finding) still stands.
fn owed_findings<'f, 'a>(
    findings: &'f [crate::DerivedFinding<'a>],
    skipped: &std::collections::BTreeSet<&str>,
) -> std::borrow::Cow<'f, [crate::DerivedFinding<'a>]> {
    if skipped.is_empty() {
        return std::borrow::Cow::Borrowed(findings);
    }
    findings
        .iter()
        .filter(|finding| !skipped.contains(finding.facts.path.as_str()))
        .cloned()
        .collect()
}

/// Content may be owed again during the structure pass.
fn require_document_step(store: &Store) -> Result<(), StoreError> {
    if matches!(store.construction_step(), Some(Step::Content | Step::Structure)) {
        Ok(())
    } else {
        Err(refused(store, "writing construction content", "document step required"))
    }
}
/// Reject a writer invoked outside its construction phase.
fn require_step(store: &Store, step: Step) -> Result<(), StoreError> {
    if store.construction_step() == Some(step) {
        Ok(())
    } else {
        Err(refused(store, "writing construction facts", "wrong step"))
    }
}
/// Keep each host chunk within the shared page bound.
fn check_limit(limit: usize) -> Result<(), StoreError> {
    if limit == 0 || limit > crate::MAX_PAGE {
        Err(StoreError::Bound {
            what: "construction chunk",
            limit: crate::MAX_PAGE,
            given: limit,
        })
    } else {
        Ok(())
    }
}
/// Seek a selected document only if it still owes the requested phase.
fn content_id(
    connection: &Connection,
    path: &crate::DocumentPath,
    stage: i64,
) -> Result<Option<i64>, StoreError> {
    use norn_db::rusqlite::OptionalExtension;
    connection
        .query_row(CONTENT_ID_SQL, (path.as_str(), stage), |r| r.get(0))
        .optional()
        .map_err(|e| error::sql("reading construction identity", e))
}

pub(crate) const CONTENT_PAGE_SQL: &str = "SELECT d.path FROM temp.construction_progress AS p JOIN documents AS d ON d.id = p.document WHERE p.stage = 1 ORDER BY p.document LIMIT ?1";
pub(crate) const STRUCTURE_PAGE_SQL: &str = "SELECT d.id, d.path, d.content_hash, d.body, d.byte_length, d.body_offset, p.body_line, p.body_column, p.frontmatter_links, p.frontmatter_tags FROM temp.construction_progress AS p JOIN documents AS d ON d.id = p.document WHERE p.stage = 2 AND d.body IS NOT NULL ORDER BY p.document LIMIT ?1";
pub(crate) const STRUCTURE_DOCUMENT_SQL: &str = "SELECT d.id, d.path, d.content_hash, d.body, d.byte_length, d.body_offset, p.body_line, p.body_column, p.frontmatter_links, p.frontmatter_tags FROM documents AS d JOIN temp.construction_progress AS p ON p.document = d.id WHERE d.path = ?1 AND p.stage = 2 AND d.body IS NOT NULL";
pub(crate) const FRONTMATTER_TAGS_SQL: &str = "SELECT name, source, span_line, span_column, span_offset FROM document_tags WHERE document = ?1 ORDER BY ordinal";
pub(crate) const CONTENT_WRITE_SQL: &str = "UPDATE documents SET content_hash = ?1, byte_length = ?2, body = ?3, body_hash = ?4, body_offset = ?5, frontmatter = ?6, frontmatter_projection_hash = ?7, frontmatter_diagnostic_count = ?8, generation = ?9, derived_at = ?10 WHERE id = ?11";

impl crate::Request<'_> {
    /// Keep a refused spelling until content settles its rendered place. Raw
    /// path bytes preserve invalid UTF-8 without a vault-sized host queue.
    pub fn defer_construction_spelling(&mut self, path: &[u8]) -> Result<(), StoreError> {
        require_step(self.store, Step::Names)?;
        self.store
            .connection()
            .execute(
                "INSERT OR IGNORE INTO temp.construction_spellings VALUES (?1)",
                [path],
            )
            .map_err(|e| error::sql("deferring a construction spelling", e))?;
        Ok(())
    }

    /// Drain one bounded batch of refused spellings after content has settled.
    pub fn construction_spellings(&mut self, limit: usize) -> Result<Vec<Vec<u8>>, StoreError> {
        check_limit(limit)?;
        let connection = self.store.connection();
        let mut statement = connection
            .prepare_cached("SELECT path FROM temp.construction_spellings ORDER BY path LIMIT ?1")
            .map_err(|e| error::sql("preparing deferred spellings", e))?;
        let paths = statement
            .query_map([limit as i64], |r| r.get(0))
            .map_err(|e| error::sql("reading deferred spellings", e))?
            .collect::<Result<Vec<Vec<u8>>, _>>()
            .map_err(|e| error::sql("reading deferred spelling bytes", e))?;
        for path in &paths {
            connection
                .execute(
                    "DELETE FROM temp.construction_spellings WHERE path = ?1",
                    [path],
                )
                .map_err(|e| error::sql("taking a deferred spelling", e))?;
        }
        Ok(paths)
    }
}

/// One document or file a target's class holds during construction, and the
/// construction step it still owes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetCandidate {
    pub path: crate::DocumentPath,
    /// [`Step::Content`] for a provisional name, [`Step::Structure`] for a
    /// document whose structure is owed, `None` for one that owes neither: a
    /// structured or broken document, or a vault file.
    pub owes: Option<Step>,
}

impl crate::Request<'_> {
    /// The documents and files `target`'s class holds under `ignore`, each
    /// with the step it still owes, resolved as a Get resolves it — exact-path
    /// precedence included — or `None` where the class holds more than
    /// `limit`. A provisional exact candidate can hide suffix candidates, so a
    /// caller re-reads after the content step settles any candidate. A target
    /// no class compiles for names nothing, as a Get finds.
    ///
    /// The NORN-468 pull-ahead caller drains Get requests through this after
    /// step 1 has ended, when the class indexes exist.
    pub fn target_candidates(
        &mut self,
        target: &str,
        ignore: &crate::AmbiguityIgnore,
        limit: usize,
    ) -> Result<Option<Vec<TargetCandidate>>, StoreError> {
        check_limit(limit)?;
        let Ok(class) = crate::resolve::TargetClass::compile(target, ignore) else {
            return Ok(Some(Vec::new()));
        };
        let (sql, parameters) = crate::find::compose_class_head(&class, limit + 1);
        let rows = Self::read_all_on(
            self.store.connection(),
            &self.read_work,
            &sql,
            norn_db::rusqlite::params_from_iter(parameters),
            |row| Ok(Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))),
            "reading a target's construction candidates",
        )?;
        if rows.len() > limit {
            return Ok(None);
        }
        rows.into_iter()
            .map(|(id, path)| {
                let owes = if id < 0 {
                    None
                } else {
                    match progress_stage(self.store.connection(), id)? {
                        Some(1) => Some(Step::Content),
                        Some(2) => Some(Step::Structure),
                        _ => None,
                    }
                };
                Ok(TargetCandidate {
                    path: crate::DocumentPath::new(&path)?,
                    owes,
                })
            })
            .collect::<Result<_, StoreError>>()
            .map(Some)
    }
}

/// A document's construction stage: 1 owes content, 2 owes structure, 3 is
/// structured; absent for a broken document, whose progress ended.
fn progress_stage(connection: &Connection, id: i64) -> Result<Option<i64>, StoreError> {
    use norn_db::rusqlite::OptionalExtension;
    connection
        .query_row(PROGRESS_STAGE_SQL, [id], |r| r.get(0))
        .optional()
        .map_err(|e| error::sql("reading a candidate's construction stage", e))
}

const PROGRESS_STAGE_SQL: &str = "SELECT stage FROM temp.construction_progress WHERE document = ?1";

impl crate::Request<'_> {
    /// Whether a provisional name still owes content (a preceding directory
    /// failure in the same chunk may have removed its queue entry).
    pub fn content_owed(&mut self, path: &crate::DocumentPath) -> Result<bool, StoreError> {
        Ok(content_id(self.store.connection(), path, 1)?.is_some())
    }
}

/// Decode the same stored body and origin for a queue page or a selected path.
fn stored_structure(row: &norn_db::rusqlite::Row<'_>) -> crate::request::Reading<StructureInput> {
    let path: String = row.get(1)?;
    let path = match crate::DocumentPath::new(&path) {
        Ok(path) => path,
        Err(error) => return Ok(Err(error)),
    };
    let mut facts = crate::DocumentFacts::new(
        path,
        row.get::<_, String>(2)?,
        row.get::<_, String>(3)?,
        row.get(4)?,
    );
    facts.body_offset = row.get(5)?;
    Ok(Ok(StructureInput {
        origin: crate::Span {
            line: row.get(6)?,
            column: row.get(7)?,
            byte_offset: facts.body_offset,
        },
        facts,
        frontmatter_links: row.get(8)?,
        frontmatter_tags: row.get(9)?,
    }))
}

impl Store {
    /// Commit each index independently before publishing this step's boundary.
    fn build_step_indexes(&mut self, step: Step) -> Result<(), StoreError> {
        #[cfg(feature = "induced-failure")]
        let _timing = norn_db::profiling::span("store.step.index_build");
        for (name, boundary) in crate::ddl::construction::DEFERRED {
            if *boundary != step {
                continue;
            }
            let transaction = self
                .database
                .immediate_transaction("building a construction index")?;
            let exists: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'index' AND name = ?1)",
                    [name],
                    |r| r.get(0),
                )
                .map_err(|e| error::sql("checking construction index", e))?;
            if !exists {
                transaction
                    .execute_batch(&crate::ddl::construction::statement(name))
                    .map_err(|e| error::sql("building construction index", e))?;
            }
            transaction
                .commit()
                .map_err(|e| error::sql("committing construction index", e))?;
        }
        Ok(())
    }
}

const CONTENT_ID_SQL: &str = "SELECT d.id FROM documents AS d JOIN temp.construction_progress AS p ON p.document = d.id WHERE d.path = ?1 AND p.stage = ?2";

const CONTENT_PROGRESS_SQL: &str = "UPDATE temp.construction_progress SET stage = 2, body_line = ?2, body_column = ?3, frontmatter_links = ?4, frontmatter_tags = ?5 WHERE document = ?1";

const WITHDRAW_PROGRESS_SQL: &str = "DELETE FROM temp.construction_progress WHERE document = ?1";

const WITHDRAW_SQL: &str = "DELETE FROM documents WHERE id = ?1";

const ORDINALS_SQL: &str = "SELECT frontmatter_links, frontmatter_tags FROM temp.construction_progress WHERE document = ?1";

const STRUCTURE_PROGRESS_SQL: &str =
    "UPDATE temp.construction_progress SET stage = 3 WHERE document = ?1";

const EXHAUSTED_SQL: &str =
    "SELECT EXISTS(SELECT 1 FROM temp.construction_progress WHERE stage <= ?1)";

#[cfg(test)]
mod plan_tests {
    use super::*;

    #[test]
    fn construction_writes_and_progress_seek_under_deferral() {
        let scratch = norn_testkit::scratch::Scratch::new("construction-private-plans");
        let mut store = Store::open(
            scratch.join("store.sqlite3"),
            crate::StoredPathOrder::Sensitive,
            crate::DerivationVersion::new(1),
        )
        .unwrap();
        store.defer_link_health().unwrap();
        store.begin_steps().unwrap();
        for phase in [Step::Names, Step::Content, Step::Structure] {
            for sql in [
                CONTENT_ID_SQL,
                CONTENT_WRITE_SQL,
                CONTENT_PROGRESS_SQL,
                WITHDRAW_PROGRESS_SQL,
                WITHDRAW_SQL,
                ORDINALS_SQL,
                STRUCTURE_PROGRESS_SQL,
                EXHAUSTED_SQL,
                PROGRESS_STAGE_SQL,
            ] {
                let count = store.connection().prepare(sql).unwrap().parameter_count();
                let parameters = vec![norn_db::rusqlite::types::Value::Integer(1); count];
                let plan = store
                    .database
                    .emitted_plan(sql, norn_db::rusqlite::params_from_iter(parameters))
                    .unwrap();
                assert!(
                    !plan.steps.iter().any(|row| row.detail.starts_with("SCAN ")
                        && !row.detail.starts_with("SCAN CONSTANT ROW")),
                    "{phase:?}: {plan:?}"
                );
            }
            store.finish_step(phase).unwrap();
        }
    }
}
