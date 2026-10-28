//! From-zero construction defers full text until the document rows are final.

use norn_store::{Store, StoreError, induced_failure};

use crate::common::Scratch;

fn assert_sql(store: &mut Store, predicate: &str) {
    induced_failure::execute_out_of_band(
        store,
        &format!(
            "CREATE TEMP TABLE construction_assertion (holds INTEGER NOT NULL CHECK(holds = 1));
             INSERT INTO construction_assertion SELECT {predicate};
             DROP TABLE construction_assertion"
        ),
    )
    .unwrap();
}

#[test]
fn triggers_are_absent_until_the_closing_transaction() {
    let scratch = Scratch::new("construction-triggers");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    assert_sql(
        &mut store,
        "(SELECT count(*) FROM sqlite_schema WHERE type = 'trigger' AND name LIKE 'documents_fts_%') = 0",
    );
    assert_sql(
        &mut store,
        "EXISTS(SELECT 1 FROM meta WHERE key = 'under_construction')",
    );
    assert!(matches!(
        store.begin_steps(),
        Err(StoreError::Lifecycle { .. })
    ));
}

#[test]
fn steps_require_an_empty_created_store_with_deferred_health() {
    let scratch = Scratch::new("construction-admission");
    let mut store = scratch.open();
    assert!(matches!(
        store.begin_steps(),
        Err(StoreError::Lifecycle { .. })
    ));
    store.finish_construction().unwrap();
    assert!(matches!(
        store.begin_steps(),
        Err(StoreError::Lifecycle { .. })
    ));
}

fn fill(store: &mut Store) {
    use crate::common::{document, path};
    use norn_store::{Change, ContentModel, IncrementProvenance};
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            [
                Change::Upsert(document("a.md", "a", "alpha beta beta")),
                Change::Unreadable { path: path("b.md") },
                Change::Upsert(document("c.md", "c", "gamma alpha")),
            ],
            &[],
            &ContentModel::none(),
        )
        .unwrap();
}

fn judge(store: &mut Store) {
    store
        .begin_request()
        .judge_link_health_once(&norn_store::ContentModel::none(), &mut || {})
        .unwrap();
}

#[test]
fn full_text_is_batched_and_counts_names_only_rows() {
    let scratch = Scratch::new("construction-full-text");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    fill(&mut store);
    judge(&mut store);
    let mut request = store.begin_request();
    assert!(!request.full_text_batch(2).unwrap());
    assert_eq!(request.counters().full_text_rows_indexed(), 2);
    assert!(request.full_text_batch(2).unwrap());
    assert_eq!(request.counters().full_text_rows_indexed(), 3);
    assert!(request.full_text_batch(2).unwrap());
    assert_eq!(request.counters().full_text_rows_indexed(), 3);
    request.finish();
    store.verify_integrity().unwrap();
    assert_sql(
        &mut store,
        "(SELECT count(*) FROM documents_fts_docsize) = 3",
    );
}

#[test]
fn an_unfinished_full_text_cannot_finish() {
    let scratch = Scratch::new("construction-unfinished");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    fill(&mut store);
    assert!(matches!(
        store.finish_construction(),
        Err(StoreError::Lifecycle { .. })
    ));
    assert!(matches!(
        store.begin_request().full_text_batch(2),
        Err(StoreError::Lifecycle { .. })
    ));
    judge(&mut store);
    assert!(matches!(
        store.finish_construction(),
        Err(StoreError::Lifecycle { .. })
    ));
    assert!(!store.begin_request().full_text_batch(2).unwrap());
    assert!(matches!(
        store.finish_construction(),
        Err(StoreError::Lifecycle { .. })
    ));
    assert!(store.under_construction());
    assert_sql(
        &mut store,
        "EXISTS(SELECT 1 FROM meta WHERE key = 'under_construction')",
    );
}

#[test]
fn finishing_restores_the_created_schema_digest() {
    let scratch = Scratch::new("construction-finish-schema");
    let oracle_scratch = Scratch::new("construction-finish-oracle");
    let mut oracle = oracle_scratch.open();
    fill(&mut oracle);
    oracle.finish_construction().unwrap();
    let oracle_audit = norn_store::startup_audit::audit(oracle.path(), |_, _, _| Ok(())).unwrap();
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    fill(&mut store);
    judge(&mut store);
    while !store.begin_request().full_text_batch(2).unwrap() {}
    store.finish_construction().unwrap();
    assert!(!store.under_construction());
    assert_sql(
        &mut store,
        "(SELECT count(*) FROM sqlite_schema WHERE type = 'trigger' AND name LIKE 'documents_fts_%') = 3",
    );
    assert_sql(
        &mut store,
        "NOT EXISTS(SELECT 1 FROM meta WHERE key = 'under_construction')",
    );
    store.verify_integrity().unwrap();
    let audit = norn_store::startup_audit::audit(store.path(), |_, _, _| Ok(())).unwrap();
    for table in ["documents_fts_vocab", "documents_fts_docsize"] {
        assert_eq!(
            audit["tables"][table], oracle_audit["tables"][table],
            "{table}"
        );
    }
}

#[test]
fn a_write_after_full_text_began_is_refused() {
    use crate::common::{document, path};
    use norn_store::{Change, ContentModel, IncrementProvenance};
    let scratch = Scratch::new("construction-frozen-documents");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    fill(&mut store);
    judge(&mut store);
    assert!(!store.begin_request().full_text_batch(1).unwrap());
    for change in [
        Change::Upsert(document("d.md", "d", "delta")),
        Change::Unreadable { path: path("a.md") },
        Change::Death {
            path: path("a.md"),
            provenance: norn_store::Provenance::PlanDelete,
        },
    ] {
        let result = store.begin_request().apply_increment(
            IncrementProvenance::Derived,
            [change],
            &[],
            &ContentModel::none(),
        );
        assert!(
            matches!(result, Err(StoreError::Lifecycle { .. })),
            "{result:?}"
        );
    }
    while !store.begin_request().full_text_batch(1).unwrap() {}
    let result = store.begin_request().apply_increment(
        IncrementProvenance::Derived,
        [Change::Upsert(document("d.md", "d", "delta"))],
        &[],
        &ContentModel::none(),
    );
    assert!(matches!(result, Err(StoreError::Lifecycle { .. })));
    store.finish_construction().unwrap();
    crate::common::write_document(&mut store.begin_request(), &document("d.md", "d", "delta"));
    store.verify_integrity().unwrap();
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: measuring the store's WAL sidecar.
fn wal_bytes(store: &Store) -> u64 {
    let mut wal = store.path().as_os_str().to_os_string();
    wal.push("-wal");
    std::fs::metadata(std::path::PathBuf::from(wal))
        .unwrap()
        .len()
}

#[test]
fn construction_pragmas_are_restored() {
    let scratch = Scratch::new("construction-pragmas");
    let mut store = scratch.open();
    induced_failure::execute_out_of_band(&mut store, "PRAGMA wal_autocheckpoint = 37").unwrap();
    let opened = induced_failure::construction_pragmas(&store).unwrap();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    let during = induced_failure::construction_pragmas(&store).unwrap();
    assert_eq!(during, [10_000, opened[1], opened[2]]);
    fill(&mut store);
    judge(&mut store);
    while !store.begin_request().full_text_batch(2).unwrap() {}
    assert_eq!(
        induced_failure::construction_pragmas(&store).unwrap(),
        during
    );
    assert!(!store.finish_construction().unwrap().checkpoint_busy);
    assert_eq!(
        induced_failure::construction_pragmas(&store).unwrap(),
        opened
    );
    assert_eq!(wal_bytes(&store), 0);
}

#[test]
fn a_busy_construction_checkpoint_is_not_fatal() {
    let scratch = Scratch::new("construction-checkpoint-busy");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    fill(&mut store);
    judge(&mut store);
    store.begin_request().full_text_batch(1).unwrap();
    let reader = std::sync::Arc::new(store.open_reader().reader.unwrap());
    let snapshot = reader.try_take().unwrap().establish().unwrap();
    while !store.begin_request().full_text_batch(1).unwrap() {}
    induced_failure::execute_out_of_band(&mut store, "PRAGMA busy_timeout = 0").unwrap();
    assert!(store.finish_construction().unwrap().checkpoint_busy);
    assert!(!store.under_construction());
    assert_eq!(
        induced_failure::construction_pragmas(&store).unwrap()[0],
        1000
    );
    drop(snapshot);
    store.verify_integrity().unwrap();
}

#[test]
fn a_schema_mismatch_rolls_back_the_closing_transaction() {
    let scratch = Scratch::new("construction-schema-mismatch");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    judge(&mut store);
    assert!(store.begin_request().full_text_batch(2).unwrap());
    induced_failure::execute_out_of_band(&mut store, "DROP INDEX documents_content_hash").unwrap();
    assert!(matches!(
        store.finish_construction(),
        Err(StoreError::Damaged { .. })
    ));
    assert!(store.under_construction());
    assert_sql(
        &mut store,
        "(SELECT count(*) FROM sqlite_schema WHERE type = 'trigger' AND name LIKE 'documents_fts_%') = 0",
    );
    assert_sql(
        &mut store,
        "EXISTS(SELECT 1 FROM meta WHERE key = 'under_construction')",
    );
}

#[test]
fn steps_accept_first_creation_open_time_rebuild_and_discard_and_reopen() {
    use norn_store::{OpenOutcome, StoredPathOrder};
    let scratch = Scratch::new("construction-fresh-routes");
    let mut store = scratch.open();
    assert_eq!(*store.open_outcome(), OpenOutcome::Created);
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    drop(store);
    let mut store = scratch.open();
    assert!(matches!(
        store.open_outcome(),
        OpenOutcome::RebuiltFromZero(_)
    ));
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    let mut store = store
        .discard_and_reopen(StoredPathOrder::Sensitive)
        .unwrap();
    assert_eq!(*store.open_outcome(), OpenOutcome::Created);
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    judge(&mut store);
    store.begin_request().full_text_batch(2).unwrap();
    store.finish_construction().unwrap();
    drop(store);
    let mut store = scratch.open();
    assert_eq!(*store.open_outcome(), OpenOutcome::Reused);
    assert!(matches!(
        store.begin_steps(),
        Err(StoreError::Lifecycle { .. })
    ));
}

#[test]
fn steps_refuse_documents_vault_files_or_tombstones() {
    use crate::common::{document, path, record_death, write_document};
    use norn_store::{Change, ContentModel, IncrementProvenance, Provenance};
    for contents in ["document", "file", "tombstone"] {
        let scratch = Scratch::new(&format!("construction-nonempty-{contents}"));
        let mut store = scratch.open();
        store.defer_link_health().unwrap();
        if contents == "file" {
            store
                .begin_request()
                .apply_increment(
                    IncrementProvenance::Derived,
                    [Change::FileBirth {
                        path: path("image.png"),
                    }],
                    &[],
                    &ContentModel::none(),
                )
                .unwrap();
        } else {
            write_document(&mut store.begin_request(), &document("a.md", "a", "alpha"));
            if contents == "tombstone" {
                record_death(
                    &mut store.begin_request(),
                    &path("a.md"),
                    Provenance::PlanDelete,
                );
            }
        }
        assert!(
            matches!(store.begin_steps(), Err(StoreError::Lifecycle { .. })),
            "{contents}"
        );
        assert_sql(
            &mut store,
            "(SELECT count(*) FROM sqlite_schema WHERE type = 'trigger' AND name LIKE 'documents_fts_%') = 3",
        );
        assert!(store.under_construction());
    }
}

#[test]
fn full_text_cursor_statements_seek_document_ids() {
    use norn_store::ExplainedStatement;
    let scratch = Scratch::new("construction-cursor-plans");
    let mut store = scratch.open();
    for statement in [
        ExplainedStatement::FullTextLast,
        ExplainedStatement::FullTextBatch,
        ExplainedStatement::FullTextRemaining,
    ] {
        let plan = store.begin_request().emitted_plan(statement).unwrap();
        assert!(
            plan.steps.iter().any(|step| step
                .detail
                .contains("SEARCH documents USING INTEGER PRIMARY KEY (rowid>?)")),
            "{plan:?}"
        );
        assert!(
            !plan
                .steps
                .iter()
                .any(|step| step.detail.contains("SCAN documents")
                    || step.detail.contains("USE TEMP B-TREE")),
            "{plan:?}"
        );
    }
}
