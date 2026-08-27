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
