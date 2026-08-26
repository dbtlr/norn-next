//! The sidecar's own lifecycle: create, adopt, rebuild, discard.

use crate::common::{CountingEmbedder, Scratch, document, write_document};
use norn_semantic::{Engine, SidecarOutcome};

/// A first open creates; a second adopts what the first wrote, under the same
/// epoch.
#[test]
fn a_reopened_sidecar_is_adopted_under_its_epoch() {
    let scratch = Scratch::new("adopt");
    let embedder = CountingEmbedder::new();

    let engine = scratch.engine(embedder.clone());
    assert_eq!(engine.open_outcome(), &SidecarOutcome::Created);
    let epoch = engine.epoch().to_string();
    drop(engine);

    let engine = scratch.engine(embedder);
    assert_eq!(engine.open_outcome(), &SidecarOutcome::Reused);
    assert_eq!(engine.epoch(), epoch);
}

/// A file that is not a sidecar this build wrote is removed and created from
/// zero, and the open reports what it found.
#[test]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging a foreign file out of band.
fn a_foreign_file_is_rebuilt_from_zero() {
    let scratch = Scratch::new("foreign");
    let parent = scratch.sidecar_path();
    std::fs::create_dir_all(parent.parent().expect("a parent")).expect("the derived directory");
    std::fs::write(&parent, b"not a database at all").expect("a foreign file");

    let engine = scratch.engine(CountingEmbedder::new());
    assert!(
        matches!(
            engine.open_outcome(),
            SidecarOutcome::RebuiltFromZero { .. }
        ),
        "{:?}",
        engine.open_outcome()
    );
    assert_eq!(engine.projection().expect("a projection"), Vec::new());
}

/// Discarding moves the epoch, empties the rows, and resets the cursors, so
/// the next drain recomputes exactly what a fresh sidecar is missing.
#[test]
fn a_discarded_sidecar_starts_over() {
    let scratch = Scratch::new("discard");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));
    write_document(&mut store, &document("docs/b.md", "hash-2", "bravo\n"));

    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());
    engine.drain(&mut store.feed_read()).expect("a drain");
    assert_eq!(embedder.calls(), 2);
    let epoch = engine.epoch().to_string();

    let mut engine = engine.discard_and_reopen().expect("a discard");
    assert_ne!(engine.epoch(), epoch, "a rebuild mints a new epoch");
    assert_eq!(engine.projection().expect("a projection"), Vec::new());

    let report = engine.drain(&mut store.feed_read()).expect("a drain");
    assert_eq!(
        report.embedded, 2,
        "the drain refills what the discard emptied"
    );
    assert_eq!(embedder.calls(), 4);
}

/// The engine only ever opens the path it is handed — proven here by the
/// engine and the store living beside each other and neither disturbing the
/// other's file.
#[test]
fn the_sidecar_and_the_store_are_separate_files() {
    let scratch = Scratch::new("separate");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));

    let mut engine = scratch.engine(CountingEmbedder::new());
    engine.drain(&mut store.feed_read()).expect("a drain");
    drop(engine);

    // The store is untouched by everything the engine did.
    store
        .verify_integrity()
        .expect("a store the engine only read");
    assert_ne!(scratch.store_path(), scratch.sidecar_path());
}

/// An engine open never creates or repairs the store: a sidecar is openable
/// with no store at all.
#[test]
fn an_engine_opens_without_a_store() {
    let scratch = Scratch::new("storeless");
    let engine = Engine::open(scratch.sidecar_path(), CountingEmbedder::new())
        .expect("opening an engine with no store");
    assert_eq!(engine.projection().expect("a projection"), Vec::new());
}
