//! The lane-2 convergence bar: settle, then exact equality against a
//! from-zero recompute over current lane-1 rows.

use crate::common::{CountingEmbedder, Scratch, document, recompute, record_death, write_document};
use norn_db::rusqlite::params;

/// The bar itself. The store churns — writes, edits, deaths, a rebirth —
/// with drains interleaved mid-history, so the sidecar arrives at settle
/// through incremental maintenance; the recompute reads the final lane-1
/// state alone. Exact equality, values included, is the claim.
#[test]
fn the_settled_sidecar_equals_a_from_zero_recompute() {
    let scratch = Scratch::new("bar");
    let mut store = scratch.store();
    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());

    write_document(&mut store, &document("docs/a.md", "hash-a1", "alpha\n"));
    write_document(&mut store, &document("docs/b.md", "hash-b1", "bravo\n"));
    engine
        .drain(&mut store.feed_read())
        .expect("a drain mid-history");

    write_document(
        &mut store,
        &document("docs/b.md", "hash-b2", "bravo rewritten\n"),
    );
    write_document(&mut store, &document("docs/c.md", "hash-c1", "charlie\n"));
    record_death(&mut store, "docs/a.md");
    engine
        .drain(&mut store.feed_read())
        .expect("a drain mid-history");

    write_document(
        &mut store,
        &document("docs/a.md", "hash-a2", "alpha reborn\n"),
    );
    record_death(&mut store, "docs/c.md");
    write_document(&mut store, &document("docs/d.md", "hash-d1", "delta\n"));

    // Settle: drain until the reading is all-zero.
    engine.drain(&mut store.feed_read()).expect("a drain");
    let settled = engine
        .drain(&mut store.feed_read())
        .expect("the settling drain");
    assert!(settled.is_settled(), "{settled:?}");

    let held = engine.projection().expect("a projection");
    let derived = recompute(&mut store, embedder.as_ref());
    assert_eq!(
        held, derived,
        "the settled sidecar is the recompute, exactly"
    );
    assert_eq!(held.len(), 3, "a, b and d stand; c died");
}

/// The bar can fail. A row tampered out of band — the values, or the
/// recorded input beside them — makes the two sides unequal, so equality is
/// a claim about content rather than about row counts.
#[test]
fn the_bar_refutes_a_tampered_row() {
    let scratch = Scratch::new("refute");
    let mut store = scratch.store();
    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());

    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));
    write_document(&mut store, &document("docs/b.md", "hash-2", "bravo\n"));
    engine.drain(&mut store.feed_read()).expect("a drain");
    assert_eq!(
        engine.projection().expect("a projection"),
        recompute(&mut store, embedder.as_ref()),
    );

    // An out-of-band writer zeroes one embedding, width preserved. The stub
    // embeds no body to all-zero values, so the row now disagrees with every
    // recompute of it. The substrate crate is the one opener there is, so
    // the tamper goes through it.
    drop(engine);
    match norn_db::connect(&scratch.sidecar_path()).expect("connecting to the sidecar") {
        norn_db::Attempt::Connected(connection) => {
            let changed = connection
                .execute(
                    "UPDATE document_vectors
                     SET embedding = zeroblob(length(embedding))
                     WHERE path = ?1",
                    params!["docs/a.md"],
                )
                .expect("tampering with a row");
            assert_eq!(changed, 1);
        }
        norn_db::Attempt::Unreadable { detail } => panic!("the sidecar is unreadable: {detail}"),
    }

    // The tampered values survive a further drain: the recorded input hash
    // still matches the feed, so triage keeps the row and cannot self-heal
    // it. The bar is what reports silent value corruption — nothing else
    // does.
    let mut engine = scratch.engine(embedder.clone());
    let report = engine.drain(&mut store.feed_read()).expect("a drain");
    assert_eq!(report.embedded, 0, "{report:?}");
    assert_ne!(
        engine.projection().expect("a projection"),
        recompute(&mut store, embedder.as_ref()),
        "a tampered value survives settle and the bar reports it"
    );
}

/// A tampered *input hash* is the other refutation — and the one the drain
/// can heal, because the recorded hash disagreeing with the feed is exactly
/// the staleness test.
#[test]
fn the_bar_refutes_a_tampered_input_hash_and_a_drain_heals_it() {
    let scratch = Scratch::new("refute-hash");
    let mut store = scratch.store();
    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());

    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));
    engine.drain(&mut store.feed_read()).expect("a drain");

    drop(engine);
    match norn_db::connect(&scratch.sidecar_path()).expect("connecting to the sidecar") {
        norn_db::Attempt::Connected(connection) => {
            let changed = connection
                .execute(
                    "UPDATE document_vectors SET input_hash = 'not-any-body' WHERE path = ?1",
                    params!["docs/a.md"],
                )
                .expect("tampering with a recorded input");
            assert_eq!(changed, 1);
        }
        norn_db::Attempt::Unreadable { detail } => panic!("the sidecar is unreadable: {detail}"),
    }

    let mut engine = scratch.engine(embedder.clone());
    assert_ne!(
        engine.projection().expect("a projection"),
        recompute(&mut store, embedder.as_ref()),
        "the bar reports the drifted input record"
    );

    // A consumed row is presented again only when the feed moves. When it
    // is, the drifted hash is exactly what triage looks for, so the row
    // re-embeds and the bar closes again.
    write_document(&mut store, &document("docs/a.md", "hash-2", "alpha\n"));
    let report = engine.drain(&mut store.feed_read()).expect("a drain");
    assert_eq!(report.embedded, 1, "{report:?}");
    assert_eq!(
        engine.projection().expect("a projection"),
        recompute(&mut store, embedder.as_ref()),
        "the staleness key heals what the feed presents"
    );
}
