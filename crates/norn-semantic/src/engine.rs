//! The engine: drain the feed, keep the sidecar converged, answer nearest.

use std::path::Path;
use std::sync::Arc;

use norn_db::Database;
use norn_db::rusqlite::{OptionalExtension, params};
use norn_embed::{Embedder, Model};
use norn_store::{DocumentPath, FeedCursor, FeedRead};

use crate::ddl;
use crate::error::{self, EngineError};
use crate::sidecar::{self, SidecarOutcome};

/// How many feed rows one page asks for. Bounded well under the store's page
/// cap; the drain loops pages, so the bound shapes memory rather than reach.
const DRAIN_PAGE: usize = 256;

/// The first lane-2 engine: a semantic sidecar converging on the lane-1
/// record.
///
/// One engine is one `(sidecar database, embedder)` pair for one vault. It
/// consumes the store's change feed through consumer-owned cursors recorded
/// in its own sidecar, embeds changed bodies through the embedder, retracts
/// deaths, and answers vector-nearest over what it holds. It never touches
/// vault files: every input reaches it through [`FeedRead`], the store's
/// read-only lane-2 surface, so a committed lane-1 record is the only thing
/// it can consume and nothing it derives can enter the store.
///
/// **One engine writes one sidecar.** Nothing inside serializes two engines
/// over one path: a second writer could interleave with the read-triage-
/// commit sequence and land an older embedding over a newer one. The owner
/// that composes engines holds one per sidecar, the same discipline the
/// store states for its own requests.
pub struct Engine {
    database: Database,
    embedder: Arc<dyn Embedder>,
    outcome: SidecarOutcome,
}

/// What one drain did, in rows.
///
/// A reading for the caller and the suites, not derived state: nothing is
/// recorded, and two drains over the same quiescent store report all-zero
/// work the same way.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DrainReport {
    /// The store lifetime moved: the recorded epoch did not match at entry —
    /// the cursors were reset and the feed rescanned — or the epoch moved
    /// mid-drain, in which case this drain's reading is not a settled one
    /// and the next drain reconciles.
    pub rescan: bool,
    /// Paths the rescan's reconcile removed: they no longer stand in the
    /// store, and the store's own tombstones did not survive into its new
    /// lifetime to say so.
    pub reconciled_away: u64,
    /// Documents whose body was embedded and written.
    pub embedded: u64,
    /// Feed rows whose recorded embedding is already over the current body —
    /// triaged by fingerprint alone, without fetching the body.
    pub skipped_current: u64,
    /// Feed rows superseded mid-drain: the stored document had already moved
    /// past the row's generation (or died), so a later feed row covers it.
    pub deferred: u64,
    /// Paths whose rows were retracted because the feed recorded a death.
    pub retracted: u64,
}

impl DrainReport {
    /// Whether the drain found nothing to do.
    ///
    /// A reading of this drain's work, not a proof of convergence: a
    /// converged sidecar can still report work (a rescan re-triages every
    /// row it kept), and quiescence holds only until the next write. What an
    /// all-zero reading does say is that this drain observed one store
    /// lifetime end to end and nothing in it was new.
    pub fn is_settled(&self) -> bool {
        *self == DrainReport::default()
    }
}

/// One sidecar row, projected for comparison: the path, the input it was
/// computed over, and the values. This is the shape the convergence bar
/// compares — a settled sidecar equals a from-zero recompute over current
/// lane-1 rows, exactly.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorRow {
    pub path: String,
    /// The store's `body_hash` of the text that was embedded.
    pub input_hash: String,
    pub values: Vec<f32>,
}

/// One nearest answer: a path and its score, higher meaning nearer.
#[derive(Clone, Debug, PartialEq)]
pub struct Neighbor {
    pub path: String,
    pub score: f32,
}

impl Engine {
    /// Open, or create, the sidecar at `path`, embedding through `embedder`.
    pub fn open(path: impl AsRef<Path>, embedder: Arc<dyn Embedder>) -> Result<Self, EngineError> {
        let path = path.as_ref();
        let (connection, outcome) = sidecar::open(path, embedder.model())?;
        Ok(Engine {
            database: Database::adopt(connection, path)?,
            embedder,
            outcome,
        })
    }

    /// How the open ended up with this sidecar.
    pub fn open_outcome(&self) -> &SidecarOutcome {
        &self.outcome
    }

    /// The sidecar's own epoch — minted at create, moved only by a rebuild.
    pub fn epoch(&self) -> &str {
        self.database.epoch()
    }

    /// The model this engine embeds with. The sidecar records it — opening
    /// under a different model rebuilds from zero — and every projection and
    /// answer scopes to it, so a row is never read under a model that did
    /// not produce it.
    pub fn model(&self) -> &Model {
        self.embedder.model()
    }

    /// Discard the sidecar and open it again from zero.
    ///
    /// The resolution for [`EngineError::SidecarDamaged`], and always safe:
    /// every row is a projection of lane-1 records, and the reset cursors
    /// make the next drain recompute exactly what a fresh sidecar is missing.
    pub fn discard_and_reopen(self) -> Result<Self, EngineError> {
        let path = self.database.path().to_path_buf();
        let embedder = self.embedder.clone();
        drop(self.database);
        let connection = sidecar::rebuild(&path, embedder.model())?;
        Ok(Engine {
            database: Database::adopt(connection, &path)?,
            embedder,
            outcome: SidecarOutcome::RebuiltFromZero {
                detail: "discarded by its owner".to_string(),
            },
        })
    }

    /// Consume the store's change feed until both feeds are drained, and
    /// converge the sidecar on what they said.
    ///
    /// # The protocol
    ///
    /// 1. **Epoch first.** A store epoch that differs from the recorded one
    ///    means the cursors name positions in a database that no longer
    ///    exists: reconcile away rows whose paths the store no longer holds
    ///    (its tombstones did not survive into the new lifetime), reset both
    ///    cursors, and record the new epoch — one transaction, so a crash
    ///    replays the whole answer rather than half of it.
    /// 2. **Documents, triaged before fetched.** A feed row whose `body_hash`
    ///    equals the recorded `input_hash` is current and costs no body read.
    ///    A changed one is fetched; a fetch that comes back at a different
    ///    generation (or gone) was superseded mid-drain and is deferred — the
    ///    feed presents the successor at its own strictly-higher generation,
    ///    so nothing is lost by not acting on the stale reading.
    /// 3. **Tombstones.** The two feeds are disjoint over stored paths at
    ///    any snapshot, so the order between the loops carries no hazard; a
    ///    death or rebirth landing between them lands at a generation a
    ///    later page or the next drain presents.
    ///
    /// Every page commits its writes and its advanced cursor in one sidecar
    /// transaction: progress recorded is exactly progress written, and a
    /// replayed page is an idempotent set of upserts and deletes. The
    /// guarantee that a hash is never written beside values it does not
    /// describe is the drain's own — the generation check carries it even
    /// against a writer interleaving through a store handle this borrow does
    /// not cover.
    pub fn drain(&mut self, feed: &mut FeedRead<'_>) -> Result<DrainReport, EngineError> {
        let mut report = DrainReport::default();
        let store_epoch = feed.epoch().to_string();
        let recorded: Option<String> = self.get_meta(ddl::meta::OBSERVED_STORE_EPOCH)?;
        if recorded.as_deref() != Some(store_epoch.as_str()) {
            report.rescan = true;
            report.reconciled_away = self.reconcile(feed, &store_epoch)?;
        }

        let mut cursor = self.read_cursor(ddl::meta::DOCUMENT_CURSOR)?;
        loop {
            let page = feed.changed_documents_after(cursor.as_ref(), DRAIN_PAGE)?;
            let Some((last, _)) = page.last() else { break };
            let advanced = last.clone();
            let full = page.len() == DRAIN_PAGE;

            let mut writes: Vec<(String, String, Vec<u8>, usize)> = Vec::new();
            for (_, document) in &page {
                let current: Option<String> = self.row_input_hash(document.path.as_str())?;
                if current.as_deref() == Some(document.body_hash.as_str()) {
                    report.skipped_current += 1;
                    continue;
                }
                let fetched = feed.stored_facts(&document.path)?;
                match fetched {
                    Some(facts) if facts.document.generation == document.generation => {
                        let embedding = self.embedder.embed(&facts.body).map_err(|error| {
                            EngineError::Embed {
                                path: document.path.as_str().to_string(),
                                error,
                            }
                        })?;
                        let promised = self.embedder.dimensions().get();
                        if embedding.dimensions() != promised {
                            return Err(EngineError::WrongWidth {
                                path: document.path.as_str().to_string(),
                                promised,
                                produced: embedding.dimensions(),
                            });
                        }
                        writes.push((
                            document.path.as_str().to_string(),
                            document.body_hash.clone(),
                            encode(embedding.values()),
                            embedding.dimensions(),
                        ));
                        report.embedded += 1;
                    }
                    // Superseded mid-drain: re-derived past this row's
                    // generation, or died. The feed's later rows carry the
                    // truth, so acting on this reading would write a hash
                    // beside values it does not describe.
                    _ => report.deferred += 1,
                }
            }

            self.commit_documents(&writes, &advanced)?;
            cursor = Some(advanced);
            if !full {
                break;
            }
        }

        let mut cursor = self.read_cursor(ddl::meta::TOMBSTONE_CURSOR)?;
        loop {
            let page = feed.changed_tombstones_after(cursor.as_ref(), DRAIN_PAGE)?;
            let Some((last, _)) = page.last() else { break };
            let advanced = last.clone();
            let full = page.len() == DRAIN_PAGE;

            let paths: Vec<String> = page
                .iter()
                .map(|(_, tombstone)| tombstone.path.as_str().to_string())
                .collect();
            report.retracted += self.commit_retractions(&paths, &advanced)?;
            cursor = Some(advanced);
            if !full {
                break;
            }
        }

        // The lifetime is re-read at exit: a store discarded and rebuilt
        // mid-drain restarts its generations below the cursors, so both
        // loops read empty pages and the work counts above say nothing. The
        // recorded epoch moves only inside `reconcile`, so the next drain
        // reconciles; this flag keeps the reading from claiming settle.
        if feed.epoch() != store_epoch {
            report.rescan = true;
        }

        Ok(report)
    }

    /// Every row of this engine's model, in path order.
    pub fn projection(&self) -> Result<Vec<VectorRow>, EngineError> {
        let model = self.embedder.model().clone();
        let connection = self.database.connection();
        let mut statement = connection
            .prepare_cached(
                "SELECT path, input_hash, dimensions, embedding FROM document_vectors
                 WHERE model_id = ?1 AND model_version = ?2 ORDER BY path",
            )
            .map_err(|error| error::sql("preparing the projection read", error))?;
        let rows = statement
            .query_map(params![model.id(), model.version()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            })
            .map_err(|error| error::sql("reading the projection", error))?;
        let mut projected = Vec::new();
        for row in rows {
            let (path, input_hash, dimensions, blob) =
                row.map_err(|error| error::sql("reading a projection row", error))?;
            projected.push(VectorRow {
                path,
                input_hash,
                values: decode(&blob, dimensions)?,
            });
        }
        Ok(projected)
    }

    /// The `limit` nearest paths to `text`, under this engine's model.
    ///
    /// The score is the dot product against the query's embedding, and the
    /// order is total: score descending under `total_cmp` — which ranks a
    /// non-finite score a model produced above every finite one, and answers
    /// it deterministically rather than hiding it — then path ascending, so
    /// equal scores answer the same way on every run. A scan of the model's
    /// rows is the floor implementation — the index that makes this
    /// sublinear is a storage mechanic that arrives with the need that
    /// proves it.
    pub fn nearest(&self, text: &str, limit: usize) -> Result<Vec<Neighbor>, EngineError> {
        let query = self
            .embedder
            .embed(text)
            .map_err(|error| EngineError::Embed {
                path: "the query".to_string(),
                error,
            })?;
        let mut scored: Vec<Neighbor> = self
            .projection()?
            .into_iter()
            .map(|row| Neighbor {
                score: dot(query.values(), &row.values),
                path: row.path,
            })
            .collect();
        scored.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.path.cmp(&b.path))
        });
        scored.truncate(limit);
        Ok(scored)
    }

    /// Remove rows whose paths the store no longer holds, reset both cursors,
    /// and record `store_epoch` — the one-transaction answer to an epoch that
    /// moved. Counts the paths removed, the unit retraction counts in.
    fn reconcile(
        &mut self,
        feed: &mut FeedRead<'_>,
        store_epoch: &str,
    ) -> Result<u64, EngineError> {
        let held: Vec<String> = {
            let connection = self.database.connection();
            let mut statement = connection
                .prepare_cached("SELECT DISTINCT path FROM document_vectors ORDER BY path")
                .map_err(|error| error::sql("preparing the reconcile read", error))?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| error::sql("reading the held paths", error))?;
            rows.collect::<Result<_, _>>()
                .map_err(|error| error::sql("reading a held path", error))?
        };

        let mut dead: Vec<String> = Vec::new();
        for path in held {
            let parsed =
                DocumentPath::new(&path).map_err(|problem| EngineError::SidecarDamaged {
                    what: format!("`{path}` is held and is not a document path: {problem}"),
                })?;
            if feed.stored_document(&parsed)?.is_none() {
                dead.push(path);
            }
        }

        let transaction = self
            .database
            .immediate_transaction("reconciling the sidecar")?;
        for path in &dead {
            transaction
                .execute(
                    "DELETE FROM document_vectors WHERE path = ?1",
                    params![path],
                )
                .map_err(|error| error::sql("reconciling a dead path away", error))?;
        }
        norn_db::meta::put_meta(&transaction, ddl::meta::DOCUMENT_CURSOR, "")?;
        norn_db::meta::put_meta(&transaction, ddl::meta::TOMBSTONE_CURSOR, "")?;
        norn_db::meta::put_meta(&transaction, ddl::meta::OBSERVED_STORE_EPOCH, store_epoch)?;
        transaction
            .commit()
            .map_err(|error| error::sql("committing the reconcile", error))?;
        Ok(dead.len() as u64)
    }

    /// One page's writes and its advanced cursor, in one transaction.
    fn commit_documents(
        &mut self,
        writes: &[(String, String, Vec<u8>, usize)],
        advanced: &FeedCursor,
    ) -> Result<(), EngineError> {
        let model = self.embedder.model().clone();
        let transaction = self
            .database
            .immediate_transaction("committing a drained page")?;
        for (path, input_hash, blob, dimensions) in writes {
            transaction
                .execute(
                    "INSERT INTO document_vectors (
                         path, model_id, model_version, input_hash, dimensions, embedding
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(path, model_id, model_version) DO UPDATE SET
                         input_hash = excluded.input_hash,
                         dimensions = excluded.dimensions,
                         embedding  = excluded.embedding",
                    params![
                        path,
                        model.id(),
                        model.version(),
                        input_hash,
                        *dimensions as i64,
                        blob,
                    ],
                )
                .map_err(|error| error::sql("writing an embedding", error))?;
        }
        norn_db::meta::put_meta(
            &transaction,
            ddl::meta::DOCUMENT_CURSOR,
            encode_cursor(advanced),
        )?;
        transaction
            .commit()
            .map_err(|error| error::sql("committing a drained page", error))
    }

    /// One page's retractions and its advanced cursor, in one transaction. A
    /// death retracts every model's rows at the path: the text they describe
    /// is gone for all of them.
    fn commit_retractions(
        &mut self,
        paths: &[String],
        advanced: &FeedCursor,
    ) -> Result<u64, EngineError> {
        let transaction = self
            .database
            .immediate_transaction("committing retractions")?;
        let mut retracted = 0_u64;
        for path in paths {
            let removed = transaction
                .execute(
                    "DELETE FROM document_vectors WHERE path = ?1",
                    params![path],
                )
                .map_err(|error| error::sql("retracting a death", error))?;
            if removed > 0 {
                retracted += 1;
            }
        }
        norn_db::meta::put_meta(
            &transaction,
            ddl::meta::TOMBSTONE_CURSOR,
            encode_cursor(advanced),
        )?;
        transaction
            .commit()
            .map_err(|error| error::sql("committing retractions", error))?;
        Ok(retracted)
    }

    /// The recorded `input_hash` for `path` under this engine's model.
    fn row_input_hash(&self, path: &str) -> Result<Option<String>, EngineError> {
        let model = self.embedder.model();
        self.database
            .connection()
            .query_row(
                "SELECT input_hash FROM document_vectors
                 WHERE path = ?1 AND model_id = ?2 AND model_version = ?3",
                params![path, model.id(), model.version()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error::sql("reading a recorded input hash", error))
    }

    fn get_meta<T: norn_db::rusqlite::types::FromSql>(
        &self,
        key: &str,
    ) -> Result<Option<T>, EngineError> {
        Ok(norn_db::meta::get_meta(self.database.connection(), key)?)
    }

    /// The recorded feed position under `key`, where the empty string and an
    /// absent key both mean the start of the feed.
    fn read_cursor(&self, key: &str) -> Result<Option<FeedCursor>, EngineError> {
        let Some(recorded) = self.get_meta::<String>(key)? else {
            return Ok(None);
        };
        if recorded.is_empty() {
            return Ok(None);
        }
        let Some((generation, path)) = recorded.split_once(':') else {
            return Err(EngineError::SidecarDamaged {
                what: format!("the recorded `{key}` position `{recorded}` has no separator"),
            });
        };
        let generation: i64 = generation
            .parse()
            .map_err(|_| EngineError::SidecarDamaged {
                what: format!("the recorded `{key}` position `{recorded}` has no generation"),
            })?;
        let path = DocumentPath::new(path).map_err(|problem| EngineError::SidecarDamaged {
            what: format!("the recorded `{key}` position names no document path: {problem}"),
        })?;
        Ok(Some(FeedCursor::at(generation, path)))
    }
}

/// `{generation}:{path}` — one scalar, so a position moves atomically. The
/// generation half cannot hold `:`, so the first `:` is always the seam.
fn encode_cursor(cursor: &FeedCursor) -> String {
    format!("{}:{}", cursor.generation(), cursor.path().as_str())
}

/// The values as little-endian `f32` bytes, in order.
fn encode(values: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// The blob back into values, held to the width the row declares beside it.
fn decode(blob: &[u8], dimensions: i64) -> Result<Vec<f32>, EngineError> {
    let (chunks, remainder) = blob.as_chunks::<4>();
    if !remainder.is_empty() || chunks.len() as i64 != dimensions {
        return Err(EngineError::SidecarDamaged {
            what: format!(
                "an embedding blob holds {} bytes and its row declares {dimensions} values",
                blob.len()
            ),
        });
    }
    Ok(chunks
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect())
}

/// The dot product, accumulated in declaration order. Widths are equal
/// because the drain's write path holds every embedding to the embedder's
/// promised width; the zip stops at the shorter side rather than judging it
/// a second time.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
