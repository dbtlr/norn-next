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
    let schema_text = |store: &Store| {
        let opened = norn_db::open_read_only(store.path()).adopted.unwrap();
        opened
            .connection()
            .prepare("SELECT type, name, sql FROM sqlite_schema ORDER BY type, name")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(schema_text(&store), schema_text(&oracle));
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
    induced_failure::execute_out_of_band(&mut store, "DROP INDEX documents_path").unwrap();
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
    apply(
        &mut store,
        vec![Change::Death {
            path: path("a.md"),
            provenance: norn_store::Provenance::HealPrune,
        }],
    )
    .unwrap();
    apply(&mut store, vec![Change::Named { path: path("a.md") }]).unwrap();
    store.finish_step(Step::Names).unwrap();
    apply(&mut store, vec![Change::Named { path: path("b.md") }]).unwrap();
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

#[test]
fn interrupted_steps_are_construction_not_damage_and_refuse_throwaway() {
    use norn_store::{OpenOutcome, RebuildReason, StoredPathOrder};
    let scratch = Scratch::new("interrupted-steps-adoption");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    drop(store);
    assert!(matches!(
        Store::open_throwaway(
            scratch.database(),
            StoredPathOrder::Sensitive,
            crate::common::DERIVATION
        ),
        Err(StoreError::Lifecycle { .. })
    ));
    let store = scratch.open();
    assert!(
        matches!(store.open_outcome(), OpenOutcome::RebuiltFromZero(RebuildReason::Client { detail }) if detail.contains("construction never finished")),
        "{:?}",
        store.open_outcome()
    );
}

#[test]
fn deferred_indexes_appear_at_their_step() {
    use norn_store::Step;
    let scratch = Scratch::new("deferred-index-boundaries");
    let mut store = scratch.open();
    let indexes = |store: &Store| {
        let opened = norn_db::open_read_only(store.path()).adopted.unwrap();
        let connection = opened.connection();
        connection.prepare("SELECT name FROM sqlite_schema WHERE type = 'index' AND sql LIKE 'CREATE INDEX %' AND tbl_name IN ('documents','vault_files','document_fields','link_keys','headings','blocks','document_tags','document_trigrams') ORDER BY name").unwrap().query_map([], |r| r.get::<_,String>(0)).unwrap().collect::<Result<Vec<_>,_>>().unwrap()
    };
    let original = indexes(&store);
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    // Kept throughout construction: two that steps 1 and 2 seek, and four whose
    // bulk-built packing would cost every later hub rename extra WAL pages.
    assert_eq!(
        indexes(&store),
        [
            "document_fields_least_raw",
            "document_fields_raw",
            "document_fields_value_type",
            "document_trigrams_document_trigram",
            "documents_path_key",
            "documents_path_nocase"
        ]
    );
    for (step, count) in [
        (Step::Names, 12),
        (Step::Content, 17),
        (Step::Structure, 24),
    ] {
        store.finish_step(step).unwrap();
        assert_eq!(indexes(&store).len(), count, "{step:?}");
    }
    judge(&mut store);
    assert!(store.begin_request().full_text_batch(128).unwrap());
    assert_eq!(indexes(&store), original);
    store.finish_construction().unwrap();
}

#[test]
fn construction_write_census_scans_no_vault_sized_table() {
    use norn_store::{Step, WriteStatement};
    let scratch = Scratch::new("construction-write-census");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    let mut scans = Vec::new();
    for phase in [Step::Names, Step::Content, Step::Structure] {
        for statement in WriteStatement::all() {
            let plan = store
                .begin_request()
                .emitted_plan(ExplainedStatement::Write(statement))
                .unwrap();
            for row in plan.steps.iter().filter(|s| {
                s.detail.starts_with("SCAN ") && !s.detail.starts_with("SCAN CONSTANT ROW")
            }) {
                scans.push(format!("{phase:?} {statement:?}: {}", row.detail));
            }
        }
        store.finish_step(phase).unwrap();
    }
    assert!(scans.is_empty(), "{scans:#?}");
}

#[test]
fn finish_defensively_restores_a_missing_deferred_index() {
    let scratch = Scratch::new("construction-defensive-index");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    judge(&mut store);
    assert!(store.begin_request().full_text_batch(2).unwrap());
    induced_failure::execute_out_of_band(&mut store, "DROP INDEX documents_content_hash").unwrap();
    store.finish_construction().unwrap();
    drop(store);
    assert!(matches!(
        scratch.open().open_outcome(),
        norn_store::OpenOutcome::Reused
    ));
}

#[test]
fn construction_page_read_steps_per_document_stay_flat_at_four_times_the_vault() {
    use crate::common::path;
    use norn_store::{
        Change, ContentModel, ContentWrite, DocumentFacts, IncrementProvenance, Span, Step,
    };
    let measure = |n: usize| {
        let scratch = Scratch::new(&format!("construction-flat-{n}"));
        let mut store = scratch.open();
        store.defer_link_health().unwrap();
        store.begin_steps().unwrap();
        for start in (0..n).step_by(128) {
            store
                .begin_request()
                .apply_increment(
                    IncrementProvenance::Derived,
                    (start..(start + 128).min(n)).map(|i| Change::Named {
                        path: path(&format!("document-{i:05}.md")),
                    }),
                    &[],
                    &ContentModel::none(),
                )
                .unwrap();
        }
        store.finish_step(Step::Names).unwrap();
        let mut content_steps = 0;
        loop {
            let mut group = store.step_group().unwrap();
            let mut request = group.begin_request();
            let rows = request.content_page(128).unwrap();
            content_steps += request.read_steps();
            let rows: Vec<_> = rows
                .into_iter()
                .map(|path| ContentWrite {
                    facts: DocumentFacts::new(path, "hash", "body", 4),
                    origin: Span {
                        line: 1,
                        column: 1,
                        byte_offset: 0,
                    },
                })
                .collect();
            let empty = rows.is_empty();
            request
                .write_content(&rows, &[], &ContentModel::none())
                .unwrap();
            request.finish();
            group.commit().unwrap();
            if empty {
                break;
            }
        }
        store.finish_step(Step::Content).unwrap();
        let mut structure_steps = 0;
        loop {
            let mut group = store.step_group().unwrap();
            let mut request = group.begin_request();
            let mut rows = request.structure_page(128).unwrap();
            for input in &mut rows {
                request.load_structure_tags(input).unwrap();
            }
            structure_steps += request.read_steps();
            let empty = rows.is_empty();
            request
                .write_structure(&rows, &[], &ContentModel::none())
                .unwrap();
            request.finish();
            group.commit().unwrap();
            if empty {
                break;
            }
        }
        store.finish_step(Step::Structure).unwrap();
        [
            content_steps as f64 / n as f64,
            structure_steps as f64 / n as f64,
        ]
    };
    let small = measure(256);
    let large = measure(1024);
    for (small, large) in small.into_iter().zip(large) {
        assert!(
            small > 0.0 && large <= small * 1.02,
            "read steps/document {small} -> {large}"
        );
    }
}

/// A target's construction candidates are its resolution class with the step
/// each still owes: exact-path precedence hides suffix candidates until the
/// exact one is withdrawn, and a class over the limit is not listed.
#[test]
fn target_candidates_bound_and_total() {
    use crate::common::{document, path};
    use norn_store::{
        AmbiguityIgnore, Change, ContentModel, ContentWrite, IncrementProvenance, Span, Step,
    };
    let scratch = Scratch::new("construction-target-candidates");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    let named = [
        "alpha.md",
        "folder/alpha.md",
        "b/dup.md",
        "c/dup.md",
        "x/many.md",
        "y/many.md",
        "z/many.md",
    ];
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            named
                .iter()
                .map(|name| Change::Named { path: path(name) })
                .chain([Change::Unreadable {
                    path: path("broken.md"),
                }]),
            &[],
            &ContentModel::none(),
        )
        .unwrap();
    store.finish_step(Step::Names).unwrap();
    let none = AmbiguityIgnore::none();
    let owes = |store: &mut Store, target: &str, limit: usize| {
        store
            .begin_request()
            .target_candidates(target, &none, limit)
            .unwrap()
            .map(|candidates| {
                candidates
                    .into_iter()
                    .map(|candidate| (candidate.path.as_str().to_owned(), candidate.owes))
                    .collect::<Vec<_>>()
            })
    };
    let pair = |name: &str, step: Option<Step>| (name.to_owned(), step);
    assert_eq!(
        owes(&mut store, "alpha", 8),
        Some(vec![pair("alpha.md", Some(Step::Content))])
    );
    assert_eq!(
        owes(&mut store, "dup", 8),
        Some(vec![
            pair("b/dup.md", Some(Step::Content)),
            pair("c/dup.md", Some(Step::Content)),
        ])
    );
    assert_eq!(owes(&mut store, "many", 2), None);
    assert_eq!(owes(&mut store, "many", 3).map(|all| all.len()), Some(3));
    assert_eq!(
        owes(&mut store, "broken", 8),
        Some(vec![pair("broken.md", None)])
    );
    assert_eq!(owes(&mut store, "missing", 8), Some(Vec::new()));
    let mut group = store.step_group().unwrap();
    group
        .begin_request()
        .withdraw_content(&path("alpha.md"))
        .unwrap();
    assert_eq!(
        owes(group.store(), "alpha", 8),
        Some(vec![pair("folder/alpha.md", Some(Step::Content))])
    );
    group
        .begin_request()
        .write_content(
            &[ContentWrite {
                facts: document("folder/alpha.md", "h", "body"),
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
    assert_eq!(
        owes(group.store(), "alpha", 8),
        Some(vec![pair("folder/alpha.md", Some(Step::Structure))])
    );
    let rows = vec![
        group
            .begin_request()
            .structure_document(&path("folder/alpha.md"))
            .unwrap()
            .unwrap(),
    ];
    group
        .begin_request()
        .write_structure(&rows, &[], &ContentModel::none())
        .unwrap();
    assert_eq!(
        owes(group.store(), "alpha", 8),
        Some(vec![pair("folder/alpha.md", None)])
    );
    // A commit inside the group keeps its guard: the next write reopens.
    let commits = group.store().construction_group_commits();
    assert!(group.store().commit_step_group().unwrap());
    assert_eq!(group.store().construction_group_commits(), commits + 1);
    assert!(
        !group.store().commit_step_group().unwrap(),
        "nothing was open"
    );
    assert_eq!(group.store().construction_group_room(), 2048);
    group
        .begin_request()
        .withdraw_content(&path("b/dup.md"))
        .unwrap();
    assert_eq!(group.store().construction_group_room(), 2047);
    group.commit().unwrap();
    assert!(store.commit_step_group().is_err());
}

#[test]
fn named_resets_a_document_in_any_document_step() {
    use crate::common::{document, path};
    use norn_store::{Change, ContentModel, IncrementProvenance, Step};
    for step in [Step::Content, Step::Structure] {
        let scratch = Scratch::new("construction-requeue-reset");
        let mut store = scratch.open();
        store.defer_link_health().unwrap();
        store.begin_steps().unwrap();
        store
            .begin_request()
            .apply_increment(
                IncrementProvenance::Derived,
                [Change::Named {
                    path: path("new.md"),
                }],
                &[],
                &ContentModel::none(),
            )
            .unwrap();
        store.finish_step(Step::Names).unwrap();
        let mut group = store.step_group().unwrap();
        group
            .begin_request()
            .write_content(
                &[norn_store::ContentWrite {
                    facts: document("new.md", "old", "old body"),
                    origin: norn_store::Span {
                        line: 1,
                        column: 1,
                        byte_offset: 0,
                    },
                }],
                &[],
                &ContentModel::none(),
            )
            .unwrap();
        let rows = group.begin_request().structure_page(8).unwrap();
        group
            .begin_request()
            .write_structure(&rows, &[], &ContentModel::none())
            .unwrap();
        group.commit().unwrap();
        if step == Step::Structure {
            store.finish_step(Step::Content).unwrap();
        }
        store
            .begin_request()
            .apply_increment(
                IncrementProvenance::Derived,
                [Change::Named {
                    path: path("new.md"),
                }],
                &[],
                &ContentModel::none(),
            )
            .unwrap();
        assert!(store.finish_step(step).is_err());
        assert!(store.begin_request().content_owed(&path("new.md")).unwrap());
        let facts = store
            .begin_request()
            .stored_facts(&path("new.md"))
            .unwrap()
            .unwrap();
        assert!(facts.body.is_none() && facts.fields.is_empty() && facts.links.is_empty());
    }
}

#[test]
fn requeued_documents_page_first_and_death_writes_no_tombstone() {
    use crate::common::{document, path};
    use norn_store::{Change, ContentModel, IncrementProvenance, Provenance, Step};
    let scratch = Scratch::new("requeue-front-content");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            [
                Change::Named { path: path("a.md") },
                Change::Named { path: path("z.md") },
            ],
            &[],
            &ContentModel::none(),
        )
        .unwrap();
    store.finish_step(Step::Names).unwrap();
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Named { path: path("z.md") }],
            &[],
            &ContentModel::none(),
        )
        .unwrap();
    assert_eq!(
        store.begin_request().requeued_page(8).unwrap(),
        [path("z.md")]
    );
    let mut group = store.step_group().unwrap();
    let row = norn_store::ContentWrite {
        facts: document("z.md", "z", "new body"),
        origin: norn_store::Span {
            line: 1,
            column: 1,
            byte_offset: 0,
        },
    };
    assert_eq!(
        group
            .begin_request()
            .write_content(std::slice::from_ref(&row), &[], &ContentModel::none())
            .unwrap(),
        0
    );
    assert_eq!(
        group
            .begin_request()
            .write_content(&[row], &[], &ContentModel::none())
            .unwrap(),
        1
    );
    group.commit().unwrap();
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            [
                Change::Death {
                    path: path("z.md"),
                    provenance: Provenance::WatcherRemoval,
                },
                Change::Death {
                    path: path("a.md"),
                    provenance: Provenance::WatcherRemoval,
                },
            ],
            &[],
            &ContentModel::none(),
        )
        .unwrap();
    assert!(
        store
            .begin_request()
            .stored_tombstone(&path("z.md"))
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .begin_request()
            .stored_document(&path("z.md"))
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .begin_request()
            .stored_tombstone(&path("a.md"))
            .unwrap()
            .is_none()
    );
    assert!(store.begin_request().content_page(8).unwrap().is_empty());
    store.finish_step(Step::Content).unwrap();
}

#[test]
fn requeued_page_work_is_independent_of_the_vault_queue() {
    use crate::common::path;
    use norn_store::{Change, ContentModel, IncrementProvenance, Step};
    let measure = |documents| {
        let scratch = Scratch::new("requeue-page-work");
        let mut store = scratch.open();
        store.defer_link_health().unwrap();
        store.begin_steps().unwrap();
        store
            .begin_request()
            .apply_increment(
                IncrementProvenance::Derived,
                (0..documents).map(|n| Change::Named {
                    path: path(&format!("doc{n:05}.md")),
                }),
                &[],
                &ContentModel::none(),
            )
            .unwrap();
        store.finish_step(Step::Names).unwrap();
        store
            .begin_request()
            .apply_increment(
                IncrementProvenance::Derived,
                [Change::Named {
                    path: path("doc00000.md"),
                }],
                &[],
                &ContentModel::none(),
            )
            .unwrap();
        let mut request = store.begin_request();
        let page = request.requeued_page(128).unwrap();
        assert_eq!(page, [path("doc00000.md")]);
        let populated = request.read_steps();
        request.dequeue_requeued(&page).unwrap();
        let before = request.read_steps();
        assert!(request.requeued_page(128).unwrap().is_empty());
        (populated, request.read_steps() - before)
    };
    let small = measure(2_000);
    let large = measure(10_000);
    eprintln!("requeued page work: 2k={small:?}, 10k={large:?}");
    assert_eq!(small, large);
}

#[test]
fn requeued_step_writers_retain_directory_marks_for_rejudgment() {
    use crate::common::{document, path};
    use norn_store::{
        Change, ContentModel, ContentWrite, DerivedFinding, DiscardScope, IncrementProvenance,
        Span, Step,
    };
    for content in [true, false] {
        let scratch = Scratch::new("requeue-directory-findings");
        let mut store = scratch.open();
        store.defer_link_health().unwrap();
        store.begin_steps().unwrap();
        let model = ContentModel::none();
        store
            .begin_request()
            .apply_increment(
                IncrementProvenance::Derived,
                [Change::Named {
                    path: path("ref.md"),
                }],
                &[],
                &model,
            )
            .unwrap();
        store.finish_step(Step::Names).unwrap();
        let mut group = store.step_group().unwrap();
        group
            .begin_request()
            .write_content(
                &[ContentWrite {
                    facts: document("ref.md", "ref", "[x](/locked/gone.md)\n"),
                    origin: Span {
                        line: 1,
                        column: 1,
                        byte_offset: 0,
                    },
                }],
                &[],
                &model,
            )
            .unwrap();
        let mut rows = group.begin_request().structure_page(8).unwrap();
        rows[0].facts.links = crate::health::derived("ref.md", "[x](/locked/gone.md)\n").links;
        group
            .begin_request()
            .write_structure(&rows, &[], &model)
            .unwrap();
        group.commit().unwrap();
        store.finish_step(Step::Content).unwrap();
        store.finish_step(Step::Structure).unwrap();
        store
            .begin_request()
            .judge_link_health_deferred(&model, &mut || {})
            .unwrap();
        assert_eq!(
            store
                .begin_request()
                .stored_findings(&path("ref.md"))
                .unwrap()
                .len(),
            1
        );
        store
            .begin_request()
            .apply_increment(
                IncrementProvenance::Derived,
                [Change::Named {
                    path: path("writer.md"),
                }],
                &[],
                &model,
            )
            .unwrap();
        let mark = [DerivedFinding {
            facts: crate::health::directory_mark("locked"),
            replaces: None,
            bytes_hash: None,
        }];
        let mut group = store.step_group().unwrap();
        group
            .begin_request()
            .write_content(
                &[ContentWrite {
                    facts: document("writer.md", "writer", "body"),
                    origin: Span {
                        line: 1,
                        column: 1,
                        byte_offset: 0,
                    },
                }],
                if content { &mark } else { &[] },
                &model,
            )
            .unwrap();
        let rows = group.begin_request().structure_page(8).unwrap();
        group
            .begin_request()
            .write_structure(&rows, if content { &[] } else { &mark }, &model)
            .unwrap();
        group.commit().unwrap();
        store.begin_request().rejudge_reach(&model).unwrap();
        assert!(
            store
                .begin_request()
                .stored_findings(&path("ref.md"))
                .unwrap()
                .is_empty()
        );
        store
            .begin_request()
            .discard_findings_about(
                &path("locked"),
                DiscardScope::Kinds(&[norn_wire::FindingKind::UnreadableDirectory]),
            )
            .unwrap();
        store.begin_request().rejudge_reach(&model).unwrap();
        assert_eq!(
            store
                .begin_request()
                .stored_findings(&path("ref.md"))
                .unwrap()
                .len(),
            1
        );
        assert!(store.link_health_deferred());
    }
}

#[test]
fn a_requeued_documents_outgoing_links_are_all_judged_across_pages() {
    use crate::common::path;
    use norn_store::{Change, ContentModel, ContentWrite, IncrementProvenance, Span, Step};
    let scratch = Scratch::new("requeued-outgoing-pages");
    let mut store = scratch.open();
    let model = ContentModel::none();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    for step in [Step::Names, Step::Content, Step::Structure] {
        store.finish_step(step).unwrap();
    }
    store
        .begin_request()
        .judge_link_health_deferred(&model, &mut || {})
        .unwrap();
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Named {
                path: path("holder.md"),
            }],
            &[],
            &model,
        )
        .unwrap();
    let body = "[[missing]]\n".repeat(600);
    let mut facts = crate::health::derived("holder.md", &body);
    let links = std::mem::take(&mut facts.links);
    let mut group = store.step_group().unwrap();
    group
        .begin_request()
        .write_content(
            &[ContentWrite {
                facts,
                origin: Span {
                    line: 1,
                    column: 1,
                    byte_offset: 0,
                },
            }],
            &[],
            &model,
        )
        .unwrap();
    let mut rows = group.begin_request().structure_page(8).unwrap();
    rows[0].facts.links = links;
    group
        .begin_request()
        .write_structure(&rows, &[], &model)
        .unwrap();
    group.commit().unwrap();
    assert_eq!(store.begin_request().rejudge_reach(&model).unwrap(), 600);
    let held = store
        .begin_request()
        .stored_findings(&path("holder.md"))
        .unwrap();
    assert_eq!(held.len(), 600);
    let judged = store
        .begin_request()
        .judge_link_health(&[path("holder.md")], &model)
        .unwrap();
    assert_eq!(judged.len(), 600);
    assert_eq!(
        held.iter().map(|f| f.ordinal).collect::<Vec<_>>(),
        judged.iter().map(|f| f.ordinal).collect::<Vec<_>>()
    );
}

#[test]
fn a_rewritten_documents_path_links_are_rejudged_once() {
    use crate::common::path;
    use norn_store::{Change, ContentModel, ContentWrite, IncrementProvenance, Span, Step};
    let scratch = Scratch::new("requeued-path-once");
    let mut store = scratch.open();
    let model = ContentModel::none();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    for step in [Step::Names, Step::Content, Step::Structure] {
        store.finish_step(step).unwrap();
    }
    store
        .begin_request()
        .judge_link_health_deferred(&model, &mut || {})
        .unwrap();
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Named {
                path: path("holder.md"),
            }],
            &[],
            &model,
        )
        .unwrap();
    let body = "[self](/holder.md#missing)\n";
    let mut facts = crate::health::derived("holder.md", body);
    let links = std::mem::take(&mut facts.links);
    let mut group = store.step_group().unwrap();
    group
        .begin_request()
        .write_content(
            &[ContentWrite {
                facts,
                origin: Span {
                    line: 1,
                    column: 1,
                    byte_offset: 0,
                },
            }],
            &[],
            &model,
        )
        .unwrap();
    let mut rows = group.begin_request().structure_page(8).unwrap();
    rows[0].facts.links = links;
    group
        .begin_request()
        .write_structure(&rows, &[], &model)
        .unwrap();
    group.commit().unwrap();
    assert_eq!(store.begin_request().rejudge_reach(&model).unwrap(), 1);
    let held = store
        .begin_request()
        .stored_findings(&path("holder.md"))
        .unwrap();
    assert_eq!(held.len(), 1);
    let judged = store
        .begin_request()
        .judge_link_health(&[path("holder.md")], &model)
        .unwrap();
    assert_eq!(judged.len(), 1);
    assert_eq!(
        held.iter().map(|f| f.ordinal).collect::<Vec<_>>(),
        judged.iter().map(|f| f.ordinal).collect::<Vec<_>>()
    );
}

#[test]
fn an_increment_ending_a_nested_directory_mark_rejudges_its_links() {
    use crate::common::{document, path};
    use norn_store::{
        Change, ContentModel, ContentWrite, DerivedFinding, DirectoryPrefix, IncrementProvenance,
        Span, Step,
    };
    let scratch = Scratch::new("increment-directory-reach");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    let model = ContentModel::none();
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Named {
                path: path("ref.md"),
            }],
            &[],
            &model,
        )
        .unwrap();
    store.finish_step(Step::Names).unwrap();
    let mut group = store.step_group().unwrap();
    group
        .begin_request()
        .write_content(
            &[ContentWrite {
                facts: document("ref.md", "ref", "[x](/locked/inner/gone.md)\n"),
                origin: Span {
                    line: 1,
                    column: 1,
                    byte_offset: 0,
                },
            }],
            &[],
            &model,
        )
        .unwrap();
    let mut rows = group.begin_request().structure_page(8).unwrap();
    rows[0].facts.links = crate::health::derived("ref.md", "[x](/locked/inner/gone.md)\n").links;
    group
        .begin_request()
        .write_structure(&rows, &[], &model)
        .unwrap();
    group.commit().unwrap();
    store.finish_step(Step::Content).unwrap();
    store.finish_step(Step::Structure).unwrap();
    store
        .begin_request()
        .judge_link_health_deferred(&model, &mut || {})
        .unwrap();
    assert_eq!(
        store
            .begin_request()
            .stored_findings(&path("ref.md"))
            .unwrap()
            .len(),
        1
    );

    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            [],
            &[DerivedFinding {
                facts: crate::health::directory_mark("locked/inner"),
                replaces: None,
                bytes_hash: None,
            }],
            &model,
        )
        .unwrap();
    store.begin_request().rejudge_reach(&model).unwrap();
    assert!(
        store
            .begin_request()
            .stored_findings(&path("ref.md"))
            .unwrap()
            .is_empty()
    );

    // Withdrawing the parent ends its nested mark. The target remains absent,
    // so the formerly unjudged link must report broken again.
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::BeneathUnlistable {
                prefix: DirectoryPrefix::new("locked").unwrap(),
            }],
            &[],
            &model,
        )
        .unwrap();
    store.begin_request().rejudge_reach(&model).unwrap();
    let findings = store
        .begin_request()
        .stored_findings(&path("ref.md"))
        .unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].kind, norn_wire::FindingKind::Broken.as_str());
}
