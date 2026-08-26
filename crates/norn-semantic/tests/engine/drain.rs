//! Drain semantics: triage by fingerprint, cursors that persist, epochs that
//! reconcile, deaths that retract.

use crate::common::{CountingEmbedder, Scratch, document, path, record_death, write_document};
use norn_store::FrontmatterValue;

/// An empty store drains to an empty sidecar; the drain after it is the
/// settled all-zero reading.
#[test]
fn an_empty_store_settles_immediately() {
    let scratch = Scratch::new("empty");
    let mut store = scratch.store();
    let mut engine = scratch.engine(CountingEmbedder::new());

    let first = engine.drain(&mut store).expect("a drain");
    assert!(first.rescan, "a fresh sidecar has no recorded epoch");
    assert_eq!(first.embedded, 0);

    let second = engine.drain(&mut store).expect("a drain");
    assert!(second.is_settled(), "{second:?}");
}

/// Derived documents are embedded once: the second drain triages every row as
/// current by fingerprint alone.
#[test]
fn derived_documents_are_embedded_once() {
    let scratch = Scratch::new("once");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));
    write_document(&mut store, &document("docs/b.md", "hash-2", "bravo\n"));

    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());
    let report = engine.drain(&mut store).expect("a drain");
    assert_eq!(report.embedded, 2);
    assert_eq!(embedder.calls(), 2);

    let report = engine.drain(&mut store).expect("a drain");
    assert!(report.is_settled(), "{report:?}");
    assert_eq!(embedder.calls(), 2, "a settled drain recomputes nothing");
}

/// A frontmatter-only edit moves the document's generation and its
/// `content_hash`, and the drain still recomputes nothing: the body's own
/// fingerprint is the input key, and it did not move.
#[test]
fn a_frontmatter_only_edit_does_not_reembed() {
    let scratch = Scratch::new("frontmatter");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));

    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());
    engine.drain(&mut store).expect("a drain");
    assert_eq!(embedder.calls(), 1);

    let mut edited = document("docs/a.md", "hash-2", "alpha\n");
    edited.frontmatter = Some(FrontmatterValue::Map(vec![(
        "title".to_string(),
        FrontmatterValue::String("Alpha".to_string()),
    )]));
    write_document(&mut store, &edited);

    let report = engine.drain(&mut store).expect("a drain");
    assert_eq!(report.skipped_current, 1, "{report:?}");
    assert_eq!(report.embedded, 0);
    assert_eq!(
        embedder.calls(),
        1,
        "the body did not change, so nothing recomputes"
    );
}

/// A body edit re-embeds, and the row's recorded input moves with it.
#[test]
fn a_body_edit_reembeds() {
    let scratch = Scratch::new("body-edit");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));

    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());
    engine.drain(&mut store).expect("a drain");
    let before = engine.projection().expect("a projection");

    write_document(
        &mut store,
        &document("docs/a.md", "hash-2", "alpha rewritten\n"),
    );
    let report = engine.drain(&mut store).expect("a drain");
    assert_eq!(report.embedded, 1);
    assert_eq!(embedder.calls(), 2);

    let after = engine.projection().expect("a projection");
    assert_eq!(after.len(), 1);
    assert_ne!(after[0].input_hash, before[0].input_hash);
    assert_ne!(after[0].values, before[0].values);
}

/// A death retracts the path's rows; a re-derivation after it embeds them
/// again.
#[test]
fn a_death_retracts_and_a_rederivation_reembeds() {
    let scratch = Scratch::new("death");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));
    write_document(&mut store, &document("docs/b.md", "hash-2", "bravo\n"));

    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());
    engine.drain(&mut store).expect("a drain");

    record_death(&mut store, "docs/a.md");
    let report = engine.drain(&mut store).expect("a drain");
    assert_eq!(report.retracted, 1, "{report:?}");
    let held: Vec<String> = engine
        .projection()
        .expect("a projection")
        .into_iter()
        .map(|row| row.path)
        .collect();
    assert_eq!(held, vec!["docs/b.md".to_string()]);

    write_document(
        &mut store,
        &document("docs/a.md", "hash-3", "alpha reborn\n"),
    );
    let report = engine.drain(&mut store).expect("a drain");
    assert_eq!(report.embedded, 1);
    assert_eq!(engine.projection().expect("a projection").len(), 2);
}

/// The cursors are the sidecar's rows, not the engine value's: a reopened
/// engine resumes where the drained feed left off.
#[test]
fn the_cursor_survives_an_engine_reopen() {
    let scratch = Scratch::new("cursor");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));
    write_document(&mut store, &document("docs/b.md", "hash-2", "bravo\n"));

    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder);
    engine.drain(&mut store).expect("a drain");
    drop(engine);

    write_document(&mut store, &document("docs/c.md", "hash-3", "charlie\n"));
    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());
    let report = engine.drain(&mut store).expect("a drain");
    assert!(!report.rescan, "the recorded epoch still names this store");
    assert_eq!(report.embedded, 1, "{report:?}");
    assert_eq!(
        report.skipped_current, 0,
        "a held cursor never re-reads the drained prefix"
    );
    assert_eq!(embedder.calls(), 1);
}

/// A store rebuilt from zero moves its epoch: the drain rescans, and the
/// content-addressed rows make the rescan recompute nothing that did not
/// change.
#[test]
fn a_store_rebuild_rescans_but_recomputes_nothing() {
    let scratch = Scratch::new("rebuild");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));
    write_document(&mut store, &document("docs/b.md", "hash-2", "bravo\n"));

    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());
    engine.drain(&mut store).expect("a drain");
    assert_eq!(embedder.calls(), 2);

    let mut store = store
        .discard_and_reopen()
        .expect("a store rebuilt from zero");
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));
    write_document(&mut store, &document("docs/b.md", "hash-2", "bravo\n"));

    let report = engine.drain(&mut store).expect("a drain");
    assert!(report.rescan, "a moved epoch is a rescan");
    assert_eq!(report.reconciled_away, 0);
    assert_eq!(report.skipped_current, 2, "{report:?}");
    assert_eq!(embedder.calls(), 2, "unchanged bodies recompute nothing");
}

/// A death the engine never saw — the store was rebuilt without the path, and
/// no tombstone survived into the new lifetime — is reconciled away by the
/// rescan.
#[test]
fn a_rescan_reconciles_paths_the_new_lifetime_does_not_hold() {
    let scratch = Scratch::new("reconcile");
    let mut store = scratch.store();
    write_document(&mut store, &document("docs/a.md", "hash-1", "alpha\n"));
    write_document(&mut store, &document("docs/b.md", "hash-2", "bravo\n"));

    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());
    engine.drain(&mut store).expect("a drain");

    let mut store = store
        .discard_and_reopen()
        .expect("a store rebuilt from zero");
    write_document(&mut store, &document("docs/b.md", "hash-2", "bravo\n"));

    let report = engine.drain(&mut store).expect("a drain");
    assert!(report.rescan);
    assert_eq!(report.reconciled_away, 1, "{report:?}");
    let held: Vec<String> = engine
        .projection()
        .expect("a projection")
        .into_iter()
        .map(|row| row.path)
        .collect();
    assert_eq!(held, vec!["docs/b.md".to_string()]);
    assert_eq!(embedder.calls(), 2, "the surviving body did not change");
}

/// The drain pages: a working set past one page arrives whole.
#[test]
fn a_drain_pages_past_its_page_bound() {
    let scratch = Scratch::new("paging");
    let mut store = scratch.store();
    // Comfortably past the drain's page bound of 256.
    for serial in 0..300 {
        let at = format!("docs/{serial:03}.md");
        let hash = format!("hash-{serial}");
        let body = format!("body {serial}\n");
        write_document(&mut store, &document(&at, &hash, &body));
    }

    let embedder = CountingEmbedder::new();
    let mut engine = scratch.engine(embedder.clone());
    let report = engine.drain(&mut store).expect("a drain");
    assert_eq!(report.embedded, 300);
    assert_eq!(engine.projection().expect("a projection").len(), 300);

    // And the path type stays honest across the page seam.
    assert!(
        engine
            .projection()
            .expect("a projection")
            .iter()
            .all(|row| {
                path(&row.path);
                true
            })
    );
}
