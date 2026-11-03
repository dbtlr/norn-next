//! A drain in three phases: a step read in one snapshot of a feed source, its
//! bodies embedded holding nothing, and its page committed afterwards.

use crate::common::{CountingEmbedder, Scratch, document, recompute, write_document};
use norn_semantic::{DrainStep, Engine};
use norn_store::FeedSource;

/// Run steps over `source` until one owes nothing, each page embedded and
/// committed outside the step's snapshot. Answers how many steps ran.
fn settle(engine: &mut Engine, source: &mut FeedSource) -> usize {
    for steps in 1..64 {
        let step = engine
            .step(&mut source.page().expect("a page"))
            .expect("a step");
        let owed = match step {
            DrainStep::Done(report) => report.owed,
            DrainStep::Embed(pending) => {
                let embedded = pending.embed().expect("the embedding");
                engine.commit(embedded).expect("the commit").owed
            }
        };
        if !owed {
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
