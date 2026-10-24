//! Transaction granularity is observed through SQLite, including rollback.
use norn_db::{Attempt, Database, connect, meta};
use norn_testkit::scratch::Scratch;

fn database(scratch: &Scratch) -> Database {
    let path = scratch.join("store.sqlite3");
    let Attempt::Connected(connection) = connect(&path).unwrap() else {
        panic!("fresh database")
    };
    for statement in meta::statements() {
        connection.execute_batch(&statement).unwrap();
    }
    meta::put_meta(&connection, meta::STORE_EPOCH, "test-epoch").unwrap();
    connection
        .execute_batch("CREATE TABLE units (id INTEGER PRIMARY KEY, body BLOB)")
        .unwrap();
    Database::adopt(connection, &path).unwrap()
}

#[test]
fn ordinary_units_commit_individually_and_drop_rolls_back() {
    let scratch = Scratch::new("ordinary-unit-commits");
    let mut db = database(&scratch);
    for id in 1..=2 {
        let tx = db.immediate_transaction("ordinary unit").unwrap();
        tx.execute("INSERT INTO units(id) VALUES (?1)", [id])
            .unwrap();
        tx.commit().unwrap();
        assert!(db.connection().is_autocommit());
        let reader = norn_db::open_read_only(db.path()).adopted.unwrap();
        let count: i64 = reader
            .connection()
            .query_row("SELECT count(*) FROM units", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, id);
    }
    {
        let tx = db.immediate_transaction("abandoned ordinary unit").unwrap();
        tx.execute("INSERT INTO units(id) VALUES (3)", []).unwrap();
    }
    assert_eq!(count(&db), 2);
}

fn count(db: &Database) -> i64 {
    db.connection()
        .query_row("SELECT count(*) FROM units", [], |r| r.get(0))
        .unwrap()
}

#[cfg(feature = "induced-failure")]
#[test]
fn construction_counts_real_commits_and_composes_with_statement_trace() {
    use norn_db::construction::Phase;
    for (group, expected) in [(1, 35), (16, 3), (0, 1)] {
        let scratch = Scratch::new("construction-real-commits");
        let mut db = database(&scratch);
        db.count_statements_begun();
        norn_db::faults::record_statements_begun();
        norn_db::faults::take_recorded_statements();
        let before = norn_db::statements_begun_on_this_thread();
        db.begin_construction(group).unwrap();
        for id in 1..=35 {
            let unit = db
                .construction_unit(Phase::Documents, "document unit")
                .unwrap();
            unit.execute("INSERT INTO units(id) VALUES (?1)", [id])
                .unwrap();
            unit.commit().unwrap();
        }
        db.finish_construction().unwrap();
        let statements = norn_db::faults::take_recorded_statements();
        norn_db::faults::stop_recording_statements();
        let observed = statements.iter().filter(|s| s.as_str() == "COMMIT").count() as u64;
        assert_eq!(observed, expected);
        let receipt = db.construction_receipt().unwrap();
        assert_eq!(receipt.commits, observed);
        assert_eq!(receipt.commit_attempts, observed);
        assert_eq!(receipt.document_changesets, 35);
        assert_eq!(receipt.rollbacks, 0);
        assert_eq!(
            norn_db::statements_begun_on_this_thread() - before,
            statements.len() as u64
        );
        assert_eq!(count(&db), 35);
        assert!(db.begin_construction(group).is_err(), "first attempt only");
    }
}

#[cfg(feature = "induced-failure")]
#[test]
fn later_unit_errors_and_panics_abandon_only_the_current_real_transaction() {
    use norn_db::construction::Phase;
    for (group, committed) in [(1, 18), (16, 16), (0, 0)] {
        for panic in [false, true] {
            let scratch = Scratch::new("construction-abandon");
            let mut db = database(&scratch);
            db.begin_construction(group).unwrap();
            for id in 1..=18 {
                let unit = db.construction_unit(Phase::Documents, "unit").unwrap();
                unit.execute("INSERT INTO units(id) VALUES (?1)", [id])
                    .unwrap();
                unit.commit().unwrap();
            }
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let unit = db
                    .construction_unit(Phase::HealthGeneration, "failed final health")
                    .unwrap();
                unit.execute("INSERT INTO units(id) VALUES (19)", [])
                    .unwrap();
                if panic {
                    panic!("construction interrupted")
                }
                assert!(
                    unit.execute("INSERT INTO units(id) VALUES (19)", [])
                        .is_err()
                );
                // Returning without commit is cancellation/error, like Transaction.
            }));
            assert_eq!(result.is_err(), panic);
            assert!(db.connection().is_autocommit());
            assert_eq!(count(&db), committed);
            assert!(db.finish_construction().is_err());
            assert!(
                db.construction_unit(Phase::Documents, "no accidental autocommit")
                    .is_err()
            );
            assert!(db.immediate_transaction("unconverted writer").is_err());
        }
    }
}

#[cfg(feature = "induced-failure")]
#[test]
fn sqlite_full_autorollback_is_detected_before_another_unit_can_write() {
    use norn_db::construction::Phase;
    let scratch = Scratch::new("construction-full");
    let mut db = database(&scratch);
    let pages: i64 = db
        .connection()
        .pragma_query_value(None, "page_count", |r| r.get(0))
        .unwrap();
    db.connection()
        .pragma_update(None, "max_page_count", pages + 1)
        .unwrap();
    db.begin_construction(0).unwrap();
    {
        let unit = db
            .construction_unit(Phase::Documents, "full disk unit")
            .unwrap();
        unit.execute("INSERT INTO units(id) VALUES (1)", [])
            .unwrap();
        let error = unit
            .execute("INSERT INTO units(body) VALUES (zeroblob(1048576))", [])
            .unwrap_err();
        assert_eq!(
            error.sqlite_error_code(),
            Some(norn_db::rusqlite::ErrorCode::DiskFull)
        );
        assert!(unit.is_autocommit(), "SQLite FULL really rolled back");
        assert!(
            unit.commit().is_err(),
            "never count an automatic rollback as a commit"
        );
    }
    assert_eq!(count(&db), 0);
    assert_eq!(db.construction_receipt().unwrap().commits, 0);
    assert!(
        db.construction_unit(Phase::Documents, "after FULL")
            .is_err()
    );
    assert!(db.finish_construction().is_err());
}

#[cfg(feature = "induced-failure")]
#[test]
fn unconverted_writer_refuses_and_abandons_the_group_without_relying_on_begin_error() {
    use norn_db::construction::Phase;
    let scratch = Scratch::new("construction-nested");
    let mut db = database(&scratch);
    db.begin_construction(16).unwrap();
    let unit = db.construction_unit(Phase::Documents, "unit").unwrap();
    unit.execute("INSERT INTO units(id) VALUES (1)", [])
        .unwrap();
    unit.commit().unwrap();
    assert!(db.immediate_transaction("unconverted writer").is_err());
    assert!(db.connection().is_autocommit());
    assert_eq!(count(&db), 0);
    assert!(db.finish_construction().is_err());
}

#[cfg(feature = "induced-failure")]
#[test]
fn failed_real_commit_is_an_attempt_and_explicit_rollback_is_counted() {
    use norn_db::construction::Phase;
    for group in [1, 16, 0] {
        let scratch = Scratch::new("construction-commit-failure");
        let mut db = database(&scratch);
        db.connection()
            .execute_batch(
                "CREATE TABLE parent(id INTEGER PRIMARY KEY);
            CREATE TABLE child(parent INTEGER REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED)",
            )
            .unwrap();
        db.begin_construction(group).unwrap();
        let unit = db
            .construction_unit(Phase::Documents, "deferred constraint")
            .unwrap();
        unit.execute("INSERT INTO child VALUES (99)", []).unwrap();
        if group == 1 {
            assert!(unit.commit().is_err());
        } else {
            unit.commit().unwrap();
            assert!(db.finish_construction().is_err());
        }
        let r = db.construction_receipt().unwrap();
        assert_eq!(r.commit_attempts, 1);
        assert_eq!(r.commits, 0);
        assert_eq!(r.rollbacks, 1);
        assert!(db.connection().is_autocommit());
    }
}

#[cfg(feature = "induced-failure")]
#[test]
fn sqlite_interrupt_abandons_the_transaction_and_refuses_continuation() {
    use norn_db::construction::Phase;
    let scratch = Scratch::new("construction-sqlite-interrupt");
    let mut db = database(&scratch);
    db.begin_construction(0).unwrap();
    let unit = db.construction_unit(Phase::Documents, "interrupt").unwrap();
    unit.execute("INSERT INTO units(id) VALUES (1)", [])
        .unwrap();
    unit.progress_handler(1, Some(|| true)).unwrap();
    let error = unit
        .execute("INSERT INTO units(id) VALUES (2)", [])
        .unwrap_err();
    unit.progress_handler(0, None::<fn() -> bool>).unwrap();
    assert_eq!(
        error.sqlite_error_code(),
        Some(norn_db::rusqlite::ErrorCode::OperationInterrupted)
    );
    drop(unit);
    assert_eq!(count(&db), 0);
    assert!(db.connection().is_autocommit());
    assert!(db.finish_construction().is_err());
    assert!(
        db.construction_unit(Phase::Documents, "after interrupt")
            .is_err()
    );
}

#[cfg(feature = "induced-failure")]
#[test]
fn completed_lab_account_does_not_poison_ordinary_warm_error_recovery() {
    use norn_db::construction::Phase;
    let scratch = Scratch::new("construction-warm-errors");
    let mut db = database(&scratch);
    db.begin_construction(16).unwrap();
    let unit = db.construction_unit(Phase::Documents, "initial").unwrap();
    unit.execute("INSERT INTO units(id) VALUES (1)", [])
        .unwrap();
    unit.commit().unwrap();
    db.finish_construction().unwrap();
    db.seal_construction_receipt().unwrap();
    let unit = db
        .construction_unit(Phase::Documents, "ordinary warm error")
        .unwrap();
    assert!(
        unit.execute("INSERT INTO units(id) VALUES (1)", [])
            .is_err()
    );
    drop(unit);
    let unit = db
        .construction_unit(Phase::Documents, "ordinary warm retry")
        .unwrap();
    unit.execute("INSERT INTO units(id) VALUES (2)", [])
        .unwrap();
    unit.commit().unwrap();
    assert_eq!(count(&db), 2);
    assert_eq!(db.construction_receipt().unwrap().commits, 1);
    assert_eq!(db.construction_receipt().unwrap().drain_changesets, 0);
}
