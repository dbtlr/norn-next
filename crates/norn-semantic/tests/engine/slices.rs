//! Bounded slices: a drain that stops at its budget, resumes where its last
//! committed page left it, and settles where an unbounded drain would.

use std::num::NonZeroUsize;

use crate::common::{CountingEmbedder, Scratch, document, recompute, write_document};
use norn_semantic::{DrainReport, Engine, SliceBudget};
use norn_store::Store;

/// One page of work per slice.
fn one_page() -> SliceBudget {
    SliceBudget::pages(NonZeroUsize::new(1).expect("one is not zero"))
}

/// Write `count` documents `docs/000.md`, `docs/001.md`, … into `store`.
fn write_numbered(store: &mut Store, count: usize, keep: impl Fn(usize) -> bool) {
    for serial in (0..count).filter(|serial| keep(*serial)) {
        let at = format!("docs/{serial:03}.md");
        write_document(
            store,
            &document(&at, &format!("hash-{serial}"), &format!("body {serial}\n")),
        );
    }
}

/// Drain `engine` one page at a time until a slice owes nothing, answering
/// every slice's report.
fn slices_until_settled(engine: &mut Engine, store: &mut Store) -> Vec<DrainReport> {
    let mut reports = Vec::new();
    loop {
        let report = engine
            .drain_bounded(&mut store.feed_read(), one_page())
            .expect("a slice");
        let owed = report.owed;
        reports.push(report);
        assert!(reports.len() < 64, "the slices never settle: {reports:?}");
        if !owed {
            return reports;
        }
    }
}

/// **A bounded drain stops at its budget and resumes to the settled
/// recompute.** Six hundred documents are three feed pages: each slice
/// embeds at most one page and says it still owes work, no watermark is
/// claimed for a feed until the page that completes it, and the slices
/// together arrive at exactly what a from-zero recompute derives.
#[test]
fn a_bounded_drain_stops_at_its_budget_and_resumes_to_the_settled_recompute() {
    let scratch = Scratch::new("bounded");
    let mut store = scratch.store();
    write_numbered(&mut store, 600, |_| true);
    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());

    let first = engine
        .drain_bounded(&mut store.feed_read(), one_page())
        .expect("a slice");
    assert!(first.owed, "{first:?}");
    assert_eq!(first.embedded, 256, "{first:?}");
    assert_eq!(
        engine.watermarks().documents,
        None,
        "no page has completed the document feed"
    );

    let rest = slices_until_settled(&mut engine, &mut store);
    let embedded: Vec<u64> = rest.iter().map(|report| report.embedded).collect();
    assert_eq!(embedded, [256, 88, 0], "{rest:?}");
    assert!(engine.watermarks().documents.is_some());
    assert!(engine.watermarks().tombstones.is_some());

    let held = engine.projection().expect("a projection");
    assert_eq!(held.len(), 600);
    assert_eq!(held, recompute(&mut store, embedder.as_ref()));
    let settled = engine
        .drain_bounded(&mut store.feed_read(), one_page())
        .expect("a slice");
    assert!(settled.is_settled(), "{settled:?}");
}

/// **A store-epoch reconcile is paged too, and keeps the epoch it observed
/// until its last page.** Six hundred held paths are three reconcile pages.
/// A slice that stops part way has removed only what it checked, still owes
/// work and still answers from the old lifetime's watermarks; an engine
/// dropped there and opened again rescans rather than trusting cursors from
/// the old lifetime, and the slices converge on the from-zero recompute.
#[test]
fn a_partial_reconcile_keeps_the_observed_epoch_and_converges_after_a_reopen() {
    let scratch = Scratch::new("reconcile-paged");
    let mut store = scratch.store();
    write_numbered(&mut store, 600, |_| true);
    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());
    engine.drain(&mut store.feed_read()).expect("a drain");
    let old_epoch = store.feed_read().epoch().to_string();

    // The rebuilt lifetime keeps every odd document, and no tombstone says the
    // even ones died.
    let mut store = store
        .discard_and_reopen(norn_store::StoredPathOrder::Sensitive)
        .expect("a store rebuilt from zero");
    write_numbered(&mut store, 600, |serial| serial % 2 == 1);

    let partial = engine
        .drain_bounded(&mut store.feed_read(), one_page())
        .expect("a slice");
    assert!(partial.rescan && partial.owed, "{partial:?}");
    assert_eq!(partial.reconciled_away, 128, "{partial:?}");
    assert_eq!(partial.embedded, 0, "the feed waits for the reconcile");
    assert_eq!(
        engine
            .watermarks()
            .documents
            .as_ref()
            .map(|mark| mark.store_epoch.as_str()),
        Some(old_epoch.as_str()),
        "a partial reconcile claims nothing of the new lifetime"
    );

    // A crash between pages: every page committed alone.
    drop(engine);
    let mut engine = scratch.engine(embedder.clone());
    let resumed = slices_until_settled(&mut engine, &mut store);
    assert!(
        resumed[0].rescan,
        "the reopened engine still owes the rescan"
    );
    let removed: u64 = resumed.iter().map(|report| report.reconciled_away).sum();
    assert_eq!(removed, 172, "the rest of the dead paths: {resumed:?}");
    assert_eq!(
        resumed.iter().map(|report| report.embedded).sum::<u64>(),
        0,
        "content-addressed rows recompute nothing"
    );

    let held = engine.projection().expect("a projection");
    assert_eq!(held.len(), 300);
    assert_eq!(held, recompute(&mut store, embedder.as_ref()));
}

/// **A reconcile overtaken by another store lifetime starts over for it.**
/// Paths a partial pass checked against one lifetime can be gone from the
/// next, so a pass never resumes across lifetimes: no row it kept escapes
/// the final convergence.
#[test]
fn a_reconcile_overtaken_by_another_lifetime_restarts_and_converges() {
    let scratch = Scratch::new("reconcile-overtaken");
    let mut store = scratch.store();
    write_numbered(&mut store, 600, |_| true);
    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());
    engine.drain(&mut store.feed_read()).expect("a drain");

    let mut store = store
        .discard_and_reopen(norn_store::StoredPathOrder::Sensitive)
        .expect("a store rebuilt from zero");
    write_numbered(&mut store, 600, |_| true);
    let partial = engine
        .drain_bounded(&mut store.feed_read(), one_page())
        .expect("a slice");
    assert!(partial.owed, "{partial:?}");
    assert_eq!(
        partial.reconciled_away, 0,
        "every path stands in this lifetime"
    );

    // The next lifetime holds only the first hundred: among them paths the
    // overtaken pass already checked as standing.
    let mut store = store
        .discard_and_reopen(norn_store::StoredPathOrder::Sensitive)
        .expect("a store rebuilt from zero again");
    write_numbered(&mut store, 600, |serial| serial < 100);
    let reports = slices_until_settled(&mut engine, &mut store);
    let removed: u64 = reports.iter().map(|report| report.reconciled_away).sum();
    assert_eq!(removed, 500, "{reports:?}");

    let held = engine.projection().expect("a projection");
    assert_eq!(held.len(), 100);
    assert_eq!(held, recompute(&mut store, embedder.as_ref()));
}
