//! How far the engine has got: the two feeds' watermarks against the store, and
//! the sidecar revision an answer is identified by.

use norn_semantic::{Engine, RebuildReason, SidecarOutcome, SidecarRevision, Watermark};
use norn_store::Store;
use norn_store::induced_failure;

use crate::common::{
    CountingEmbedder, InterferingEmbedder, RefusingEmbedder, Scratch, document, record_death,
    write_document,
};

/// The store's last committed write generation, read the way a drain reads it.
fn store_generation(store: &mut Store) -> i64 {
    store
        .feed_read()
        .write_generation()
        .expect("the store's write generation")
}

/// The watermark a drain that completed now would record.
fn caught_up(store: &mut Store) -> Watermark {
    Watermark {
        store_epoch: store.epoch().to_string(),
        generation: store_generation(store),
    }
}

/// Drain, and answer the revision the drain left.
fn drained(engine: &mut Engine, store: &mut Store) -> SidecarRevision {
    engine.drain(&mut store.feed_read()).expect("a drain");
    engine.revision()
}

/// **The revision moves with every committed sidecar mutation, and a drain that
/// commits nothing leaves it where it was.** A drained page, a watermark that
/// advanced over an empty feed, and a retraction each commit, so each moves it;
/// a drain over a quiescent store commits nothing, so an answer taken before it
/// and one taken after it name the same sidecar state.
#[test]
fn the_revision_moves_with_every_committed_mutation_and_only_then() {
    let scratch = Scratch::new("revision");
    let mut store = scratch.store();
    let mut engine = scratch.engine(CountingEmbedder::new());
    let created = engine.revision();
    assert_eq!(
        created.revision, 0,
        "a created sidecar has committed nothing"
    );
    assert_eq!(created.epoch, engine.epoch());

    // The first drain reconciles and completes two empty feeds.
    let first = drained(&mut engine, &mut store);
    assert!(first.revision > created.revision, "{first:?}");

    // Negative control: nothing moved in the store, so nothing commits.
    assert_eq!(
        drained(&mut engine, &mut store),
        first,
        "an idle drain committed"
    );

    // A drained page.
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));
    let paged = drained(&mut engine, &mut store);
    assert!(paged.revision > first.revision, "{paged:?}");

    // An empty drain that advances the watermarks: the schema pin takes a
    // generation and presents no feed row.
    store
        .begin_request()
        .pin_vault_schema(b"schema", "fingerprint")
        .expect("a schema pin");
    let advanced = drained(&mut engine, &mut store);
    assert!(advanced.revision > paged.revision, "{advanced:?}");

    // A retraction.
    record_death(&mut store, "docs/a.md");
    let retracted = drained(&mut engine, &mut store);
    assert!(retracted.revision > advanced.revision, "{retracted:?}");
    assert!(engine.projection().expect("a projection").is_empty());

    assert_eq!(
        drained(&mut engine, &mut store),
        retracted,
        "an idle drain committed"
    );
}

/// **A rebuild is a different sidecar, so the pair differs even where the count
/// starts over.** The epoch moves, and the revision the rebuilt sidecar starts
/// from is its own.
#[test]
fn a_rebuilt_sidecar_answers_from_another_revision_pair() {
    let scratch = Scratch::new("revision-rebuild");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));
    let mut engine = scratch.engine(CountingEmbedder::new());
    let before = drained(&mut engine, &mut store);

    let mut engine = engine.discard_and_reopen().expect("a discard");
    let rebuilt = engine.revision();
    assert_ne!(rebuilt, before);
    assert_ne!(rebuilt.epoch, before.epoch, "a rebuild mints a new epoch");
    assert_eq!(rebuilt.revision, 0);
    assert_eq!(
        engine.watermarks(),
        &Default::default(),
        "a rebuilt sidecar has drained nothing"
    );

    let after = drained(&mut engine, &mut store);
    assert!(after.revision > rebuilt.revision);
    assert_eq!(
        after.epoch, rebuilt.epoch,
        "a drain does not move the epoch"
    );
}

/// **A completed drain's watermarks are the store generation at its end, and an
/// empty feed advances its own.** The tombstone feed is empty throughout the
/// first half and still catches up to every document write; both feeds catch
/// up to a write neither presents.
#[test]
fn a_completed_drain_catches_both_watermarks_up_to_the_store() {
    let scratch = Scratch::new("watermark");
    let mut store = scratch.store();
    let mut engine = scratch.engine(CountingEmbedder::new());
    assert_eq!(engine.watermarks().documents, None);
    assert_eq!(engine.watermarks().tombstones, None);

    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));
    write_document(&mut store, &document("docs/b.md", "hash-2", "bravo\n"));
    engine.drain(&mut store.feed_read()).expect("a drain");
    let expected = Some(caught_up(&mut store));
    assert_eq!(engine.watermarks().documents, expected);
    assert_eq!(
        engine.watermarks().tombstones,
        expected,
        "an empty tombstone feed did not advance its watermark"
    );

    store
        .begin_request()
        .pin_vault_schema(b"schema", "fingerprint")
        .expect("a schema pin");
    engine.drain(&mut store.feed_read()).expect("a drain");
    let expected = Some(caught_up(&mut store));
    assert_eq!(engine.watermarks().documents, expected);
    assert_eq!(engine.watermarks().tombstones, expected);

    record_death(&mut store, "docs/b.md");
    engine.drain(&mut store.feed_read()).expect("a drain");
    let expected = Some(caught_up(&mut store));
    assert_eq!(engine.watermarks().documents, expected);
    assert_eq!(engine.watermarks().tombstones, expected);
}

/// **A write landing while the completing page is embedded reads as lag.** A
/// writer lands a newer body for the page being embedded: the completed
/// document feed records a generation one short of the store, while the
/// tombstone feed — completing after the write — records the store's, and the
/// next drain presents the write and catches up.
#[test]
fn a_write_landing_during_the_drain_reads_as_lag() {
    let scratch = Scratch::new("watermark-race");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));
    let before = store_generation(&mut store);

    let embedder = InterferingEmbedder::rewriting(
        scratch.store_path(),
        document("docs/a.md", "hash-2", "alpha, rewritten\n"),
    );
    let mut engine = scratch.engine(embedder);
    engine.drain(&mut store.feed_read()).expect("a drain");

    let after = store_generation(&mut store);
    assert!(after > before, "the interference took a generation");
    let documents = engine.watermarks().documents.clone().expect("a watermark");
    assert_eq!(
        documents.generation, before,
        "the document watermark claims a write its feed never presented"
    );
    let tombstones = engine.watermarks().tombstones.clone().expect("a watermark");
    assert_eq!(tombstones.generation, after);

    engine
        .drain(&mut store.feed_read())
        .expect("the next drain");
    assert_eq!(
        engine.watermarks().documents,
        Some(caught_up(&mut store)),
        "the next drain presents the write and catches up"
    );
}

/// **A watermark is read before the page that completes its feed.** A write
/// committed right after that page is read is one the page never presented, so
/// the watermark must stand below it and the write read as lag. A watermark
/// read after the page would record the write's generation and claim it
/// covered. The write takes a generation without presenting a feed row — a
/// schema pin — so the next drain's completion is what covers it.
#[test]
fn a_write_after_the_completing_page_is_read_reads_as_lag() {
    // A drain over a store holding one document reads two pages: the
    // document feed's completing page, then the tombstone feed's.
    for (label, page) in [("documents", 1), ("tombstones", 2)] {
        let scratch = Scratch::new(&format!("watermark-order-{label}"));
        let mut store = scratch.store();
        write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));
        let before = store_generation(&mut store);
        let mut engine = scratch.engine(CountingEmbedder::new());

        induced_failure::write_after_feed_pages(page, |store| {
            store
                .begin_request()
                .pin_vault_schema(b"schema", "fingerprint")
                .expect("a schema pin");
        });
        engine.drain(&mut store.feed_read()).expect("a drain");

        // The arm is one-shot, so a generation that moved says it fired and
        // left nothing standing on this thread.
        let after = store_generation(&mut store);
        assert!(
            after > before,
            "{label}: the armed write took no generation"
        );
        let watermarks = engine.watermarks().clone();
        let (landed, other) = match label {
            "documents" => (watermarks.documents, watermarks.tombstones),
            _ => (watermarks.tombstones, watermarks.documents),
        };
        assert_eq!(
            landed.expect("a watermark").generation,
            before,
            "{label}: the watermark claims a write its completing page never presented"
        );
        let other = other.expect("a watermark").generation;
        match label {
            "documents" => assert_eq!(other, after, "the later feed read after the write"),
            _ => assert_eq!(other, before, "the earlier feed read before the write"),
        }

        engine
            .drain(&mut store.feed_read())
            .expect("the next drain");
        assert_eq!(engine.watermarks().documents, Some(caught_up(&mut store)));
        assert_eq!(engine.watermarks().tombstones, Some(caught_up(&mut store)));
    }
}

/// **A watermark names the store lifetime it was taken in.** Across a store
/// rebuild the recorded watermarks keep the old epoch until a drain completes
/// in the new one, so nothing compares a generation across two databases.
#[test]
fn a_watermark_carries_the_store_epoch_it_was_taken_in() {
    let scratch = Scratch::new("watermark-epoch");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));
    let mut engine = scratch.engine(CountingEmbedder::new());
    engine.drain(&mut store.feed_read()).expect("a drain");
    let old_epoch = store.epoch().to_string();

    let mut store = store
        .discard_and_reopen(norn_store::StoredPathOrder::Sensitive)
        .expect("a store rebuilt from zero");
    assert_ne!(store.epoch(), old_epoch);
    let recorded = engine.watermarks().documents.clone().expect("a watermark");
    assert_eq!(recorded.store_epoch, old_epoch);

    engine.drain(&mut store.feed_read()).expect("a drain");
    assert_eq!(engine.watermarks().documents, Some(caught_up(&mut store)));
    assert_eq!(engine.watermarks().tombstones, Some(caught_up(&mut store)));
}

/// **Progress is recorded, not remembered.** A reopened engine answers the
/// revision and the watermarks its sidecar committed, so a restart does not
/// read as a sidecar that drained nothing. The second drain completes both
/// feeds on pages that carry rows — a document and a death — so each
/// watermark it leaves was committed with a page's writes rather than alone.
#[test]
fn a_reopened_engine_answers_the_progress_its_sidecar_recorded() {
    let scratch = Scratch::new("progress-reopen");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));
    let mut engine = scratch.engine(CountingEmbedder::new());
    engine.drain(&mut store.feed_read()).expect("a drain");

    write_document(&mut store, &document("docs/b.md", "hash-2", "bravo\n"));
    record_death(&mut store, "docs/a.md");
    engine.drain(&mut store.feed_read()).expect("a drain");
    let revision = engine.revision();
    let watermarks = engine.watermarks().clone();
    assert_eq!(watermarks.documents, Some(caught_up(&mut store)));
    assert_eq!(watermarks.tombstones, Some(caught_up(&mut store)));
    drop(engine);

    let engine = scratch.engine(CountingEmbedder::new());
    assert_eq!(engine.open_outcome(), &SidecarOutcome::Reused);
    assert_eq!(engine.revision(), revision);
    assert_eq!(engine.watermarks(), &watermarks);
}

/// Write `statement` on a direct connection to the sidecar, outside every
/// handle the engine holds.
fn out_of_band(path: &std::path::Path, statement: &str) {
    match norn_db::connect(path).expect("connecting to the sidecar") {
        norn_db::Attempt::Connected(connection) => {
            connection.execute(statement, []).expect(statement);
        }
        norn_db::Attempt::Unreadable { detail } => panic!("the sidecar is unreadable: {detail}"),
    }
}

/// **Progress that will not read is a rebuild, not a refusal.** A sidecar
/// recording no revision, or a watermark no reader of this build wrote, is a
/// file this build did not write, and resolves by the rebuild floor.
#[test]
fn recorded_progress_that_will_not_read_is_rebuilt_from_zero() {
    for (label, statement) in [
        (
            "no-revision",
            "DELETE FROM meta WHERE key = 'write_generation'",
        ),
        (
            "watermark",
            "INSERT INTO meta (key, value) VALUES ('document_watermark', 'no separator')",
        ),
        (
            "watermark-generation",
            "INSERT INTO meta (key, value) VALUES ('tombstone_watermark', 'x:epoch')",
        ),
        (
            "watermark-epoch",
            "INSERT INTO meta (key, value) VALUES ('document_watermark', '12:')",
        ),
    ] {
        let scratch = Scratch::new(&format!("progress-unreadable-{label}"));
        drop(scratch.engine(CountingEmbedder::new()));
        out_of_band(&scratch.sidecar_path(), statement);

        let engine = scratch.engine(CountingEmbedder::new());
        match engine.open_outcome() {
            SidecarOutcome::RebuiltFromZero(RebuildReason::Client { detail }) => {
                assert!(!detail.is_empty(), "{label}: the rebuild named nothing");
            }
            other => panic!("{label}: the sidecar opened as {other:?} rather than rebuilding"),
        }
        assert_eq!(engine.revision().revision, 0, "{label}");
    }
}

/// **A drain that fails still answers the revision of what it committed.** The
/// reconcile after a store rebuild commits on its own, so a drain whose embed
/// then refuses has moved the sidecar — the reconciled-away row is gone — and
/// an answer taken after it must not name the state before it.
#[test]
fn a_drain_that_fails_after_its_reconcile_answers_the_reconciled_revision() {
    let scratch = Scratch::new("revision-reconcile");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));
    let mut engine = scratch.engine(CountingEmbedder::new());
    engine.drain(&mut store.feed_read()).expect("a drain");
    drop(engine);

    let mut store = store
        .discard_and_reopen(norn_store::StoredPathOrder::Sensitive)
        .expect("a store rebuilt from zero");
    write_document(&mut store, &document("docs/c.md", "hash-3", "charlie\n"));

    let mut engine = scratch.engine(RefusingEmbedder::new());
    let before = engine.revision();
    engine
        .drain(&mut store.feed_read())
        .expect_err("the embedder refuses");
    assert!(
        engine.projection().expect("a projection").is_empty(),
        "the reconcile committed"
    );
    assert!(
        engine.revision().revision > before.revision,
        "the revision does not name the reconcile the sidecar committed"
    );
}
