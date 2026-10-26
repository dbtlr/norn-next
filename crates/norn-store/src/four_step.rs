//! Disposable phased construction experiments. Nothing here is a warm write path.

use crate::{ContentModel, DocumentPath, OpenOutcome, Store, StoreError};
use crate::{error, hash, request, write_path::WriteStatement};
use norn_db::rusqlite::{Connection, OptionalExtension, params};

pub(crate) const MARKER: &str = "four_step_incomplete";
const FTS_CURSOR: &str = "four_step_fts_cursor";
const FTS_MODE: &str = "four_step_fts_mode";

/// Fixed inclusive timing vocabulary for isolated loading workers.
pub const PROFILE_LABELS: &[&str] = &[
    "lab.commit",
    "lab.index_build",
    "lab.trigrams",
    "lab.checkpoint",
    "lab.fts",
    "lab.read_hash",
    "lab.parse_derive",
    "lab.sql",
    "lab.rows.documents",
    "lab.rows.vault_files",
    "lab.rows.links",
    "lab.rows.link_keys",
    "lab.rows.headings",
    "lab.rows.blocks",
    "lab.rows.document_tags",
    "lab.rows.document_fields",
    "store.increment",
    "store.link_health",
    "store.commit",
    "store.upsert",
    "store.document_row",
    "health.discard",
    "health.prepare_discard",
    "health.judge",
    "health.read_links",
    "health.resolve_keys",
    "health.anchors",
    "health.candidate_names",
    "store.prepare_read",
    "store.write_finding",
    "store.finding_metadata",
    "host.heal",
    "host.watch_start",
    "host.store_open",
    "host.watch_sync_wait",
    "host.schema_pin",
    "host.attach",
    "host.reconcile",
    "host.document_read",
    "host.close_job",
    "host.parse_map",
    "host.frontmatter_parse",
    "host.body_scan",
    "host.document_plan",
];

/// Chunk work; commits counts published SQLite transactions. Row counts cover
/// documents/files; fact counts include
/// link keys and stem trigrams. Lab bookkeeping rows are excluded. The host
/// supplies `files_read`; marker creation/removal each report their own commit.
/// `rows_rewritten` counts the content/metadata transition; `body_rows_rewritten`
/// counts the separate payload update when content omitted the body. SQLite's
/// internal FTS shadow-table writes are not included in these counts.
/// `fts_rows_inserted` counts the document rows indexed by the full-text pass.
/// `keys_resolved` and `candidates_read` are final health's key resolutions and
/// the candidates they read, as the ordinary judge counts them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StepCounters {
    pub rows_inserted: u64,
    pub documents_processed: u64,
    pub rows_rewritten: u64,
    pub body_rows_rewritten: u64,
    pub fts_rows_inserted: u64,
    pub rows_deleted: u64,
    pub tombstones_discarded: u64,
    pub fact_rows_written: u64,
    pub links_judged: u64,
    pub keys_resolved: u64,
    pub candidates_read: u64,
    pub findings_written: u64,
    pub findings_discarded: u64,
    pub commits: u64,
    pub files_read: u64,
}

/// Persist the nonadoption guard, remove FTS triggers, and create private progress
/// in one commit. Bodies remain at rest while full-text indexing is deferred.
pub fn begin(store: &mut Store) -> Result<StepCounters, StoreError> {
    if !matches!(store.open_outcome(), OpenOutcome::Created) {
        return Err(refused(
            "four-step construction requires a newly created store",
        ));
    }
    let cache: i64 = store
        .connection()
        .pragma_query_value(None, "cache_size", |r| r.get(0))
        .map_err(|e| error::sql("reading build cache setting", e))?;
    let wal: i64 = store
        .connection()
        .pragma_query_value(None, "wal_autocheckpoint", |r| r.get(0))
        .map_err(|e| error::sql("reading build WAL setting", e))?;
    let path_order = match store.path_order() {
        crate::StoredPathOrder::Sensitive => "path",
        crate::StoredPathOrder::AsciiCaseInsensitive => "path COLLATE NOCASE, path",
    };
    let tx = Chunk::new(store.connection(), "starting four-step construction")?;
    let empty: bool = tx
        .query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM documents) AND NOT EXISTS(SELECT 1 FROM vault_files)
         AND NOT EXISTS(SELECT 1 FROM tombstones)
         AND NOT EXISTS(SELECT 1 FROM meta WHERE key = ?1)",
            [MARKER],
            |r| r.get(0),
        )
        .map_err(|e| error::sql("checking four-step eligibility", e))?;
    if !empty {
        return Err(refused(
            "four-step construction requires empty documents, files and tombstones",
        ));
    }
    norn_db::meta::put_meta(&tx, "four_step_cache", cache)?;
    norn_db::meta::put_meta(&tx, "four_step_wal", wal)?;
    tx.execute_batch("PRAGMA wal_autocheckpoint=10000; PRAGMA cache_size=-65536")
        .map_err(|e| error::sql("tuning build connection", e))?;
    norn_db::meta::put_meta(&tx, MARKER, 1_i64)?;
    tx.execute_batch(
        "DROP TRIGGER documents_fts_insert;
         DROP TRIGGER documents_fts_delete;
         DROP TRIGGER documents_fts_update;",
    )
    .map_err(|e| error::sql("deferring four-step full-text indexing", e))?;
    tx.execute_batch("CREATE TABLE four_step_indexes(name TEXT PRIMARY KEY, sql TEXT NOT NULL, boundary INTEGER NOT NULL) WITHOUT ROWID;
        CREATE TEMP TABLE four_step_trigrams(trigram TEXT NOT NULL, document INTEGER NOT NULL);")
        .map_err(|e| error::sql("creating deferred build state", e))?;
    for sql in crate::ddl::statements()
        .into_iter()
        .filter(|sql| sql.starts_with("CREATE INDEX "))
    {
        let name = sql.split_whitespace().nth(2).expect("index name");
        // Findings are admitted by the first per-document Get. Their indexes
        // also bound per-document finding replacement during content derivation.
        let boundary = if matches!(name, "findings_path" | "findings_rule_set") {
            1
        } else if name.starts_with("document_fields_") || name == "documents_path_nocase" {
            2
        } else if name.starts_with("documents_")
            && name != "documents_change_feed"
            && name != "documents_content_hash"
            || name.starts_with("vault_files_")
            || name.starts_with("link_keys_")
            || name.starts_with("headings_")
            || name.starts_with("blocks_")
        {
            3
        } else {
            5
        };
        tx.execute(
            "INSERT INTO four_step_indexes VALUES (?1, ?2, ?3)",
            params![name, sql, boundary],
        )
        .map_err(|e| error::sql("retaining production index SQL", e))?;
        tx.execute_batch(&format!("DROP INDEX {name}"))
            .map_err(|e| error::sql("deferring secondary index", e))?;
    }
    // A valid empty body can equal the sentinel. Private progress state makes
    // pull-ahead skipping unambiguous and retains source-line/append offsets;
    // dropping it at completion preserves the production schema fingerprint.
    tx.execute_batch(&format!(
        "CREATE TABLE four_step_state (
        path TEXT PRIMARY KEY REFERENCES documents(path) ON DELETE CASCADE,
        stage INTEGER NOT NULL DEFAULT 0,
        structure_ns INTEGER NOT NULL DEFAULT 0,
        body_line_offset INTEGER NOT NULL DEFAULT 0,
        body_column_offset INTEGER NOT NULL DEFAULT 0,
        frontmatter_tag_names TEXT NOT NULL DEFAULT '[]',
        frontmatter_links INTEGER NOT NULL DEFAULT 0,
        frontmatter_tags INTEGER NOT NULL DEFAULT 0,
        frontmatter_fields INTEGER NOT NULL DEFAULT 0
    ) WITHOUT ROWID;
    CREATE TABLE four_step_refused (
        id INTEGER PRIMARY KEY,
        path TEXT NOT NULL,
        kind TEXT NOT NULL CHECK(kind IN ('document/path-bytes-not-utf8', 'document/path-names-no-document')),
        message TEXT NOT NULL,
        detail TEXT
    );
    CREATE INDEX four_step_pending ON four_step_state(stage, {path_order});"
    ))
    .map_err(|e| error::sql("creating four-step progress", e))?;
    committed(tx, StepCounters::default())
}

fn build_indexes(store: &mut Store, boundary: i64) -> Result<StepCounters, StoreError> {
    let indexes = store
        .connection()
        .prepare_cached(
            "SELECT name, sql FROM four_step_indexes WHERE boundary <= ?1 ORDER BY name",
        )
        .and_then(|mut q| {
            q.query_map([boundary], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()
        })
        .map_err(|e| error::sql("reading deferred indexes", e))?;
    if indexes.is_empty() {
        return Ok(StepCounters::default());
    }
    let tx = Chunk::new(store.connection(), "building capability indexes")?;
    for (name, sql) in indexes {
        let _span = norn_db::profiling::span("lab.index_build");
        tx.execute_batch(&sql)
            .map_err(|e| error::sql("building production index", e))?;
        tx.execute("DELETE FROM four_step_indexes WHERE name = ?1", [name])
            .map_err(|e| error::sql("completing deferred index", e))?;
    }
    committed(tx, StepCounters::default())
}

/// Seal name-only construction before any per-document read is admitted.
pub fn names_complete(store: &mut Store) -> Result<StepCounters, StoreError> {
    build_indexes(store, 1)
}

/// Build the metadata indexes before admitting field/path Find.
pub fn content_complete(store: &mut Store) -> Result<StepCounters, StoreError> {
    require_complete(store.connection(), "content_done")?;
    build_indexes(store, 2)
}

/// Build resolution indexes and sorted trigrams before the first health judgment.
pub fn structure_complete(store: &mut Store) -> Result<StepCounters, StoreError> {
    require_complete(store.connection(), "structure_done")?;
    let mut counters = build_indexes(store, 3)?;
    let empty: bool = store
        .connection()
        .query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM temp.four_step_trigrams)",
            [],
            |r| r.get(0),
        )
        .map_err(|e| error::sql("checking buffered trigrams", e))?;
    if empty {
        return Ok(counters);
    }
    let tx = Chunk::new(store.connection(), "sorting build trigrams")?;
    let _span = norn_db::profiling::span("lab.trigrams");
    tx.execute_batch(
        "INSERT OR IGNORE INTO document_trigrams SELECT t.trigram, t.document
        FROM temp.four_step_trigrams t JOIN documents d ON d.id = t.document
        ORDER BY t.trigram, t.document; DELETE FROM temp.four_step_trigrams",
    )
    .map_err(|e| error::sql("writing sorted trigrams", e))?;
    counters.commits += tx.commit()?;
    Ok(counters)
}

/// Insert one host-bounded, strictly path-ordered name chunk. File names use
/// the ordinary negative IDs and document names the ordinary positive IDs.
pub fn names(
    store: &mut Store,
    documents: &[DocumentPath],
    files: &[DocumentPath],
    declared: &ContentModel,
) -> Result<StepCounters, StoreError> {
    names_with_refused(store, documents, files, &[], declared)
}

/// Queue spelling findings in the names commit. The private queue retains only
/// path, kind, message and detail until content establishes document eligibility.
pub fn names_with_refused(
    store: &mut Store,
    documents: &[DocumentPath],
    files: &[DocumentPath],
    refused_findings: &[crate::FindingFacts],
    declared: &ContentModel,
) -> Result<StepCounters, StoreError> {
    let _sql_span = norn_db::profiling::span("lab.sql");
    bounded(refused_findings.len())?;
    for finding in refused_findings {
        check_refused_spelling(finding)?;
    }
    bounded(documents.len())?;
    bounded(files.len())?;
    let order = store.path_order();

    let tx = Chunk::new(store.connection(), "writing four-step names")?;
    guard(&tx, declared)?;
    phase(&tx, 1)?;
    let mut counters = StepCounters::default();
    let generation = 0_i64;
    for (table, paths) in [("documents", documents), ("vault_files", files)] {
        let direction = if table == "documents" { "DESC" } else { "ASC" };
        let mut last: Option<String> = tx
            .prepare_cached(&format!(
                "SELECT path FROM {table} ORDER BY id {direction} LIMIT 1"
            ))
            .and_then(|mut q| q.query_row([], |r| r.get(0)))
            .optional()
            .map_err(|e| error::sql("reading the last four-step name", e))?;
        for path in paths {
            if last.as_deref().is_some_and(|last| {
                let next = path.as_str();
                let comparison = match order {
                    crate::StoredPathOrder::Sensitive => last.cmp(next),
                    crate::StoredPathOrder::AsciiCaseInsensitive => last
                        .bytes()
                        .map(|byte| byte.to_ascii_lowercase())
                        .cmp(next.bytes().map(|byte| byte.to_ascii_lowercase()))
                        .then_with(|| last.cmp(next)),
                };
                !comparison.is_lt()
            }) {
                return Err(refused(
                    "name chunks must be in strictly increasing path order",
                ));
            }
            last = Some(path.as_str().to_owned());
            if table == "vault_files" && !path.is_vault_file() {
                continue;
            }
            let admitting = declared
                .ambiguity_ignore()
                .admitting_segments(path.as_str());
            let _row_span = norn_db::profiling::span(if table == "documents" {
                "lab.rows.documents"
            } else {
                "lab.rows.vault_files"
            });
            if table == "documents" {
                let id: i64 = tx
                    .prepare_cached(WriteStatement::UpsertDocument.sql())
                    .and_then(|mut q| {
                        q.query_row(
                            params![
                                path.as_str(),
                                path.suffix_key(),
                                path.path_key(),
                                admitting as i64,
                                "",
                                0_i64,
                                "",
                                hash::sub_fingerprint(""),
                                0_i64,
                                Option::<String>::None,
                                Option::<String>::None,
                                0_i64,
                                generation,
                                request::unix_seconds()
                            ],
                            |r| r.get(0),
                        )
                    })
                    .map_err(|e| error::sql("inserting a provisional document", e))?;
                tx.prepare_cached("INSERT INTO four_step_state(path) VALUES (?1)")
                    .and_then(|mut q| q.execute([path.as_str()]))
                    .map_err(|e| error::sql("tracking a provisional document", e))?;
                for trigram in crate::similarity::trigrams(path.stem()) {
                    counters.fact_rows_written += tx
                        .prepare_cached("INSERT INTO temp.four_step_trigrams VALUES (?1, ?2)")
                        .and_then(|mut q| q.execute(params![trigram, id]))
                        .map_err(|e| error::sql("writing a stem trigram", e))?
                        as u64;
                }
            } else {
                tx.prepare_cached(WriteStatement::UpsertVaultFile.sql())
                    .and_then(|mut q| {
                        q.execute(params![
                            path.as_str(),
                            path.suffix_key(),
                            path.path_key(),
                            admitting as i64,
                            generation
                        ])
                    })
                    .map_err(|e| error::sql("inserting a four-step file", e))?;
            }
            counters.rows_inserted += 1;
        }
    }
    for finding in refused_findings {
        tx.prepare_cached(
            "INSERT INTO four_step_refused(path, kind, message, detail) VALUES (?1, ?2, ?3, ?4)",
        )
        .and_then(|mut q| {
            q.execute(params![
                finding.path.as_str(),
                finding.kind.as_str(),
                finding.message,
                finding.detail
            ])
        })
        .map_err(|e| error::sql("queueing a four-step refused spelling", e))?;
    }
    committed(tx, counters)
}

fn check_refused_spelling(finding: &crate::FindingFacts) -> Result<(), StoreError> {
    use norn_wire::FindingKind;
    if !matches!(
        finding.kind,
        FindingKind::PathBytesNotUtf8 | FindingKind::PathNamesNoDocument
    ) || finding.severity != finding.kind.default_severity()
        || !finding.class_keys.is_empty()
        || !finding.path_keys.is_empty()
        || finding.target.is_some()
        || finding.span.is_some()
        || finding.ordinal.is_some()
        || !finding.candidates.is_empty()
        || finding.candidates_total != 0
        || !finding.rules.is_empty()
        || finding.value.is_some()
    {
        return Err(refused(
            "a queued spelling finding may carry only its path, canonical kind/severity, message and detail",
        ));
    }
    Ok(())
}

/// Whether bounded structure work still owes any deferred spelling findings.
pub fn has_refused(store: &Store) -> Result<bool, StoreError> {
    if norn_db::meta::get_meta::<i64>(store.connection(), MARKER)?.is_none() {
        return Err(refused("four-step construction has not begun"));
    }
    refused_pending(store.connection())
}

fn refused_pending(connection: &Connection) -> Result<bool, StoreError> {
    connection
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM four_step_refused)")
        .and_then(|mut q| q.query_row([], |row| row.get(0)))
        .map_err(|e| error::sql("checking queued four-step refused spellings", e))
}

fn drain_refused(
    tx: &Connection,
    limit: usize,
    counters: &mut StepCounters,
) -> Result<(), StoreError> {
    let mut query = tx
        .prepare_cached(
            "SELECT r.id, r.path, r.kind, r.message, r.detail,
        NOT EXISTS(SELECT 1 FROM documents d WHERE d.path = r.path)
        FROM four_step_refused r ORDER BY r.id LIMIT ?1",
        )
        .map_err(|e| error::sql("preparing queued four-step refused spellings", e))?;
    let rows = query
        .query_map([limit as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, bool>(5)?,
            ))
        })
        .map_err(|e| error::sql("reading queued four-step refused spellings", e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| error::sql("reading a queued four-step refused spelling", e))?;
    let Some(last) = rows.last().map(|row| row.0) else {
        return Ok(());
    };
    let mut findings = Vec::new();
    for (_, path, kind, message, detail, recordable) in rows {
        if !recordable {
            continue;
        }
        let kind = match kind.as_str() {
            "document/path-bytes-not-utf8" => norn_wire::FindingKind::PathBytesNotUtf8,
            "document/path-names-no-document" => norn_wire::FindingKind::PathNamesNoDocument,
            _ => return Err(refused("invalid queued four-step refused spelling kind")),
        };
        findings.push(crate::DerivedFinding {
            facts: crate::FindingFacts {
                path: DocumentPath::new(&path)?,
                kind,
                severity: kind.default_severity(),
                message,
                detail,
                class_keys: Default::default(),
                path_keys: Default::default(),
                target: None,
                span: None,
                ordinal: None,
                candidates: vec![],
                candidates_total: 0,
                rules: Default::default(),
                value: None,
            },
            replaces: None,
            bytes_hash: None,
        });
    }
    crate::increment::check_derived_findings(&findings)?;
    counters.findings_discarded += crate::increment::write_derived_findings(tx, &findings)?;
    counters.findings_written += findings.len() as u64;
    tx.prepare_cached("DELETE FROM four_step_refused WHERE id <= ?1")
        .and_then(|mut q| q.execute([last]))
        .map_err(|e| error::sql("consuming queued four-step refused spellings", e))?;
    Ok(())
}

fn bounded(size: usize) -> Result<(), StoreError> {
    if size > crate::MAX_PAGE {
        return Err(StoreError::Bound {
            what: "a four-step host chunk",
            limit: crate::MAX_PAGE,
            given: size,
        });
    }
    Ok(())
}
fn refused(message: &str) -> StoreError {
    StoreError::Damaged {
        what: message.into(),
    }
}
fn guard(connection: &Connection, declared: &ContentModel) -> Result<(), StoreError> {
    if norn_db::meta::get_meta::<i64>(connection, MARKER)?.is_none() {
        return Err(refused("four-step construction has not begun"));
    }
    let pinned: Option<String> =
        norn_db::meta::get_meta(connection, crate::ddl::meta::VAULT_SCHEMA_FINGERPRINT)?;
    if pinned.as_deref() != declared.schema() {
        return Err(StoreError::UnpinnedDeclaration {
            what: "four-step construction declaration",
            derived_under: declared.schema().map(str::to_owned),
            pinned,
        });
    }
    Ok(())
}
// A failed chunk rolls back its writes even inside a larger host transaction.
// Ordinary store writes still use the substrate's immediate transactions.
struct Chunk<'a> {
    connection: &'a Connection,
    grouped: bool,
    complete: bool,
}
impl<'a> Chunk<'a> {
    fn new(connection: &'a Connection, operation: &'static str) -> Result<Self, StoreError> {
        let grouped = !connection.is_autocommit();
        connection
            .execute_batch(if grouped {
                "SAVEPOINT four_step_chunk"
            } else {
                "BEGIN IMMEDIATE"
            })
            .map_err(|e| error::sql(operation, e))?;
        Ok(Self {
            connection,
            grouped,
            complete: false,
        })
    }
    fn commit(mut self) -> Result<u64, StoreError> {
        let _span = if self.grouped {
            None
        } else {
            norn_db::profiling::span("lab.commit")
        };
        self.connection
            .execute_batch(if self.grouped {
                "RELEASE four_step_chunk"
            } else {
                "COMMIT"
            })
            .map_err(|e| error::sql("committing a four-step chunk", e))?;
        self.complete = true;
        Ok(u64::from(!self.grouped))
    }
}
impl std::ops::Deref for Chunk<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.connection
    }
}
impl Drop for Chunk<'_> {
    fn drop(&mut self) {
        if !self.complete {
            let _ = self.connection.execute_batch(if self.grouped {
                "ROLLBACK TO four_step_chunk; RELEASE four_step_chunk"
            } else {
                "ROLLBACK"
            });
        }
    }
}
fn committed(tx: Chunk<'_>, mut counters: StepCounters) -> Result<StepCounters, StoreError> {
    counters.commits += tx.commit()?;
    Ok(counters)
}

/// Start a bounded host group. Each chunk remains separately rollback-safe.
pub fn begin_group(store: &mut Store) -> Result<u64, StoreError> {
    store
        .connection()
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| error::sql("starting a four-step transaction group", e))?;
    Ok(store.connection().total_changes())
}

/// Publish the group's chunks to snapshot readers, reporting actual commits.
pub fn commit_group(store: &mut Store, changes_at_start: u64) -> Result<StepCounters, StoreError> {
    if store.connection().is_autocommit() {
        return Ok(StepCounters::default());
    }
    if store.connection().total_changes() == changes_at_start {
        store
            .connection()
            .execute_batch("ROLLBACK")
            .map_err(|e| error::sql("closing an empty four-step group", e))?;
        return Ok(StepCounters::default());
    }
    let _span = norn_db::profiling::span("lab.commit");
    store
        .connection()
        .execute_batch("COMMIT")
        .map_err(|e| error::sql("committing a four-step transaction group", e))?;
    Ok(StepCounters {
        commits: 1,
        ..StepCounters::default()
    })
}

/// Roll back an unfinished host group on error or unwind.
pub fn rollback_group(store: &mut Store) -> Result<(), StoreError> {
    if !store.connection().is_autocommit() {
        store
            .connection()
            .execute_batch("ROLLBACK")
            .map_err(|e| error::sql("rolling back four-step construction", e))?;
    }
    Ok(())
}

/// The next pass whose incomplete rows the host wants to drive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Step {
    Content,
    Structure,
    Health,
}

/// A bounded source row; frontmatter counts are the append ordinals for body facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingDocument {
    pub path: DocumentPath,
    pub content_hash: String,
    pub byte_length: u64,
    pub body: String,
    pub body_offset: u64,
    pub body_line_offset: u64,
    pub body_column_offset: u64,
    pub frontmatter_tag_names: Vec<String>,
    pub frontmatter_links: u64,
    pub frontmatter_tags: u64,
    pub frontmatter_fields: u64,
    receipt: SourceReceipt,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceReceipt {
    epoch: String,
    origin: std::path::PathBuf,
    metadata_hash: String,
}

impl PendingDocument {
    // The receipt protects mutable public source metadata without fetching it
    // again. Epoch and origin distinguish stores, including copied databases.
    fn metadata_hash(&self) -> String {
        let metadata = serde_json::to_string(&(
            self.path.as_str(),
            &self.content_hash,
            self.byte_length,
            self.body_offset,
            self.body_line_offset,
            self.body_column_offset,
            self.frontmatter_links,
            self.frontmatter_tags,
            self.frontmatter_fields,
            &self.frontmatter_tag_names,
        ))
        .expect("pending source metadata encodes as JSON");
        hash::sub_fingerprint(&metadata)
    }
}

/// Read the first incomplete rows in store path order. Completing a chunk
/// advances this query; no vault-sized cursor or host map is needed.
pub fn pending(
    store: &Store,
    step: Step,
    limit: usize,
) -> Result<Vec<PendingDocument>, StoreError> {
    pending_rows(store, step, limit, None)
}

/// Point seek for a requested document that can be pulled ahead of the main pass.
pub fn pending_at(
    store: &Store,
    step: Step,
    path: &DocumentPath,
) -> Result<Option<PendingDocument>, StoreError> {
    Ok(pending_rows(store, step, 1, Some(path))?.pop())
}

fn pending_rows(
    store: &Store,
    step: Step,
    limit: usize,
    path: Option<&DocumentPath>,
) -> Result<Vec<PendingDocument>, StoreError> {
    bounded(limit)?;
    if limit == 0 {
        return Err(refused("a four-step page must be nonempty"));
    }
    if norn_db::meta::get_meta::<i64>(store.connection(), MARKER)?.is_none() {
        return Err(refused("four-step construction has not begun"));
    }
    let condition = match step {
        Step::Content => "s.stage = 0",
        Step::Structure => "s.stage = 1",
        Step::Health => "s.stage = 2",
    };
    let point = if path.is_some() {
        "s.path = ?2"
    } else {
        "?2 IS NULL"
    };
    let collation = store.path_order().collation();
    let mut q = store
        .connection()
        .prepare_cached(&format!(
            "SELECT d.path, d.content_hash, d.byte_length, d.body, d.body_offset,
                s.body_line_offset, s.frontmatter_links, s.frontmatter_tags, s.frontmatter_fields,
                s.body_column_offset, s.frontmatter_tag_names
         FROM four_step_state AS s CROSS JOIN documents AS d
         WHERE d.path = s.path AND {condition} AND {point}
         ORDER BY s.path{collation}, s.path LIMIT ?1"
        ))
        .map_err(|e| error::sql("preparing four-step progress page", e))?;
    let rows = q
        .query_map(params![limit as i64, path.map(DocumentPath::as_str)], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get::<_, String>(10)?,
            ))
        })
        .map_err(|e| error::sql("reading four-step progress", e))?;
    rows.map(|row| {
        let (
            path,
            content_hash,
            byte_length,
            body,
            body_offset,
            body_line_offset,
            frontmatter_links,
            frontmatter_tags,
            frontmatter_fields,
            body_column_offset,
            frontmatter_tag_names,
        ) = row.map_err(|e| error::sql("reading four-step source row", e))?;
        let mut document = PendingDocument {
            path: DocumentPath::new(&path)?,
            content_hash,
            byte_length,
            body,
            body_offset,
            body_line_offset,
            body_column_offset,
            frontmatter_tag_names: serde_json::from_str(&frontmatter_tag_names)
                .map_err(|e| refused(&format!("invalid four-step tag names: {e}")))?,
            frontmatter_links,
            frontmatter_tags,
            frontmatter_fields,
            receipt: SourceReceipt {
                epoch: store.database.epoch().into(),
                origin: store.path().to_owned(),
                metadata_hash: String::new(),
            },
        };
        document.receipt.metadata_hash = document.metadata_hash();
        Ok(document)
    })
    .collect()
}

/// Fill content columns once and insert frontmatter facts only. `body_origins`
/// is parallel to `documents`: zero-based (line, column) body origins. With
/// `no_body`, structure later supplies the body omitted here.
pub fn content(
    store: &mut Store,
    documents: &[crate::DocumentFacts],
    findings: &[crate::DerivedFinding<'_>],
    declared: &ContentModel,
    no_body: bool,
    body_origins: &[(u64, u64)],
) -> Result<StepCounters, StoreError> {
    let _sql_span = norn_db::profiling::span("lab.sql");
    bounded(documents.len())?;
    if documents.len() != body_origins.len() {
        return Err(refused("body origins must match the content chunk"));
    }
    crate::increment::check_derived_findings(findings)?;
    names_complete(store)?;
    let tx = Chunk::new(store.connection(), "writing four-step content")?;
    guard(&tx, declared)?;
    phase(&tx, 2)?;
    let pinned: Option<String> =
        norn_db::meta::get_meta(&tx, crate::ddl::meta::VAULT_SCHEMA_FINGERPRINT)?;
    let mut counters = StepCounters::default();
    let generation = norn_db::meta::next_generation(&tx)?;
    {
        let mut statements = crate::increment::Statements::prepare(&tx)?;
        let body_columns = if no_body {
            ""
        } else {
            "body = ?3, body_hash = ?4,"
        };
        let mut update = tx
            .prepare_cached(&format!(
                "UPDATE documents SET content_hash = ?1, byte_length = ?2, {body_columns}
            body_offset = ?5, frontmatter = ?6, frontmatter_projection_hash = ?7,
            frontmatter_diagnostic_count = ?8, generation = ?9, derived_at = ?10 WHERE id = ?11"
            ))
            .map_err(|e| error::sql("preparing four-step content", e))?;
        let mut progress = tx
            .prepare_cached(
                "UPDATE four_step_state SET stage = 1, body_line_offset = ?1,
            frontmatter_links = ?2, frontmatter_tags = ?3, frontmatter_fields = ?4,
            body_column_offset = ?6, frontmatter_tag_names = ?7 WHERE path = ?5",
            )
            .map_err(|e| error::sql("preparing four-step content progress", e))?;
        for (facts, (line_offset, column_offset)) in documents.iter().zip(body_origins) {
            crate::increment::refuse_a_document_that_does_not_add_up(facts)?;
            crate::increment::refuse_typed_values_the_pin_does_not_derive(
                pinned.as_deref(),
                facts,
            )?;
            let document = incomplete_document(&tx, &facts.path, Step::Content)?;
            let projection = facts
                .frontmatter()
                .map(crate::json::canonical_json)
                .transpose()?;
            let body = if no_body { "" } else { &facts.body };
            let row_span = norn_db::profiling::span("lab.rows.documents");
            update
                .execute(params![
                    facts.content_hash,
                    facts.byte_length,
                    body,
                    hash::sub_fingerprint(body),
                    facts.body_offset,
                    projection,
                    projection.as_deref().map(hash::sub_fingerprint),
                    facts.frontmatter_diagnostic_count,
                    generation,
                    request::unix_seconds(),
                    document
                ])
                .map_err(|e| error::sql("updating four-step content", e))?;
            drop(row_span);
            counters.rows_rewritten += 1;
            counters.fact_rows_written += crate::increment::insert_fact_rows(
                &mut statements,
                document,
                facts,
                crate::increment::FactPart::Frontmatter,
                0,
                0,
            )?;
            let tag_names = facts
                .tags
                .iter()
                .filter(|tag| tag.source == crate::TagSource::Frontmatter)
                .map(|tag| tag.name.as_str())
                .collect::<Vec<_>>();
            progress
                .execute(params![
                    line_offset,
                    facts.links.iter().filter(|l| l.field.is_some()).count() as i64,
                    tag_names.len() as i64,
                    facts.fields().rows().len() as i64,
                    facts.path.as_str(),
                    column_offset,
                    serde_json::to_string(&tag_names).expect("tag names encode as JSON")
                ])
                .map_err(|e| error::sql("completing four-step content row", e))?;
        }
    }
    // A provisional name is not a document whose death was observed. Removing
    // an undecodable name therefore leaves no tombstone, matching a build from
    // zero. Its already allocated ID gap is deliberately accepted.
    for finding in findings
        .iter()
        .filter(|f| f.facts.kind == crate::CONTENT_FINDING_KIND)
    {
        let path = &finding.facts.path;
        let done: Option<i64> = tx
            .prepare_cached("SELECT stage FROM four_step_state WHERE path = ?1")
            .and_then(|mut q| q.query_row([path.as_str()], |r| r.get(0)))
            .optional()
            .map_err(|e| error::sql("checking a provisional quarantine", e))?;
        if done.is_some_and(|stage| stage >= 1) {
            return Err(refused("a completed content row cannot be quarantined"));
        }
        if done.is_some() {
            counters.rows_deleted += tx
                .prepare_cached("DELETE FROM documents WHERE path = ?1")
                .and_then(|mut q| q.execute([path.as_str()]))
                .map_err(|e| error::sql("removing a provisional quarantine", e))?
                as u64;
        }
    }
    counters.findings_discarded = crate::increment::write_derived_findings(&tx, findings)?;
    counters.findings_written = findings.len() as u64;
    committed(tx, counters)
}

fn phase(tx: &Connection, phase: i64) -> Result<(), StoreError> {
    let current: i64 = norn_db::meta::get_meta(tx, MARKER)?
        .ok_or_else(|| refused("four-step construction has not begun"))?;
    if current > phase {
        return Err(refused("a completed four-step phase cannot be revisited"));
    }
    norn_db::meta::put_meta(tx, MARKER, phase)?;
    Ok(())
}

fn incomplete_document(
    tx: &Connection,
    path: &DocumentPath,
    step: Step,
) -> Result<i64, StoreError> {
    let sql = match step {
        Step::Content => {
            "SELECT d.id FROM documents d JOIN four_step_state s ON s.path = d.path
            WHERE d.path = ?1 AND s.stage = 0"
        }
        Step::Structure => {
            "SELECT d.id FROM documents d JOIN four_step_state s ON s.path = d.path
            WHERE d.path = ?1 AND s.stage = 1"
        }
        Step::Health => {
            "SELECT d.id FROM documents d JOIN four_step_state s ON s.path = d.path
            WHERE d.path = ?1 AND s.stage = 2"
        }
    };
    tx.prepare_cached(sql)
        .and_then(|mut q| q.query_row([path.as_str()], |r| r.get(0)))
        .optional()
        .map_err(|e| error::sql("checking an incomplete four-step document", e))?
        .ok_or_else(|| refused("a four-step document is absent or this step is already complete"))
}

/// Append only body facts, preserving the frontmatter rows and their ordinals.
/// When content omitted the body, fill only its payload and sub-fingerprint.
/// `pending` is parallel to `documents`, carrying the source identity and append
/// offsets already read by the host. Host findings commit with the body facts;
/// an empty document chunk can flush deferred spelling findings without touching
/// provisional document rows.
pub fn structure(
    store: &mut Store,
    documents: &[crate::DocumentFacts],
    pending: &[PendingDocument],
    findings: &[crate::DerivedFinding<'_>],
    declared: &ContentModel,
    no_body: bool,
) -> Result<StepCounters, StoreError> {
    structure_with_refused(store, documents, pending, findings, declared, no_body, 0)
}

/// Complete structure and consume at most `refused_limit` queued spellings in
/// the same commit. Withheld findings consume their queue slots too, so each
/// call holds at most one bounded queue page. Draining requires complete content.
pub fn structure_with_refused(
    store: &mut Store,
    documents: &[crate::DocumentFacts],
    pending: &[PendingDocument],
    findings: &[crate::DerivedFinding<'_>],
    declared: &ContentModel,
    no_body: bool,
    refused_limit: usize,
) -> Result<StepCounters, StoreError> {
    let _sql_span = norn_db::profiling::span("lab.sql");
    bounded(refused_limit)?;
    bounded(documents.len())?;
    if documents.len() != pending.len() {
        return Err(refused("pending rows must match the structure chunk"));
    }
    crate::increment::check_derived_findings(findings)?;
    for source in pending {
        if source.receipt.epoch != store.database.epoch()
            || source.receipt.origin != store.path()
            || source.receipt.metadata_hash != source.metadata_hash()
        {
            return Err(refused(
                "structure requires unchanged pending metadata from this store",
            ));
        }
    }
    let tx = Chunk::new(store.connection(), "writing four-step structure")?;
    guard(&tx, declared)?;
    if refused_limit > 0 {
        require_complete(&tx, "content_done")?;
    }
    // A requested row may finish structure while other rows still owe
    // content. Names are sealed globally; progress after that is per path.
    phase(&tx, 2)?;
    let mut counters = StepCounters::default();
    {
        let mut statements = crate::increment::Statements::prepare(&tx)?;
        for (facts, source) in documents.iter().zip(pending) {
            crate::increment::refuse_a_document_that_does_not_add_up(facts)?;
            let document = incomplete_document(&tx, &facts.path, Step::Structure)?;
            if source.path != facts.path
                || source.content_hash != facts.content_hash
                || source.byte_length != facts.byte_length
                || source.body_offset != facts.body_offset
            {
                return Err(refused(
                    "structure must describe the bytes content recorded",
                ));
            }
            counters.fact_rows_written += crate::increment::insert_fact_rows(
                &mut statements,
                document,
                facts,
                crate::increment::FactPart::Body,
                source.frontmatter_links as usize,
                source.frontmatter_tags as usize,
            )?;
            if no_body {
                tx.prepare_cached("UPDATE documents SET body = ?1, body_hash = ?2 WHERE id = ?3")
                    .and_then(|mut q| {
                        q.execute(params![
                            facts.body,
                            hash::sub_fingerprint(&facts.body),
                            document
                        ])
                    })
                    .map_err(|e| error::sql("filling the four-step body", e))?;
                counters.body_rows_rewritten += 1;
            }
            tx.prepare_cached("UPDATE four_step_state SET stage = 2 WHERE path = ?1")
                .and_then(|mut q| q.execute([facts.path.as_str()]))
                .map_err(|e| error::sql("completing four-step structure row", e))?;
        }
    }
    counters.findings_discarded = crate::increment::write_derived_findings(&tx, findings)?;
    counters.findings_written = findings.len() as u64;
    if refused_limit > 0 {
        drain_refused(&tx, refused_limit, &mut counters)?;
    }
    committed(tx, counters)
}

fn require_complete(tx: &Connection, column: &str) -> Result<(), StoreError> {
    let stage = match column {
        "content_done" => 1,
        "structure_done" => 2,
        "health_done" => 3,
        _ => unreachable!(),
    };
    let incomplete: bool = tx
        .query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM four_step_state WHERE stage < {stage})"),
            [],
            |r| r.get(0),
        )
        .map_err(|e| error::sql("checking the preceding four-step pass", e))?;
    if incomplete {
        return Err(refused("the preceding four-step pass is incomplete"));
    }
    Ok(())
}

/// Retain a committed structure chunk's elapsed time for later request timings.
/// This private bookkeeping transaction does not advance the write generation.
pub fn record_structure_ns(
    store: &mut Store,
    paths: &[DocumentPath],
    ns: u128,
) -> Result<(), StoreError> {
    bounded(paths.len())?;
    let ns = i64::try_from(ns)
        .map_err(|_| refused("four-step structure time exceeds SQLite's integer range"))?;
    if paths.is_empty() {
        return Ok(());
    }
    let tx = Chunk::new(store.connection(), "recording four-step structure time")?;
    for path in paths {
        let changed = tx
            .prepare_cached(
                "UPDATE four_step_state SET structure_ns = ?1 WHERE path = ?2 AND stage >= 2",
            )
            .and_then(|mut query| query.execute(params![ns, path.as_str()]))
            .map_err(|e| error::sql("recording a four-step structure time", e))?;
        if changed != 1 {
            return Err(refused(
                "four-step structure time requires a completed document",
            ));
        }
    }
    tx.commit().map(|_| ())
}

/// Read the retained time only when this document's structure is complete.
pub fn structure_ns(store: &Store, path: &DocumentPath) -> Result<Option<u128>, StoreError> {
    store
        .connection()
        .prepare_cached("SELECT structure_ns FROM four_step_state WHERE path = ?1 AND stage >= 2")
        .and_then(|mut query| query.query_row([path.as_str()], |row| row.get::<_, u64>(0)))
        .optional()
        .map(|value| value.map(u128::from))
        .map_err(|e| error::sql("reading a four-step structure time", e))
}

/// Judge the next bounded chunk of completed documents, once per eligible link.
/// The chunk's links and findings are paged across its documents in the
/// ordinary judge, which shares one key-summary set for the transaction.
/// An exhausted pass opens no write transaction and reports zero commits.
pub fn health(
    store: &mut Store,
    limit: usize,
    declared: &ContentModel,
) -> Result<StepCounters, StoreError> {
    let _sql_span = norn_db::profiling::span("lab.sql");
    bounded(limit)?;
    if limit == 0 {
        return Err(refused("a four-step page must be nonempty"));
    }
    guard(store.connection(), declared)?;
    require_complete(store.connection(), "structure_done")?;
    if refused_pending(store.connection())? {
        return Err(refused("four-step refused spellings remain queued"));
    }
    let collation = store.path_order().collation();
    // Health needs identities alone; reading the pending source projection
    // would materialize bodies that this pass never uses.
    let documents = {
        let mut query = store
            .connection()
            .prepare_cached(&format!(
                "SELECT path FROM four_step_state WHERE stage = 2
                 ORDER BY path{collation}, path LIMIT ?1"
            ))
            .map_err(|e| error::sql("preparing four-step health documents", e))?;
        query
            .query_map([limit as i64], |row| row.get::<_, String>(0))
            .map_err(|e| error::sql("reading four-step health documents", e))?
            .map(|row| {
                DocumentPath::new(
                    &row.map_err(|e| error::sql("reading a four-step health path", e))?,
                )
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    if documents.is_empty() {
        return Ok(StepCounters::default());
    }
    structure_complete(store)?;
    let tx = Chunk::new(store.connection(), "writing four-step link health")?;
    guard(&tx, declared)?;
    phase(&tx, 4)?;
    let work = request::ReadWork::default();
    let mut counters = StepCounters::default();
    let mut summaries = crate::health::KeySummaries::default();
    for document in &documents {
        incomplete_document(&tx, document, Step::Health)?;
    }
    (counters.links_judged, counters.findings_written) = crate::health::four_step_documents(
        &tx,
        &work,
        declared.ambiguity_ignore(),
        &documents,
        &mut summaries,
    )?;
    for document in &documents {
        tx.prepare_cached("UPDATE four_step_state SET stage = 3 WHERE path = ?1")
            .and_then(|mut q| q.execute([document.as_str()]))
            .map_err(|e| error::sql("completing four-step link health row", e))?;
    }
    counters.documents_processed = documents.len() as u64;
    counters.keys_resolved = summaries.keys_resolved();
    counters.candidates_read = summaries.candidates_read();
    committed(tx, counters)
}

/// The isolated full-text construction strategy used by the lab's fifth pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FtsBuildMode {
    Rebuild,
    Batched,
}

/// Build full text from every current document, including reconciled additions.
/// Rebuild runs once; Batched commits at most `bound` document rows per call,
/// seeking by ID without materializing bodies in Rust. The final write installs
/// the production triggers atomically. Call again for zero commits at exhaustion;
/// the host can drain requests after every nonzero result. Mode cannot change
/// after construction starts. A rebuild deliberately has no internal boundary.
pub fn full_text(
    store: &mut Store,
    mode: FtsBuildMode,
    bound: usize,
) -> Result<StepCounters, StoreError> {
    let _sql_span = norn_db::profiling::span("lab.sql");
    bounded(bound)?;
    if bound == 0 {
        return Err(refused("a four-step page must be nonempty"));
    }
    let current: i64 = norn_db::meta::get_meta(store.connection(), MARKER)?
        .ok_or_else(|| refused("four-step construction has not begun"))?;
    let mode_name = match mode {
        FtsBuildMode::Rebuild => "rebuild",
        FtsBuildMode::Batched => "batched",
    };
    let previous_mode: Option<String> = norn_db::meta::get_meta(store.connection(), FTS_MODE)?;
    if previous_mode
        .as_deref()
        .is_some_and(|previous| previous != mode_name)
    {
        return Err(refused("four-step full-text build mode cannot change"));
    }
    if current == 6 {
        return Ok(StepCounters::default());
    }
    require_complete(store.connection(), "content_done")?;
    require_complete(store.connection(), "structure_done")?;
    require_complete(store.connection(), "health_done")?;
    if refused_pending(store.connection())? {
        return Err(refused("four-step refused spellings remain queued"));
    }
    let tx = Chunk::new(store.connection(), "building four-step full text")?;
    phase(&tx, 5)?;
    norn_db::meta::put_meta(&tx, FTS_MODE, mode_name)?;
    let fts_span = norn_db::profiling::span("lab.fts");
    let (fts_rows_inserted, complete) = match mode {
        FtsBuildMode::Rebuild => {
            let rows: u64 = tx
                .query_row("SELECT count(*) FROM documents", [], |row| row.get(0))
                .map_err(|e| error::sql("counting four-step full-text documents", e))?;
            tx.execute(
                "INSERT INTO documents_fts(documents_fts) VALUES ('rebuild')",
                [],
            )
            .map_err(|e| error::sql("rebuilding four-step full text", e))?;
            (rows, true)
        }
        FtsBuildMode::Batched => {
            let cursor = norn_db::meta::get_meta::<i64>(&tx, FTS_CURSOR)?.unwrap_or(0);
            let last: i64 = tx
                .query_row(
                    "SELECT coalesce(max(id), ?1) FROM
                     (SELECT id FROM documents WHERE id > ?1 ORDER BY id LIMIT ?2)",
                    params![cursor, bound as i64],
                    |row| row.get(0),
                )
                .map_err(|e| error::sql("seeking a four-step full-text batch", e))?;
            let rows = tx
                .execute(
                    "INSERT INTO documents_fts(rowid, body)
                     SELECT id, body FROM documents WHERE id > ?1 ORDER BY id LIMIT ?2",
                    params![cursor, bound as i64],
                )
                .map_err(|e| error::sql("inserting a four-step full-text batch", e))?;
            norn_db::meta::put_meta(&tx, FTS_CURSOR, last)?;
            let more: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM documents WHERE id > ?1)",
                    [last],
                    |row| row.get(0),
                )
                .map_err(|e| error::sql("checking remaining four-step full-text documents", e))?;
            (rows as u64, !more)
        }
    };
    drop(fts_span);
    if complete {
        for statement in crate::ddl::fts::statements()
            .into_iter()
            .filter(|statement| statement.starts_with("CREATE TRIGGER "))
        {
            tx.execute_batch(&statement)
                .map_err(|e| error::sql("restoring production full-text triggers", e))?;
        }
        phase(&tx, 6)?;
    }
    committed(
        tx,
        StepCounters {
            fts_rows_inserted,
            ..StepCounters::default()
        },
    )
}

/// Restore every ordinary access path before falling back to churn healing.
pub fn prepare_reconcile(store: &mut Store) -> Result<StepCounters, StoreError> {
    build_indexes(store, 5)
}

/// Remove the private table before clearing the marker, atomically. A crash
/// before this commit still rebuilds; a crash after it can adopt a normal store.
pub fn finish(store: &mut Store) -> Result<StepCounters, StoreError> {
    let indexes = build_indexes(store, 5)?;
    let tx = Chunk::new(store.connection(), "finishing four-step construction")?;
    match norn_db::meta::get_meta::<i64>(&tx, MARKER)? {
        None => return Err(refused("four-step construction has not begun")),
        Some(6) => {}
        Some(_) => return Err(refused("four-step full-text construction is incomplete")),
    }
    require_complete(&tx, "content_done")?;
    require_complete(&tx, "structure_done")?;
    require_complete(&tx, "health_done")?;
    if refused_pending(&tx)? {
        return Err(refused("four-step refused spellings remain queued"));
    }
    // No construction state has been adoptable. Deaths the ordinary churn
    // reconciliation observed are transient first-build history; a build from
    // zero over the final tree has none. begin() refuses pre-existing deaths,
    // so this cannot remove history from an adopted store.
    let tombstones_discarded =
        tx.execute("DELETE FROM tombstones", [])
            .map_err(|e| error::sql("discarding construction-only tombstones", e))? as u64;
    tx.execute_batch(
        "DROP TABLE four_step_state; DROP TABLE four_step_refused; DROP TABLE four_step_indexes",
    )
    .map_err(|e| error::sql("dropping four-step progress", e))?;
    tx.execute(
        "DELETE FROM meta WHERE key IN (?1, ?2, ?3)",
        [MARKER, FTS_CURSOR, FTS_MODE],
    )
    .map_err(|e| error::sql("clearing four-step construction marker", e))?;
    let cache: i64 = norn_db::meta::get_meta(&tx, "four_step_cache")?.expect("saved cache setting");
    let wal: i64 = norn_db::meta::get_meta(&tx, "four_step_wal")?.expect("saved WAL setting");
    tx.execute(
        "DELETE FROM meta WHERE key IN ('four_step_cache', 'four_step_wal')",
        [],
    )
    .map_err(|e| error::sql("clearing build settings", e))?;
    let counters = committed(
        tx,
        StepCounters {
            tombstones_discarded,
            commits: indexes.commits,
            ..StepCounters::default()
        },
    )?;
    store
        .connection()
        .execute_batch(&format!(
            "PRAGMA cache_size={cache}; PRAGMA wal_autocheckpoint={wal}"
        ))
        .map_err(|e| error::sql("restoring build connection", e))?;
    let _span = norn_db::profiling::span("lab.checkpoint");
    let (busy, _, _): (i64, i64, i64) = store
        .connection()
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(|e| error::sql("checkpointing completed construction", e))?;
    if busy != 0 {
        return Err(refused("completed construction checkpoint is busy"));
    }
    Ok(counters)
}

/// Discard an initial-build row whose source vanished or changed before it
/// became authoritative. This is no observed document death, so no tombstone
/// is recorded. Its progress and facts cascade with the provisional row.
pub fn forget(store: &mut Store, path: &DocumentPath) -> Result<StepCounters, StoreError> {
    let tx = Chunk::new(store.connection(), "forgetting a four-step source")?;
    let current: i64 = norn_db::meta::get_meta(&tx, MARKER)?
        .ok_or_else(|| refused("four-step construction has not begun"))?;
    if current >= 4 {
        return Err(refused("final health has already begun"));
    }
    let rows_deleted =
        tx.prepare_cached("DELETE FROM documents WHERE path = ?1")
            .and_then(|mut q| q.execute([path.as_str()]))
            .map_err(|e| error::sql("forgetting a provisional document", e))? as u64;
    let findings_discarded = request::discard_about_in(&tx, path, crate::DiscardScope::EveryKind)?;
    committed(
        tx,
        StepCounters {
            rows_deleted,
            findings_discarded,
            ..StepCounters::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn chunk_failure_and_host_abort_do_not_publish_partial_groups() {
        let scratch = norn_testkit::scratch::Scratch::under(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch"),
            "four-step-group-rollback",
        )
        .unwrap();
        let mut store = Store::open(
            scratch.join("store.sqlite3"),
            crate::StoredPathOrder::Sensitive,
            crate::DerivationVersion::new(1),
        )
        .unwrap();
        begin(&mut store).unwrap();
        let model = ContentModel::none();
        let a = DocumentPath::new("a.md").unwrap();
        let changes = begin_group(&mut store).unwrap();
        assert_eq!(
            names(&mut store, std::slice::from_ref(&a), &[], &model)
                .unwrap()
                .commits,
            0
        );
        assert!(names(&mut store, std::slice::from_ref(&a), &[], &model).is_err());
        assert_eq!(
            store
                .connection()
                .query_row("SELECT count(*) FROM documents", [], |r| r.get::<_, u64>(0))
                .unwrap(),
            1
        );
        let other = norn_db::open_read_only(store.path()).adopted.unwrap();
        assert_eq!(
            other
                .connection()
                .query_row("SELECT count(*) FROM documents", [], |r| r.get::<_, u64>(0))
                .unwrap(),
            0
        );
        rollback_group(&mut store).unwrap();
        assert_eq!(
            store
                .connection()
                .query_row("SELECT count(*) FROM documents", [], |r| r.get::<_, u64>(0))
                .unwrap(),
            0
        );
        let _ = changes;
        let changes = begin_group(&mut store).unwrap();
        names(&mut store, &[a], &[], &model).unwrap();
        assert_eq!(commit_group(&mut store, changes).unwrap().commits, 1);
        assert_eq!(
            other
                .connection()
                .query_row("SELECT count(*) FROM documents", [], |r| r.get::<_, u64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Inspect the test store's WAL, never vault files.
    fn ready_restores_connection_settings_and_has_no_checkpoint_tail() {
        let scratch = norn_testkit::scratch::Scratch::under(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch"),
            "four-step-checkpoint",
        )
        .unwrap();
        let path = scratch.join("store.sqlite3");
        let mut store = Store::open(
            &path,
            crate::StoredPathOrder::Sensitive,
            crate::DerivationVersion::new(1),
        )
        .unwrap();
        let pragma = |store: &Store, name| {
            store
                .connection()
                .pragma_query_value(None, name, |r| r.get::<_, i64>(0))
                .unwrap()
        };
        let cache = pragma(&store, "cache_size");
        let wal = pragma(&store, "wal_autocheckpoint");
        let sync = pragma(&store, "synchronous");
        begin(&mut store).unwrap();
        assert_eq!(pragma(&store, "cache_size"), -65536);
        assert_eq!(pragma(&store, "wal_autocheckpoint"), 10000);
        assert_eq!(pragma(&store, "synchronous"), sync);
        full_text(&mut store, FtsBuildMode::Rebuild, 128).unwrap();
        finish(&mut store).unwrap();
        assert_eq!(pragma(&store, "cache_size"), cache);
        assert_eq!(pragma(&store, "wal_autocheckpoint"), wal);
        assert_eq!(pragma(&store, "synchronous"), sync);
        assert_eq!(
            std::fs::metadata(path.with_extension("sqlite3-wal"))
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn secondary_indexes_wait_for_their_first_capability() {
        let scratch = norn_testkit::scratch::Scratch::under(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch"),
            "four-step-index-boundaries",
        )
        .unwrap();
        let mut store = Store::open(
            scratch.join("store.sqlite3"),
            crate::StoredPathOrder::Sensitive,
            crate::DerivationVersion::new(1),
        )
        .unwrap();
        begin(&mut store).unwrap();
        fn exists(store: &Store, name: &str) -> bool {
            store
                .connection()
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = ?1)",
                    [name],
                    |row| row.get::<_, bool>(0),
                )
                .unwrap()
        }
        assert!(exists(&store, "documents_path"));
        assert!(!exists(&store, "documents_content_hash"));
        assert!(!exists(&store, "document_fields_raw"));
        assert!(!exists(&store, "documents_suffix_key"));
        names_complete(&mut store).unwrap();
        assert!(exists(&store, "findings_path"));
        assert!(!exists(&store, "document_fields_raw"));
        content_complete(&mut store).unwrap();
        assert!(exists(&store, "documents_path_nocase"));
        assert!(exists(&store, "document_fields_raw"));
        assert!(!exists(&store, "documents_suffix_key"));
        structure_complete(&mut store).unwrap();
        assert!(exists(&store, "documents_suffix_key"));
        assert!(exists(&store, "vault_files_path_key"));
        assert!(exists(&store, "link_keys_key"));
        assert!(!exists(&store, "documents_content_hash"));
        full_text(&mut store, FtsBuildMode::Rebuild, 128).unwrap();
        finish(&mut store).unwrap();
        assert!(exists(&store, "documents_content_hash"));
        assert!(exists(&store, "document_trigrams_document_trigram"));
        assert!(!exists(&store, "four_step_indexes"));
    }

    #[test]
    fn full_text_restores_exact_schema_text_and_preserves_document_ids() {
        fn schema(store: &Store) -> Vec<(String, String, String, Option<String>)> {
            store
                .connection()
                .prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name")
                .unwrap()
                .query_map([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        }
        fn identities(store: &Store) -> Vec<(String, i64)> {
            store
                .connection()
                .prepare("SELECT path, id FROM documents ORDER BY path")
                .unwrap()
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        }
        for mode in [FtsBuildMode::Rebuild, FtsBuildMode::Batched] {
            let scratch = norn_testkit::scratch::Scratch::under(
                &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch"),
                "four-step-schema-fts",
            )
            .unwrap();
            let mut lab = Store::open(
                scratch.join("lab.sqlite3"),
                crate::StoredPathOrder::Sensitive,
                crate::DerivationVersion::new(1),
            )
            .unwrap();
            let mut ordinary = Store::open(
                scratch.join("ordinary.sqlite3"),
                crate::StoredPathOrder::Sensitive,
                crate::DerivationVersion::new(1),
            )
            .unwrap();
            let model = ContentModel::none();
            let facts = ["a.md", "b.md", "z.md"].map(|path| {
                crate::DocumentFacts::new(DocumentPath::new(path).unwrap(), path, "body", 4)
            });
            ordinary
                .begin_request()
                .apply_increment(
                    crate::IncrementProvenance::Derived,
                    facts.iter().cloned().map(crate::Change::Upsert),
                    &[],
                    &model,
                )
                .unwrap();
            let expected_schema = schema(&ordinary);
            begin(&mut lab).unwrap();
            names(
                &mut lab,
                &facts
                    .iter()
                    .map(|facts| facts.path.clone())
                    .collect::<Vec<_>>(),
                &[],
                &model,
            )
            .unwrap();
            content(&mut lab, &facts, &[], &model, false, &[(0, 0); 3]).unwrap();
            let pending = pending(&lab, Step::Structure, 3).unwrap();
            structure(&mut lab, &facts, &pending, &[], &model, false).unwrap();
            health(&mut lab, 3, &model).unwrap();
            let first = full_text(&mut lab, mode, 2).unwrap();
            assert_eq!(first.commits, 1);
            if mode == FtsBuildMode::Batched {
                let indexed = lab
                    .connection()
                    .prepare("SELECT id FROM documents_fts_docsize ORDER BY id")
                    .unwrap()
                    .query_map([], |row| row.get::<_, i64>(0))
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                assert_eq!(indexed, [1, 2], "batches advance in document ID order");
                let triggers: u64 = lab.connection().query_row("SELECT count(*) FROM sqlite_schema WHERE type = 'trigger' AND name LIKE 'documents_fts_%'", [], |row| row.get(0)).unwrap();
                assert_eq!(triggers, 0, "triggers wait for the final batch");
            }
            while full_text(&mut lab, mode, 2).unwrap().commits != 0 {}
            finish(&mut lab).unwrap();
            assert_eq!(schema(&lab), expected_schema);
            assert_eq!(identities(&lab), identities(&ordinary));
            lab.verify_integrity().unwrap();
            for store in [&mut lab, &mut ordinary] {
                store
                    .begin_request()
                    .apply_increment(
                        crate::IncrementProvenance::Derived,
                        [
                            crate::Change::Upsert(crate::DocumentFacts::new(
                                facts[0].path.clone(),
                                "changed",
                                "changed body",
                                12,
                            )),
                            crate::Change::Death {
                                path: facts[1].path.clone(),
                                provenance: crate::Provenance::WatcherRemoval,
                            },
                            crate::Change::Upsert(crate::DocumentFacts::new(
                                DocumentPath::new("zz.md").unwrap(),
                                "new",
                                "new body",
                                8,
                            )),
                        ],
                        &[],
                        &model,
                    )
                    .unwrap();
                store.verify_integrity().unwrap();
            }
            assert_eq!(identities(&lab), identities(&ordinary));
        }
    }

    #[test]
    fn first_four_passes_store_bodies_without_indexing() {
        let scratch = norn_testkit::scratch::Scratch::under(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch"),
            "four-step-deferred-fts",
        )
        .unwrap();
        let mut store = Store::open(
            scratch.join("store.sqlite3"),
            crate::StoredPathOrder::Sensitive,
            crate::DerivationVersion::new(1),
        )
        .unwrap();
        begin(&mut store).unwrap();
        let triggers: u64 = store
            .connection()
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type = 'trigger' AND name IN
                 ('documents_fts_insert', 'documents_fts_delete', 'documents_fts_update')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(triggers, 0, "lab construction defers all FTS writes");
        let model = ContentModel::none();
        let facts = crate::DocumentFacts::new(
            DocumentPath::new("a.md").unwrap(),
            "hash",
            "deferred searchable body",
            24,
        );
        names(&mut store, std::slice::from_ref(&facts.path), &[], &model).unwrap();
        content(
            &mut store,
            std::slice::from_ref(&facts),
            &[],
            &model,
            false,
            &[(0, 0)],
        )
        .unwrap();
        let pending = pending(&store, Step::Structure, 1).unwrap();
        structure(&mut store, &[facts], &pending, &[], &model, false).unwrap();
        health(&mut store, 1, &model).unwrap();
        let (body, indexed_rows, terms): (String, u64, u64) = store
            .connection()
            .query_row(
                "SELECT body, (SELECT count(*) FROM documents_fts_docsize),
                 (SELECT count(*) FROM documents_fts_vocab) FROM documents",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(body, "deferred searchable body");
        assert_eq!((indexed_rows, terms), (0, 0));
        assert!(
            store.verify_integrity().is_err(),
            "rank 1 detects unbuilt FTS"
        );
    }

    #[test]
    fn an_empty_document_set_with_existing_death_history_is_not_a_new_lab_store() {
        let scratch = norn_testkit::scratch::Scratch::under(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch"),
            "four-step-existing-deaths",
        )
        .unwrap();
        let mut store = Store::open(
            scratch.join("store.sqlite3"),
            crate::StoredPathOrder::Sensitive,
            crate::DerivationVersion::new(1),
        )
        .unwrap();
        store
            .begin_request()
            .apply_increment(
                crate::IncrementProvenance::Derived,
                [crate::Change::Death {
                    path: DocumentPath::new("prior.md").unwrap(),
                    provenance: crate::Provenance::WatcherRemoval,
                }],
                &[],
                &ContentModel::none(),
            )
            .unwrap();
        assert!(begin(&mut store).is_err());
    }

    #[test]
    fn the_marker_alone_prevents_adoption_even_when_the_production_schema_matches() {
        let scratch = norn_testkit::scratch::Scratch::under(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch"),
            "four-step-marker-only",
        )
        .unwrap();
        let path = scratch.join("store.sqlite3");
        let mut store = Store::open(
            &path,
            crate::StoredPathOrder::Sensitive,
            crate::DerivationVersion::new(1),
        )
        .unwrap();
        begin(&mut store).unwrap();
        let document = DocumentPath::new("unfinished.md").unwrap();
        names(
            &mut store,
            std::slice::from_ref(&document),
            &[],
            &ContentModel::none(),
        )
        .unwrap();
        build_indexes(&mut store, 5).unwrap();
        // Isolate the persisted guard from schema-digest rebuilding: a removed
        // progress table restores the production schema but not complete data.
        store
            .connection()
            .execute_batch("DROP TABLE four_step_state; DROP TABLE four_step_refused; DROP TABLE four_step_indexes")
            .unwrap();
        for statement in crate::ddl::fts::statements()
            .into_iter()
            .filter(|statement| statement.starts_with("CREATE TRIGGER "))
        {
            store.connection().execute_batch(&statement).unwrap();
        }
        drop(store);
        let mut reopened = Store::open(
            &path,
            crate::StoredPathOrder::Sensitive,
            crate::DerivationVersion::new(1),
        )
        .unwrap();
        assert!(
            format!("{:?}", reopened.open_outcome()).contains("interrupted four-step construction")
        );
        assert!(
            reopened
                .begin_request()
                .stored_document(&document)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn two_row_progress_pages_do_not_scan_completed_or_later_documents() {
        for order in [
            crate::StoredPathOrder::Sensitive,
            crate::StoredPathOrder::AsciiCaseInsensitive,
        ] {
            let mut work = Vec::new();
            for size in [1_000, 10_000] {
                let scratch = norn_testkit::scratch::Scratch::under(
                    &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch"),
                    "four-step-page-work",
                )
                .unwrap();
                let mut store = Store::open(
                    scratch.join("store.sqlite3"),
                    order,
                    crate::DerivationVersion::new(1),
                )
                .unwrap();
                begin(&mut store).unwrap();
                let model = ContentModel::none();
                for start in (0..size).step_by(128) {
                    let paths = (start..(start + 128).min(size))
                        .map(|n| DocumentPath::new(&format!("d{n:05}.md")).unwrap())
                        .collect::<Vec<_>>();
                    names(&mut store, &paths, &[], &model).unwrap();
                }
                let mut readings = Vec::new();
                for step in [Step::Content, Step::Structure, Step::Health] {
                    if !matches!(step, Step::Content) {
                        // Leave only the final two rows pending. A planner choosing
                        // another phase's index would walk every completed row.
                        store
                            .connection()
                            .execute("UPDATE four_step_state SET stage=3", [])
                            .unwrap();
                        let stage = if matches!(step, Step::Structure) {
                            1
                        } else {
                            2
                        };
                        store
                            .connection()
                            .execute(
                                &format!(
                                    "UPDATE four_step_state SET stage={stage} WHERE path >= ?1"
                                ),
                                [format!("d{:05}.md", size - 2)],
                            )
                            .unwrap();
                    }
                    let operations = Arc::new(AtomicU64::new(0));
                    let observed = Arc::clone(&operations);
                    store
                        .connection()
                        .progress_handler(
                            1,
                            Some(move || {
                                observed.fetch_add(1, Ordering::Relaxed);
                                false
                            }),
                        )
                        .unwrap();
                    assert_eq!(pending(&store, step, 2).unwrap().len(), 2);
                    store
                        .connection()
                        .progress_handler(0, None::<fn() -> bool>)
                        .unwrap();
                    readings.push(operations.load(Ordering::Relaxed));
                }
                work.push(readings);
            }
            for (small, large) in work[0].iter().zip(&work[1]) {
                assert!(
                    *large <= small + 32,
                    "a two-row {order:?} page grew with the vault: {work:?}"
                );
            }
        }
    }
}
