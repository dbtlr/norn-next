//! Nonadoption guards outlive every experimental transaction.
#![cfg(feature = "induced-failure")]
use norn_store::{DerivationVersion, Store, StoredPathOrder};
use norn_testkit::scratch::Scratch;

fn open(scratch: &Scratch) -> Store {
    Store::open(
        scratch.join("store.sqlite3"),
        StoredPathOrder::Sensitive,
        DerivationVersion::new(1),
    )
    .unwrap()
}

#[test]
fn construction_requires_created_marker_first_attempt_and_inactive_completion() {
    let scratch = Scratch::new("construction-marker-guards");
    let mut store = open(&scratch);
    assert!(
        store
            .begin_request()
            .begin_startup_construction(16)
            .is_err()
    );
    store.begin_request().start_startup_experiment().unwrap();
    store
        .begin_request()
        .begin_startup_construction(16)
        .unwrap();
    store
        .begin_request()
        .apply_startup_increment(
            [norn_store::Change::Upsert(norn_store::DocumentFacts::new(
                norn_store::DocumentPath::new("first.md").unwrap(),
                "hash",
                "body",
                4,
            ))],
            &[],
            &norn_store::ContentModel::none(),
        )
        .unwrap();
    assert!(
        store
            .begin_request()
            .complete_startup_health_once()
            .is_err()
    );
    store.begin_request().finish_startup_construction().unwrap();
    assert!(
        store
            .begin_request()
            .begin_startup_construction(16)
            .is_err()
    );
    store
        .begin_request()
        .complete_startup_health_once()
        .unwrap();
    drop(store);
    let mut store = open(&scratch);
    store.begin_request().start_startup_experiment().unwrap();
    assert!(
        store
            .begin_request()
            .begin_startup_construction(16)
            .is_err(),
        "existing store"
    );
}

#[test]
fn abandoned_construction_cannot_clear_the_committed_marker_or_reopen() {
    let scratch = Scratch::new("construction-marker-abandoned");
    let mut store = open(&scratch);
    store.begin_request().start_startup_experiment().unwrap();
    store.begin_request().begin_startup_construction(0).unwrap();
    store
        .begin_request()
        .abandon_startup_construction()
        .unwrap();
    assert!(
        store
            .begin_request()
            .complete_startup_health_once()
            .is_err()
    );
    drop(store);
    assert!(
        Store::open(
            scratch.join("store.sqlite3"),
            StoredPathOrder::Sensitive,
            DerivationVersion::new(1)
        )
        .is_err()
    );
}

#[test]
#[allow(clippy::disallowed_methods)] // This test owns and joins its aborting child.
fn existing_fault_hooks_tear_later_units_without_publishing_partial_groups() {
    use norn_store::{Change, ContentModel, DocumentFacts, DocumentPath, induced_failure};
    const CHILD: &str = "NORN_CONSTRUCTION_TEAR_DATABASE";
    const CASE: &str = "existing_fault_hooks_tear_later_units_without_publishing_partial_groups";
    if let Some(path) = std::env::var_os(CHILD) {
        let group: usize = std::env::var("NORN_CONSTRUCTION_TEAR_GROUP")
            .unwrap()
            .parse()
            .unwrap();
        let mut store =
            Store::open(path, StoredPathOrder::Sensitive, DerivationVersion::new(1)).unwrap();
        store.begin_request().start_startup_experiment().unwrap();
        store
            .begin_request()
            .begin_startup_construction(group)
            .unwrap();
        let boundary = std::env::var("NORN_CONSTRUCTION_TEAR_KIND").unwrap() == "boundary";
        if boundary {
            induced_failure::abort_at_the_chunk_boundary(18);
        }
        for id in 1..=40 {
            if !boundary && id == 19 {
                induced_failure::abort_after_changeset_entries(1);
            }
            store
                .begin_request()
                .apply_startup_increment(
                    [Change::Upsert(DocumentFacts::new(
                        DocumentPath::new(&format!("note-{id:03}.md")).unwrap(),
                        "hash",
                        "body",
                        4,
                    ))],
                    &[],
                    &ContentModel::none(),
                )
                .unwrap();
        }
        panic!("the existing fault hook did not fire");
    }
    for (group, expected) in [(1, 18), (16, 16), (0, 0)] {
        for kind in ["boundary", "inside"] {
            let scratch = Scratch::new("construction-torn-later-unit");
            let path = scratch.join("store.sqlite3");
            let hits = scratch.join("arm-hits.log");
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", CASE, "--nocapture"])
                .env(CHILD, &path)
                .env("NORN_CONSTRUCTION_TEAR_GROUP", group.to_string())
                .env("NORN_CONSTRUCTION_TEAR_KIND", kind)
                .env(induced_failure::ARM_HITS, &hits)
                .env_remove("TMPDIR")
                .env_remove("NORN_TEST_ISOLATION_DIR")
                .output()
                .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                assert_eq!(
                    output.status.signal(),
                    Some(6),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            assert!(!output.status.success());
            assert!(
                std::fs::read_to_string(hits)
                    .unwrap()
                    .contains(induced_failure::INCREMENT_SEAM)
            );
            let db = norn_db::open_read_only(&path).adopted.unwrap();
            let documents: i64 = db
                .connection()
                .query_row("SELECT count(*) FROM documents", [], |r| r.get(0))
                .unwrap();
            assert_eq!(documents, expected, "group {group}, {kind}");
            let marker: i64 = db
                .connection()
                .query_row(
                    "SELECT value FROM meta WHERE key='startup_experiment_incomplete'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(marker, 1);
            drop(db);
            assert!(
                Store::open(&path, StoredPathOrder::Sensitive, DerivationVersion::new(1)).is_err()
            );
        }
    }
}

fn document(id: usize) -> norn_store::DocumentFacts {
    let mut facts = norn_store::DocumentFacts::new(
        norn_store::DocumentPath::new(&format!("note-{id:03}.md")).unwrap(),
        "hash",
        "[[missing]]",
        11,
    );
    facts.links.push(norn_store::LinkFact {
        field: None,
        family: norn_store::LinkFamily::Wikilink,
        embed: false,
        protocol: None,
        target: "missing".into(),
        title: None,
        anchor: None,
        span: None,
    });
    facts
}

#[test]
fn control_preserves_the_complete_sql_trace_and_group_receipts_match_sqlite_execution() {
    use norn_store::{Change, ContentModel, induced_failure};
    let mut control = None;
    for group in [None, Some(1), Some(16), Some(0)] {
        let scratch = Scratch::new("construction-store-statement-trace");
        induced_failure::record_statements_begun();
        let mut store = open(&scratch);
        store.begin_request().start_startup_experiment().unwrap();
        induced_failure::statements_begun();
        if let Some(group) = group {
            store
                .begin_request()
                .begin_startup_construction(group)
                .unwrap();
        }
        for id in 1..=17 {
            store
                .begin_request()
                .apply_startup_increment([Change::Upsert(document(id))], &[], &ContentModel::none())
                .unwrap();
        }
        store
            .begin_request()
            .finish_startup_health(&ContentModel::none())
            .unwrap();
        if group.is_some() {
            store.begin_request().finish_startup_construction().unwrap();
        }
        let receipt = store.begin_request().startup_construction_work();
        let trace = induced_failure::statements_begun();
        induced_failure::stop_recording_statements();
        if let Some(group) = group {
            let actual_commits = trace.iter().filter(|s| s.as_str() == "COMMIT").count() as u64;
            assert_eq!(receipt["construction_commits"], actual_commits);
            assert_eq!(
                actual_commits,
                match group {
                    1 => 35,
                    16 => 3,
                    0 => 1,
                    _ => unreachable!(),
                }
            );
            assert_eq!(receipt["construction_document_changesets"], 17);
            assert_eq!(receipt["construction_health_generation_groups"], 17);
            assert_eq!(receipt["construction_health_resets"], 1);
            assert_eq!(
                actual_commits,
                receipt["construction_commits_documents"]
                    + receipt["construction_commits_health_reset"]
                    + receipt["construction_commits_health_generation"]
            );
            if group == 1 {
                assert_eq!(
                    control.as_ref().unwrap(),
                    &trace,
                    "control changed actual SQLite execution"
                );
            }
        } else {
            control = Some(trace);
        }
        store
            .begin_request()
            .complete_startup_health_once()
            .unwrap();
        store.verify_integrity().unwrap();
    }
}

#[test]
fn error_in_a_later_final_health_generation_abandons_the_current_group_and_keeps_marker() {
    use norn_store::{Change, ContentModel, induced_failure};
    for group in [1, 16, 0] {
        let scratch = Scratch::new("construction-final-health-error");
        let mut store = open(&scratch);
        // A test-only SQL fault after prior generations have been judged. The
        // trigger is removed before reopen so DDL damage cannot mask the marker.
        induced_failure::execute_out_of_band(
            &mut store,
            "CREATE TRIGGER refuse_later_health BEFORE INSERT ON findings
            WHEN NEW.path='note-017.md' BEGIN SELECT RAISE(ABORT, 'later health fault'); END",
        )
        .unwrap();
        store.begin_request().start_startup_experiment().unwrap();
        store
            .begin_request()
            .begin_startup_construction(group)
            .unwrap();
        for id in 1..=18 {
            store
                .begin_request()
                .apply_startup_increment([Change::Upsert(document(id))], &[], &ContentModel::none())
                .unwrap();
        }
        let error = store
            .begin_request()
            .finish_startup_health(&ContentModel::none())
            .unwrap_err();
        assert!(error.to_string().contains("later health fault"));
        store
            .begin_request()
            .abandon_startup_construction()
            .unwrap();
        assert!(
            store
                .begin_request()
                .complete_startup_health_once()
                .is_err()
        );
        induced_failure::execute_out_of_band(&mut store, "DROP TRIGGER refuse_later_health")
            .unwrap();
        let receipt = store.begin_request().startup_construction_work();
        assert!(receipt["construction_health_generation_groups"] >= 16);
        drop(store);
        let db = norn_db::open_read_only(&scratch.join("store.sqlite3"))
            .adopted
            .unwrap();
        let count: i64 = db
            .connection()
            .query_row("SELECT count(*) FROM documents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, if group == 0 { 0 } else { 18 });
        drop(db);
        assert!(
            Store::open(
                scratch.join("store.sqlite3"),
                StoredPathOrder::Sensitive,
                DerivationVersion::new(1)
            )
            .is_err()
        );
    }
}

#[test]
fn sqlite_full_keeps_marker_and_never_allows_an_autocommit_continuation() {
    use norn_store::{Change, ContentModel, DocumentFacts, DocumentPath, induced_failure};
    for group in [1, 16, 0] {
        let scratch = Scratch::new("construction-full-marker");
        let mut store = open(&scratch);
        store.begin_request().start_startup_experiment().unwrap();
        let pages = induced_failure::page_count(&mut store).unwrap();
        induced_failure::execute_out_of_band(
            &mut store,
            &format!("PRAGMA max_page_count = {}", pages + 1),
        )
        .unwrap();
        store
            .begin_request()
            .begin_startup_construction(group)
            .unwrap();
        let large = "x".repeat(1024 * 1024);
        let error = store
            .begin_request()
            .apply_startup_increment(
                [Change::Upsert(DocumentFacts::new(
                    DocumentPath::new("large.md").unwrap(),
                    "hash",
                    &large,
                    large.len() as u64,
                ))],
                &[],
                &ContentModel::none(),
            )
            .unwrap_err();
        assert!(error.damage().is_none());
        assert!(
            error.to_string().contains("database or disk is full"),
            "{error}"
        );
        assert!(
            store
                .begin_request()
                .apply_startup_increment([Change::Upsert(document(2))], &[], &ContentModel::none())
                .is_err()
        );
        assert!(
            store
                .begin_request()
                .complete_startup_health_once()
                .is_err()
        );
        let receipt = store.begin_request().startup_construction_work();
        assert_eq!(receipt["construction_commits"], 0);
        assert_eq!(
            receipt["construction_automatic_rollbacks"] + receipt["construction_rollbacks"],
            1
        );
        store
            .begin_request()
            .abandon_startup_construction()
            .unwrap();
        drop(store);
        assert!(
            Store::open(
                scratch.join("store.sqlite3"),
                StoredPathOrder::Sensitive,
                DerivationVersion::new(1)
            )
            .is_err()
        );
    }
}

#[test]
fn cleanup_units_preserve_sql_and_count_real_commits_separately_from_changesets() {
    use norn_store::{DiscardScope, DocumentPath, induced_failure};
    use norn_wire::FindingKind;
    let mut legacy_sql = None;
    for group in [None, Some(1), Some(16), Some(0)] {
        let scratch = Scratch::new("construction-cleanup-sql");
        induced_failure::record_statements_begun();
        let mut store = open(&scratch);
        store.begin_request().start_startup_experiment().unwrap();
        induced_failure::statements_begun();
        if let Some(group) = group {
            store
                .begin_request()
                .begin_startup_construction(group)
                .unwrap();
        }
        let path = DocumentPath::new("quarantined.md").unwrap();
        for _ in 0..16 {
            store
                .begin_request()
                .discard_findings_about(
                    &path,
                    DiscardScope::Kinds(&[FindingKind::BodyBytesNotUtf8]),
                )
                .unwrap();
        }
        if group.is_some() {
            store.begin_request().finish_startup_construction().unwrap();
        }
        let trace = induced_failure::statements_begun();
        induced_failure::stop_recording_statements();
        let commits = trace.iter().filter(|sql| sql.as_str() == "COMMIT").count() as u64;
        let body: Vec<_> = trace
            .into_iter()
            .filter(|sql| !sql.starts_with("BEGIN") && sql != "COMMIT")
            .collect();
        if let Some(group) = group {
            let receipt = store.begin_request().startup_construction_work();
            assert_eq!(commits, if group == 1 { 16 } else { 1 });
            assert_eq!(receipt["construction_commits"], commits);
            assert_eq!(receipt["construction_commits_cleanup"], commits);
            assert_eq!(receipt["construction_cleanup_units"], 16);
            assert_eq!(receipt["construction_document_changesets"], 0);
            assert_eq!(&body, legacy_sql.as_ref().unwrap(), "cleanup SQL changed");
            // Captured watcher application uses ordinary one-unit commits even
            // after an unbounded construction group has ended.
            store
                .begin_request()
                .discard_findings_about(
                    &path,
                    DiscardScope::Kinds(&[FindingKind::BodyBytesNotUtf8]),
                )
                .unwrap();
            let receipt = store.begin_request().startup_construction_work();
            assert_eq!(receipt["construction_drain_changesets"], 0);
            assert_eq!(receipt["construction_drain_cleanup_units"], 1);
            assert_eq!(receipt["construction_commits_drain"], 1);
        } else {
            assert_eq!(commits, 0);
            legacy_sql = Some(body);
        }
        store
            .begin_request()
            .complete_startup_health_once()
            .unwrap();
    }
}
