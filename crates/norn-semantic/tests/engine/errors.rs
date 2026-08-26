//! The refusal surface: what each failure names, which refusals say the
//! sidecar is damaged, and that a damaged sidecar recovers by discard.

use crate::common::{
    CountingEmbedder, NarrowEmbedder, RefusingEmbedder, Scratch, document, recompute,
    write_document,
};
use norn_db::rusqlite::params;
use norn_semantic::EngineError;

/// A refusing embedder surfaces as [`EngineError::Embed`] naming the
/// document, and the failed page's cursor does not advance: the same work is
/// still owed after the refusal.
#[test]
fn an_embedder_refusal_names_the_document_and_loses_no_work() {
    let scratch = Scratch::new("refusal");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));

    let mut engine = scratch.engine(RefusingEmbedder::new());
    let error = engine
        .drain(&mut store.feed_read())
        .expect_err("a refused embedding");
    let EngineError::Embed { path, .. } = &error else {
        panic!("the refusal is not the embedder's: {error:?}");
    };
    assert_eq!(path, "docs/a.md");
    assert_eq!(error.sidecar_damage(), None);
    drop(engine);

    // The page never committed, so a working embedder picks the row up.
    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());
    let report = engine.drain(&mut store.feed_read()).expect("a drain");
    assert_eq!(report.embedded, 1, "{report:?}");
}

/// An embedder answering off its promised width is refused at the write —
/// a narrow row would decode cleanly and then score against a prefix.
#[test]
fn an_off_width_embedding_is_refused() {
    let scratch = Scratch::new("width");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));

    let mut engine = scratch.engine(NarrowEmbedder::new());
    let error = engine
        .drain(&mut store.feed_read())
        .expect_err("a narrow embedding");
    let EngineError::WrongWidth {
        path,
        promised,
        produced,
    } = &error
    else {
        panic!("the narrow answer was not refused as one: {error:?}");
    };
    assert_eq!(path, "docs/a.md");
    assert_eq!(*produced, promised - 1);
    assert_eq!(engine.projection().expect("a projection"), Vec::new());
}

/// A corrupted recorded cursor is a damaged sidecar: the refusal says so
/// through [`EngineError::sidecar_damage`], and discarding recovers — the
/// rebuilt sidecar re-derives everything from the feed.
#[test]
fn a_corrupt_cursor_is_damage_and_discard_recovers() {
    let scratch = Scratch::new("bad-cursor");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));

    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());
    engine.drain(&mut store.feed_read()).expect("a drain");
    drop(engine);

    match norn_db::connect(&scratch.sidecar_path()).expect("connecting to the sidecar") {
        norn_db::Attempt::Connected(connection) => {
            connection
                .execute(
                    "UPDATE meta SET value = 'no separator here' WHERE key = 'document_cursor'",
                    [],
                )
                .expect("corrupting the recorded cursor");
        }
        norn_db::Attempt::Unreadable { detail } => panic!("the sidecar is unreadable: {detail}"),
    }

    let mut engine = scratch.engine(embedder.clone());
    let error = engine
        .drain(&mut store.feed_read())
        .expect_err("a position that parses to nothing");
    assert!(error.sidecar_damage().is_some(), "{error:?}");

    let mut engine = engine.discard_and_reopen().expect("a discard");
    let report = engine.drain(&mut store.feed_read()).expect("a drain");
    assert_eq!(report.embedded, 1, "{report:?}");
    assert_eq!(
        engine.projection().expect("a projection"),
        recompute(&mut store, embedder.as_ref()),
        "the rebuilt sidecar converges"
    );
}

/// A blob that disagrees with the width its row declares is damage at the
/// read, not a short answer.
#[test]
fn a_width_torn_row_reads_as_damage() {
    let scratch = Scratch::new("torn-width");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));

    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());
    engine.drain(&mut store.feed_read()).expect("a drain");
    drop(engine);

    match norn_db::connect(&scratch.sidecar_path()).expect("connecting to the sidecar") {
        norn_db::Attempt::Connected(connection) => {
            connection
                .execute(
                    "UPDATE document_vectors SET dimensions = dimensions - 1 WHERE path = ?1",
                    params!["docs/a.md"],
                )
                .expect("tearing width from blob");
        }
        norn_db::Attempt::Unreadable { detail } => panic!("the sidecar is unreadable: {detail}"),
    }

    let engine = scratch.engine(embedder);
    let error = engine.projection().expect_err("a torn row");
    assert!(error.sidecar_damage().is_some(), "{error:?}");
}
