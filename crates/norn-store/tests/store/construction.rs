//! From-zero construction defers full text until the document rows are final.

use norn_store::{ExplainedStatement, Store, StoreError, induced_failure};

pub(crate) const FULL_TEXT: [ExplainedStatement<'static>; 3] = [
    ExplainedStatement::FullTextLast,
    ExplainedStatement::FullTextBatch,
    ExplainedStatement::FullTextRemaining,
];

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
    // PR C requires names/content/structure before PR A's unchanged FTS bars.
    if store.construction_step() == Some(norn_store::Step::Names) {
        store
            .begin_request()
            .apply_increment(
                IncrementProvenance::Derived,
                [
                    Change::Named { path: path("a.md") },
                    Change::Unreadable { path: path("b.md") },
                    Change::Named { path: path("c.md") },
                ],
                &[],
                &ContentModel::none(),
            )
            .unwrap();
        store.finish_step(norn_store::Step::Names).unwrap();
        let mut group = store.step_group().unwrap();
        group
            .begin_request()
            .write_content(
                &[
                    norn_store::ContentWrite {
                        facts: document("a.md", "a", "alpha beta beta"),
                        origin: norn_store::Span {
                            line: 1,
                            column: 1,
                            byte_offset: 0,
                        },
                    },
                    norn_store::ContentWrite {
                        facts: document("c.md", "c", "gamma alpha"),
                        origin: norn_store::Span {
                            line: 1,
                            column: 1,
                            byte_offset: 0,
                        },
                    },
                ],
                &[],
                &ContentModel::none(),
            )
            .unwrap();
        group.commit().unwrap();
        store.finish_step(norn_store::Step::Content).unwrap();
        let mut group = store.step_group().unwrap();
        let rows = group.begin_request().structure_page(8).unwrap();
        group
            .begin_request()
            .write_structure(&rows, &[], &ContentModel::none())
            .unwrap();
        group.commit().unwrap();
        store.finish_step(norn_store::Step::Structure).unwrap();
        return;
    }
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
    // Empty PR A fixtures still traverse PR C's three empty document steps.
    if store.construction_step() == Some(norn_store::Step::Names) {
        for step in [
            norn_store::Step::Names,
            norn_store::Step::Content,
            norn_store::Step::Structure,
        ] {
            store.finish_step(step).unwrap();
        }
    }
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
    let scratch = Scratch::new("construction-cursor-plans");
    let mut store = scratch.open();
    for statement in FULL_TEXT {
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

/// A provisional name is legal only in names; rich changes never enter an open build.
#[test]
fn named_and_rich_writes_are_refused_outside_their_step() {
    use crate::common::{document, path};
    use norn_store::{Change, ContentModel, IncrementProvenance, Step};
    let scratch = Scratch::new("construction-write-admission");
    let mut store = scratch.open();
    let apply = |store: &mut Store, changes| {
        store.begin_request().apply_increment(
            IncrementProvenance::Derived,
            changes,
            &[],
            &ContentModel::none(),
        )
    };
    assert!(matches!(
        apply(&mut store, vec![Change::Named { path: path("a.md") }]),
        Err(StoreError::Lifecycle { .. })
    ));
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    assert!(matches!(
        apply(
            &mut store,
            vec![Change::Upsert(document("a.md", "a", "body"))]
        ),
        Err(StoreError::Lifecycle { .. })
    ));
    assert!(matches!(
        apply(
            &mut store,
            vec![Change::Death {
                path: path("a.md"),
                provenance: norn_store::Provenance::HealPrune
            }]
        ),
        Err(StoreError::Lifecycle { .. })
    ));
    apply(&mut store, vec![Change::Named { path: path("a.md") }]).unwrap();
    store.finish_step(Step::Names).unwrap();
    assert!(matches!(
        apply(&mut store, vec![Change::Named { path: path("b.md") }]),
        Err(StoreError::Lifecycle { .. })
    ));
}

/// Withdrawal removes provisional identity and progress, without inventing a death.
#[test]
fn a_withdrawal_records_no_death() {
    use crate::common::path;
    use norn_store::{Change, ContentModel, IncrementProvenance, Step};
    let scratch = Scratch::new("construction-withdrawal");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Named {
                path: path("gone.md"),
            }],
            &[],
            &ContentModel::none(),
        )
        .unwrap();
    store.finish_step(Step::Names).unwrap();
    {
        let mut group = store.step_group().unwrap();
        group
            .begin_request()
            .withdraw_content(&path("gone.md"))
            .unwrap();
        group.commit().unwrap();
    }
    assert!(
        store
            .begin_request()
            .stored_document(&path("gone.md"))
            .unwrap()
            .is_none()
    );
    assert!(store.begin_request().content_page(8).unwrap().is_empty());
    assert_sql(&mut store, "NOT EXISTS(SELECT 1 FROM tombstones)");
}

/// A failed chunk cannot leave earlier chunks of its transaction committed.
#[test]
fn a_dropped_step_group_rolls_back_its_chunks() {
    use crate::common::{document, path};
    use norn_store::{Change, ContentModel, ContentWrite, IncrementProvenance, Span, Step};
    let scratch = Scratch::new("construction-group-rollback");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Named { path: path("a.md") }],
            &[],
            &ContentModel::none(),
        )
        .unwrap();
    store.finish_step(Step::Names).unwrap();
    {
        let mut group = store.step_group().unwrap();
        group
            .begin_request()
            .write_content(
                &[ContentWrite {
                    facts: document("a.md", "hash", "body"),
                    origin: Span {
                        line: 1,
                        column: 1,
                        byte_offset: 0,
                    },
                }],
                &[],
                &ContentModel::none(),
            )
            .unwrap();
    }
    assert!(store.begin_request().content_owed(&path("a.md")).unwrap());
    assert!(
        store
            .begin_request()
            .stored_document(&path("a.md"))
            .unwrap()
            .unwrap()
            .content_hash
            .is_none()
    );
}

/// An arbitrary single-path set can finish ahead of the vault pass; stage
/// selection then omits it and a repeated writer records no second fact.
#[test]
fn a_document_run_ahead_is_not_written_twice() {
    use crate::common::{document_with_every_fact, path};
    use norn_store::{
        Change, ContentModel, ContentWrite, IncrementProvenance, Span, Step, TagSource,
    };
    let scratch = Scratch::new("construction-ahead");
    let mut store = scratch.open();
    let mut facts = document_with_every_fact("a.md", "hash");
    facts.links[0].field = Some("ref".into());
    facts.tags[0].source = TagSource::Frontmatter;
    let front_links = facts.links.iter().filter(|l| l.field.is_some()).count();
    let front_tags = facts
        .tags
        .iter()
        .filter(|t| t.source == TagSource::Frontmatter)
        .count();
    let mut content = facts.clone();
    content.links.retain(|l| l.field.is_some());
    content.tags.retain(|t| t.source == TagSource::Frontmatter);
    content.headings.clear();
    content.blocks.clear();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Named { path: path("a.md") }],
            &[],
            &ContentModel::none(),
        )
        .unwrap();
    store.finish_step(Step::Names).unwrap();
    let mut group = store.step_group().unwrap();
    group
        .begin_request()
        .write_content(
            &[ContentWrite {
                facts: content,
                origin: Span {
                    line: 4,
                    column: 1,
                    byte_offset: facts.body_offset,
                },
            }],
            &[],
            &ContentModel::none(),
        )
        .unwrap();
    let mut rows = vec![
        group
            .begin_request()
            .structure_document(&path("a.md"))
            .unwrap()
            .unwrap(),
    ];
    assert_eq!(
        (rows[0].frontmatter_links, rows[0].frontmatter_tags),
        (front_links, front_tags)
    );
    rows[0].facts = facts.clone();
    // The structure step's own finding rides with its rows: a repeated call
    // for a document past the step files it no second time.
    let structure_finding = [norn_store::DerivedFinding {
        facts: crate::common::violation("a.md"),
        replaces: None,
        bytes_hash: None,
    }];
    let mut request = group.begin_request();
    request
        .write_structure(&rows, &structure_finding, &ContentModel::none())
        .unwrap();
    request
        .write_structure(&rows, &structure_finding, &ContentModel::none())
        .unwrap();
    assert_eq!(request.counters().structure_rows_written(), 1);
    request.finish();
    group.commit().unwrap();
    let filed: i64 = norn_db::open_read_only(store.path())
        .adopted
        .unwrap()
        .connection()
        .query_row(
            "SELECT count(*) FROM findings WHERE path = 'a.md'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        filed, 1,
        "a repeated structure write filed its finding again"
    );
    assert!(store.begin_request().content_page(8).unwrap().is_empty());
    assert!(store.begin_request().structure_page(8).unwrap().is_empty());
    let got = store
        .begin_request()
        .stored_facts(&facts.path)
        .unwrap()
        .unwrap();
    let oracle_scratch = Scratch::new("construction-ahead-oracle");
    let mut oracle = oracle_scratch.open();
    crate::common::write_document(&mut oracle.begin_request(), &facts);
    let expected = oracle
        .begin_request()
        .stored_facts(&facts.path)
        .unwrap()
        .unwrap();
    assert_eq!(got.links, expected.links);
    assert_eq!(got.tags, expected.tags);
    assert_eq!(got.headings, expected.headings);
    assert_eq!(got.blocks, expected.blocks);
    assert_eq!(got.fields, expected.fields);
}

#[test]
fn names_keep_path_ids_and_withdrawal_leaves_one_gap_on_both_orders() {
    use crate::common::{document, path};
    use norn_store::{Change, ContentModel, IncrementProvenance, Step, StoredPathOrder};
    for order in [
        StoredPathOrder::Sensitive,
        StoredPathOrder::AsciiCaseInsensitive,
    ] {
        let scratch = Scratch::new("construction-name-ids");
        let oracle_scratch = Scratch::new("construction-name-ids-oracle");
        let mut store = scratch.open_under(order);
        let mut oracle = oracle_scratch.open_under(order);
        store.defer_link_health().unwrap();
        store.begin_steps().unwrap();
        let paths = ["a.md", "b.md", "c.md"];
        store
            .begin_request()
            .apply_increment(
                IncrementProvenance::Derived,
                paths.map(|p| Change::Named { path: path(p) }),
                &[],
                &ContentModel::none(),
            )
            .unwrap();
        oracle
            .begin_request()
            .apply_increment(
                IncrementProvenance::Derived,
                paths.map(|p| Change::Upsert(document(p, "hash", "body"))),
                &[],
                &ContentModel::none(),
            )
            .unwrap();
        for target in [&mut store, &mut oracle] {
            assert_sql(
                target,
                "(SELECT group_concat(path || ':' || id, ',') FROM (SELECT path, id FROM documents ORDER BY id)) = 'a.md:1,b.md:2,c.md:3'",
            );
        }
        store.finish_step(Step::Names).unwrap();
        let mut group = store.step_group().unwrap();
        group
            .begin_request()
            .withdraw_content(&path("b.md"))
            .unwrap();
        group.commit().unwrap();
        assert_sql(
            &mut store,
            "(SELECT group_concat(id, ',') FROM (SELECT id FROM documents ORDER BY id)) = '1,3'",
        );
    }
}

pub(crate) const PROGRESS: [ExplainedStatement<'static>; 4] = [
    ExplainedStatement::ConstructionContentPage,
    ExplainedStatement::ConstructionStructurePage,
    ExplainedStatement::ConstructionFrontmatterTags,
    ExplainedStatement::ConstructionStructureDocument,
];

/// Queue pages seek the stage index and each document; frontmatter tags seek
/// their document/ordinal index. Neither pass scans the vault's stored facts.
#[test]
fn construction_pages_seek_their_progress_and_document_indexes() {
    let scratch = Scratch::new("construction-page-plans");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    for statement in PROGRESS {
        let plan = store.begin_request().emitted_plan(statement).unwrap();
        assert!(
            plan.steps
                .iter()
                .any(|step| step.detail.starts_with("SEARCH ")),
            "{plan:?}"
        );
        assert!(
            !plan
                .steps
                .iter()
                .any(|step| step.detail.starts_with("SCAN ")
                    || step.detail.contains("USE TEMP B-TREE")),
            "{plan:?}"
        );
    }
}

/// Host chunks can share a transaction up to exactly 2,048 documents; the
/// next chunk must commit separately, without a savepoint or partial write.
#[test]
fn a_step_group_never_exceeds_2048_documents() {
    use crate::common::path;
    use norn_store::{
        Change, ContentModel, ContentWrite, DocumentFacts, IncrementProvenance, Span, Step,
    };
    let scratch = Scratch::new("construction-group-bound");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    for range in [0..1024, 1024..2050] {
        store
            .begin_request()
            .apply_increment(
                IncrementProvenance::Derived,
                range.map(|n| Change::Named {
                    path: path(&format!("doc-{n:04}.md")),
                }),
                &[],
                &ContentModel::none(),
            )
            .unwrap();
    }
    store.finish_step(Step::Names).unwrap();
    let rows = |paths: Vec<norn_store::DocumentPath>| {
        paths
            .into_iter()
            .map(|path| ContentWrite {
                facts: DocumentFacts::new(path, "hash", "body", 4),
                origin: Span {
                    line: 1,
                    column: 1,
                    byte_offset: 0,
                },
            })
            .collect::<Vec<_>>()
    };
    let mut group = store.step_group().unwrap();
    for _ in 0..2 {
        let chunk = rows(group.begin_request().content_page(1024).unwrap());
        assert_eq!(chunk.len(), 1024);
        group
            .begin_request()
            .write_content(&chunk, &[], &ContentModel::none())
            .unwrap();
    }
    let last = rows(group.begin_request().content_page(1024).unwrap());
    assert_eq!(last.len(), 2);
    assert!(matches!(
        group
            .begin_request()
            .write_content(&last, &[], &ContentModel::none()),
        Err(StoreError::Bound { limit: 2048, .. })
    ));
    group.commit().unwrap();
    let mut group = store.step_group().unwrap();
    group
        .begin_request()
        .write_content(&last, &[], &ContentModel::none())
        .unwrap();
    group.commit().unwrap();
    assert!(store.begin_request().content_page(1024).unwrap().is_empty());
    store.finish_step(Step::Content).unwrap();
}
