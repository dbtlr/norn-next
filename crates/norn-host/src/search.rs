//! `search`: the ladder a request selects, resolved against what the host
//! holds for the vault when it answers, run over one snapshot, and fused.
//!
//! **A search answers through the read seam.** It takes one hold on a ready
//! entry, so a vault that is not ready is refused with the ordinary
//! answer-reading refusal before any rung runs, and every rung reads the
//! vault's store through the one snapshot that hold established.
//!
//! # Resolving the selection
//!
//! The vault's enabled set holds the lexical floor always, and the vector rung
//! where the engine section the host was delivered enables one. Expansion and
//! re-ranking have no runtime here, so no enabled set holds them and an exact
//! set naming one is refused `engine/not-enabled` before any engine is
//! sampled. The vector rung is sampled once, under the engine slot's lock,
//! together with its answer ([`SemanticEngines::nearest_among`]), and the
//! selection is resolved against that one sample:
//!
//! - An exact set naming the vector rung is refused where the sample refuses,
//!   with the one composition every vector refusal goes through
//!   ([`VectorRefusal`]).
//! - The enabled selection leaves the rung out silently where the enabled set
//!   does not hold it, leaves it out advised as skipped where it is enabled and
//!   no engine stands for it, and is refused where an engine stands and the
//!   answer failed.
//! - A resolution left holding no retrieval rung is refused with the code of
//!   the vector rung that failed it, never answered empty.
//!
//! # The ladders
//!
//! **The lexical floor alone is the store's page**: the request's floor, bound
//! and cursor are the lexical rung's own, so [`Snapshot::search`] answers it as
//! it stands, and the answer declares `[lexical]`, repeatable.
//!
//! **A ladder holding the vector rung is ranked here.** Each retrieval rung
//! contributes at most [`RUNG_DEPTH`] candidates, and an answer whose rung
//! reached that depth is advised so. The lexical rung contributes its first
//! page at that depth, unfloored. The vector rung takes one of two paths, and
//! on both no answer names a document its snapshot lacks — a vector row whose
//! path the snapshot does not hold, the sidecar ahead of the snapshot or
//! behind it, is never answered:
//!
//! - **A filtered search** — one whose conjunction names a part — scores only
//!   the documents the conjunction admits on the snapshot, drawn first by
//!   paging the store's candidates, a find's pages, so a document the
//!   conjunction does not admit is never scored. The scan holds at most the
//!   depth's scored rows; the admitted set it is tested against is held whole,
//!   so what the search holds is proportional to the documents the
//!   conjunction admits.
//! - **An unfiltered search** takes no admitted-set pass. The scan holds the
//!   depth and a margin more rows: the engine's drain lag counted in feed rows
//!   on the snapshot — the documents changed and the deaths recorded past each
//!   feed's watermark, however few write generations stamped them — capped at
//!   [`VECTOR_MARGIN_CAP`], and the cap where no lag can be read: a watermark
//!   from another store lifetime or past the snapshot, or a feed never
//!   drained. Each row the
//!   scan kept, and each lexical hit, is then checked against the snapshot by
//!   path, one seek each; a row the snapshot lacks is dropped, and at most the
//!   depth of the rest are ranked. So what the search holds is bounded by the
//!   depth and the cap at any vault size, and the check costs them and never
//!   the vault. Where more rows the snapshot lacks stood in the scan than the
//!   margin absorbed, the rung delivers fewer than the depth and the answer is
//!   advised how many ([`AnswerAdvisory::RungShortOfDepth`]); it never claims
//!   a depth it did not deliver.
//!
//! A two-rung ladder is fused by reciprocal rank: a document's score is the
//! sum over the rungs that ranked it of `1 / (RRF_K + rank)`, its rank in
//! each counted from one. A one-rung ladder keeps its rung's scale, the vector
//! rung's score being the engine's. Either way the hits are ordered by score
//! descending then path in byte order, the request's floor applies to that
//! final scale, and a page of the request's bound is cut from that order.
//!
//! **The cursor names the ladder and the sidecar state.** A page's cursor is a
//! hit cursor over the resolved ladder, carrying the snapshot's reading and
//! the sidecar revision sampled with the answer; it is judged by the store's
//! one ranked-cursor judgment, so a continuation under a moved sidecar reports
//! that the sidecar moved and one ranked by another ladder is refused.
//!
//! **Every answer declares its ladder.** A ladder holding the vector rung
//! names the model that derived its vectors and how far that derivation
//! trails the snapshot's store reading ([`freshness`]), and it is not
//! repeatable: its order depends on state that drains underneath it.

use std::collections::{BTreeMap, BTreeSet};

use norn_semantic::{NearestWork, Neighbor, Watermark, Watermarks};
use norn_store::{
    Candidate, ContentModel, LexicalQuery, MAX_PAGE, PageRefusal, SearchWork, Snapshot, page_limit,
};
use norn_wire::{
    AnswerAdvisory, Cursor, CursorKey, ErrorDetail, ErrorEnvelope, FindParams, LadderDeclaration,
    ModelIdentity, Page, RUNG_DEPTH, Rung, RungReport, RungSelection, RungSet, Score, SearchParams,
    SearchReport, Unsatisfied, VaultName,
};

use crate::lifecycle::{EntryOps, Host, ReadSource, SnapshotSource};
use crate::read::{Answered, BuildRefused, Built};
use crate::semantic::{SemanticAnswer, VectorRefusal, freshness};

/// The constant of reciprocal-rank fusion: a document ranked `rank` by a rung,
/// counted from one, scores `1 / (RRF_K + rank)` from that rung.
///
/// An authored threshold ([ADR
/// 0007](../../../docs/decisions/0007-authored-measurement-thresholds.md)),
/// changed only by a reviewed edit: it sets how far a rung's head outweighs
/// its tail, and a fused score read under another constant is on another
/// scale.
pub const RRF_K: u32 = 60;

/// The most rows an unfiltered search's vector rung holds beyond its depth,
/// for the rows its sidecar holds that the answer's snapshot may lack.
///
/// An authored threshold ([ADR
/// 0007](../../../docs/decisions/0007-authored-measurement-thresholds.md)),
/// changed only by a reviewed edit. It is the rung's own depth: at the cap the
/// scan holds twice [`RUNG_DEPTH`] scored rows — a path and a score each, tens
/// of kilobytes — and an engine trailing its store by as many feed rows as the
/// rung ranks, a mass delete among them, still delivers the depth. A lag past
/// it costs the answer candidates, advised in band, and never costs memory
/// that grows with the lag.
pub const VECTOR_MARGIN_CAP: u32 = RUNG_DEPTH;

/// What one search read, by rung.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchCost {
    /// What the lexical rung's page read, where the ladder held it.
    pub lexical: Option<SearchWork>,
    /// The candidate pages the vector rung was restricted by, where the rung
    /// was sampled on a filtered search, and zero where it was not.
    pub candidate_pages: u64,
    /// The candidates those pages held: the documents the conjunction admits,
    /// where the rung was sampled on a filtered search, and zero where it was
    /// not.
    pub candidates: u64,
    /// The rows the vector rung's scan held beyond its depth, where the rung
    /// was sampled on an unfiltered search: the engine's drain lag in feed
    /// rows, capped at [`VECTOR_MARGIN_CAP`]. Zero on a filtered search.
    pub margin: u64,
    /// The paths an unfiltered search asked its snapshot whether it holds:
    /// the vector rung's rows and the lexical rung's hits, each once. Zero on
    /// a filtered search.
    pub paths_checked: u64,
    /// What the vector rung's scan read, scored and held, where it answered.
    pub vector: Option<NearestWork>,
}

/// The documents a request's conjunction admits on one snapshot, by path,
/// with what reading the conjunction reported.
struct Admitted {
    by_path: BTreeMap<String, Candidate>,
    unsatisfied: Vec<Unsatisfied>,
    advisories: Vec<AnswerAdvisory>,
    snapshot: norn_wire::Snapshot,
}

/// What the vector rung's scan was restricted by.
enum Restriction {
    /// A filtered search: the documents its conjunction admits, drawn whole
    /// before the scan, which scores no other.
    Admitted(Admitted),
    /// An unfiltered search: nothing. The scan held the rung's depth and
    /// `margin` rows more, and each row it kept is checked against the
    /// snapshot after it.
    Unfiltered { margin: usize },
}

/// What the host found of the vector rung when it sampled the vault.
enum VectorSample {
    /// The selection does not name the rung, so it was not sampled.
    Unsampled,
    /// An engine stands and answered, restricted as the request's conjunction
    /// restricts it.
    Answered(Box<(SemanticAnswer, Restriction)>),
    /// The rung cannot answer, and why.
    Refused(VectorRefusal),
}

/// The ladder a selection resolved to, and the rung it skipped, where it
/// skipped one.
#[derive(Debug, PartialEq)]
struct Resolved {
    ladder: RungSet,
    skipped: Option<AnswerAdvisory>,
}

/// The refusal an exact set naming `rung` meets where no runtime answers it.
fn no_runtime(rung: Rung) -> ErrorEnvelope {
    ErrorEnvelope::new(
        "this vault answers no rung that no runtime answers",
        ErrorDetail::engine_not_enabled(
            rung,
            "no runtime for this rung ships in this build, so no vault enables it",
        ),
    )
}

/// A selection naming no rung that has no runtime here: the only selection
/// [`samples_vector`] and [`resolve`] take, so no ladder is resolved holding
/// a rung nothing would run.
#[derive(Clone, Copy, Debug)]
struct WithRuntime<'a>(&'a RungSelection);

/// `selection`, where every rung it names has a runtime here, or the refusal
/// an exact set naming one that has none meets, which it meets before any
/// engine is sampled. The enabled set holds no such rung.
fn with_runtime(selection: &RungSelection) -> Result<WithRuntime<'_>, ErrorEnvelope> {
    match selection {
        RungSelection::Enabled { .. } => Ok(WithRuntime(selection)),
        RungSelection::Exactly { rungs, .. } => match rungs
            .rungs()
            .iter()
            .find(|rung| !matches!(rung, Rung::Lexical | Rung::Vector))
        {
            Some(rung) => Err(no_runtime(*rung)),
            None => Ok(WithRuntime(selection)),
        },
    }
}

/// Whether `selection` names the vector rung, and so samples it.
fn samples_vector(selection: WithRuntime<'_>) -> bool {
    match selection.0 {
        RungSelection::Enabled { without, .. } => !without.rungs().contains(&Rung::Vector),
        RungSelection::Exactly { rungs, .. } => rungs.rungs().contains(&Rung::Vector),
    }
}

/// Resolve `selection` against the vector rung's one sample. The ladder it
/// resolves holds the vector rung exactly where the sample answered.
///
/// A host that composes no semantic engine samples the rung as not enabled
/// ([`VectorRefusal::not_composed`]) whatever the vault's engine section says,
/// because no section was delivered to an engine: the enabled selection then
/// leaves the rung out silently, and an exact set naming it is refused
/// `engine/not-enabled`, told to serve the vault from a host that composes the
/// engine.
fn resolve(selection: WithRuntime<'_>, vector: &VectorSample) -> Result<Resolved, ErrorEnvelope> {
    match selection.0 {
        RungSelection::Exactly { rungs, .. } => {
            if rungs.rungs().contains(&Rung::Vector) {
                match vector {
                    VectorSample::Answered(_) => {}
                    VectorSample::Refused(refusal) => return Err(refusal.clone().envelope()),
                    // A selection naming the rung samples it.
                    VectorSample::Unsampled => {
                        return Err(VectorRefusal::not_composed().envelope());
                    }
                }
            }
            Ok(Resolved {
                ladder: rungs.clone(),
                skipped: None,
            })
        }
        RungSelection::Enabled { without, .. } => {
            let mut ladder = Vec::new();
            if !without.rungs().contains(&Rung::Lexical) {
                ladder.push(Rung::Lexical);
            }
            let mut skipped = None;
            let mut left_out = None;
            match vector {
                VectorSample::Unsampled => {}
                VectorSample::Answered(_) => ladder.push(Rung::Vector),
                VectorSample::Refused(refusal) => match refusal {
                    VectorRefusal::NotEnabled { .. } => left_out = Some(refusal),
                    VectorRefusal::Unavailable { .. } => {
                        skipped = refusal
                            .skip_reason()
                            .map(|reason| AnswerAdvisory::rung_skipped(Rung::Vector, reason));
                        left_out = Some(refusal);
                    }
                    VectorRefusal::Failed { .. } => return Err(refusal.clone().envelope()),
                },
            }
            match RungSet::of(ladder) {
                Ok(ladder) => Ok(Resolved { ladder, skipped }),
                // The subtraction left out the lexical floor, and the vector
                // rung did not join: refused with the vector rung's own code.
                Err(_) => Err(left_out
                    .cloned()
                    .unwrap_or_else(VectorRefusal::not_composed)
                    .envelope()),
            }
        }
    }
}

/// One scored document of a ranking: its path and its score on the ladder's
/// scale.
#[derive(Clone, Debug, PartialEq)]
struct Ranked {
    path: String,
    score: f64,
}

/// Order `ranked` by score descending, then path in byte order.
fn order(ranked: &mut [Ranked]) {
    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.path.cmp(&b.path))
    });
}

/// The reciprocal-rank fusion of `rankings`, each a rung's documents best
/// first, in ladder order: every document's score is the sum over the rungs
/// that ranked it of `1 / (RRF_K + rank)`, summed in ladder order.
fn fused(rankings: &[Vec<String>]) -> Vec<Ranked> {
    let mut scores: BTreeMap<&str, f64> = BTreeMap::new();
    for ranking in rankings {
        for (at, path) in ranking.iter().enumerate() {
            let rank = at as f64 + 1.0;
            *scores.entry(path.as_str()).or_insert(0.0) += 1.0 / (f64::from(RRF_K) + rank);
        }
    }
    let mut ranked: Vec<Ranked> = scores
        .into_iter()
        .map(|(path, score)| Ranked {
            path: path.to_string(),
            score,
        })
        .collect();
    order(&mut ranked);
    ranked
}

/// The vector rung's neighbors on their own scale, ordered. Every score is
/// finite: the engine refuses an answer that scored a row otherwise.
fn vector_scale(neighbors: &[Neighbor]) -> Vec<Ranked> {
    let mut ranked: Vec<Ranked> = neighbors
        .iter()
        .map(|neighbor| Ranked {
            path: neighbor.path.clone(),
            // Adding zero turns a negative zero into zero, so the order,
            // the page a cursor resumes and the score a cursor carries
            // agree on one zero.
            score: f64::from(neighbor.score) + 0.0,
        })
        .collect();
    order(&mut ranked);
    ranked
}

/// The sidecar state an answer read, as a cursor carries it, or the refusal
/// that the engine reported a revision below zero, which names no state.
fn wire_sidecar(
    sidecar: &norn_semantic::SidecarRevision,
) -> Result<norn_wire::SidecarRevision, VectorRefusal> {
    let revision = u64::try_from(sidecar.revision).map_err(|_| VectorRefusal::Failed {
        message: "this vault's engine failed to answer the vector rung".to_string(),
        detail: format!(
            "the sidecar reported revision {}, which names no state",
            sidecar.revision
        ),
    })?;
    Ok(norn_wire::SidecarRevision::new(
        sidecar.epoch.clone(),
        revision,
    ))
}

/// One page of `ranked`, which is in ranking order: the documents scored at or
/// above `floor`, after the position `resume` names, at most `limit` of them,
/// and whether a document stands after the page.
fn page_of(
    ranked: Vec<Ranked>,
    floor: Option<Score>,
    resume: Option<(f64, &str)>,
    limit: usize,
) -> (Vec<Ranked>, bool) {
    let mut kept = ranked
        .into_iter()
        .filter(|hit| floor.is_none_or(|floor| hit.score >= floor.get()))
        .filter(|hit| match resume {
            None => true,
            Some((score, path)) => score
                .total_cmp(&hit.score)
                .then_with(|| hit.path.as_str().cmp(path))
                .is_gt(),
        });
    let page: Vec<Ranked> = kept.by_ref().take(limit).collect();
    let more = kept.next().is_some();
    (page, more)
}

/// The lexical rung's request for `params`: the request's own floor, bound and
/// cursor, where the lexical floor is the whole ladder.
fn lexical_page(params: &SearchParams) -> LexicalQuery {
    let mut query = LexicalQuery::new(params.query.clone())
        .with_predicates(params.predicates.clone())
        .with_columns(params.columns.clone());
    query.min_score = params.min_score;
    query.limit = params.limit;
    query.after = params.after.clone();
    query
}

/// The lexical rung's candidates for a fused ladder: its first page at the
/// rung's depth, unfloored, hydrating nothing.
fn lexical_candidates(params: &SearchParams) -> LexicalQuery {
    LexicalQuery::new(params.query.clone())
        .with_predicates(params.predicates.clone())
        .with_limit(RUNG_DEPTH)
}

/// Every document `params`' conjunction admits on `snapshot`, drawn a page of
/// the store's candidates at a time, and how many pages that took.
fn admitted(
    snapshot: &Snapshot,
    params: &SearchParams,
    declared: &ContentModel,
) -> Result<(Admitted, u64), PageRefusal> {
    let request = FindParams::new(params.vault.clone())
        .with_predicates(params.predicates.clone())
        .with_limit(MAX_PAGE as u32);
    let first = snapshot.search_candidates(&request, declared)?;
    let mut pages = 1;
    let mut admitted = Admitted {
        by_path: BTreeMap::new(),
        unsatisfied: first.unsatisfied,
        advisories: first.advisories,
        snapshot: first.snapshot,
    };
    let mut page = first.candidates;
    let mut next = first.next;
    loop {
        for candidate in page {
            admitted
                .by_path
                .insert(candidate.path().to_string(), candidate);
        }
        let Some(after) = next else {
            break;
        };
        let continued = snapshot.search_candidates(&request.clone().with_after(after), declared)?;
        pages += 1;
        page = continued.candidates;
        next = continued.next;
    }
    Ok((admitted, pages))
}

/// The rows an unfiltered search's vector rung holds beyond its depth: how
/// far the engine's drain trails `snapshot` in feed rows — the documents
/// changed and the deaths recorded past each feed's watermark, however many
/// write generations stamped them — capped at [`VECTOR_MARGIN_CAP`].
///
/// A feed whose watermark was taken in another store lifetime, past the
/// snapshot's write generation — a drain that finished after the snapshot was
/// taken, over rows the snapshot cannot count — or that has completed no
/// drain, trails by a count nothing can read, so the margin is the cap. The
/// count is bounded at one past the cap, so it costs at most that many rows
/// of each feed.
fn margin(snapshot: &Snapshot, watermarks: &Watermarks) -> Result<usize, PageRefusal> {
    let cap = VECTOR_MARGIN_CAP as usize;
    let reading = snapshot.reading();
    let past = |watermark: &Option<Watermark>| match watermark {
        Some(watermark)
            if watermark.store_epoch == reading.epoch()
                && watermark.generation <= reading.write_generation() =>
        {
            Some(watermark.generation)
        }
        Some(_) | None => None,
    };
    let (Some(documents), Some(tombstones)) =
        (past(&watermarks.documents), past(&watermarks.tombstones))
    else {
        return Ok(cap);
    };
    let rows = snapshot.feed_rows_after(documents, tombstones, u64::from(VECTOR_MARGIN_CAP) + 1)?;
    let lag = rows.documents.saturating_add(rows.tombstones);
    Ok(usize::try_from(lag).map_or(cap, |lag| lag.min(cap)))
}

impl<O> Host<O>
where
    O: EntryOps,
    <O::Attachment as SnapshotSource>::Reader: ReadSource<Snapshot = Snapshot>,
{
    /// Answer a `search`: one page of the hits the ladder `params` selects
    /// ranks for its query, from the vault it addresses.
    pub fn search(
        &self,
        params: &SearchParams,
    ) -> Result<Answered<SearchReport, SearchCost>, ErrorEnvelope> {
        self.answer_read(&params.vault, |vault, snapshot, declared| {
            let selection = with_runtime(&params.rungs).map_err(BuildRefused::Answered)?;
            let limit = page_limit(params.limit)?;
            let mut cost = SearchCost::default();
            let vector = if samples_vector(selection) {
                self.sample_vector(vault, snapshot, params, declared, &mut cost)?
            } else {
                VectorSample::Unsampled
            };
            let resolved = resolve(selection, &vector).map_err(BuildRefused::Answered)?;
            // A resolution over an answered sample holds the vector rung, and
            // one over any other sample does not, so the sample alone says
            // which ladder runs.
            let VectorSample::Answered(answered) = vector else {
                return lexical_answer(snapshot, params, declared, resolved, cost);
            };
            let (answer, restriction) = *answered;
            ranked_answer(
                snapshot,
                params,
                declared,
                RankedInputs {
                    resolved,
                    answer,
                    restriction,
                    limit,
                },
                cost,
            )
        })
    }

    /// Sample the vector rung of `vault`, the vault `params` addresses:
    /// whether an engine stands and, where one does, its answer.
    ///
    /// Whether an engine stands is read first, with how far it had drained,
    /// so a vault whose engine is not running pays for no restriction; the
    /// answer then samples the slot again, under its lock, and that sample is
    /// the one the selection resolves by.
    ///
    /// **A filtered search scores only what its conjunction admits**: the
    /// admitted set is drawn whole from `snapshot` first, and the scan holds
    /// at most the rung's depth. **An unfiltered search takes no admitted-set
    /// pass**: the scan holds the depth and a margin — the drain lag in feed
    /// rows, capped ([`margin`]) — so rows the sidecar holds for documents the
    /// snapshot lacks leave the depth standing once they are dropped.
    fn sample_vector(
        &self,
        vault: &VaultName,
        snapshot: &Snapshot,
        params: &SearchParams,
        declared: &ContentModel,
        cost: &mut SearchCost,
    ) -> Result<VectorSample, BuildRefused> {
        let Some(engines) = self.ops().semantic() else {
            return Ok(VectorSample::Refused(VectorRefusal::not_composed()));
        };
        let watermarks = match engines.standing(vault) {
            Ok(watermarks) => watermarks,
            Err(refusal) => return Ok(VectorSample::Refused(VectorRefusal::of(refusal))),
        };
        let depth = RUNG_DEPTH as usize;
        let answered = if params.predicates.is_empty() {
            let margin = margin(snapshot, &watermarks)?;
            cost.margin = margin as u64;
            engines
                .nearest_among(vault, &params.query, depth + margin, |_| true)
                .map(|answer| (answer, Restriction::Unfiltered { margin }))
        } else {
            let (admitted, pages) = admitted(snapshot, params, declared)?;
            cost.candidate_pages = pages;
            cost.candidates = admitted.by_path.len() as u64;
            engines
                .nearest_among(vault, &params.query, depth, |path| {
                    admitted.by_path.contains_key(path)
                })
                .map(|answer| (answer, Restriction::Admitted(admitted)))
        };
        match answered {
            Ok((answer, restriction)) => {
                cost.vector = Some(answer.work);
                Ok(VectorSample::Answered(Box::new((answer, restriction))))
            }
            Err(refusal) => Ok(VectorSample::Refused(VectorRefusal::of(refusal))),
        }
    }
}

/// The lexical floor alone: the store's page, advised of the rung the
/// resolution skipped.
fn lexical_answer(
    snapshot: &Snapshot,
    params: &SearchParams,
    declared: &ContentModel,
    resolved: Resolved,
    mut cost: SearchCost,
) -> Result<Built<SearchReport, SearchCost>, BuildRefused> {
    let searched = snapshot.search(&lexical_page(params), declared)?;
    cost.lexical = Some(searched.work);
    let (unsatisfied, mut advisories, report) = searched.into_report();
    advisories.extend(resolved.skipped);
    Ok(Built {
        unsatisfied,
        advisories,
        report,
        work: cost,
    })
}

/// The ladder an answer holding the vector rung ran: the lexical floor where
/// `lexical_ran`, and the vector rung, naming the model that derived its
/// vectors and how far that derivation trails `snapshot`'s store reading. It
/// is not repeatable.
///
/// It is built from the rungs that ran, so no rung is declared that did not
/// run and none that ran is left out. A ladder holding the vector rung holds a
/// retrieval rung, in ladder order, so the declaration is never malformed;
/// were it, the vector rung's answer could not be declared, which is refused
/// `engine/failed` like any other vector answer that cannot be told.
fn declaration(
    lexical_ran: bool,
    answer: &SemanticAnswer,
    snapshot: &Snapshot,
) -> Result<LadderDeclaration, BuildRefused> {
    let vector = RungReport::vector(
        ModelIdentity::new(answer.model.id(), answer.model.version()),
        freshness(&answer.watermarks, snapshot.reading()),
    );
    let rungs = if lexical_ran {
        vec![RungReport::lexical(), vector]
    } else {
        vec![vector]
    };
    LadderDeclaration::new(rungs, false).map_err(|malformed| {
        BuildRefused::Answered(
            VectorRefusal::Failed {
                message: "this vault's engine failed to answer the vector rung".to_string(),
                detail: format!("the answer declared no ladder: {malformed}"),
            }
            .envelope(),
        )
    })
}

/// What a ladder holding the vector rung is ranked from.
struct RankedInputs {
    resolved: Resolved,
    answer: SemanticAnswer,
    restriction: Restriction,
    limit: usize,
}

/// The vector rung's contribution to a ranking: its hits on its own scale,
/// best first, at most the rung's depth of them; the documents the ranking
/// may name, by path; and what the answer is advised of the rung's depth.
struct Restricted {
    vector: Vec<Ranked>,
    admitted: Admitted,
    depth: Option<AnswerAdvisory>,
}

/// Restrict the vector rung's hits, `vector`, as `restriction` says, beside
/// the lexical rung's hits `lexical`.
///
/// A filtered search's hits are all admitted already, and the rung reached
/// its depth where it scored more rows than the depth.
///
/// An unfiltered search asks `snapshot` which of the vector rung's rows and
/// the lexical rung's hits it holds — one seek per path, so the check costs
/// the depth and the margin and never the vault — and drops each vector row
/// whose path it does not hold, then keeps at most the depth. The scan cut
/// rows where it scored more than it held; so the rung reached its depth
/// where more survived than the depth, or the depth survived and the scan cut
/// rows, and it fell short of its depth where fewer survived and the scan cut
/// rows it never checked.
fn restricted(
    snapshot: &Snapshot,
    restriction: Restriction,
    vector: Vec<Ranked>,
    lexical: &[String],
    work: &NearestWork,
    cost: &mut SearchCost,
) -> Result<Restricted, PageRefusal> {
    let depth = RUNG_DEPTH as usize;
    let margin = match restriction {
        Restriction::Admitted(admitted) => {
            return Ok(Restricted {
                vector,
                admitted,
                depth: (work.rows_scored > u64::from(RUNG_DEPTH))
                    .then(|| AnswerAdvisory::rung_depth_reached(Rung::Vector)),
            });
        }
        Restriction::Unfiltered { margin } => margin,
    };
    let asked: BTreeSet<&str> = vector
        .iter()
        .map(|hit| hit.path.as_str())
        .chain(lexical.iter().map(String::as_str))
        .collect();
    cost.paths_checked = asked.len() as u64;
    let held = snapshot.held_candidates(&asked.into_iter().collect::<Vec<_>>())?;
    let by_path: BTreeMap<String, Candidate> = held
        .candidates
        .into_iter()
        .map(|candidate| (candidate.path().to_string(), candidate))
        .collect();
    let mut vector: Vec<Ranked> = vector
        .into_iter()
        .filter(|hit| by_path.contains_key(&hit.path))
        .collect();
    let survived = vector.len();
    vector.truncate(depth);
    let cut = work.rows_scored > (depth + margin) as u64;
    let advised = if survived > depth || (survived == depth && cut) {
        Some(AnswerAdvisory::rung_depth_reached(Rung::Vector))
    } else if cut {
        // Fewer than the depth survived, so the count fits the depth's type.
        Some(AnswerAdvisory::rung_short_of_depth(
            Rung::Vector,
            u32::try_from(survived).unwrap_or(RUNG_DEPTH),
        ))
    } else {
        None
    };
    Ok(Restricted {
        vector,
        admitted: Admitted {
            by_path,
            unsatisfied: Vec::new(),
            advisories: Vec::new(),
            snapshot: held.snapshot,
        },
        depth: advised,
    })
}

/// A ladder holding the vector rung: each rung's candidates to its depth,
/// fused where the ladder holds two, paged, and hydrated.
fn ranked_answer(
    snapshot: &Snapshot,
    params: &SearchParams,
    declared: &ContentModel,
    inputs: RankedInputs,
    mut cost: SearchCost,
) -> Result<Built<SearchReport, SearchCost>, BuildRefused> {
    let RankedInputs {
        resolved,
        answer,
        restriction,
        limit,
    } = inputs;
    let sidecar = wire_sidecar(&answer.sidecar)
        .map_err(|refusal| BuildRefused::Answered(refusal.envelope()))?;
    let vector = vector_scale(&answer.neighbors);
    let lexical_ran = resolved.ladder.rungs().contains(&Rung::Lexical);
    let declaration = declaration(lexical_ran, &answer, snapshot)?;
    let ladder = declaration.rung_set();
    let mut advisories = Vec::new();
    let mut depth_reached = Vec::new();
    let searched = if lexical_ran {
        Some(snapshot.search(&lexical_candidates(params), declared)?)
    } else {
        None
    };
    let lexical: Vec<String> = searched
        .iter()
        .flat_map(|searched| searched.hits.iter())
        .map(|hit| hit.path.as_str().to_string())
        .collect();
    let Restricted {
        vector,
        admitted,
        depth: vector_depth,
    } = restricted(
        snapshot,
        restriction,
        vector,
        &lexical,
        &answer.work,
        &mut cost,
    )?;
    let (unsatisfied, ranked) = match searched {
        Some(searched) => {
            if searched.next.is_some() {
                depth_reached.push(AnswerAdvisory::rung_depth_reached(Rung::Lexical));
            }
            advisories.extend(searched.advisories.iter().cloned());
            // Every lexical hit satisfies the conjunction on the same
            // snapshot, so each one is admitted — or, unfiltered, held — and
            // has a candidate to hydrate. A hit that had none could not be
            // hydrated, so it is left out of the fusion rather than answered.
            let lexical: Vec<String> = lexical
                .into_iter()
                .filter(|path| {
                    let admits = admitted.by_path.contains_key(path);
                    debug_assert!(admits, "the lexical hit `{path}` is not admitted");
                    admits
                })
                .collect();
            let vector: Vec<String> = vector.into_iter().map(|hit| hit.path).collect();
            cost.lexical = Some(searched.work);
            (searched.unsatisfied, fused(&[lexical, vector]))
        }
        None => {
            advisories.extend(admitted.advisories.iter().cloned());
            (admitted.unsatisfied.clone(), vector)
        }
    };
    depth_reached.extend(vector_depth);

    let (resume, moved) = match &params.after {
        None => (None, Vec::new()),
        Some(cursor) => {
            let resume = snapshot.judge_hit_cursor(cursor, &ladder, Some(sidecar.clone()))?;
            (
                Some((resume.score.get(), resume.path.to_string())),
                resume.moved,
            )
        }
    };
    let (page, more) = page_of(
        ranked,
        params.min_score,
        resume.as_ref().map(|(score, path)| (*score, path.as_str())),
        limit,
    );
    let scored: Vec<(Candidate, Score)> = page
        .iter()
        .filter_map(|hit| {
            let candidate = admitted.by_path.get(&hit.path)?.clone();
            Some(Score::new(hit.score).map(|score| (candidate, score)))
        })
        .collect::<Result<_, _>>()
        .map_err(|_| {
            BuildRefused::Answered(
                VectorRefusal::Failed {
                    message: "this vault's engine failed to answer the vector rung".to_string(),
                    detail: "a fused score is no relevance score".to_string(),
                }
                .envelope(),
            )
        })?;
    let mut sidecar_reading = admitted.snapshot.clone();
    sidecar_reading.sidecar_revision = Some(sidecar);
    let next = match (more, scored.last()) {
        (true, Some((candidate, score))) => Some(Cursor::new(
            sidecar_reading,
            CursorKey::hit(ladder.clone(), *score, candidate.path()),
        )),
        _ => None,
    };
    let hydrated = snapshot.hydrate_hits(&scored, &params.columns, declared)?;

    let mut unsatisfied = unsatisfied;
    unsatisfied.extend(hydrated.unsatisfied);
    advisories.extend(resolved.skipped);
    advisories.extend(depth_reached);
    Ok(Built {
        unsatisfied,
        advisories,
        report: SearchReport::new(declaration, Page::new(hydrated.hits, next, moved)),
        work: cost,
    })
}

#[cfg(test)]
mod tests {

    /// **A page resumes by the comparison that ordered it**: a ranking holding
    /// both zeros, ordered by `total_cmp`, drains a page at a time to the
    /// ranking whole, and the vector rung reads a negative zero as zero.
    #[test]
    fn a_page_resumes_by_the_order_that_ranked_it() {
        let mut ranked = vec![
            Ranked {
                path: "a".to_string(),
                score: -0.0,
            },
            Ranked {
                path: "b".to_string(),
                score: 0.0,
            },
            Ranked {
                path: "c".to_string(),
                score: -1.0,
            },
        ];
        order(&mut ranked);
        let whole: Vec<String> = ranked.iter().map(|hit| hit.path.clone()).collect();
        let mut drained = Vec::new();
        let mut resume: Option<(f64, String)> = None;
        loop {
            let (page, more) = page_of(
                ranked.clone(),
                None,
                resume.as_ref().map(|(score, path)| (*score, path.as_str())),
                1,
            );
            let last = page.last().expect("a page before the end");
            resume = Some((last.score, last.path.clone()));
            drained.extend(page.iter().map(|hit| hit.path.clone()));
            if !more {
                break;
            }
        }
        assert_eq!(drained, whole);

        let scaled = vector_scale(&[Neighbor {
            path: "z".to_string(),
            score: -0.0,
        }]);
        assert!(scaled[0].score.is_sign_positive());
    }
    use norn_wire::{EngineSection, ReasonCode, RungSkipReason};

    use super::*;
    use crate::semantic::SemanticRefusal;

    /// A store its own scratch directory holds, a snapshot read handle over
    /// it, and the generation it stood at before `documents` were derived in
    /// one write and the first `deaths` of them recorded dead in another.
    struct Lagging {
        _scratch: norn_testkit::scratch::Scratch,
        store: norn_store::Store,
        reader: std::sync::Arc<norn_store::SnapshotReader>,
        before: i64,
    }

    impl Lagging {
        fn new(label: &str, documents: usize, deaths: usize) -> Self {
            let scratch = norn_testkit::scratch::Scratch::new(label);
            let mut store = norn_store::Store::open(
                scratch.join("store.sqlite3"),
                norn_store::StoredPathOrder::Sensitive,
                crate::DERIVATION_VERSION,
            )
            .expect("a store");
            let before = store
                .begin_request()
                .write_generation()
                .expect("the store's generation");
            let path = |at: usize| {
                norn_store::DocumentPath::new(&format!("d/{at:04}.md")).expect("a path")
            };
            let derived: Vec<norn_store::Change> = (0..documents)
                .map(|at| {
                    norn_store::Change::Upsert(norn_store::DocumentFacts::new(
                        path(at),
                        format!("hash-{at}"),
                        "a body\n",
                        7,
                    ))
                })
                .collect();
            let dead: Vec<norn_store::Change> = (0..deaths)
                .map(|at| norn_store::Change::Death {
                    path: path(at),
                    provenance: norn_store::Provenance::WatcherRemoval,
                })
                .collect();
            for changes in [derived, dead] {
                store
                    .begin_request()
                    .apply_increment(norn_store::IncrementProvenance::Derived, changes, &[])
                    .expect("one write");
            }
            let reader = std::sync::Arc::new(store.open_reader().reader.expect("a reader"));
            Lagging {
                _scratch: scratch,
                store,
                reader,
                before,
            }
        }

        fn snapshot(&self) -> Snapshot {
            self.reader
                .try_take()
                .expect("a handle nothing reads")
                .establish()
                .snapshot
                .expect("a snapshot")
        }

        /// Both feeds' watermarks at `generation`, in this store's lifetime.
        fn marks(&self, generation: i64) -> Watermarks {
            let at = Some(Watermark {
                store_epoch: self.store.epoch().to_string(),
                generation,
            });
            Watermarks {
                documents: at.clone(),
                tombstones: at,
            }
        }
    }

    /// **The margin is the drain lag in feed rows, never in write
    /// generations.** Forty documents derived in one write and thirty of them
    /// recorded dead in another trail watermarks taken before both by the ten
    /// documents that stand and the thirty deaths — two generations, forty
    /// rows. Watermarks taken after both trail by nothing.
    #[test]
    fn the_margin_is_the_drain_lag_in_feed_rows() {
        let lagging = Lagging::new("search-margin-rows", 40, 30);
        let snapshot = lagging.snapshot();
        let now = snapshot.reading().write_generation();
        assert_eq!(now, lagging.before + 2, "the writes took other generations");
        assert_eq!(margin(&snapshot, &lagging.marks(lagging.before)), Ok(40));
        assert_eq!(margin(&snapshot, &lagging.marks(now)), Ok(0));
    }

    /// **The margin is capped, and a lag nothing can read is the cap.** More
    /// feed rows past the watermarks than the cap make a margin of the cap; a
    /// feed that has completed no drain, or whose watermark was taken in
    /// another store lifetime, trails by a count nothing reads, and the margin
    /// is the cap.
    #[test]
    fn the_margin_is_capped_and_an_unreadable_lag_is_the_cap() {
        let cap = VECTOR_MARGIN_CAP as usize;
        let lagging = Lagging::new("search-margin-cap", cap + 8, 0);
        let snapshot = lagging.snapshot();
        assert_eq!(margin(&snapshot, &lagging.marks(lagging.before)), Ok(cap));

        let now = snapshot.reading().write_generation();
        let mut undrained = lagging.marks(now);
        undrained.tombstones = None;
        assert_eq!(margin(&snapshot, &undrained), Ok(cap));
        let mut elsewhere = lagging.marks(now);
        elsewhere.documents = Some(Watermark {
            store_epoch: "another lifetime".to_string(),
            generation: now,
        });
        assert_eq!(margin(&snapshot, &elsewhere), Ok(cap));
    }

    /// **A watermark past the snapshot is a lag nothing can read.** A drain
    /// that finished after the hold's snapshot was taken leaves a watermark
    /// past that snapshot's write generation; the rows it drained past the
    /// snapshot are not counted on it, so the margin is the cap, for either
    /// feed.
    #[test]
    fn a_watermark_past_the_snapshot_is_the_cap() {
        let cap = VECTOR_MARGIN_CAP as usize;
        let lagging = Lagging::new("search-margin-ahead", 40, 30);
        let snapshot = lagging.snapshot();
        let now = snapshot.reading().write_generation();
        assert_eq!(margin(&snapshot, &lagging.marks(now + 1)), Ok(cap));
        let mut documents_ahead = lagging.marks(now);
        documents_ahead.documents = lagging.marks(now + 1).documents;
        assert_eq!(margin(&snapshot, &documents_ahead), Ok(cap));
        let mut tombstones_ahead = lagging.marks(now);
        tombstones_ahead.tombstones = lagging.marks(now + 1).tombstones;
        assert_eq!(margin(&snapshot, &tombstones_ahead), Ok(cap));
    }

    fn paths(ranked: &[Ranked]) -> Vec<&str> {
        ranked.iter().map(|hit| hit.path.as_str()).collect()
    }

    fn named(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| path.to_string()).collect()
    }

    /// **Reciprocal rank, summed over the rungs that ranked a document, with
    /// the path breaking a tie.** A document both rungs ranked outranks one
    /// either ranked alone; two documents each ranked first by one rung score
    /// alike and stand in path order.
    #[test]
    fn a_fused_score_is_the_reciprocal_rank_summed_over_the_rungs() {
        let ranked = fused(&[named(&["b.md", "both.md"]), named(&["a.md", "both.md"])]);
        assert_eq!(paths(&ranked), ["both.md", "a.md", "b.md"]);
        let k = f64::from(RRF_K);
        assert_eq!(ranked[0].score, 1.0 / (k + 2.0) + 1.0 / (k + 2.0));
        assert_eq!(ranked[1].score, 1.0 / (k + 1.0));
        assert_eq!(ranked[1].score, ranked[2].score);
    }

    /// **A page of a ranking is cut after its cursor's position and under its
    /// floor**, and drained a page at a time it is the ranking whole, each
    /// document once.
    #[test]
    fn a_ranking_drained_a_page_at_a_time_is_the_ranking_whole() {
        let ranked = fused(&[
            named(&["a.md", "b.md", "c.md", "d.md"]),
            named(&["d.md", "e.md", "a.md"]),
        ]);
        let mut drained = Vec::new();
        let mut resume: Option<(f64, String)> = None;
        loop {
            let (page, more) = page_of(
                ranked.clone(),
                None,
                resume.as_ref().map(|(score, path)| (*score, path.as_str())),
                2,
            );
            let last = page.last().cloned();
            drained.extend(page);
            match (more, last) {
                (true, Some(last)) => resume = Some((last.score, last.path)),
                _ => break,
            }
        }
        assert_eq!(drained, ranked);

        let floor = Score::new(ranked[1].score).expect("a score");
        let (floored, more) = page_of(ranked.clone(), Some(floor), None, 10);
        assert_eq!(floored, ranked[..2]);
        assert!(!more);
    }

    fn unavailable() -> VectorRefusal {
        VectorRefusal::of(SemanticRefusal::SelfDisabled {
            detail: "the sidecar did not open".to_string(),
        })
    }

    fn not_enabled() -> VectorRefusal {
        VectorRefusal::of(SemanticRefusal::NoEngine {
            section: Some(EngineSection::absent()),
        })
    }

    fn failed() -> VectorRefusal {
        VectorRefusal::of(SemanticRefusal::Failed {
            detail: "the answer failed".to_string(),
        })
    }

    fn runs(selection: &RungSelection) -> WithRuntime<'_> {
        with_runtime(selection).expect("a selection every rung of which has a runtime")
    }

    fn lexical_without_vector() -> RungSelection {
        RungSelection::enabled_without([Rung::Vector]).expect("a selection")
    }

    fn vector_alone() -> RungSelection {
        RungSelection::exactly(RungSet::of([Rung::Vector]).expect("a set"))
    }

    fn vector_without_lexical() -> RungSelection {
        RungSelection::enabled_without([Rung::Lexical]).expect("a selection")
    }

    /// **The enabled selection resolves by the vector rung's one sample**: a
    /// rung the enabled set does not hold is left out silently, an enabled rung
    /// no engine stands for is left out and advised as skipped with its reason,
    /// and an answer that failed refuses. A rung the request subtracted is not
    /// sampled at all.
    #[test]
    fn the_enabled_selection_resolves_by_the_one_sample() {
        let lexical = Resolved {
            ladder: RungSet::lexical(),
            skipped: None,
        };
        assert_eq!(
            resolve(
                runs(&RungSelection::enabled()),
                &VectorSample::Refused(not_enabled())
            ),
            Ok(lexical)
        );
        assert!(!samples_vector(runs(&lexical_without_vector())));
        assert_eq!(
            resolve(runs(&lexical_without_vector()), &VectorSample::Unsampled)
                .map(|resolved| resolved.ladder),
            Ok(RungSet::lexical())
        );
        assert_eq!(
            resolve(
                runs(&RungSelection::enabled()),
                &VectorSample::Refused(unavailable())
            ),
            Ok(Resolved {
                ladder: RungSet::lexical(),
                skipped: Some(AnswerAdvisory::rung_skipped(
                    Rung::Vector,
                    RungSkipReason::unavailable("the sidecar did not open")
                )),
            })
        );
        assert_eq!(
            resolve(
                runs(&RungSelection::enabled()),
                &VectorSample::Refused(failed())
            )
            .map_err(|envelope| envelope.code().clone()),
            Err(ReasonCode::EngineFailed)
        );
    }

    /// **A resolution left holding no retrieval rung is refused with the
    /// vector rung's own code**, never answered empty: the lexical floor
    /// subtracted and the vector rung not enabled refuses `engine/not-enabled`,
    /// and not available refuses `engine/unavailable`.
    #[test]
    fn a_resolution_holding_no_retrieval_rung_is_refused_with_that_rungs_code() {
        for (sample, code) in [
            (not_enabled(), ReasonCode::EngineNotEnabled),
            (unavailable(), ReasonCode::EngineUnavailable),
            (failed(), ReasonCode::EngineFailed),
        ] {
            assert_eq!(
                resolve(
                    runs(&vector_without_lexical()),
                    &VectorSample::Refused(sample)
                )
                .map_err(|envelope| envelope.code().clone()),
                Err(code)
            );
        }
    }

    /// **An exact set is refused by any rung it names that cannot answer**:
    /// the vector rung by its sample's composition, and a rung no runtime
    /// answers as not enabled before any engine is sampled, so no selection
    /// naming one reaches resolution.
    #[test]
    fn an_exact_set_is_refused_by_a_rung_that_cannot_answer() {
        for (sample, code) in [
            (not_enabled(), ReasonCode::EngineNotEnabled),
            (unavailable(), ReasonCode::EngineUnavailable),
            (failed(), ReasonCode::EngineFailed),
            (VectorRefusal::not_composed(), ReasonCode::EngineNotEnabled),
        ] {
            assert_eq!(
                resolve(runs(&vector_alone()), &VectorSample::Refused(sample))
                    .map_err(|envelope| envelope.code().clone()),
                Err(code)
            );
        }
        for rung in [Rung::Expansion, Rung::Rerank] {
            let selection = RungSelection::exactly(
                RungSet::of([Rung::Lexical, rung]).expect("a set holding the floor"),
            );
            let refused = with_runtime(&selection).expect_err("a refusal");
            assert_eq!(refused.code(), &ReasonCode::EngineNotEnabled);
            assert_eq!(
                refused.detail(),
                &ErrorDetail::engine_not_enabled(
                    rung,
                    "no runtime for this rung ships in this build, so no vault enables it"
                )
            );
        }
        assert!(with_runtime(&vector_alone()).is_ok());
    }
}
