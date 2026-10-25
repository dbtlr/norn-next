//! The engine: drain the feed, keep the sidecar converged, answer nearest.

use std::collections::BinaryHeap;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

use norn_db::Database;
use norn_db::rusqlite::{OptionalExtension, params};
use norn_embed::{Embedder, Model};
use norn_store::{DocumentPath, FeedCursor, FeedRead};

use crate::ddl;
use crate::error::{self, EngineError};
use crate::progress::{self, Feed, Recorded, SidecarRevision, Watermark, Watermarks};
use crate::sidecar;
use norn_db::{OpenOutcome as SidecarOutcome, RebuildReason};

/// How many feed rows one page asks for. Bounded well under the store's page
/// cap; the drain loops pages, so the bound shapes memory rather than reach.
const DRAIN_PAGE: usize = 256;

/// How many held paths one reconcile page checks against the store. The
/// reconcile pages the way the feed does, so a sidecar the size of the vault
/// is checked in bounded memory and bounded transactions.
const RECONCILE_PAGE: usize = 256;

/// How much one slice of a drain may do, counted in pages: a feed page that
/// presented rows, or a reconcile page that checked held paths.
///
/// A page that comes back empty costs one query and is not counted, so a
/// slice over a store with nothing new settles within any budget. The unit is
/// work, not time: one page can hold up to a page of embeddings, and a single
/// embedding or sidecar operation is not interrupted part way.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SliceBudget {
    pages: usize,
}

impl SliceBudget {
    /// No bound: the slice drains until both feeds are complete.
    pub const UNBOUNDED: SliceBudget = SliceBudget { pages: usize::MAX };

    /// At most `pages` counted pages.
    pub const fn pages(pages: NonZeroUsize) -> Self {
        SliceBudget { pages: pages.get() }
    }
}

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
    /// The sidecar's revision and watermarks as its last commit left them:
    /// loaded at open and moved only after a transaction that wrote them
    /// commits, so a reading never names progress the sidecar does not hold.
    recorded: Recorded,
}

/// What one drain did, in rows.
///
/// A reading for the caller and the suites, not derived state: nothing is
/// recorded, and two drains over the same quiescent store report all-zero
/// work the same way.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DrainReport {
    /// The recorded store epoch did not match at entry, so the cursors were
    /// reset and the feed rescanned from the start.
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
    /// The slice's budget ran out before the reconcile and both feeds were
    /// complete, so the next slice has work to resume from the positions this
    /// one committed.
    pub owed: bool,
}

impl DrainReport {
    /// Whether the drain found nothing to do.
    ///
    /// A reading of this drain's work, not a proof of convergence: a
    /// converged sidecar can still report work (a rescan re-triages every
    /// row it kept), and quiescence holds only until the next write. The
    /// drain reads one store lifetime, because the handle's epoch is pinned
    /// at its open and the feed borrow is exclusive; a store file replaced
    /// underneath a live handle is the maintainer singleton's to prevent
    /// (ADR 0016, one maintainer per derived store), not this reading's to
    /// detect.
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

/// One nearest answer: a path and its score, higher meaning nearer. A score
/// an answer carries is finite: a scan that scores a row otherwise refuses.
#[derive(Clone, Debug, PartialEq)]
pub struct Neighbor {
    pub path: String,
    pub score: f32,
}

/// The nearest paths one answer found, nearest first, and what it read.
#[derive(Clone, Debug, PartialEq)]
pub struct Nearest {
    pub neighbors: Vec<Neighbor>,
    pub work: NearestWork,
}

/// What one nearest answer read, in rows.
///
/// A reading for the caller and the suites: what a scan costs grows with the
/// rows it reads, and what it holds is bounded by its limit.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NearestWork {
    /// Rows of the engine's model the scan read.
    pub rows_read: u64,
    /// Rows among those the answer admitted, decoded and scored.
    pub rows_scored: u64,
    /// The most scored rows the answer held at once: never more than its
    /// limit.
    pub peak_held: u64,
}

/// A scored row in the bounded heap an answer keeps, ordered so the worst row
/// kept is the greatest: a lower score, or at an equal score a later path.
#[derive(Debug)]
struct Ranked(Neighbor);

impl Ord for Ranked {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .0
            .score
            .total_cmp(&self.0.score)
            .then_with(|| self.0.path.cmp(&other.0.path))
    }
}

impl PartialOrd for Ranked {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Ranked {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for Ranked {}

impl Engine {
    /// Open, or create, the sidecar at `path`, embedding through `embedder`.
    pub fn open(path: impl AsRef<Path>, embedder: Arc<dyn Embedder>) -> Result<Self, EngineError> {
        let path = path.as_ref();
        let (connection, outcome) = sidecar::open(path, embedder.model())?;
        let recorded = progress::read(&connection).map_err(progress::Unread::into_engine_error)?;
        Ok(Engine {
            database: Database::adopt(connection, path)?,
            embedder,
            outcome,
            recorded,
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

    /// Which sidecar state this engine answers from: its epoch and the count of
    /// committed mutations within it.
    ///
    /// Read through `&self` from what the last commit left, so an owner that
    /// serializes drains against answers samples it with the answer it
    /// describes. A drain that commits nothing leaves it where it was.
    pub fn revision(&self) -> SidecarRevision {
        SidecarRevision {
            epoch: self.database.epoch().to_string(),
            revision: self.recorded.revision,
        }
    }

    /// How far each feed was drained against the lane-1 store — see
    /// [`Watermark`]. Read the way [`Engine::revision`] is.
    pub fn watermarks(&self) -> &Watermarks {
        &self.recorded.watermarks
    }

    /// The model this engine embeds with. The sidecar records it — opening
    /// under a different model rebuilds from zero — and every projection and
    /// answer scopes to it, so a row is never read under a model that did
    /// not produce it.
    pub fn model(&self) -> &Model {
        self.embedder.model()
    }

    /// Discard the sidecar at `path`, and every file its journal leaves beside
    /// it, without opening it.
    ///
    /// For a projection nothing is going to open again: the vault its store
    /// was derived from is no longer served. The caller holds the maintainer
    /// lock over the directory, so no engine is open on the file. A sidecar
    /// that is not there is already discarded.
    pub fn discard_at(path: &Path) -> Result<(), EngineError> {
        Ok(norn_db::remove_database(path)?)
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
        let recorded = progress::read(&connection).map_err(progress::Unread::into_engine_error)?;
        Ok(Engine {
            database: Database::adopt(connection, &path)?,
            embedder,
            outcome: SidecarOutcome::RebuiltFromZero(RebuildReason::Client {
                detail: "discarded by its owner".to_string(),
            }),
            recorded,
        })
    }

    /// Consume the store's change feed until both feeds are drained, and
    /// converge the sidecar on what they said: [`Engine::drain_bounded`]
    /// with no bound.
    pub fn drain(&mut self, feed: &mut FeedRead<'_>) -> Result<DrainReport, EngineError> {
        self.drain_bounded(feed, SliceBudget::UNBOUNDED)
    }

    /// Consume the store's change feed for at most `budget`, and converge the
    /// sidecar on what the consumed pages said. A report whose
    /// [`DrainReport::owed`] is set stopped at its budget; the next slice
    /// resumes from the positions this one committed, and slices repeated
    /// until one owes nothing settle where an unbounded drain would.
    ///
    /// # The protocol
    ///
    /// 1. **Epoch first.** A store epoch that differs from the observed one
    ///    means the cursors name positions in a database that no longer
    ///    exists: reconcile away rows whose paths the store no longer holds
    ///    (its tombstones did not survive into the new lifetime), a page of
    ///    held paths at a time, each page committing its removals and its
    ///    position together. Only the page that completes the pass resets
    ///    both cursors and records the new epoch as observed, so a slice or
    ///    a crash that stops part way leaves the old epoch observed and the
    ///    next slice resumes the pass. A pass recorded for another lifetime
    ///    than the store's starts over.
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
    /// replayed page is an idempotent set of upserts and deletes.
    ///
    /// # Watermarks and the revision
    ///
    /// Before every page the drain reads the store's write generation, and the
    /// page that completes a feed — the one that comes back short — records
    /// the generation read before it as that feed's [`Watermark`], in the same
    /// transaction as the page's own writes. A completing page that is empty
    /// has no writes to ride with, so it commits the watermark alone, and only
    /// where the watermark moved: a drain over a quiescent store commits
    /// nothing. Every committing transaction takes the next sidecar revision,
    /// so the revision moves exactly when the sidecar does. The
    /// guarantee that a hash is never written beside values it does not
    /// describe is the drain's own — the generation check carries it even
    /// against a writer interleaving through a store handle this borrow does
    /// not cover.
    pub fn drain_bounded(
        &mut self,
        feed: &mut FeedRead<'_>,
        budget: SliceBudget,
    ) -> Result<DrainReport, EngineError> {
        let mut report = DrainReport::default();
        let mut left = budget.pages;
        let store_epoch = feed.epoch().to_string();
        let observed: Option<String> = self.get_meta(ddl::meta::OBSERVED_STORE_EPOCH)?;
        let complete = (observed.as_deref() == Some(store_epoch.as_str()) || {
            report.rescan = true;
            self.reconcile(feed, &store_epoch, &mut left, &mut report)?
        }) && self.drain_documents(feed, &store_epoch, &mut left, &mut report)?
            && self.drain_tombstones(feed, &store_epoch, &mut left, &mut report)?;
        report.owed = !complete;
        Ok(report)
    }

    /// Drain the document feed within `left` counted pages, answering
    /// whether the feed completed.
    fn drain_documents(
        &mut self,
        feed: &mut FeedRead<'_>,
        store_epoch: &str,
        left: &mut usize,
        report: &mut DrainReport,
    ) -> Result<bool, EngineError> {
        let mut cursor = self.read_cursor(ddl::meta::DOCUMENT_CURSOR)?;
        loop {
            if *left == 0 {
                return Ok(false);
            }
            let observed = feed.write_generation()?;
            let page = feed.changed_documents_after(cursor.as_ref(), DRAIN_PAGE)?;
            let full = page.len() == DRAIN_PAGE;
            let completed = (!full).then(|| Watermark {
                store_epoch: store_epoch.to_string(),
                generation: observed,
            });
            let Some((last, _)) = page.last() else {
                self.advance_watermark(Feed::Documents, completed)?;
                return Ok(true);
            };
            let advanced = last.clone();

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

            self.commit_documents(&writes, &advanced, completed)?;
            *left -= 1;
            cursor = Some(advanced);
            if !full {
                return Ok(true);
            }
        }
    }

    /// Drain the tombstone feed within `left` counted pages, answering
    /// whether the feed completed.
    fn drain_tombstones(
        &mut self,
        feed: &mut FeedRead<'_>,
        store_epoch: &str,
        left: &mut usize,
        report: &mut DrainReport,
    ) -> Result<bool, EngineError> {
        let mut cursor = self.read_cursor(ddl::meta::TOMBSTONE_CURSOR)?;
        loop {
            if *left == 0 {
                return Ok(false);
            }
            let observed = feed.write_generation()?;
            let page = feed.changed_tombstones_after(cursor.as_ref(), DRAIN_PAGE)?;
            let full = page.len() == DRAIN_PAGE;
            let completed = (!full).then(|| Watermark {
                store_epoch: store_epoch.to_string(),
                generation: observed,
            });
            let Some((last, _)) = page.last() else {
                self.advance_watermark(Feed::Tombstones, completed)?;
                return Ok(true);
            };
            let advanced = last.clone();

            let paths: Vec<String> = page
                .iter()
                .map(|(_, tombstone)| tombstone.path.as_str().to_string())
                .collect();
            report.retracted += self.commit_retractions(&paths, &advanced, completed)?;
            *left -= 1;
            cursor = Some(advanced);
            if !full {
                return Ok(true);
            }
        }
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
    /// [`Engine::nearest_among`] over every row the engine holds.
    pub fn nearest(&self, text: &str, limit: usize) -> Result<Vec<Neighbor>, EngineError> {
        Ok(self.nearest_among(text, limit, |_| true)?.neighbors)
    }

    /// The `limit` nearest paths to `text` among the rows whose path `admits`,
    /// under this engine's model, and what the answer read.
    ///
    /// The score is the dot product against the query's embedding, and the
    /// order is total: score descending, then path ascending, so equal scores
    /// answer the same way on every run.
    ///
    /// **Every score the scan computes is judged finite.** A row scored NaN
    /// or infinite refuses the answer ([`EngineError::NonFiniteScore`]) the
    /// moment it is scored, whether or not it would be kept: a negative NaN or
    /// infinity ranks below every finite score, so a scan scoring more rows
    /// than its limit would otherwise push it out unseen. What the answer
    /// holds is therefore finite, and so is every score it answers.
    ///
    /// **The scan streams, and holds at most `limit` scored rows.** Each row
    /// of the model is read, tested against `admits`, decoded and scored one
    /// at a time, and kept only while it is among the best `limit` scored so
    /// far — a heap whose top is the worst row kept — so what an answer holds
    /// is bounded by its limit at any vault size, and a row `admits` refuses
    /// is never decoded or scored. The scan reads every row of the model: the
    /// index that makes it sublinear is a storage mechanic that arrives with
    /// the need that proves it.
    pub fn nearest_among(
        &self,
        text: &str,
        limit: usize,
        admits: impl Fn(&str) -> bool,
    ) -> Result<Nearest, EngineError> {
        let query = self
            .embedder
            .embed(text)
            .map_err(|error| EngineError::Embed {
                path: "the query".to_string(),
                error,
            })?;
        let promised = self.embedder.dimensions().get();
        if query.dimensions() != promised {
            return Err(EngineError::WrongWidth {
                path: "the query".to_string(),
                promised,
                produced: query.dimensions(),
            });
        }
        let model = self.embedder.model().clone();
        let connection = self.database.connection();
        let mut statement = connection
            .prepare_cached(
                "SELECT path, dimensions, embedding FROM document_vectors
                 WHERE model_id = ?1 AND model_version = ?2",
            )
            .map_err(|error| error::sql("preparing the nearest scan", error))?;
        let rows = statement
            .query_map(params![model.id(), model.version()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            })
            .map_err(|error| error::sql("scanning the model's rows", error))?;
        let mut work = NearestWork::default();
        let mut kept: BinaryHeap<Ranked> = BinaryHeap::with_capacity(limit.min(DRAIN_PAGE));
        for row in rows {
            let (path, dimensions, blob) =
                row.map_err(|error| error::sql("reading a scanned row", error))?;
            work.rows_read += 1;
            if limit == 0 || !admits(&path) {
                continue;
            }
            let score = dot(query.values(), &decode(&blob, dimensions)?);
            work.rows_scored += 1;
            if !score.is_finite() {
                return Err(EngineError::NonFiniteScore { path, score });
            }
            let ranked = Ranked(Neighbor { path, score });
            if kept.len() < limit {
                kept.push(ranked);
            } else if kept.peek().is_some_and(|worst| ranked < *worst) {
                kept.pop();
                kept.push(ranked);
            }
            work.peak_held = work.peak_held.max(kept.len() as u64);
        }
        Ok(Nearest {
            neighbors: kept
                .into_sorted_vec()
                .into_iter()
                .map(|ranked| ranked.0)
                .collect(),
            work,
        })
    }

    /// Reconcile the sidecar toward `store_epoch` within `left` counted pages,
    /// answering whether the pass completed.
    ///
    /// Each page checks the next held paths in path order against the store
    /// and commits the removal of those it no longer holds together with the
    /// pass's position. The page that comes back short completes the pass: in
    /// the same transaction it resets both cursors, records `store_epoch` as
    /// observed and clears the position, so the observed epoch never moves
    /// ahead of a whole pass. A recorded position is resumed only for the
    /// lifetime it was recorded in.
    fn reconcile(
        &mut self,
        feed: &mut FeedRead<'_>,
        store_epoch: &str,
        left: &mut usize,
        report: &mut DrainReport,
    ) -> Result<bool, EngineError> {
        let target: Option<String> = self.get_meta(ddl::meta::RECONCILE_STORE_EPOCH)?;
        let mut after: Option<String> = if target.as_deref() == Some(store_epoch) {
            self.get_meta::<String>(ddl::meta::RECONCILE_CURSOR)?
                .filter(|cursor| !cursor.is_empty())
        } else {
            None
        };
        loop {
            if *left == 0 {
                return Ok(false);
            }
            let held = self.held_paths_after(after.as_deref())?;
            let full = held.len() == RECONCILE_PAGE;
            let mut dead: Vec<&str> = Vec::new();
            for path in &held {
                let parsed =
                    DocumentPath::new(path).map_err(|problem| EngineError::SidecarDamaged {
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
            match held.last().filter(|_| full) {
                Some(last) => {
                    norn_db::meta::put_meta(
                        &transaction,
                        ddl::meta::RECONCILE_STORE_EPOCH,
                        store_epoch,
                    )?;
                    norn_db::meta::put_meta(&transaction, ddl::meta::RECONCILE_CURSOR, last)?;
                }
                None => {
                    norn_db::meta::put_meta(&transaction, ddl::meta::DOCUMENT_CURSOR, "")?;
                    norn_db::meta::put_meta(&transaction, ddl::meta::TOMBSTONE_CURSOR, "")?;
                    norn_db::meta::put_meta(
                        &transaction,
                        ddl::meta::OBSERVED_STORE_EPOCH,
                        store_epoch,
                    )?;
                    norn_db::meta::put_meta(&transaction, ddl::meta::RECONCILE_STORE_EPOCH, "")?;
                    norn_db::meta::put_meta(&transaction, ddl::meta::RECONCILE_CURSOR, "")?;
                }
            }
            let revision = norn_db::meta::next_generation(&transaction)?;
            transaction
                .commit()
                .map_err(|error| error::sql("committing the reconcile", error))?;
            self.recorded.revision = revision;
            report.reconciled_away += dead.len() as u64;
            if !held.is_empty() {
                *left -= 1;
            }
            if !full {
                return Ok(true);
            }
            after = held.last().cloned();
        }
    }

    /// The next page of held paths after `after`, in path order, each once
    /// whichever models hold rows at it. Separate first/after statements let
    /// SQLite seek past the cursor without scanning the consumed prefix.
    fn held_paths_after(&self, after: Option<&str>) -> Result<Vec<String>, EngineError> {
        let connection = self.database.connection();
        let mut statement = connection
            .prepare_cached(match after {
                None => "SELECT DISTINCT path FROM document_vectors ORDER BY path LIMIT ?1",
                Some(_) => {
                    "SELECT DISTINCT path FROM document_vectors
                            WHERE path > ?1 ORDER BY path LIMIT ?2"
                }
            })
            .map_err(|error| error::sql("preparing the reconcile read", error))?;
        let path = |row: &norn_db::rusqlite::Row<'_>| row.get::<_, String>(0);
        let rows = match after {
            None => statement.query_map(params![RECONCILE_PAGE as i64], path),
            Some(after) => statement.query_map(params![after, RECONCILE_PAGE as i64], path),
        }
        .map_err(|error| error::sql("reading the held paths", error))?;
        rows.collect::<Result<_, _>>()
            .map_err(|error| error::sql("reading a held path", error))
    }

    /// One page's writes and its advanced cursor, in one transaction, with the
    /// feed's watermark where this page completed the feed.
    fn commit_documents(
        &mut self,
        writes: &[(String, String, Vec<u8>, usize)],
        advanced: &FeedCursor,
        completed: Option<Watermark>,
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
        let committed = commit_progress(
            transaction,
            "committing a drained page",
            Feed::Documents,
            completed.as_ref(),
        )?;
        self.recorded.revision = committed;
        if let Some(watermark) = completed {
            self.recorded.watermarks.set(Feed::Documents, watermark);
        }
        Ok(())
    }

    /// One page's retractions and its advanced cursor, in one transaction, with
    /// the feed's watermark where this page completed the feed. A death
    /// retracts every model's rows at the path: the text they describe is gone
    /// for all of them.
    fn commit_retractions(
        &mut self,
        paths: &[String],
        advanced: &FeedCursor,
        completed: Option<Watermark>,
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
        let committed = commit_progress(
            transaction,
            "committing retractions",
            Feed::Tombstones,
            completed.as_ref(),
        )?;
        self.recorded.revision = committed;
        if let Some(watermark) = completed {
            self.recorded.watermarks.set(Feed::Tombstones, watermark);
        }
        Ok(retracted)
    }

    /// Record the watermark an empty completing page observed, where it moved.
    ///
    /// `completed` is `None` only where the page was full, and an empty page
    /// never is; the arm is answered rather than asserted. A watermark equal to
    /// the recorded one commits nothing, so a drain over a quiescent store
    /// leaves the revision where it was.
    fn advance_watermark(
        &mut self,
        feed: Feed,
        completed: Option<Watermark>,
    ) -> Result<(), EngineError> {
        let Some(watermark) = completed else {
            return Ok(());
        };
        if self.recorded.watermarks.of(feed) == Some(&watermark) {
            return Ok(());
        }
        let transaction = self
            .database
            .immediate_transaction("advancing a watermark")?;
        let committed =
            commit_progress(transaction, "advancing a watermark", feed, Some(&watermark))?;
        self.recorded.revision = committed;
        self.recorded.watermarks.set(feed, watermark);
        Ok(())
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

/// Finish a committing transaction: record `watermark` under `feed` where the
/// transaction completed it, take the next sidecar revision, and commit as
/// `operation`. Answers the revision the commit recorded.
fn commit_progress(
    transaction: norn_db::rusqlite::Transaction<'_>,
    operation: &'static str,
    feed: Feed,
    watermark: Option<&Watermark>,
) -> Result<i64, EngineError> {
    if let Some(watermark) = watermark {
        norn_db::meta::put_meta(
            &transaction,
            feed.watermark_key(),
            progress::encode_watermark(watermark),
        )?;
    }
    let revision = norn_db::meta::next_generation(&transaction)?;
    transaction
        .commit()
        .map_err(|error| error::sql(operation, error))?;
    Ok(revision)
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
/// because both operands are held to the embedder's promised width — every
/// row at the drain's write, the query at its embed; the zip stops at the
/// shorter side rather than judging it a second time.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
#[cfg(test)]
mod reconcile_query_tests {
    use std::cell::RefCell;
    use std::sync::Arc;

    use norn_db::rusqlite::{
        StatementStatus,
        trace::{TraceEvent, TraceEventCodes},
    };
    use norn_embed::StubEmbedder;
    use norn_testkit::scratch::Scratch;

    use super::{Engine, RECONCILE_PAGE};

    thread_local! {
        static QUERIES: RefCell<Vec<(String, i32, i32)>> = const { RefCell::new(Vec::new()) };
    }

    /// Capture work from the statement the engine actually executed. The
    /// profile event's elapsed time is deliberately unused.
    fn capture_query(event: TraceEvent<'_>) {
        if let TraceEvent::Profile(statement, _) = event
            && statement
                .sql()
                .starts_with("SELECT DISTINCT path FROM document_vectors")
        {
            QUERIES.with_borrow_mut(|queries| {
                queries.push((
                    statement.expanded_sql().unwrap(),
                    statement.get_status(StatementStatus::VmStep),
                    statement.get_status(StatementStatus::FullscanStep),
                ))
            });
        }
    }

    #[test]
    fn reconcile_queries_seek_late_and_empty_pages_without_scanning_unrelated_prefixes() {
        let mut samples = Vec::new();
        for models in [1, 2] {
            for count in [1024, 8192, 32768] {
                let scratch = Scratch::new("semantic-reconcile-query-work");
                let engine = Engine::open(
                    scratch.join("semantic.sqlite3"),
                    Arc::new(StubEmbedder::new()),
                )
                .unwrap();
                let connection = engine.database.connection();
                let transaction = connection.unchecked_transaction().unwrap();
                {
                    let mut insert = transaction
                        .prepare(
                            "INSERT INTO document_vectors VALUES (?1,?2,'1','hash',1,x'00000000')",
                        )
                        .unwrap();
                    for n in 0..count {
                        for model in 0..models {
                            insert
                                .execute(norn_db::rusqlite::params![
                                    format!("docs/{n:06}.md"),
                                    format!("model-{model}")
                                ])
                                .unwrap();
                        }
                    }
                }
                transaction.commit().unwrap();
                connection.trace_v2(TraceEventCodes::SQLITE_TRACE_PROFILE, Some(capture_query));
                let late = format!("docs/{:06}.md", count - RECONCILE_PAGE - 1);
                let end = format!("docs/{:06}.md", count - 1);
                for (boundary, after, expected) in [
                    ("first", None, (0..RECONCILE_PAGE).collect::<Vec<_>>()),
                    (
                        "late",
                        Some(late.as_str()),
                        (count - RECONCILE_PAGE..count).collect(),
                    ),
                    ("empty", Some(end.as_str()), Vec::new()),
                ] {
                    // Each counter must cover one execution, including when
                    // successive cursors reuse the same cached SQL shape.
                    connection.flush_prepared_statement_cache();
                    QUERIES.with_borrow_mut(Vec::clear);
                    let paths = engine.held_paths_after(after).unwrap();
                    assert_eq!(
                        paths,
                        expected
                            .iter()
                            .map(|n| format!("docs/{n:06}.md"))
                            .collect::<Vec<_>>()
                    );
                    assert!(paths.len() <= RECONCILE_PAGE);
                    let queries = QUERIES.with_borrow_mut(std::mem::take);
                    assert_eq!(queries.len(), 1);
                    let (sql, steps, scans) = queries.into_iter().next().unwrap();
                    let plan = connection
                        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                        .unwrap()
                        .query_map([], |row| row.get::<_, String>(3))
                        .unwrap()
                        .collect::<Result<Vec<_>, _>>()
                        .unwrap();
                    eprintln!(
                        "models={models}, paths={count}, boundary={boundary}, rows={}, VM={steps}, fullscan={scans}, SQL={sql:?}, plan={plan:?}",
                        paths.len()
                    );
                    samples.push((models, count, boundary, steps, scans, plan));
                }
            }
        }
        for (models, count, boundary, steps, scans, plan) in &samples {
            let first = samples
                .iter()
                .find(|s| s.0 == *models && s.1 == *count && s.2 == "first")
                .unwrap()
                .3;
            let small = samples
                .iter()
                .find(|s| s.0 == *models && s.1 == 1024 && s.2 == *boundary)
                .unwrap()
                .3;
            assert!(
                *steps <= 2 * first && *steps <= 2 * small,
                "query work grew with an unrelated prefix: models={models}, count={count}, boundary={boundary}, VM={steps}, first={first}, small={small}"
            );
            if *boundary != "first" {
                assert_eq!(
                    *scans, 0,
                    "a keyset page must seek, including an empty page"
                );
                assert!(
                    plan.iter()
                        .any(|detail| detail.contains("SEARCH") && detail.contains("path>?")),
                    "{plan:?}"
                );
            }
            assert!(
                !plan.iter().any(|detail| detail.contains("TEMP B-TREE")),
                "{plan:?}"
            );
        }
    }
}
