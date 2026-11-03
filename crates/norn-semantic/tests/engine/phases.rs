//! A drain in three phases: a step read in one snapshot of a feed source, its
//! bodies embedded holding nothing, and its page committed afterwards.

use crate::common::{CountingEmbedder, Scratch, document, recompute, record_death, write_document};
use norn_semantic::{DrainStep, Engine};
use norn_store::FeedSource;

/// Run steps over `source` until one owes nothing, each page embedded and
/// committed outside the step's snapshot. Answers how many steps ran.
fn settle(engine: &mut Engine, source: &mut FeedSource) -> usize {
    for steps in 1..64 {
        if !one_step(engine, source) {
            return steps;
        }
    }
    panic!("the steps never settle");
}

/// **A step reads its page in the feed source's snapshot, and its bodies are
/// embedded and committed after that snapshot has ended.** The writer goes
/// on writing between the phases; what it wrote is the next steps' to read,
/// and the steps settle on exactly what a from-zero recompute derives.
#[test]
fn a_step_read_in_one_snapshot_commits_after_its_embedding_and_settles() {
    let scratch = Scratch::new("phases");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-a1", "alpha\n"));
    let mut source = store.feed_source();
    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());

    let DrainStep::Embed(pending) = engine
        .step(&mut source.page().expect("a page"))
        .expect("the first step")
    else {
        panic!("the first page owes an embedding");
    };
    assert_eq!(pending.rows(), 1);
    // The page's snapshot has ended: the writer is free, and its write is a
    // later step's.
    write_document(&mut store, &document("docs/b.md", "hash-b1", "bravo\n"));
    let report = engine
        .commit(pending.embed().expect("the embedding"))
        .expect("the commit");
    assert_eq!(report.embedded, 1, "{report:?}");
    assert!(report.owed, "the tombstone feed is still to read");

    settle(&mut engine, &mut source);
    assert_eq!(
        engine.projection().expect("a projection"),
        recompute(&mut store, embedder.as_ref())
    );
}

/// **A page the sidecar moved past while it was embedded commits nothing.**
/// The engine is its sidecar's one writer, so the only way the sidecar moves
/// under a page is another page committed first; the overtaken page is
/// discarded, owes its step again, and the next step finds nothing left.
#[test]
fn a_page_overtaken_while_it_was_embedded_commits_nothing() {
    let scratch = Scratch::new("phases-overtaken");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-a1", "alpha\n"));
    let mut source = store.feed_source();
    let mut engine = scratch.engine(CountingEmbedder::new());

    let read = |engine: &mut Engine, source: &mut FeedSource| match engine
        .step(&mut source.page().expect("a page"))
        .expect("a step")
    {
        DrainStep::Embed(pending) => pending.embed().expect("the embedding"),
        DrainStep::Done(report) => panic!("the page owes an embedding: {report:?}"),
    };
    let overtaken = read(&mut engine, &mut source);
    let first = read(&mut engine, &mut source);
    engine.commit(first).expect("the first commit");
    let revision = engine.revision();

    let discarded = engine.commit(overtaken).expect("the overtaken commit");
    assert_eq!(engine.revision(), revision, "an overtaken page committed");
    assert!(discarded.owed, "{discarded:?}");
    settle(&mut engine, &mut source);
    assert_eq!(engine.projection().expect("a projection").len(), 1);
}

/// Run one step over `source`, its page embedded and committed where it owes
/// an embedding, and answer whether more steps are owed.
fn one_step(engine: &mut Engine, source: &mut FeedSource) -> bool {
    match engine
        .step(&mut source.page().expect("a page"))
        .expect("a step")
    {
        DrainStep::Done(report) => report.owed,
        DrainStep::Embed(pending) => {
            let embedded = pending.embed().expect("the embedding");
            engine.commit(embedded).expect("the commit").owed
        }
    }
}

/// **A death is retracted while document writes continue.** Every step
/// below finds a document change written just before it, so a step that
/// always read the document feed first would never reach the tombstones:
/// the deleted document's vector would stay, and the tombstone watermark
/// would never move. A document page that completes its feed hands the
/// turn to the tombstone feed, so the death is retracted within a bounded
/// number of steps.
#[test]
fn a_death_is_retracted_while_document_writes_continue() {
    let scratch = Scratch::new("phases-tombstone-turn");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-a0", "alpha 0\n"));
    write_document(&mut store, &document("docs/b.md", "hash-b1", "bravo\n"));
    let mut source = store.feed_source();
    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());
    settle(&mut engine, &mut source);
    let tombstones_before = engine.watermarks().tombstones.clone();

    record_death(&mut store, "docs/b.md");
    let held_b = |engine: &Engine| {
        engine
            .projection()
            .expect("a projection")
            .iter()
            .any(|row| row.path == "docs/b.md")
    };
    let mut steps = 0;
    while held_b(&engine) {
        assert!(
            steps < 4,
            "the death was not retracted within {steps} steps while writes continued"
        );
        steps += 1;
        let body = format!("alpha {steps}\n");
        write_document(
            &mut store,
            &document("docs/a.md", &format!("hash-a{steps}"), &body),
        );
        one_step(&mut engine, &mut source);
    }
    assert_ne!(
        engine.watermarks().tombstones,
        tombstones_before,
        "the tombstone watermark did not move"
    );

    settle(&mut engine, &mut source);
    assert_eq!(
        engine.projection().expect("a projection"),
        recompute(&mut store, embedder.as_ref())
    );
}
