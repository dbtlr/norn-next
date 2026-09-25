//! Vector-nearest over the sidecar: deterministic order, a bounded answer,
//! and rows scoped to the answering engine's model.

use std::num::NonZeroUsize;
use std::sync::Arc;

use crate::common::{CountingEmbedder, Scratch, document, write_document};
use norn_embed::{Model, StubEmbedder};

/// The order is total — score descending, path ascending on ties — and the
/// answer is bounded by the limit.
#[test]
fn nearest_answers_in_a_total_order_under_a_bound() {
    let scratch = Scratch::new("nearest");
    let mut store = scratch.store();
    write_document(
        &mut store,
        &document("docs/alpha.md", "hash-1", "alpha alpha alpha\n"),
    );
    write_document(
        &mut store,
        &document("docs/bravo.md", "hash-2", "bravo bravo\n"),
    );
    write_document(
        &mut store,
        &document("docs/mixed.md", "hash-3", "alpha bravo\n"),
    );

    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder);
    engine.drain(&mut store.feed_read()).expect("a drain");

    let neighbors = engine.nearest("alpha", 3).expect("an answer");
    assert_eq!(neighbors.len(), 3);
    assert_eq!(
        neighbors[0].path, "docs/alpha.md",
        "the document that is the query's word repeated scores highest: {neighbors:?}"
    );
    assert!(
        neighbors[0].score >= neighbors[1].score && neighbors[1].score >= neighbors[2].score,
        "{neighbors:?}"
    );

    // Deterministic: the same question answers the same way.
    assert_eq!(neighbors, engine.nearest("alpha", 3).expect("an answer"));

    // Bounded: a limit of one is one row, the top one.
    let top = engine.nearest("alpha", 1).expect("an answer");
    assert_eq!(top.len(), 1);
    assert_eq!(top[0], neighbors[0]);
}

/// Equal scores answer in path order: two documents with one body embed to
/// one vector, and the tie is broken by the path, not by arrival order.
#[test]
fn equal_scores_answer_in_path_order() {
    let scratch = Scratch::new("ties");
    let mut store = scratch.store();
    write_document(
        &mut store,
        &document("docs/twin-b.md", "hash-1", "same body\n"),
    );
    write_document(
        &mut store,
        &document("docs/twin-a.md", "hash-2", "same body\n"),
    );

    let mut engine = scratch.engine(CountingEmbedder::new());
    engine.drain(&mut store.feed_read()).expect("a drain");

    let neighbors = engine.nearest("same", 2).expect("an answer");
    assert_eq!(neighbors.len(), 2);
    assert_eq!(
        neighbors[0].score, neighbors[1].score,
        "one body is one vector: {neighbors:?}"
    );
    assert_eq!(neighbors[0].path, "docs/twin-a.md");
    assert_eq!(neighbors[1].path, "docs/twin-b.md");
}

/// A row under another model — placed out of band, since no open leaves one
/// behind — is not the engine's to answer with or project.
#[test]
fn a_foreign_models_row_is_not_answered_with() {
    let scratch = Scratch::new("foreign-row");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));

    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder);
    engine.drain(&mut store.feed_read()).expect("a drain");
    drop(engine);

    match norn_db::connect(&scratch.sidecar_path()).expect("connecting to the sidecar") {
        norn_db::Attempt::Connected(connection) => {
            connection
                .execute(
                    "INSERT INTO document_vectors
                         (path, model_id, model_version, input_hash, dimensions, embedding)
                     VALUES ('docs/foreign.md', 'other-model', '9', 'hash-x', 1, zeroblob(4))",
                    [],
                )
                .expect("planting a foreign row");
        }
        norn_db::Attempt::Unreadable { detail } => panic!("the sidecar is unreadable: {detail}"),
    }

    let engine = scratch.engine(CountingEmbedder::new());
    let held: Vec<String> = engine
        .projection()
        .expect("a projection")
        .into_iter()
        .map(|row| row.path)
        .collect();
    assert_eq!(held, vec!["docs/a.md".to_string()]);
    let answer = engine.nearest("alpha", 10).expect("an answer");
    assert!(
        answer
            .iter()
            .all(|neighbor| neighbor.path != "docs/foreign.md"),
        "{answer:?}"
    );
}

/// A sidecar written for one model is not adopted by another: the cursors
/// are model-blind, so the open resolves a moved model by the
/// wholesale-rebuild floor — fresh sidecar, full recompute, answers under
/// the new model only.
#[test]
fn a_moved_model_rebuilds_the_sidecar_from_zero() {
    let scratch = Scratch::new("moved-model");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));

    let mut engine = scratch.engine(CountingEmbedder::new());
    engine.drain(&mut store.feed_read()).expect("a drain");
    let before = engine.projection().expect("a projection");
    assert_eq!(before.len(), 1);
    drop(engine);

    let upgraded = StubEmbedder::with_model(
        Model::new("other-stub", "1"),
        NonZeroUsize::new(8).expect("eight is not zero"),
    )
    .expect("a free identity at a free width");
    let mut engine = scratch.engine(Arc::new(upgraded));
    assert!(
        matches!(
            engine.open_outcome(),
            norn_semantic::SidecarOutcome::RebuiltFromZero { .. }
        ),
        "{:?}",
        engine.open_outcome()
    );
    assert_eq!(
        engine.projection().expect("a projection"),
        Vec::new(),
        "nothing of the old model survives into the new sidecar"
    );

    let report = engine.drain(&mut store.feed_read()).expect("a drain");
    assert_eq!(report.embedded, 1, "{report:?}");
    let after = engine.projection().expect("a projection");
    assert_eq!(after.len(), 1);
    assert_ne!(
        after[0].values.len(),
        before[0].values.len(),
        "the new model's width answers now"
    );

    let answer = engine.nearest("alpha", 10).expect("an answer");
    assert_eq!(answer.len(), 1);
}

/// A store holding `count` documents whose bodies share words unevenly, and
/// an engine drained over it.
fn drained_over(label: &str, count: usize) -> (Scratch, norn_semantic::Engine) {
    let scratch = Scratch::new(label);
    let mut store = scratch.store();
    for at in 0..count {
        write_document(
            &mut store,
            &document(
                &format!("docs/{at:04}.md"),
                &format!("hash-{at}"),
                &format!("alpha{} bravo{} note{at}\n", at % 7, at % 3),
            ),
        );
    }
    let mut engine = scratch.engine(CountingEmbedder::new());
    engine.drain(&mut store.feed_read()).expect("a drain");
    (scratch, engine)
}

/// **The scan holds at most its limit, at any vault size.** Over two vaults
/// four times apart, an answer bounded at eight reads every row and scores
/// every row, and never holds more than eight; the rows it answers are the
/// best eight of the whole ranking. The negative control is an answer whose
/// limit is the vault: it holds every row, so the reading counts what the
/// scan holds rather than what it answers.
#[test]
fn the_scan_holds_no_more_than_its_limit_at_any_size() {
    const LIMIT: usize = 8;
    for count in [40, 160] {
        let (_scratch, engine) = drained_over(&format!("nearest-bounded-{count}"), count);
        let bounded = engine
            .nearest_among("alpha3 bravo1", LIMIT, |_| true)
            .expect("a bounded answer");
        assert_eq!(bounded.work.rows_read, count as u64);
        assert_eq!(bounded.work.rows_scored, count as u64);
        assert_eq!(
            bounded.work.peak_held, LIMIT as u64,
            "an answer bounded at {LIMIT} over {count} rows held another count"
        );

        let whole = engine
            .nearest_among("alpha3 bravo1", count, |_| true)
            .expect("an answer as wide as the vault");
        assert_eq!(whole.work.peak_held, count as u64);
        assert_eq!(
            bounded.neighbors,
            whole.neighbors[..LIMIT],
            "the bounded answer is not the head of the whole ranking"
        );
    }
}

/// **A row the answer does not admit is never scored.** Without a test the
/// document holding the query's word answers first; with a test refusing it,
/// it is not answered, the scan still reads it, and it is not scored.
#[test]
fn a_row_the_answer_does_not_admit_is_never_scored() {
    let scratch = Scratch::new("nearest-admits");
    let mut store = scratch.store();
    for (path, hash, body) in [
        ("docs/alpha.md", "hash-1", "alpha alpha alpha\n"),
        ("docs/bravo.md", "hash-2", "bravo bravo\n"),
        ("docs/mixed.md", "hash-3", "alpha bravo\n"),
    ] {
        write_document(&mut store, &document(path, hash, body));
    }
    let mut engine = scratch.engine(CountingEmbedder::new());
    engine.drain(&mut store.feed_read()).expect("a drain");

    let open = engine
        .nearest_among("alpha", 3, |_| true)
        .expect("an unrestricted answer");
    assert_eq!(open.neighbors[0].path, "docs/alpha.md");

    let restricted = engine
        .nearest_among("alpha", 3, |path| path != "docs/alpha.md")
        .expect("a restricted answer");
    assert!(
        restricted
            .neighbors
            .iter()
            .all(|neighbor| neighbor.path != "docs/alpha.md"),
        "a row the test refused was answered: {:?}",
        restricted.neighbors
    );
    assert_eq!(restricted.neighbors.len(), 2);
    assert_eq!(restricted.work.rows_read, 3);
    assert_eq!(restricted.work.rows_scored, 2);
}

/// **Every row the scan scores is judged a relevance score, kept or not.** A
/// row whose embedding is overwritten with a positive or a negative NaN, or a
/// positive or a negative infinity, refuses the answer naming that row, at a
/// limit of one over forty rows: a score ranked below every finite one is
/// pushed out of what the scan holds, and it refuses all the same. A test
/// refusing that row leaves it unscored, and the answer stands.
#[test]
fn a_non_finite_score_refuses_the_answer_wherever_the_scan_ranks_it() {
    let (scratch, engine) = drained_over("nearest-non-finite", 40);
    let dimensions = engine.projection().expect("a projection")[0].values.len();
    let connection = match norn_db::connect(&scratch.sidecar_path()).expect("the sidecar") {
        norn_db::Attempt::Connected(connection) => connection,
        norn_db::Attempt::Unreadable { detail } => panic!("the sidecar is unreadable: {detail}"),
    };
    // Little-endian f32 words: +NaN, -NaN, +infinity, -infinity.
    for word in ["0000c07f", "0000c0ff", "0000807f", "000080ff"] {
        connection
            .execute_batch(&format!(
                "UPDATE document_vectors SET embedding = x'{}' WHERE path = 'docs/0000.md'",
                word.repeat(dimensions)
            ))
            .expect("damaging one row");
        let refused = engine
            .nearest_among("alpha3 bravo1", 1, |_| true)
            .expect_err("an answer over a row scored with no relevance score");
        let norn_semantic::EngineError::NonFiniteScore { path, .. } = &refused else {
            panic!("{word}: {refused:?}");
        };
        assert_eq!(path, "docs/0000.md", "{word}");
        assert!(refused.sidecar_damage().is_none(), "{word}");

        let unscored = engine
            .nearest_among("alpha3 bravo1", 1, |path| path != "docs/0000.md")
            .expect("an answer that never scored the row");
        assert_eq!(unscored.work.rows_scored, 39, "{word}");
    }
}
