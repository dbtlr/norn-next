//! The engine: drain the feed, keep the sidecar converged, answer nearest.

use std::collections::BinaryHeap;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

use norn_db::Database;
use norn_db::rusqlite::{OptionalExtension, params};
use norn_embed::{Embedder, Model};
use norn_store::{DocumentPath, FeedCursor, FeedRead, StoreReading};

use crate::census::{self, Coverage};
use crate::ddl;
use crate::error::{self, EngineError};
use crate::progress::{self, Feed, ReadThroughs, Recorded, SidecarRevision, Watermark, Watermarks};
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
/// deaths and unreadable content, and answers vector-nearest over what it
/// holds. It never touches
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
    /// The document feed came back complete, so [`Engine::step`] hands the
    /// turn to the tombstone feed until a tombstone page comes back short.
    /// Without it, a write landing before every step would keep each step on
    /// the document feed and starve the deaths. Held in memory only: an
    /// engine opened afresh starts on the document feed, which a step passes
    /// through to the tombstones when it has nothing to present.
    tombstones_turn: bool,
}

/// What one drain did, in rows.
///
/// A reading for the caller and the suites, not derived state: nothing is
/// recorded, and two drains over the same quiescent store report all-zero
/// work the same way.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DrainReport {
    /// The recorded store epoch did not match at entry, or the engine had not
    /// read through the store's death floor, so the drain reconciled the
    /// sidecar against current state and reset the cursors to rescan the
    /// feed from the start.
    pub rescan: bool,
    /// Reconciles the drain began: each one a pass that read its first page,
    /// whether it reconciled toward a new store epoch or below the death
    /// floor. A pass a drain resumes is not counted again.
    pub reconciles: u64,
    /// Census recounts the drain began below the death floor, counted as
    /// [`DrainReport::reconciles`] are.
    pub recounts: u64,
    /// Paths the rescan's reconcile removed: they no longer stand in the
    /// store, and the store's own tombstones did not survive into its new
    /// epoch to say so.
    pub reconciled_away: u64,
    /// Documents whose body was embedded and written.
    pub embedded: u64,
    /// Feed rows whose recorded embedding is already over the current body —
    /// triaged by fingerprint alone, without fetching the body.
    pub skipped_current: u64,
    /// Feed rows superseded mid-drain: the stored document had already moved
    /// past the row's generation (or died), so a later feed row covers it.
    pub deferred: u64,
    /// Paths whose held rows were retracted because the feed recorded a death
    /// or a document with unreadable content.
    pub retracted: u64,
    /// Feed rows the census read to keep the coverage counts: the counting's
    /// own work, which follows the rows that moved in the store.
    pub censused: u64,
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
    /// drain reads one store epoch, because the handle's epoch is pinned
    /// at its open — a writer borrow, or a feed source that refuses a page
    /// over another epoch; a store file replaced underneath a live handle
    /// is the maintainer singleton's to prevent (ADR 0016, one maintainer
    /// per derived store), not this reading's to detect.
    pub fn is_settled(&self) -> bool {
        *self == DrainReport::default()
    }
}

impl DrainReport {
    /// Add `step`'s row counts to this report, and take its rescan where it
    /// rescanned. Whether work is still owed is the caller's to say.
    fn absorb(&mut self, step: &DrainReport) {
        self.rescan |= step.rescan;
        self.reconciles += step.reconciles;
        self.recounts += step.recounts;
        self.reconciled_away += step.reconciled_away;
        self.embedded += step.embedded;
        self.skipped_current += step.skipped_current;
        self.deferred += step.deferred;
        self.retracted += step.retracted;
        self.censused += step.censused;
    }
}

/// One step of a drain, as [`Engine::step`] read it.
#[derive(Debug)]
pub enum DrainStep {
    /// The step committed what its page held, or found nothing to do, and
    /// embedded nothing. [`DrainReport::owed`] says whether more steps are
    /// owed.
    Done(DrainReport),
    /// A document page whose bodies must be embedded before it can commit:
    /// [`PendingPage::embed`], then [`Engine::commit`].
    Embed(Box<PendingPage>),
}

/// A document page read and triaged, whose bodies are still to embed.
///
/// It holds everything the embedding needs and nothing of the engine's
/// sidecar or of the store, so an owner embeds it holding no lock, and an
/// owner that finds the page's delivery retired when it comes back drops it,
/// committing nothing.
pub struct PendingPage {
    embedder: Arc<dyn Embedder>,
    rows: Vec<PendingRow>,
    retractions: Vec<String>,
    advanced: FeedCursor,
    completed: Option<Watermark>,
    /// The sidecar state the page was triaged against.
    revision: SidecarRevision,
    report: DrainReport,
}

impl std::fmt::Debug for PendingPage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingPage")
            .field("rows", &self.rows.len())
            .field("retractions", &self.retractions.len())
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

/// One row a page will embed: its path, the body hash the embedding is
/// recorded under, and the body itself, fetched in the page's snapshot.
struct PendingRow {
    path: String,
    input_hash: String,
    body: String,
}

impl PendingPage {
    /// How many bodies the page embeds.
    pub fn rows(&self) -> usize {
        self.rows.len()
    }

    /// Embed every body the page holds, in order. One embedding is never
    /// interrupted, and neither is the page: it is the unit an owner's turn
    /// is bounded by.
    pub fn embed(self) -> Result<EmbeddedPage, EngineError> {
        let mut report = self.report;
        let mut writes = Vec::with_capacity(self.rows.len());
        let promised = self.embedder.dimensions().get();
        for row in self.rows {
            let embedding = self
                .embedder
                .embed(&row.body)
                .map_err(|error| EngineError::Embed {
                    path: row.path.clone(),
                    error,
                })?;
            if embedding.dimensions() != promised {
                return Err(EngineError::WrongWidth {
                    path: row.path,
                    promised,
                    produced: embedding.dimensions(),
                });
            }
            let dimensions = embedding.dimensions();
            writes.push((
                row.path,
                row.input_hash,
                encode(embedding.values()),
                dimensions,
            ));
            report.embedded += 1;
        }
        Ok(EmbeddedPage {
            writes,
            retractions: self.retractions,
            advanced: self.advanced,
            completed: self.completed,
            revision: self.revision,
            report,
        })
    }
}

/// A page whose bodies are embedded, ready for [`Engine::commit`].
pub struct EmbeddedPage {
    writes: Vec<(String, String, Vec<u8>, usize)>,
    retractions: Vec<String>,
    advanced: FeedCursor,
    completed: Option<Watermark>,
    revision: SidecarRevision,
    report: DrainReport,
}

impl std::fmt::Debug for EmbeddedPage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EmbeddedPage")
            .field("writes", &self.writes.len())
            .field("revision", &self.revision)
            .finish_non_exhaustive()
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
            tombstones_turn: false,
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

    /// How far each of the engine's two readers of the death feed, the drain
    /// and the census, has read it — see [`ReadThroughs`]. Read the way
    /// [`Engine::revision`] is, from what the last commit left.
    ///
    /// The host hands the death prune the lower of the two in the store's
    /// epoch ([`ReadThroughs::lower_in`]), and judges the engine's freshness
    /// rescanning where it has not read through a reading's floor
    /// ([`ReadThroughs::engine_below_floor`]).
    pub fn read_through(&self) -> &ReadThroughs {
        &self.recorded.read_through
    }

    /// How many of the documents the store holds readable have embeddings
    /// over their current content, counted at the snapshot in which the
    /// census last read both feeds to their ends; `None` until both counts
    /// have been taken whole. A census still catching up over a burst wider
    /// than its page answers what it last counted whole, and so does an
    /// engine reopened part way through that catch-up: the sidecar records
    /// the last whole count. A census catching up over a new store epoch or a
    /// fresh sidecar has no whole count to answer.
    pub fn coverage(&self) -> Option<Coverage> {
        self.recorded.counts.answer()
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
            tombstones_turn: false,
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
    /// It is [`Engine::step`] repeated, each page embedded and committed in
    /// turn: the spelling a caller holding the feed for the whole drain
    /// takes. An owner that must not hold its lock across an embedding runs
    /// the three phases itself.
    ///
    /// # The protocol
    ///
    /// 1. **Epoch and floor first.** A store epoch that differs from the
    ///    observed one means the cursors name positions in a database that
    ///    no longer exists, and a death floor above the engine's
    ///    read-through generation means deaths it never read may be gone:
    ///    either way, reconcile away rows whose paths the store no longer
    ///    holds, a page of held paths at a time, each page committing its
    ///    removals and its position together. The pass's first page records
    ///    its G0, the write generation it read; only the page that completes
    ///    the pass resets both cursors, records the epoch as observed and G0
    ///    as read through, so a slice or a crash that stops part way leaves
    ///    the earlier records and the next slice resumes the pass with its
    ///    G0. A pass recorded for another epoch than the store's starts over;
    ///    a pass is never started over because of the floor, and one that
    ///    completes with the floor above its G0 is followed by another.
    /// 2. **Documents, triaged before fetched.** A feed row whose `body_hash`
    ///    is absent has no content to embed: every held vector at its path is
    ///    retracted with this page's progress, without fetching a body. A hash
    ///    equal to the recorded `input_hash` is current and costs no body read.
    ///    A changed one is fetched; a fetch that comes back at a different
    ///    generation (or gone) was superseded mid-drain and is deferred — the
    ///    feed presents the successor at its own strictly-higher generation,
    ///    so nothing is lost by not acting on the stale reading.
    /// 3. **Tombstones.** The two feeds are disjoint over stored paths at
    ///    any snapshot, so the order between the loops carries no hazard; a
    ///    death or rebirth landing between them lands at a generation a
    ///    later page or the next drain presents.
    ///
    /// Before each step's page, the census behind [`Engine::coverage`] reads at most
    /// one page of each feed past its own positions in the same snapshot and
    /// commits what moved, so the coverage counts describe the store state the
    /// step read. A census still catching up owes another step.
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
        // One drain reads each feed through once: a document feed it has
        // completed is not read again before its tombstones, so a write that
        // lands after the completing page is the next drain's to present.
        let mut documents_complete = false;
        loop {
            if left == 0 {
                report.owed = true;
                return Ok(report);
            }
            let (step, counted, completed) = self.step_counted(feed, documents_complete)?;
            documents_complete |= completed;
            let ended = match step {
                DrainStep::Done(ended) => ended,
                DrainStep::Embed(pending) => self.commit(pending.embed()?)?,
            };
            report.absorb(&ended);
            if counted {
                left -= 1;
            }
            if !ended.owed {
                report.owed = false;
                return Ok(report);
            }
        }
    }

    /// Run one step of the drain over `feed`: at most one counted page — a
    /// reconcile page, a document page or a tombstone page — read in full
    /// from `feed`, so a caller that opens one snapshot for the step reads
    /// every row it acts on from one instant of the store.
    ///
    /// A step that embeds nothing commits here and answers
    /// [`DrainStep::Done`], whose report says whether more steps are owed. A
    /// document page with bodies to embed answers [`DrainStep::Embed`]: the
    /// rows and their bodies, embedded with no lock of the caller's held
    /// ([`PendingPage::embed`]) and committed by [`Engine::commit`]. Pages
    /// that come back empty are not counted, and a step passes through them
    /// to the next feed, so a step over a settled sidecar reads every feed
    /// once and owes nothing.
    ///
    /// **Deaths get their turn while documents keep changing.** A committed
    /// document page that completes the document feed hands the next steps
    /// to the tombstone feed, until a tombstone page comes back short; only
    /// then does a step read documents again. A step that skipped the
    /// document feed owes another step, so a step owes nothing only where it
    /// read both feeds complete in its own snapshot.
    pub fn step(&mut self, feed: &mut FeedRead<'_>) -> Result<DrainStep, EngineError> {
        let documents_skipped = self.tombstones_turn;
        let (step, _, _) = self.step_counted(feed, documents_skipped)?;
        Ok(match step {
            DrainStep::Done(mut report) if documents_skipped => {
                report.owed = true;
                DrainStep::Done(report)
            }
            step => step,
        })
    }

    /// [`Engine::step`], passing the document feed by where
    /// `documents_complete` says this drain already completed it, and
    /// answering beside the step whether it counted a page and whether it
    /// completed the document feed.
    fn step_counted(
        &mut self,
        feed: &mut FeedRead<'_>,
        documents_complete: bool,
    ) -> Result<(DrainStep, bool, bool), EngineError> {
        let mut report = DrainReport::default();
        // The epoch, the write generation and the death floor of the step's
        // own snapshot: what the census and the drain each judge the floor
        // against before reading their pages, and the G0 a reconcile or a
        // recount beginning on this step records.
        let reading = feed.store_reading()?;
        let census = self.advance_census(feed, &reading)?;
        report.censused = census.rows;
        report.recounts = u64::from(census.recount_begun);
        let (step, counted, completed) =
            self.drain_step_counted(feed, &reading, report, documents_complete)?;
        // A census still catching up owes another step, and a step that read
        // census rows did work a budget counts.
        let step = match step {
            DrainStep::Done(mut done) if !census.complete => {
                done.owed = true;
                DrainStep::Done(done)
            }
            step => step,
        };
        Ok((step, counted || census.rows > 0, completed))
    }

    /// Read the census's next page of each feed in `feed`'s snapshot and
    /// apply it, moving the counts row by row. A page pair that moves nothing
    /// commits nothing.
    fn advance_census(
        &mut self,
        feed: &mut FeedRead<'_>,
        reading: &StoreReading,
    ) -> Result<census::Advance, EngineError> {
        let position = census::Position {
            epoch: self.get_meta(ddl::meta::CENSUS_STORE_EPOCH)?,
            documents_after: self.read_cursor(ddl::meta::CENSUS_DOCUMENT_CURSOR)?,
            tombstones_after: self.read_cursor(ddl::meta::CENSUS_TOMBSTONE_CURSOR)?,
            recount: self.get_meta(ddl::meta::CENSUS_RECOUNT_G0)?,
            completion: self.get_meta(ddl::meta::CENSUS_COMPLETION)?,
        };
        let pages = census::Pages::read(
            feed,
            reading,
            position,
            self.recorded.read_through.census.as_ref(),
        )?;
        let advance = pages.advance();
        // A step that reads nothing past counts left part way, by a page
        // that ended exactly at a feed's end, still records them whole.
        if pages.changes_anything(&self.recorded.counts) {
            let model = self.embedder.model().clone();
            let mut counts = self.recorded.counts;
            let transaction = self
                .database
                .immediate_transaction("advancing the census")?;
            pages.apply(&transaction, &model, &mut counts)?;
            let (revision, read_through) = seal(transaction, "committing the census")?;
            self.recorded.revision = revision;
            self.recorded.counts = counts;
            self.recorded.read_through = read_through;
        }
        Ok(advance)
    }

    /// The drain's own half of a step, after the census has read its pages.
    ///
    /// The drain reconciles before it reads the feed where the store epoch
    /// is not the one it observed, where a reconcile toward this epoch is part
    /// way through, or where it has not read through the floor read in the
    /// step's snapshot: deaths it never read may be gone, so it does not seek
    /// forward. A running reconcile is never started over because of the
    /// floor.
    fn drain_step_counted(
        &mut self,
        feed: &mut FeedRead<'_>,
        reading: &StoreReading,
        mut report: DrainReport,
        documents_complete: bool,
    ) -> Result<(DrainStep, bool, bool), EngineError> {
        let store_epoch = reading.epoch().to_string();
        let observed: Option<String> = self.get_meta(ddl::meta::OBSERVED_STORE_EPOCH)?;
        let running = self.running_reconcile(&store_epoch)?;
        if observed.as_deref() != Some(store_epoch.as_str())
            || running.is_some()
            || self.recorded.read_through.engine_below_floor(reading)
        {
            report.rescan = true;
            // The rescan resets both cursors, and the document feed reads
            // from its start again before the tombstones take their turn.
            self.tombstones_turn = false;
            let (complete, counted) = self.reconcile_page(feed, reading, running, &mut report)?;
            // A pass that completes with the floor above its G0 is followed
            // by another (ADR 0052), even where its completing page held
            // nothing: reading the feeds from their starts now would record a
            // watermark over deaths the prune removed unread.
            let again = complete && self.recorded.read_through.engine_below_floor(reading);
            if counted || !complete || again {
                report.owed = true;
                return Ok((DrainStep::Done(report), counted, false));
            }
        }
        if !documents_complete
            && let Some((step, completed)) =
                self.document_step(feed, &store_epoch, report.clone())?
        {
            return Ok((step, true, completed));
        }
        let (step, counted) = self.tombstone_step(feed, &store_epoch, report)?;
        Ok((step, counted, true))
    }

    /// Commit `page`'s writes and position, where the
    /// sidecar still stands where the page was read: the engine is the
    /// sidecar's one writer, so a page whose revision moved since it was read
    /// was overtaken and is discarded rather than committed over what moved
    /// it. Answers the step's report; a discarded page owes its step again.
    pub fn commit(&mut self, page: EmbeddedPage) -> Result<DrainReport, EngineError> {
        let mut report = page.report;
        if self.revision() != page.revision {
            report.owed = true;
            return Ok(report);
        }
        let retractions: Vec<&str> = page.retractions.iter().map(String::as_str).collect();
        let completes = page.completed.is_some();
        report.retracted +=
            self.commit_documents(&page.writes, &retractions, &page.advanced, page.completed)?;
        self.tombstones_turn |= completes;
        report.owed = true;
        Ok(report)
    }

    /// The next document page, where the feed presents one: committed here
    /// where it embeds nothing, or handed back to embed, beside whether the
    /// page completed the feed. `None` where the page came back empty, its
    /// watermark recorded.
    fn document_step(
        &mut self,
        feed: &mut FeedRead<'_>,
        store_epoch: &str,
        mut report: DrainReport,
    ) -> Result<Option<(DrainStep, bool)>, EngineError> {
        let cursor = self.read_cursor(ddl::meta::DOCUMENT_CURSOR)?;
        let observed = feed.write_generation()?;
        let page = feed.changed_documents_after(cursor.as_ref(), DRAIN_PAGE)?;
        let full = page.len() == DRAIN_PAGE;
        let completed = (!full).then(|| Watermark {
            store_epoch: store_epoch.to_string(),
            generation: observed,
        });
        let Some((last, _)) = page.last() else {
            self.advance_watermark(Feed::Documents, completed)?;
            return Ok(None);
        };
        let advanced = last.clone();

        let mut rows: Vec<PendingRow> = Vec::new();
        let mut retractions = Vec::new();
        for (_, document) in &page {
            let Some(body_hash) = document.body_hash.as_ref() else {
                retractions.push(document.path.as_str().to_string());
                continue;
            };
            let current: Option<String> = self.row_input_hash(document.path.as_str())?;
            if current.as_deref() == Some(body_hash.as_str()) {
                report.skipped_current += 1;
                continue;
            }
            match feed.stored_facts(&document.path)? {
                Some(facts) if facts.document.generation == document.generation => {
                    let Some(body) = facts.body else {
                        report.deferred += 1;
                        continue;
                    };
                    rows.push(PendingRow {
                        path: document.path.as_str().to_string(),
                        input_hash: body_hash.clone(),
                        body,
                    });
                }
                // Superseded mid-drain: re-derived past this row's
                // generation, or died. The feed's later rows carry the
                // truth, so acting on this reading would write a hash
                // beside values it does not describe.
                _ => report.deferred += 1,
            }
        }
        let pending = PendingPage {
            embedder: Arc::clone(&self.embedder),
            rows,
            retractions,
            advanced,
            completed,
            revision: self.revision(),
            report,
        };
        if pending.rows.is_empty() {
            let embedded = pending.embed()?;
            return self
                .commit(embedded)
                .map(|done| Some((DrainStep::Done(done), !full)));
        }
        Ok(Some((DrainStep::Embed(Box::new(pending)), !full)))
    }

    /// The next tombstone page, committed whole: a death embeds nothing.
    /// Answers beside the step whether the page presented rows, which is
    /// what counts it.
    fn tombstone_step(
        &mut self,
        feed: &mut FeedRead<'_>,
        store_epoch: &str,
        mut report: DrainReport,
    ) -> Result<(DrainStep, bool), EngineError> {
        let cursor = self.read_cursor(ddl::meta::TOMBSTONE_CURSOR)?;
        let observed = feed.write_generation()?;
        let page = feed.changed_tombstones_after(cursor.as_ref(), DRAIN_PAGE)?;
        let full = page.len() == DRAIN_PAGE;
        let completed = (!full).then(|| Watermark {
            store_epoch: store_epoch.to_string(),
            generation: observed,
        });
        let Some((last, _)) = page.last() else {
            self.advance_watermark(Feed::Tombstones, completed)?;
            self.tombstones_turn = false;
            return Ok((DrainStep::Done(report), false));
        };
        let advanced = last.clone();
        let paths: Vec<String> = page
            .iter()
            .map(|(_, tombstone)| tombstone.path.as_str().to_string())
            .collect();
        report.retracted += self.commit_retractions(&paths, &advanced, completed)?;
        // A short page ends the tombstones' turn, once its writes commit.
        self.tombstones_turn &= full;
        report.owed = full;
        Ok((DrainStep::Done(report), true))
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

    /// The reconcile part way through toward `store_epoch`, where one is
    /// recorded with its G0. A pass recorded for another epoch names a
    /// database that no longer exists, and one recorded without a G0 cannot
    /// say which deaths it accounts for; neither is resumed.
    fn running_reconcile(&self, store_epoch: &str) -> Result<Option<Running>, EngineError> {
        let target: Option<String> = self.get_meta(ddl::meta::RECONCILE_STORE_EPOCH)?;
        if target.as_deref() != Some(store_epoch) {
            return Ok(None);
        }
        let Some(g0) = self.get_meta::<i64>(ddl::meta::RECONCILE_G0)? else {
            return Ok(None);
        };
        Ok(Some(Running {
            after: self
                .get_meta::<String>(ddl::meta::RECONCILE_CURSOR)?
                .filter(|cursor| !cursor.is_empty()),
            g0,
            read_through: self
                .get_meta(ddl::meta::RECONCILE_READ_THROUGH)?
                .unwrap_or_default(),
        }))
    }

    /// Reconcile the sidecar toward `reading`'s store epoch by one page of
    /// held paths, resuming `running` where it is part way, answering whether
    /// the pass completed and whether the page held any path, which is what
    /// counts it.
    ///
    /// The page checks the next held paths in path order against the store
    /// and commits the removal of those it no longer holds together with the
    /// pass's position. **A pass's first page reads its G0**, the write
    /// generation of `reading`, and every page stores it beside the position
    /// with the engine's read-through generation as its records stood when
    /// the pass began ([ADR 0052]): every death at or below G0 was committed
    /// before that page, so the pass meets its path as no document or as one
    /// derived again, whether or not the death still stands. The page that
    /// comes back short completes the pass: in the same transaction it resets
    /// both cursors, records the epoch as observed and G0 itself as read
    /// through, and clears the position, so the observed epoch never moves
    /// ahead of a whole pass. A reconcile page embeds nothing, so it runs
    /// whole inside its step.
    ///
    /// [ADR 0052]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0052-a-death-is-kept-only-while-lane-2-or-a-broken-link-needs-it.md
    fn reconcile_page(
        &mut self,
        feed: &mut FeedRead<'_>,
        reading: &StoreReading,
        running: Option<Running>,
        report: &mut DrainReport,
    ) -> Result<(bool, bool), EngineError> {
        let store_epoch = reading.epoch();
        let running = running.unwrap_or_else(|| {
            report.reconciles += 1;
            Running {
                after: None,
                g0: reading.write_generation(),
                read_through: progress::encode_read_through(
                    self.recorded.read_through.engine.as_ref(),
                ),
            }
        });
        let held = self.held_paths_after(running.after.as_deref())?;
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

        let model = self.embedder.model().clone();
        let mut counts = self.recorded.counts;
        let transaction = self
            .database
            .immediate_transaction("reconciling the sidecar")?;
        for path in &dead {
            counts.vector_moves(&transaction, &model, path, None)?;
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
                norn_db::meta::put_meta(&transaction, ddl::meta::RECONCILE_G0, running.g0)?;
                norn_db::meta::put_meta(
                    &transaction,
                    ddl::meta::RECONCILE_READ_THROUGH,
                    &running.read_through,
                )?;
            }
            None => {
                norn_db::meta::put_meta(&transaction, ddl::meta::DOCUMENT_CURSOR, "")?;
                norn_db::meta::put_meta(&transaction, ddl::meta::TOMBSTONE_CURSOR, "")?;
                norn_db::meta::put_meta(
                    &transaction,
                    ddl::meta::OBSERVED_STORE_EPOCH,
                    store_epoch,
                )?;
                norn_db::meta::put_meta(
                    &transaction,
                    ddl::meta::RECONCILED_THROUGH,
                    progress::encode_generation_in(running.g0, store_epoch),
                )?;
                norn_db::meta::put_meta(&transaction, ddl::meta::RECONCILE_STORE_EPOCH, "")?;
                norn_db::meta::put_meta(&transaction, ddl::meta::RECONCILE_CURSOR, "")?;
                norn_db::meta::delete_meta(&transaction, ddl::meta::RECONCILE_G0)?;
                norn_db::meta::delete_meta(&transaction, ddl::meta::RECONCILE_READ_THROUGH)?;
            }
        }
        counts.put(&transaction)?;
        let (revision, read_through) = seal(transaction, "committing the reconcile")?;
        self.recorded.revision = revision;
        self.recorded.counts = counts;
        self.recorded.read_through = read_through;
        report.reconciled_away += dead.len() as u64;
        Ok((!full, !held.is_empty()))
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

    /// One page's writes, unreadable-content retractions and advanced cursor,
    /// in one transaction, with the watermark where this page completed the
    /// feed. Unknown content invalidates every model's rows at its path.
    fn commit_documents(
        &mut self,
        writes: &[(String, String, Vec<u8>, usize)],
        retractions: &[&str],
        advanced: &FeedCursor,
        completed: Option<Watermark>,
    ) -> Result<u64, EngineError> {
        let model = self.embedder.model().clone();
        let mut counts = self.recorded.counts;
        let transaction = self
            .database
            .immediate_transaction("committing a drained page")?;
        for (path, input_hash, blob, dimensions) in writes {
            counts.vector_moves(&transaction, &model, path, Some(input_hash))?;
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
        let mut retracted = 0;
        for path in retractions {
            counts.vector_moves(&transaction, &model, path, None)?;
            let removed = transaction
                .execute(
                    "DELETE FROM document_vectors WHERE path = ?1",
                    params![path],
                )
                .map_err(|error| error::sql("retracting unreadable content", error))?;
            if removed > 0 {
                retracted += 1;
            }
        }
        norn_db::meta::put_meta(
            &transaction,
            ddl::meta::DOCUMENT_CURSOR,
            encode_cursor(advanced),
        )?;
        counts.put(&transaction)?;
        let (committed, read_through) = commit_progress(
            transaction,
            "committing a drained page",
            Feed::Documents,
            completed.as_ref(),
        )?;
        self.recorded.revision = committed;
        self.recorded.read_through = read_through;
        self.recorded.counts = counts;
        if let Some(watermark) = completed {
            self.recorded.watermarks.set(Feed::Documents, watermark);
        }
        Ok(retracted)
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
        let model = self.embedder.model().clone();
        let mut counts = self.recorded.counts;
        let transaction = self
            .database
            .immediate_transaction("committing retractions")?;
        let mut retracted = 0_u64;
        for path in paths {
            counts.vector_moves(&transaction, &model, path, None)?;
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
        counts.put(&transaction)?;
        let (committed, read_through) = commit_progress(
            transaction,
            "committing retractions",
            Feed::Tombstones,
            completed.as_ref(),
        )?;
        self.recorded.revision = committed;
        self.recorded.read_through = read_through;
        self.recorded.counts = counts;
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
        let (committed, read_through) =
            commit_progress(transaction, "advancing a watermark", feed, Some(&watermark))?;
        self.recorded.revision = committed;
        self.recorded.read_through = read_through;
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
) -> Result<(i64, ReadThroughs), EngineError> {
    if let Some(watermark) = watermark {
        norn_db::meta::put_meta(
            &transaction,
            feed.watermark_key(),
            progress::encode_watermark(watermark),
        )?;
    }
    seal(transaction, operation)
}

/// Take the next sidecar revision, read the two readers' read-through
/// generations as the transaction leaves their records, and commit as
/// `operation`. Answers the revision and the read-through generations the
/// commit recorded, which the engine holds only once it has committed.
fn seal(
    transaction: norn_db::rusqlite::Transaction<'_>,
    operation: &'static str,
) -> Result<(i64, ReadThroughs), EngineError> {
    let read_through =
        progress::read_throughs(&transaction).map_err(progress::Unread::into_engine_error)?;
    let revision = norn_db::meta::next_generation(&transaction)?;
    transaction
        .commit()
        .map_err(|error| error::sql(operation, error))?;
    Ok((revision, read_through))
}

/// A reconcile part way through: the held path its last page checked, the
/// G0 its first page read, and the engine's read-through generation as its
/// records stood when it began, spelled as the sidecar records it.
struct Running {
    after: Option<String>,
    g0: i64,
    read_through: String,
}

/// `{generation}:{path}` — one scalar, so a position moves atomically. The
/// generation half cannot hold `:`, so the first `:` is always the seam.
pub(crate) fn encode_cursor(cursor: &FeedCursor) -> String {
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
