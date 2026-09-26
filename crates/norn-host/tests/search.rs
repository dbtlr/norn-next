//! **`search` through the host, end to end**: a real vault, a real attachment,
//! the semantic engine the host composes over it, and the ladder a request
//! selects resolved against what the host holds when it answers.
//!
//! The vault is a handful of documents written for the case, because the
//! subject is which rungs answer and how their rankings fuse, and a ranking a
//! reader can check by hand needs a corpus small enough to read.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own hand-written tree.

mod attach;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use norn_host::{
    Answered, DemandLease, ProductionEntryOps, RRF_K, SearchCost, SemanticEngines, SemanticStatus,
};
use norn_store::LexicalQuery;
use norn_testkit::process::Sandbox;
use norn_testkit::wait::{Budget, Observed, wait_until};
use norn_wire::{
    AnswerAdvisory, CursorKey, CursorOrderChanged, ErrorDetail, ErrorEnvelope, Freshness,
    LadderDeclaration, ModelIdentity, Moved, Predicate, ReasonCode, Rung, RungReport,
    RungSelection, RungSet, RungSkipReason, SearchParams, SearchReport, TrustState, VaultAddress,
};

/// The documents every case's vault holds, by path.
const DOCUMENTS: &[(&str, &str)] = &[
    ("docs/alpha.md", "alpha alpha alpha\n"),
    ("docs/mixed.md", "alpha bravo\n"),
    ("docs/bravo.md", "bravo bravo\n"),
    ("docs/charlie.md", "charlie alpha charlie charlie\n"),
    ("docs/lone.md", "lone words here\n"),
    ("keep/kept.md", "kept alpha words\n"),
    ("keep/other.md", "bravo kept\n"),
];

/// A sandbox and a vault holding [`DOCUMENTS`], its engine configured by
/// `config` where it names one.
fn a_vault(label: &str, config: Option<&str>) -> (Sandbox, attach::Vault) {
    a_vault_holding(label, DOCUMENTS, config)
}

/// A sandbox and a vault holding `documents`, by path, its engine configured
/// by `config` where it names one.
fn a_vault_holding<P: AsRef<str>, B: AsRef<str>>(
    label: &str,
    documents: &[(P, B)],
    config: Option<&str>,
) -> (Sandbox, attach::Vault) {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let root = sandbox.work_dir().join("attached");
    let vault = root.join("vault");
    for (path, body) in documents {
        let (path, body) = (path.as_ref(), body.as_ref());
        let at = vault.join(path);
        std::fs::create_dir_all(at.parent().expect("a parent")).expect("a document directory");
        std::fs::write(at, body).expect("a document");
    }
    std::fs::create_dir_all(vault.join(".norn")).expect("the control directory");
    std::fs::write(vault.join(".norn/schema.yaml"), attach::SCHEMA).expect("the vault schema");
    if let Some(config) = config {
        std::fs::write(vault.join(".norn/config.toml"), config).expect("the vault config");
    }
    (sandbox, attach::Vault::adopt(&root))
}

/// A host composing the semantic engine over `vault`, attached and ready.
struct Serving {
    host: attach::ServingHost,
    engines: Arc<SemanticEngines>,
    _lease: DemandLease<ProductionEntryOps>,
}

fn serve(vault: &attach::Vault) -> Serving {
    let (host, engines) = vault.host_with_semantic();
    let lease = attach::attach_and_wait(&host, vault.name());
    Serving {
        host,
        engines,
        _lease: lease,
    }
}

fn searching(vault: &attach::Vault, query: &str) -> SearchParams {
    SearchParams::new(VaultAddress::name(vault.name().clone()), query)
}

fn exactly(rungs: impl IntoIterator<Item = Rung>) -> RungSelection {
    RungSelection::exactly(RungSet::of(rungs).expect("a ladder"))
}

fn answered(serving: &Serving, params: &SearchParams) -> Answered<SearchReport, SearchCost> {
    serving
        .host
        .search(params)
        .unwrap_or_else(|refusal| panic!("a search of {params:?}: {refusal:?}"))
}

fn refused(serving: &Serving, params: &SearchParams) -> ErrorEnvelope {
    serving.host.search(params).expect_err("a refused search")
}

fn paths(answer: &Answered<SearchReport, SearchCost>) -> Vec<String> {
    answer
        .answer
        .report
        .page
        .rows
        .iter()
        .map(|hit| hit.path.as_str().to_string())
        .collect()
}

fn skipped(answer: &Answered<SearchReport, SearchCost>) -> Vec<&AnswerAdvisory> {
    answer
        .answer
        .advisories
        .iter()
        .filter(|advisory| matches!(advisory, AnswerAdvisory::RungSkipped { .. }))
        .collect()
}

/// **The lexical floor alone is the store's page, declared `[lexical]`**, and a
/// vault with no engine section enables no vector rung: a bare search answers
/// the floor with nothing skipped, because nothing enabled was left out. An
/// exact set naming vectors is refused as not enabled, told what to turn on,
/// and one naming re-ranking is refused as not enabled, because no runtime
/// answers it.
#[test]
fn a_bare_search_on_a_vault_without_an_engine_is_the_lexical_page() {
    let (_sandbox, vault) = a_vault("search-lexical", None);
    let serving = serve(&vault);

    let bare = answered(&serving, &searching(&vault, "alpha").with_limit(2));
    assert_eq!(bare.answer.report.ladder, LadderDeclaration::lexical());
    assert!(skipped(&bare).is_empty(), "{:?}", bare.answer.advisories);
    assert_eq!(bare.work.vector, None);
    assert_eq!(bare.work.candidate_pages, 0);

    let mut store = vault.store();
    let declared = attach::read::the_pinned_declaration(&mut store);
    let reader = Arc::new(store.open_reader().reader.expect("a reader"));
    let snapshot = reader
        .try_take()
        .expect("a handle nothing reads")
        .establish()
        .expect("a snapshot");
    let page = snapshot
        .search(&LexicalQuery::new("alpha").with_limit(2), &declared)
        .expect("the store's page");
    assert_eq!(bare.answer.report.page.rows, page.hits);
    assert_eq!(bare.answer.report.page.next, page.next);
    drop(snapshot);

    let not_enabled = refused(
        &serving,
        &searching(&vault, "alpha").with_rungs(exactly([Rung::Vector])),
    );
    assert_eq!(
        not_enabled.detail(),
        &ErrorDetail::engine_not_enabled(
            Rung::Vector,
            "enable the engine section in .norn/config.toml and run vault reload"
        )
    );
    let rerank = refused(
        &serving,
        &searching(&vault, "alpha").with_rungs(exactly([Rung::Lexical, Rung::Rerank])),
    );
    assert_eq!(rerank.code(), &ReasonCode::EngineNotEnabled);
    assert!(matches!(
        rerank.detail(),
        ErrorDetail::EngineNotEnabled {
            rung: Rung::Rerank,
            ..
        }
    ));
    let without_lexical = refused(
        &serving,
        &searching(&vault, "alpha")
            .with_rungs(RungSelection::enabled_without([Rung::Lexical]).expect("a selection")),
    );
    assert_eq!(without_lexical.code(), &ReasonCode::EngineNotEnabled);
}

/// **An engine that took itself out of service is skipped by the enabled
/// selection and refuses an exact one**, whichever way it stood down: a
/// section that could not be read, carrying that section's own error, and an
/// enabled section whose sidecar could not open, carrying the engine's
/// retained diagnostic. The bare search answers the lexical floor advised that
/// the vector rung was skipped and why; an exact set naming vectors, and the
/// enabled set less the floor, are refused `engine/unavailable`.
#[test]
fn a_self_disabled_engine_is_skipped_or_refuses() {
    let (_malformed_sandbox, malformed) = a_vault(
        "search-self-disabled-section",
        Some("[engine.semantic]\nenabled = \"yes\"\n"),
    );
    let (_unopened_sandbox, unopened) =
        a_vault("search-self-disabled-sidecar", Some("[engine.semantic]\n"));
    // A directory where the sidecar file belongs: the engine cannot open it.
    std::fs::create_dir_all(unopened.sidecar()).expect("a directory at the sidecar's path");

    for (vault, names) in [(&malformed, "boolean"), (&unopened, "sidecar did not open")] {
        let serving = serve(vault);
        let SemanticStatus::SelfDisabled { detail } = serving.engines.status(vault.name()) else {
            panic!(
                "the engine is not self-disabled: {:?}",
                serving.engines.status(vault.name())
            );
        };
        assert!(detail.contains(names), "{detail}");

        let bare = answered(&serving, &searching(vault, "alpha"));
        assert_eq!(bare.answer.report.ladder, LadderDeclaration::lexical());
        assert_eq!(
            skipped(&bare),
            [&AnswerAdvisory::rung_skipped(
                Rung::Vector,
                RungSkipReason::unavailable(detail.clone())
            )]
        );
        assert_eq!(
            bare.work.candidate_pages, 0,
            "a stood-down engine paid for candidates"
        );

        for selection in [
            exactly([Rung::Vector]),
            RungSelection::enabled_without([Rung::Lexical]).expect("a selection"),
        ] {
            let refusal = refused(&serving, &searching(vault, "alpha").with_rungs(selection));
            assert_eq!(
                refusal.detail(),
                &ErrorDetail::engine_unavailable(Rung::Vector, detail.clone())
            );
        }
    }
}

/// The paths and scores of every hit `params` ranks, drained a page at a
/// time, and the cursors the pages minted.
fn drained(serving: &Serving, params: &SearchParams, limit: u32) -> Vec<(String, f64)> {
    let mut hits = Vec::new();
    let mut request = params.clone().with_limit(limit);
    loop {
        let page = answered(serving, &request);
        assert!(page.answer.report.page.moved.is_empty());
        hits.extend(
            page.answer
                .report
                .page
                .rows
                .iter()
                .map(|hit| (hit.path.as_str().to_string(), hit.score.get())),
        );
        match page.answer.report.page.next.clone() {
            Some(next) => request = request.with_after(next),
            None => break,
        }
    }
    hits
}

/// **A hybrid search fuses both rungs by reciprocal rank, the path breaking a
/// tie, and declares the ladder that ran**: the lexical floor and the vector
/// rung, naming the model that derived the vectors and trailing the store by
/// nothing once the attach drained, and not repeatable. The expected order is
/// computed here from each rung's own ranking, answered through the same host
/// under an exact set.
#[test]
fn a_hybrid_search_fuses_both_rungs_by_reciprocal_rank() {
    let (_sandbox, vault) = a_vault("search-hybrid", Some("[engine.semantic]\n"));
    let serving = serve(&vault);

    let lexical = paths(&answered(
        &serving,
        &searching(&vault, "alpha").with_rungs(exactly([Rung::Lexical])),
    ));
    let vector_answer = answered(
        &serving,
        &searching(&vault, "alpha").with_rungs(exactly([Rung::Vector])),
    );
    let vector = paths(&vector_answer);
    assert_eq!(
        vector.len(),
        DOCUMENTS.len(),
        "the vector rung scores every document"
    );
    assert!(
        !lexical.is_empty() && lexical.len() < vector.len(),
        "the corpus does not tell the rungs apart: {lexical:?} {vector:?}"
    );

    let mut expected: BTreeMap<String, f64> = BTreeMap::new();
    for ranking in [&lexical, &vector] {
        for (at, path) in ranking.iter().enumerate() {
            *expected.entry(path.clone()).or_insert(0.0) +=
                1.0 / (f64::from(RRF_K) + at as f64 + 1.0);
        }
    }
    let mut expected: Vec<(String, f64)> = expected.into_iter().collect();
    expected.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let hybrid = answered(&serving, &searching(&vault, "alpha"));
    assert_eq!(
        hybrid
            .answer
            .report
            .page
            .rows
            .iter()
            .map(|hit| (hit.path.as_str().to_string(), hit.score.get()))
            .collect::<Vec<_>>(),
        expected
    );
    let model = norn_embed::Embedder::model(&norn_embed::StubEmbedder::new()).clone();
    assert_eq!(
        hybrid.answer.report.ladder,
        LadderDeclaration::new(
            vec![
                RungReport::lexical(),
                RungReport::vector(
                    ModelIdentity::new(model.id(), model.version()),
                    Freshness::trailing(0)
                ),
            ],
            false
        )
        .expect("a ladder")
    );
    assert!(skipped(&hybrid).is_empty());
    let vector_work = hybrid.work.vector.expect("the vector rung ran");
    assert!(vector_work.peak_held <= u64::from(norn_wire::RUNG_DEPTH));
    assert_eq!(
        vector_answer.answer.report.ladder.rungs().len(),
        1,
        "an exact vector set declared another ladder"
    );
}

/// **The vector rung scores only the documents the conjunction admits.** Over
/// the whole vault the document that is the query's word repeated answers
/// first; under a path part keeping one folder, no document outside it is
/// answered, and the engine scored only the folder's documents and held at
/// most the rung's depth of them. Its scan still read every row of the
/// model: the rung is answered by the engine's scan, not by a seek of what
/// the part admits.
#[test]
fn a_predicate_narrows_the_vector_rung_to_the_documents_it_admits() {
    let (_sandbox, vault) = a_vault("search-restricted", Some("[engine.semantic]\n"));
    let serving = serve(&vault);
    let vector = searching(&vault, "alpha").with_rungs(exactly([Rung::Vector]));

    let open = answered(&serving, &vector);
    assert_eq!(
        paths(&open).first().map(String::as_str),
        Some("docs/alpha.md")
    );

    let narrowed = answered(
        &serving,
        &vector.clone().with_predicates([Predicate::path("keep/**")]),
    );
    assert_eq!(paths(&narrowed), ["keep/kept.md", "keep/other.md"]);
    let work = narrowed.work.vector.expect("the vector rung ran");
    assert_eq!(work.rows_scored, 2);
    assert!(work.peak_held <= u64::from(norn_wire::RUNG_DEPTH));
    assert_eq!(work.rows_read, DOCUMENTS.len() as u64);
}

/// **A narrowed vector rung holds its depth of scored rows when the
/// conjunction admits more than the depth.** Under a path part keeping one
/// folder that holds more documents than the rung's depth, the engine scores
/// every document the folder holds and none outside it, holds exactly the
/// depth of them, and its scan reads every row of the model.
#[test]
fn a_narrowed_vector_rung_holds_its_depth_of_what_it_admits() {
    let depth = norn_wire::RUNG_DEPTH as usize;
    let kept = depth + 16;
    let dropped = 16;
    let documents: Vec<(String, String)> = (0..kept)
        .map(|at| (format!("keep/{at:05}.md"), format!("alpha w{at}\n")))
        .chain((0..dropped).map(|at| (format!("drop/{at:05}.md"), format!("alpha w{at}\n"))))
        .collect();
    let (_sandbox, vault) = a_vault_holding(
        "search-narrowed-depth",
        &documents,
        Some("[engine.semantic]\n"),
    );
    let serving = serve(&vault);

    let narrowed = answered(
        &serving,
        &searching(&vault, "alpha")
            .with_rungs(exactly([Rung::Vector]))
            .with_limit(norn_wire::RUNG_DEPTH)
            .with_predicates([Predicate::path("keep/**")]),
    );
    let delivered = paths(&narrowed);
    assert_eq!(delivered.len(), depth);
    assert!(
        delivered.iter().all(|path| path.starts_with("keep/")),
        "the rung answered a document the part does not admit"
    );
    assert_eq!(narrowed.work.candidates, kept as u64);
    let work = narrowed.work.vector.expect("the vector rung ran");
    assert_eq!(work.rows_scored, kept as u64);
    assert_eq!(work.peak_held, depth as u64);
    assert_eq!(work.rows_read, (kept + dropped) as u64);
}

/// The sidecar state `serving`'s engine answers from.
fn sidecar_state(serving: &Serving, vault: &attach::Vault) -> norn_semantic::SidecarRevision {
    match serving.engines.status(vault.name()) {
        SemanticStatus::On { sidecar, .. } => sidecar,
        other => panic!("the engine is not on: {other:?}"),
    }
}

/// **A fused continuation pages the ranking exactly, reports a sidecar that
/// moved, and refuses under another ladder.** On a vault nothing writes to,
/// the ranking drained two hits at a time is the ranking one page answers,
/// each hit once. After a document lands and the engine drains it, the cursor
/// minted before continues and reports that the sidecar moved. The same cursor
/// continued under the lexical floor alone, or the vector rung alone, is
/// refused, naming both ladders.
#[test]
fn a_fused_continuation_pages_exactly_and_is_judged_by_its_ladder() {
    let (_sandbox, vault) = a_vault("search-continuation", Some("[engine.semantic]\n"));
    let serving = serve(&vault);
    let hybrid = searching(&vault, "alpha");

    let whole: Vec<(String, f64)> = answered(&serving, &hybrid)
        .answer
        .report
        .page
        .rows
        .iter()
        .map(|hit| (hit.path.as_str().to_string(), hit.score.get()))
        .collect();
    assert_eq!(drained(&serving, &hybrid, 2), whole);

    let first = answered(&serving, &hybrid.clone().with_limit(2));
    let cursor = first.answer.report.page.next.clone().expect("a next page");
    let fused = RungSet::of([Rung::Lexical, Rung::Vector]).expect("a ladder");
    let CursorKey::Hit { ladder, .. } = cursor.key() else {
        panic!("a search minted a cursor that is no hit's: {cursor:?}");
    };
    assert_eq!(ladder, &fused);
    assert!(cursor.snapshot().sidecar_revision.is_some());

    for rungs in [[Rung::Lexical], [Rung::Vector]] {
        let other = RungSet::of(rungs).expect("a ladder");
        let refusal = refused(
            &serving,
            &hybrid
                .clone()
                .with_rungs(RungSelection::exactly(other.clone()))
                .with_after(cursor.clone()),
        );
        assert_eq!(
            refusal.detail(),
            &ErrorDetail::cursor_order_changed(
                CursorOrderChanged::minted_raw(None).in_ladders(fused.clone(), other)
            )
        );
    }

    let before = sidecar_state(&serving, &vault);
    std::fs::write(vault.path().join("docs/late.md"), "alpha arrives late\n")
        .expect("a document landing");
    // The leg that derives the document drains the engine and publishes the
    // entry ready again, and a search answers only once both have happened.
    wait_until(
        "the engine to drain the document that landed, and the entry to serve",
        Budget::new(attach::READY_LIMIT, attach::STATE_PROBE),
        || {
            let now = sidecar_state(&serving, &vault);
            let state = serving.host.state(vault.name());
            if now != before && matches!(state, Ok(TrustState::Ready)) {
                Observed::Met(())
            } else {
                Observed::pending(format!(
                    "the sidecar stands at {now:?}, the entry at {state:?}"
                ))
            }
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
    let continued = answered(&serving, &hybrid.clone().with_limit(2).with_after(cursor));
    assert!(
        continued
            .answer
            .report
            .page
            .moved
            .contains(&Moved::SidecarRevision),
        "{:?}",
        continued.answer.report.page.moved
    );
}

/// **An engine that stands and fails its answer refuses `engine/failed`**,
/// under an exact set and under the enabled one alike: a failure is not an
/// engine missing, so it is never skipped. The sidecar's rows are damaged
/// under the running engine, which is what its scan then meets.
#[test]
fn an_engine_that_fails_its_answer_refuses_engine_failed() {
    let (_sandbox, vault) = a_vault("search-failed", Some("[engine.semantic]\n"));
    let serving = serve(&vault);
    assert!(matches!(
        serving.engines.status(vault.name()),
        SemanticStatus::On { .. }
    ));
    on_the_sidecar(&vault, |sidecar| {
        sidecar.execute_batch("UPDATE document_vectors SET embedding = x'00'")
    });

    for selection in [exactly([Rung::Vector]), RungSelection::enabled()] {
        let refusal = refused(&serving, &searching(&vault, "alpha").with_rungs(selection));
        assert_eq!(refusal.code(), &ReasonCode::EngineFailed, "{refusal:?}");
    }
    let lexical = answered(
        &serving,
        &searching(&vault, "alpha").with_rungs(exactly([Rung::Lexical])),
    );
    assert!(
        !paths(&lexical).is_empty(),
        "the lexical floor stands beside it"
    );
}

/// Run `statements` on `vault`'s sidecar under the running engine, and what
/// they answered.
fn on_the_sidecar<T>(
    vault: &attach::Vault,
    statements: impl FnOnce(&norn_db::rusqlite::Connection) -> Result<T, norn_db::rusqlite::Error>,
) -> T {
    match norn_db::connect(&vault.sidecar()).expect("connecting to the sidecar") {
        norn_db::Attempt::Connected(connection) => {
            statements(&connection).expect("the statements on the sidecar")
        }
        norn_db::Attempt::Unreadable { detail } => panic!("the sidecar is unreadable: {detail}"),
    }
}

/// **A non-finite score refuses `engine/failed` wherever the scan ranks it, at
/// any vault size.** Over a vault larger than the rows the vector rung's scan
/// holds, one row's embedding is overwritten with a positive and a negative
/// NaN, then a positive and a negative infinity. A negative score ranks below
/// every finite one, so the scan pushes it out of what it holds before any
/// answer is ranked; every ladder holding the vector rung, filtered or not,
/// is refused all the same.
#[test]
fn a_non_finite_score_the_scan_pushes_out_still_refuses_engine_failed() {
    let depth = norn_wire::RUNG_DEPTH as usize;
    let documents = depth + 16;
    let (_sandbox, vault) = a_vault_of_alphas("search-non-finite-pushed-out", documents);
    let serving = serve(&vault);
    let drained = answered(
        &serving,
        &searching(&vault, "alpha").with_rungs(exactly([Rung::Vector])),
    );
    assert_eq!(drained.work.margin, 0, "the engine trails the snapshot");
    let dimensions: i64 = on_the_sidecar(&vault, |sidecar| {
        sidecar.query_row(
            "SELECT dimensions FROM document_vectors WHERE path = 'd/00000.md'",
            [],
            |row| row.get(0),
        )
    });
    // Little-endian f32 words: +NaN, -NaN, +infinity, -infinity.
    for word in ["0000c07f", "0000c0ff", "0000807f", "000080ff"] {
        let embedding = word.repeat(usize::try_from(dimensions).expect("a dimension count"));
        on_the_sidecar(&vault, |sidecar| {
            sidecar.execute_batch(&format!(
                "UPDATE document_vectors SET embedding = x'{embedding}' WHERE path = 'd/00000.md'"
            ))
        });
        for selection in [exactly([Rung::Vector]), RungSelection::enabled()] {
            let request = searching(&vault, "alpha").with_rungs(selection.clone());
            for request in [request.clone(), request.with_predicates([every_document()])] {
                let refusal = refused(&serving, &request);
                assert_eq!(
                    refusal.code(),
                    &ReasonCode::EngineFailed,
                    "{word} under {request:?}: {refusal:?}"
                );
            }
        }
    }
}

/// The rungs `answer` is advised reached their depth, in the order advised.
fn depth_reached(answer: &Answered<SearchReport, SearchCost>) -> Vec<Rung> {
    answer
        .answer
        .advisories
        .iter()
        .filter_map(|advisory| match advisory {
            AnswerAdvisory::RungDepthReached { rung, .. } => Some(*rung),
            _ => None,
        })
        .collect()
}

/// A vault of `documents` documents, each holding the word `alpha`, so every
/// one is a lexical hit and every one is admitted.
fn a_vault_of_alphas(label: &str, documents: usize) -> (Sandbox, attach::Vault) {
    let documents: Vec<(String, String)> = (0..documents)
        .map(|at| (format!("d/{at:05}.md"), format!("alpha w{at}\n")))
        .collect();
    a_vault_holding(label, &documents, Some("[engine.semantic]\n"))
}

/// **A rung is advised that it reached its depth only when it had more than
/// `RUNG_DEPTH` candidates, and the vector rung holds no more than that.** At
/// exactly the depth, neither the fused ladder nor the vector rung alone is
/// advised; one document past it, the fused answer is advised for both rungs
/// in ladder order and the vector rung alone for itself, and the vector
/// rung's scan held the depth's rows while scoring every admitted document.
#[test]
fn a_rung_is_advised_at_its_depth_only_past_it() {
    let depth = norn_wire::RUNG_DEPTH as usize;
    for (documents, fused_advised, vector_advised) in [
        (depth, vec![], vec![]),
        (
            depth + 1,
            vec![Rung::Lexical, Rung::Vector],
            vec![Rung::Vector],
        ),
    ] {
        let (_sandbox, vault) = a_vault_of_alphas(&format!("search-depth-{documents}"), documents);
        let serving = serve(&vault);
        let fused = answered(&serving, &searching(&vault, "alpha").with_limit(1));
        let vector = answered(
            &serving,
            &searching(&vault, "alpha")
                .with_rungs(exactly([Rung::Vector]))
                .with_limit(1),
        );
        assert_eq!(
            depth_reached(&fused),
            fused_advised,
            "{documents} documents"
        );
        assert_eq!(
            depth_reached(&vector),
            vector_advised,
            "{documents} documents"
        );
        for answer in [&fused, &vector] {
            let work = answer.work.vector.expect("the vector rung ran");
            assert_eq!(work.rows_scored, documents as u64);
            assert_eq!(work.peak_held, depth as u64, "{documents} documents");
        }
    }
}

/// The paths and scores of `answer`'s page, in page order.
fn scored(answer: &Answered<SearchReport, SearchCost>) -> Vec<(String, f64)> {
    answer
        .answer
        .report
        .page
        .rows
        .iter()
        .map(|hit| (hit.path.as_str().to_string(), hit.score.get()))
        .collect()
}

/// **The request's floor applies to the scale the answer is ranked on.** A
/// floor taken from the third fused score keeps exactly the fused hits at or
/// above it; a floor taken from the vector rung's second score, under the
/// vector rung alone, keeps exactly the hits the engine scored at or above
/// it. Each floor leaves hits out, so it is seen to apply.
#[test]
fn the_floor_applies_to_the_answers_own_scale() {
    let (_sandbox, vault) = a_vault("search-floor", Some("[engine.semantic]\n"));
    let serving = serve(&vault);
    for (selection, at) in [(RungSelection::enabled(), 2), (exactly([Rung::Vector]), 1)] {
        let request = searching(&vault, "alpha").with_rungs(selection.clone());
        let whole = scored(&answered(&serving, &request));
        let floor = whole[at].1;
        let mut floored = request.clone();
        floored.min_score = Some(norn_wire::Score::new(floor).expect("a score"));
        let kept = scored(&answered(&serving, &floored));
        let expected: Vec<(String, f64)> = whole
            .iter()
            .filter(|(_, score)| *score >= floor)
            .cloned()
            .collect();
        assert_eq!(kept, expected, "{selection:?}");
        assert!(
            kept.len() > at && kept.len() < whole.len(),
            "{selection:?}: {whole:?}"
        );
    }
}

/// **The candidates reconcile the sidecar with the snapshot.** A vector row
/// for a path the snapshot does not hold — the sidecar ahead of it — is never
/// scored or answered, alone or fused; a document whose row the sidecar no
/// longer holds — the sidecar behind — is answered on the fused ladder by the
/// lexical rung alone. The fused scores are the reciprocal-rank sum of each
/// rung's own answer, so a row the vector rung scored and no answer shows
/// would move them.
#[test]
fn a_sidecar_row_the_snapshot_lacks_is_never_answered() {
    let (_sandbox, vault) = a_vault("search-reconciled", Some("[engine.semantic]\n"));
    let serving = serve(&vault);
    on_the_sidecar(&vault, |sidecar| {
        sidecar.execute_batch(
            "INSERT INTO document_vectors
               SELECT 'docs/ghost.md', model_id, model_version, input_hash, dimensions, embedding
               FROM document_vectors WHERE path = 'docs/alpha.md';
             DELETE FROM document_vectors WHERE path = 'docs/mixed.md';",
        )
    });

    let vector_request = searching(&vault, "alpha").with_rungs(exactly([Rung::Vector]));
    let vector_answer = answered(&serving, &vector_request);
    let vector = paths(&vector_answer);
    assert!(
        !vector.iter().any(|path| path == "docs/ghost.md"),
        "{vector:?}"
    );
    assert!(
        !vector.iter().any(|path| path == "docs/mixed.md"),
        "{vector:?}"
    );
    // Unfiltered, the scan scores every row the sidecar holds, the ghost
    // among them, and the snapshot check drops it.
    let work = vector_answer.work.vector.expect("the vector rung ran");
    assert_eq!(work.rows_read, DOCUMENTS.len() as u64);
    assert_eq!(work.rows_scored, DOCUMENTS.len() as u64);
    assert_eq!(vector_answer.work.candidate_pages, 0);
    assert_eq!(vector_answer.work.paths_checked, DOCUMENTS.len() as u64);
    // Filtered, the ghost is never scored at all, and the answer is the same.
    let filtered = answered(
        &serving,
        &vector_request.clone().with_predicates([every_document()]),
    );
    assert_eq!(scored(&filtered), scored(&vector_answer));
    let work = filtered.work.vector.expect("the vector rung ran");
    assert_eq!(work.rows_scored, DOCUMENTS.len() as u64 - 1);

    let lexical = paths(&answered(
        &serving,
        &searching(&vault, "alpha").with_rungs(exactly([Rung::Lexical])),
    ));
    let mixed_rank = lexical
        .iter()
        .position(|path| path == "docs/mixed.md")
        .expect("the lexical rung ranks the document the sidecar lost")
        + 1;
    let mut expected: BTreeMap<String, f64> = BTreeMap::new();
    for ranking in [&lexical, &vector] {
        for (at, path) in ranking.iter().enumerate() {
            *expected.entry(path.clone()).or_insert(0.0) +=
                1.0 / (f64::from(RRF_K) + at as f64 + 1.0);
        }
    }
    let mut expected: Vec<(String, f64)> = expected.into_iter().collect();
    expected.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let fused = scored(&answered(&serving, &searching(&vault, "alpha")));
    assert_eq!(fused, expected);
    assert_eq!(
        scored(&answered(
            &serving,
            &searching(&vault, "alpha").with_predicates([every_document()])
        )),
        expected
    );
    assert!(fused.contains(&(
        "docs/mixed.md".to_string(),
        1.0 / (f64::from(RRF_K) + mixed_rank as f64)
    )));
}

/// **A host that composes no engine answers no vector rung, over a vault
/// whose section enables one**: nothing was delivered to it, so the vault's
/// enabled set holds the lexical floor alone. A bare search answers the floor
/// with nothing skipped and pays for no candidates; an exact set naming
/// vectors is refused as not enabled, told to serve the vault from a host
/// that composes the engine.
#[test]
fn a_host_composing_no_engine_answers_no_vector_rung() {
    let (_sandbox, vault) = a_vault("search-not-composed", Some("[engine.semantic]\n"));
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());

    let bare = host
        .search(&searching(&vault, "alpha"))
        .expect("the lexical floor answers");
    assert_eq!(bare.answer.report.ladder, LadderDeclaration::lexical());
    assert!(skipped(&bare).is_empty(), "{:?}", bare.answer.advisories);
    assert_eq!(bare.work.candidate_pages, 0);

    let refusal = host
        .search(&searching(&vault, "alpha").with_rungs(exactly([Rung::Vector])))
        .expect_err("a refused search");
    assert_eq!(
        refusal.detail(),
        &ErrorDetail::engine_not_enabled(
            Rung::Vector,
            "serve the vault from a host that composes the semantic engine"
        )
    );
}

/// A part every document in these vaults satisfies: it admits them all, and
/// it makes the search a filtered one.
fn every_document() -> Predicate {
    Predicate::path("**")
}

/// **An unfiltered search takes no admitted-set pass, and its vector rung
/// holds at most its depth and its margin at any vault size.** Over a vault
/// just past the depth and one past twice it, the hybrid and the vector
/// ladders run no candidate page, the scan reads every row and holds at most
/// the depth, the margin being nothing on a vault the engine has drained, and
/// the answer — its hits, its cursor, its advisories — is the one the filtered
/// path gives under a part that admits every document, which does run its
/// candidate pages.
#[test]
fn an_unfiltered_search_takes_no_admitted_pass_and_answers_as_the_filtered_one() {
    let depth = u64::from(norn_wire::RUNG_DEPTH);
    for documents in [depth as usize + 16, 2 * depth as usize + 16] {
        let (_sandbox, vault) =
            a_vault_of_alphas(&format!("search-unfiltered-{documents}"), documents);
        let serving = serve(&vault);
        for selection in [RungSelection::enabled(), exactly([Rung::Vector])] {
            let request = searching(&vault, "alpha")
                .with_rungs(selection.clone())
                .with_limit(50);
            let unfiltered = answered(&serving, &request);
            assert_eq!(unfiltered.work.candidate_pages, 0, "{selection:?}");
            assert_eq!(unfiltered.work.candidates, 0, "{selection:?}");
            assert_eq!(unfiltered.work.margin, 0, "{selection:?}");
            let work = unfiltered.work.vector.expect("the vector rung ran");
            assert_eq!(work.rows_read, documents as u64);
            assert!(
                work.peak_held <= depth + unfiltered.work.margin,
                "{documents} documents under {selection:?}: {work:?}"
            );

            let filtered = answered(
                &serving,
                &request.clone().with_predicates([every_document()]),
            );
            assert!(
                filtered.work.candidate_pages > 0,
                "a filtered search ran no candidate page"
            );
            assert_eq!(filtered.work.candidates, documents as u64);
            assert_eq!(
                unfiltered.answer.report, filtered.answer.report,
                "{documents} documents under {selection:?}"
            );
            assert_eq!(unfiltered.answer.advisories, filtered.answer.advisories);
            assert_eq!(unfiltered.answer.unsatisfied, filtered.answer.unsatisfied);
        }
    }
}

/// How many documents `serving`'s store holds, as a count answers it.
fn documents_held(serving: &Serving, vault: &attach::Vault) -> Result<u64, String> {
    let counted = serving
        .host
        .count(&norn_wire::CountParams::new(VaultAddress::name(
            vault.name().clone(),
        )))
        .map_err(|refusal| format!("{refusal:?}"))?;
    Ok(counted
        .answer
        .report
        .rows
        .iter()
        .map(|tally| tally.count)
        .sum())
}

/// Hold `vault`'s sidecar from writing or retracting any row under the
/// running engine, so every drain that has work fails and records nothing.
fn hold_the_sidecar(vault: &attach::Vault) {
    on_the_sidecar(vault, |sidecar| {
        sidecar.execute_batch(
            "CREATE TRIGGER held_delete BEFORE DELETE ON document_vectors
             BEGIN SELECT RAISE(ABORT, 'the retraction is held'); END;
             CREATE TRIGGER held_insert BEFORE INSERT ON document_vectors
             BEGIN SELECT RAISE(ABORT, 'the write is held'); END;
             CREATE TRIGGER held_update BEFORE UPDATE ON document_vectors
             BEGIN SELECT RAISE(ABORT, 'the write is held'); END;",
        )
    });
}

/// Wait until `landed` says `vault`'s change stands in its store and the
/// entry serves again, the engine's drain having failed. `landed` answers
/// whether the change stands and what it read.
fn wait_undrained(
    serving: &Serving,
    vault: &attach::Vault,
    what: &str,
    landed: impl Fn() -> (bool, String),
) {
    wait_until(
        what,
        Budget::new(attach::READY_LIMIT, attach::STATE_PROBE),
        || {
            let (stands, read) = landed();
            let state = serving.host.state(vault.name());
            let drain_failed = matches!(
                serving.engines.status(vault.name()),
                SemanticStatus::On {
                    last_drain_error: Some(_),
                    ..
                }
            );
            if stands && matches!(state, Ok(TrustState::Ready)) && drain_failed {
                Observed::Met(())
            } else {
                Observed::pending(format!(
                    "{read}, the entry is {state:?}, the drain failed: {drain_failed}"
                ))
            }
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
}

/// Delete `deleted` from `vault` and wait until its store holds `remaining`
/// documents and the entry serves again, the engine's drain having failed.
fn delete_undrained(serving: &Serving, vault: &attach::Vault, deleted: &[String], remaining: u64) {
    for path in deleted {
        std::fs::remove_file(vault.path().join(path)).expect("a document removed");
    }
    wait_undrained(
        serving,
        vault,
        "the deletions to land in the store and the entry to serve",
        || {
            let held = documents_held(serving, vault);
            (held == Ok(remaining), format!("the store holds {held:?}"))
        },
    );
}

/// Rewrite each of `edited` in `vault` to a body holding the word `edited`,
/// and wait until the store's lexical rung finds every one of them and the
/// entry serves again, the engine's drain having failed.
fn edit_undrained(serving: &Serving, vault: &attach::Vault, edited: &[String]) {
    for path in edited {
        std::fs::write(vault.path().join(path), "alpha edited\n").expect("a document edited");
    }
    let lexical = searching(vault, "edited")
        .with_rungs(exactly([Rung::Lexical]))
        .with_limit(norn_wire::RUNG_DEPTH);
    wait_undrained(
        serving,
        vault,
        "the edits to land in the store and the entry to serve",
        || match serving.host.search(&lexical) {
            Ok(answer) => {
                let found = paths(&answer).len();
                (
                    found == edited.len(),
                    format!("the store finds {found} edits"),
                )
            }
            Err(refusal) => (false, format!("the lexical rung refused: {refusal:?}")),
        },
    );
}

/// The rungs `answer` is advised fell short of their depth, and how many
/// candidates each delivered.
fn short_of_depth(answer: &Answered<SearchReport, SearchCost>) -> Vec<(Rung, u32)> {
    answer
        .answer
        .advisories
        .iter()
        .filter_map(|advisory| match advisory {
            AnswerAdvisory::RungShortOfDepth {
                rung, delivered, ..
            } => Some((*rung, *delivered)),
            _ => None,
        })
        .collect()
}

/// **A mass delete the engine has not drained costs the vector rung its
/// margin, never its depth, while the lag stays within the cap; past the cap
/// the rung delivers fewer and the answer says how many.** The sidecar is held
/// from retracting anything, and the documents the vector rung ranks first
/// are deleted. With fewer deaths undrained than the cap, the rung's margin is
/// those deaths counted in feed rows — the watcher records them in far fewer
/// write generations — and it still delivers the depth, none of it a deleted
/// path, advised of no shortfall. With more undrained than the cap, the margin
/// is the cap, the scan holds the depth and the cap, and the rung delivers
/// the depth less the deaths past the cap, none of them deleted, advised so on
/// the vector ladder and the hybrid one alike.
#[test]
fn an_undrained_mass_delete_costs_the_vector_rung_its_margin_until_the_cap() {
    let depth = norn_wire::RUNG_DEPTH as usize;
    let cap = norn_host::VECTOR_MARGIN_CAP as usize;
    let within = depth / 2 + 88;
    let beyond = cap + 16;
    let documents = beyond + depth + 40;
    let (_sandbox, vault) = a_vault_of_alphas("search-mass-delete", documents);
    let serving = serve(&vault);
    let vector = searching(&vault, "alpha")
        .with_rungs(exactly([Rung::Vector]))
        .with_limit(norn_wire::RUNG_DEPTH);
    let ranked = paths(&answered(&serving, &vector));
    assert_eq!(ranked.len(), depth);
    hold_the_sidecar(&vault);

    let first: Vec<String> = ranked[..within].to_vec();
    delete_undrained(&serving, &vault, &first, (documents - within) as u64);
    let answer = answered(&serving, &vector);
    let delivered = paths(&answer);
    assert_eq!(
        delivered.len(),
        depth,
        "the rung delivered short of its depth"
    );
    assert!(
        !delivered.iter().any(|path| first.contains(path)),
        "the rung answered a deleted document"
    );
    assert_eq!(answer.work.margin, within as u64);
    let work = answer.work.vector.expect("the vector rung ran");
    assert!(work.peak_held <= (depth + within) as u64, "{work:?}");
    assert!(
        short_of_depth(&answer).is_empty(),
        "{:?}",
        answer.answer.advisories
    );

    let second: Vec<String> = delivered[..beyond - within].to_vec();
    delete_undrained(&serving, &vault, &second, (documents - beyond) as u64);
    let answer = answered(&serving, &vector);
    let delivered = paths(&answer);
    let expected = depth + cap - beyond;
    assert_eq!(delivered.len(), expected);
    assert!(
        !delivered
            .iter()
            .any(|path| first.contains(path) || second.contains(path)),
        "the rung answered a deleted document"
    );
    assert_eq!(answer.work.margin, cap as u64);
    let work = answer.work.vector.expect("the vector rung ran");
    assert_eq!(work.peak_held, (depth + cap) as u64);
    assert_eq!(short_of_depth(&answer), [(Rung::Vector, expected as u32)]);
    let hybrid = answered(&serving, &searching(&vault, "alpha").with_limit(1));
    assert_eq!(short_of_depth(&hybrid), [(Rung::Vector, expected as u32)]);
}

/// The vector-only request at the rung's depth over `vault`, and the same
/// request filtered by a part every document satisfies.
fn vector_at_depth(vault: &attach::Vault) -> (SearchParams, SearchParams) {
    let unfiltered = searching(vault, "alpha")
        .with_rungs(exactly([Rung::Vector]))
        .with_limit(norn_wire::RUNG_DEPTH);
    let filtered = unfiltered.clone().with_predicates([every_document()]);
    (unfiltered, filtered)
}

/// **A margin wider than the rows the snapshot lacks still delivers the depth
/// and no more.** The sidecar is held and the hundred documents the vector
/// rung ranks first are edited, so the engine trails its store by a hundred
/// feed rows while every row it holds names a document the snapshot holds.
/// The scan holds the depth and that margin, every row survives the check,
/// and the rung contributes the depth alone: the answer, its cursor and its
/// advisories are the filtered path's, a page of the depth with nothing after
/// it, advised the rung reached its depth.
#[test]
fn a_margin_wider_than_the_rows_the_snapshot_lacks_delivers_the_depth() {
    let depth = norn_wire::RUNG_DEPTH as usize;
    let lag = 100;
    let documents = depth + lag + 40;
    let (_sandbox, vault) = a_vault_of_alphas("search-margin-no-ghosts", documents);
    let serving = serve(&vault);
    let (unfiltered, filtered) = vector_at_depth(&vault);
    let ranked = paths(&answered(&serving, &unfiltered));
    hold_the_sidecar(&vault);
    edit_undrained(&serving, &vault, &ranked[..lag]);

    let answer = answered(&serving, &unfiltered);
    assert_eq!(answer.work.margin, lag as u64);
    let work = answer.work.vector.expect("the vector rung ran");
    assert_eq!(work.peak_held, (depth + lag) as u64, "{work:?}");
    assert_eq!(paths(&answer).len(), depth);
    assert_eq!(answer.answer.report.page.next, None);
    assert_eq!(depth_reached(&answer), [Rung::Vector]);

    let expected = answered(&serving, &filtered);
    assert_eq!(answer.answer.report, expected.answer.report);
    assert_eq!(answer.answer.advisories, expected.answer.advisories);
}

/// **A scan whose margin covered every row it scored cut nothing, so fewer
/// than the depth surviving is everything.** Over a vault just past the
/// depth, the sidecar is held and a hundred documents are deleted, leaving
/// fewer than the depth: the margin is those hundred deaths, the scan holds
/// every row it scores, and the rung delivers every document that stands,
/// advised of no shortfall and no depth — the filtered path's answer.
#[test]
fn a_margin_that_covered_the_scan_answers_everything_unadvised() {
    let depth = norn_wire::RUNG_DEPTH as usize;
    let deaths = 100;
    let documents = depth + 16;
    let (_sandbox, vault) = a_vault_of_alphas("search-margin-covered", documents);
    let serving = serve(&vault);
    let (unfiltered, filtered) = vector_at_depth(&vault);
    let ranked = paths(&answered(&serving, &unfiltered));
    hold_the_sidecar(&vault);
    delete_undrained(
        &serving,
        &vault,
        &ranked[..deaths],
        (documents - deaths) as u64,
    );

    let answer = answered(&serving, &unfiltered);
    assert_eq!(answer.work.margin, deaths as u64);
    let work = answer.work.vector.expect("the vector rung ran");
    assert_eq!(work.rows_scored, documents as u64);
    assert_eq!(paths(&answer).len(), documents - deaths);
    assert_eq!(answer.answer.report.page.next, None);
    assert!(
        answer.answer.advisories.is_empty(),
        "{:?}",
        answer.answer.advisories
    );

    let expected = answered(&serving, &filtered);
    assert_eq!(answer.answer.report, expected.answer.report);
    assert_eq!(answer.answer.advisories, expected.answer.advisories);
}
