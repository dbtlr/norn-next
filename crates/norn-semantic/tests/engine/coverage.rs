//! Embedding coverage: how many of the documents the engine would embed have
//! embeddings over their current content, counted at the store state the
//! engine's last step read.

use std::num::NonZeroUsize;

use crate::common::{
    CountingEmbedder, Scratch, document, recompute, record_death, record_unreadable, write_document,
};
use norn_semantic::{Coverage, Engine, SliceBudget};
use norn_store::Store;

/// One page of work per slice.
fn one_page() -> SliceBudget {
    SliceBudget::pages(NonZeroUsize::new(1).expect("one is not zero"))
}

/// Write `count` documents `docs/0000.md`, `docs/0001.md`, … into `store`.
fn write_numbered(store: &mut Store, count: usize) {
    for serial in 0..count {
        write_document(
            store,
            &document(
                &format!("docs/{serial:04}.md"),
                &format!("hash-{serial}"),
                &format!("body {serial}\n"),
            ),
        );
    }
}

/// The counts a from-zero recompute gives over `store` as it stands and the
/// rows `engine` holds: every readable document, and those of them whose held
/// row was derived from the body the store holds now.
fn recount(store: &mut Store, engine: &Engine) -> Coverage {
    let readable = recompute(store, &norn_embed::StubEmbedder::new());
    let held = engine.projection().expect("a projection");
    let embedded = readable
        .iter()
        .filter(|row| {
            held.iter()
                .any(|kept| kept.path == row.path && kept.input_hash == row.input_hash)
        })
        .count();
    Coverage {
        embedded: embedded as u64,
        total: readable.len() as u64,
    }
}

/// **Coverage is `None` until a step has counted, and a first drain counts
/// every readable document and no names-only row.** The unreadable
/// document's names stand in the store, but there is nothing to embed.
#[test]
fn a_first_drain_counts_every_readable_document_and_no_names_only_row() {
    let scratch = Scratch::new("coverage-first");
    let mut store = scratch.store();
    write_numbered(&mut store, 3);
    record_unreadable(&mut store, "docs/broken.md");
    let mut engine = scratch.engine(CountingEmbedder::new());
    assert_eq!(engine.coverage(), None, "nothing has been counted yet");

    engine.drain(&mut store.feed_read()).expect("a drain");
    assert_eq!(
        engine.coverage(),
        Some(Coverage {
            embedded: 3,
            total: 3
        })
    );
}

/// **Mid-drain, `embedded` trails `total`.** Three hundred documents are
/// more than one page: after one slice the store's every readable document
/// is counted, and only the page the slice committed is embedded.
#[test]
fn mid_drain_embedded_trails_total() {
    let scratch = Scratch::new("coverage-mid");
    let mut store = scratch.store();
    write_numbered(&mut store, 300);
    let mut engine = scratch.engine(CountingEmbedder::new());

    let report = engine
        .drain_bounded(&mut store.feed_read(), one_page())
        .expect("a slice");
    assert!(report.owed, "{report:?}");
    let coverage = engine.coverage().expect("the slice counted");
    assert_eq!(coverage.total, 300, "{coverage:?}");
    assert_eq!(coverage.embedded, report.embedded, "{coverage:?}");
    assert!(coverage.embedded < coverage.total, "{coverage:?}");
}

/// Drain to settled and answer the coverage the settled engine reports.
fn settled(engine: &mut Engine, store: &mut Store) -> Coverage {
    engine.drain(&mut store.feed_read()).expect("a drain");
    let report = engine
        .drain(&mut store.feed_read())
        .expect("the settling drain");
    assert!(report.is_settled(), "{report:?}");
    engine.coverage().expect("a settled engine has counted")
}

/// **An edit, a create, a delete and an unreadable read each move the
/// counts to the true values once a drain has run over them.** Each value is
/// the literal count the history leaves, and the recount over the store and
/// the held rows agrees with it.
#[test]
fn an_edit_a_create_and_a_delete_each_move_the_counts_to_the_true_values() {
    let scratch = Scratch::new("coverage-moves");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-a1", "alpha\n"));
    write_document(&mut store, &document("docs/b.md", "hash-b1", "bravo\n"));
    let mut engine = scratch.engine(CountingEmbedder::new());
    let full = |count| Coverage {
        embedded: count,
        total: count,
    };
    assert_eq!(settled(&mut engine, &mut store), full(2));

    write_document(
        &mut store,
        &document("docs/a.md", "hash-a2", "alpha edited\n"),
    );
    assert_eq!(settled(&mut engine, &mut store), full(2), "an edit");
    assert_eq!(recount(&mut store, &engine), full(2));

    write_document(&mut store, &document("docs/c.md", "hash-c1", "charlie\n"));
    assert_eq!(settled(&mut engine, &mut store), full(3), "a create");
    assert_eq!(recount(&mut store, &engine), full(3));

    record_death(&mut store, "docs/b.md");
    assert_eq!(settled(&mut engine, &mut store), full(2), "a delete");
    assert_eq!(recount(&mut store, &engine), full(2));

    record_unreadable(&mut store, "docs/c.md");
    assert_eq!(
        settled(&mut engine, &mut store),
        full(1),
        "an unreadable read"
    );
    assert_eq!(recount(&mut store, &engine), full(1));
}

/// **An edit the census has read and the drain has not embedded counts in
/// `total` and not in `embedded`.** A step reads the census before the
/// drain's page, and a step that hands its page to be embedded has not yet
/// written it.
#[test]
fn an_edit_read_and_not_yet_embedded_is_counted_and_not_covered() {
    let scratch = Scratch::new("coverage-pending");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-a1", "alpha\n"));
    write_document(&mut store, &document("docs/b.md", "hash-b1", "bravo\n"));
    let mut engine = scratch.engine(CountingEmbedder::new());
    settled(&mut engine, &mut store);

    write_document(
        &mut store,
        &document("docs/a.md", "hash-a2", "alpha edited\n"),
    );
    let mut source = store.feed_source();
    let norn_semantic::DrainStep::Embed(pending) = engine
        .step(&mut source.page().expect("a page"))
        .expect("a step")
    else {
        panic!("the edit owes an embedding");
    };
    assert_eq!(
        engine.coverage(),
        Some(Coverage {
            embedded: 1,
            total: 2
        }),
        "the edit is read and not yet embedded"
    );
    engine
        .commit(pending.embed().expect("the embedding"))
        .expect("the commit");
    assert_eq!(
        engine.coverage(),
        Some(Coverage {
            embedded: 2,
            total: 2
        })
    );
}

/// **A settled drain's counts equal a from-zero recount's.** The store
/// churns — writes, edits, deaths, a rebirth, an unreadable read and its
/// recovery — with bounded slices interleaved mid-history, so the counts
/// arrive at settle row by row; the recount reads the final state alone.
#[test]
fn a_settled_drain_counts_what_a_from_zero_recount_counts() {
    let scratch = Scratch::new("coverage-churn");
    let mut store = scratch.store();
    let mut engine = scratch.engine(CountingEmbedder::new());
    write_numbered(&mut store, 300);
    engine
        .drain_bounded(&mut store.feed_read(), one_page())
        .expect("a slice mid-history");

    write_document(
        &mut store,
        &document("docs/0001.md", "hash-x", "rewritten\n"),
    );
    record_death(&mut store, "docs/0002.md");
    record_death(&mut store, "docs/0290.md");
    record_unreadable(&mut store, "docs/0003.md");
    engine
        .drain_bounded(&mut store.feed_read(), one_page())
        .expect("a slice mid-history");

    write_document(&mut store, &document("docs/0002.md", "hash-y", "reborn\n"));
    write_document(&mut store, &document("docs/0003.md", "hash-3", "body 3\n"));
    record_unreadable(&mut store, "docs/0004.md");
    write_document(&mut store, &document("docs/new.md", "hash-new", "new\n"));

    let coverage = settled(&mut engine, &mut store);
    assert_eq!(coverage, recount(&mut store, &engine));
    assert_eq!(
        coverage,
        Coverage {
            embedded: 299,
            total: 299
        },
        "300 written, one died, one unreadable, one created"
    );
}

/// **A rescan leaves no count from the old store lifetime.** The store is
/// rebuilt from zero holding fewer documents; the census empties and counts
/// the new lifetime alone, and the embeddings the old lifetime left over the
/// same bodies count as embedded.
#[test]
fn a_rescan_leaves_no_stale_count() {
    let scratch = Scratch::new("coverage-rescan");
    let mut store = scratch.store();
    write_numbered(&mut store, 3);
    let mut engine = scratch.engine(CountingEmbedder::new());
    assert_eq!(settled(&mut engine, &mut store).total, 3);

    let mut store = store
        .discard_and_reopen(norn_store::StoredPathOrder::Sensitive)
        .expect("a store rebuilt from zero");
    write_document(&mut store, &document("docs/0001.md", "hash-1", "body 1\n"));
    let coverage = settled(&mut engine, &mut store);
    assert_eq!(
        coverage,
        Coverage {
            embedded: 1,
            total: 1
        }
    );
    assert_eq!(coverage, recount(&mut store, &engine));
}

/// **A sidecar rebuilt from zero counts nothing until its first step, and
/// then counts exactly.** The counts are the sidecar's own, so a discarded
/// sidecar takes its counts with it.
#[test]
fn a_rebuilt_sidecar_leaves_no_stale_count() {
    let scratch = Scratch::new("coverage-rebuild");
    let mut store = scratch.store();
    write_numbered(&mut store, 3);
    let mut engine = scratch.engine(CountingEmbedder::new());
    settled(&mut engine, &mut store);

    let mut engine = engine.discard_and_reopen().expect("a rebuilt sidecar");
    assert_eq!(engine.coverage(), None);
    record_death(&mut store, "docs/0000.md");
    let coverage = settled(&mut engine, &mut store);
    assert_eq!(
        coverage,
        Coverage {
            embedded: 2,
            total: 2
        }
    );
    assert_eq!(coverage, recount(&mut store, &engine));
}

/// **A reopened engine counts nothing until it steps, and its first step over
/// a quiescent store reads no census row.** The counts persist in the
/// sidecar, so a reopen recounts nothing.
#[test]
fn a_reopened_engine_keeps_its_counts_without_recounting() {
    let scratch = Scratch::new("coverage-reopen");
    let mut store = scratch.store();
    write_numbered(&mut store, 3);
    let mut engine = scratch.engine(CountingEmbedder::new());
    let before = settled(&mut engine, &mut store);
    drop(engine);

    let mut engine = scratch.engine(CountingEmbedder::new());
    assert_eq!(engine.coverage(), None);
    let report = engine.drain(&mut store.feed_read()).expect("a drain");
    assert_eq!(report.censused, 0, "{report:?}");
    assert_eq!(engine.coverage(), Some(before));
}

/// **The census's work follows the rows that moved, never the vault's size.**
/// A first count over more documents than one census page takes several
/// steps, each reading at most a page of each feed and owing the next; until
/// the census is whole the engine answers no count. Once settled, one edit
/// costs the census one row, at either size.
#[test]
fn the_census_reads_the_rows_that_moved_and_never_the_vault() {
    for size in [1_200, 2_400] {
        let scratch = Scratch::new(&format!("coverage-cost-{size}"));
        let mut store = scratch.store();
        write_numbered(&mut store, size);
        let mut engine = scratch.engine(CountingEmbedder::new());

        let mut source = store.feed_source();
        let first = match engine
            .step(&mut source.page().expect("a page"))
            .expect("a step")
        {
            norn_semantic::DrainStep::Embed(pending) => engine
                .commit(pending.embed().expect("an embedding"))
                .expect("a commit"),
            norn_semantic::DrainStep::Done(report) => report,
        };
        assert_eq!(
            first.censused,
            norn_store::MAX_PAGE as u64,
            "{size}: one census page per step"
        );
        assert_eq!(
            engine.coverage(),
            None,
            "{size}: the census is not yet whole"
        );
        drop(source);

        assert_eq!(
            settled(&mut engine, &mut store),
            Coverage {
                embedded: size as u64,
                total: size as u64
            }
        );
        write_document(&mut store, &document("docs/0007.md", "hash-e", "edited\n"));
        let report = engine.drain(&mut store.feed_read()).expect("a drain");
        assert_eq!(report.censused, 1, "{size}: {report:?}");
        assert_eq!(engine.coverage().expect("counted").total, size as u64);
    }
}
