//! Scratch databases enforce their own capacity without weakening durable opens.

use norn_db::{TemporaryDatabase, connect, sweep_temporary_directory};

#[test]
#[cfg(feature = "induced-failure")]
#[allow(clippy::disallowed_methods)] // Observe disposable-file lifetime at SQLite's close boundary.
fn a_snapshot_keeps_its_file_until_its_connection_closes() {
    use norn_db::rusqlite::trace::{TraceEvent, TraceEventCodes};
    std::thread_local! {
        static FILE_AT_CLOSE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
    }
    fn closing(event: TraceEvent<'_>) {
        if let TraceEvent::Close(connection) = event {
            FILE_AT_CLOSE.set(Some(
                std::path::Path::new(connection.db_filename().unwrap()).exists(),
            ));
        }
    }

    let scratch = norn_testkit::scratch::Scratch::new("snapshot-file-owner");
    let path = scratch.join("snapshot.sqlite3");
    let writer = TemporaryDatabase::create(&path, 64, None).unwrap();
    for statement in norn_db::meta::statements() {
        writer.connection().execute_batch(&statement).unwrap();
    }
    norn_db::meta::put_meta(writer.connection(), norn_db::meta::STORE_EPOCH, "trial").unwrap();
    let file = writer.shared_file().unwrap();
    drop(writer);
    let mut snapshot = file.open_snapshot().unwrap();
    snapshot
        .connection()
        .trace_v2(TraceEventCodes::SQLITE_TRACE_CLOSE, Some(closing));
    drop(file);

    assert!(
        path.exists(),
        "an open snapshot retains its file without a caller owner"
    );
    snapshot.open_snapshot().unwrap();
    assert_eq!(snapshot.epoch(), "trial");
    snapshot.close_snapshot().unwrap();
    drop(snapshot);
    assert_eq!(
        FILE_AT_CLOSE.get(),
        Some(true),
        "the file still exists when SQLite closes"
    );
    assert!(
        !path.exists(),
        "closing the final connection releases its file"
    );
}

#[test]
#[allow(clippy::disallowed_methods)] // The substrate suite arranges an external SQLite writer.
fn an_immutable_result_reader_refuses_external_lock_contention_without_waiting() {
    let scratch = norn_testkit::scratch::Scratch::new("locked-result-reader");
    let path = scratch.join("result.sqlite3");
    let writer = TemporaryDatabase::create(&path, 64, None).unwrap();
    writer
        .connection()
        .execute_batch("CREATE TABLE result(value); INSERT INTO result VALUES(42)")
        .unwrap();
    let file = writer.shared_file().unwrap();
    drop(writer);
    let external = norn_db::rusqlite::Connection::open(&path).unwrap();
    external.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let (answer_tx, answer_rx) = std::sync::mpsc::channel();
    let read = std::thread::spawn(move || {
        let answer = file.open_reader().map(|reader| {
            reader
                .connection()
                .query_row("SELECT value FROM result", [], |row| row.get::<_, i64>(0))
        });
        answer_tx.send(answer).unwrap();
    });
    let answer = answer_rx.recv_timeout(std::time::Duration::from_secs(1));
    external.execute_batch("ROLLBACK").unwrap();
    read.join().unwrap();
    let answer = answer.expect("immutable result access does not wait on an external writer");
    assert!(
        !matches!(answer, Ok(Ok(_))),
        "the result refuses transient contention"
    );
}

#[test]
fn a_process_exit_without_drop_leaves_only_sweepable_disposable_residue() {
    const STORAGE: &str = "NORN_TEST_DISPOSABLE_RESIDUE";
    if let Some(directory) = std::env::var_os(STORAGE) {
        let writer =
            TemporaryDatabase::create_in(std::path::Path::new(&directory), 64, None).unwrap();
        writer
            .connection()
            .execute_batch("CREATE TABLE result(value)")
            .unwrap();
        // End without running Rust destructors, as on process interruption.
        std::process::exit(0);
    }
    let scratch = norn_testkit::scratch::Scratch::new("interrupted-result");
    let directory = scratch.join("results");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_process_exit_without_drop_leaves_only_sweepable_disposable_residue",
        ])
        .env(STORAGE, &directory)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(sweep_temporary_directory(&directory).unwrap(), 1);
    assert_eq!(sweep_temporary_directory(&directory).unwrap(), 0);
}

#[test]
fn a_sweep_can_unlink_retired_storage_while_an_open_reader_finishes() {
    let scratch = norn_testkit::scratch::Scratch::new("retired-result-sweep");
    let directory = scratch.join("results");
    let writer = TemporaryDatabase::create_in(&directory, 64, None).unwrap();
    writer
        .connection()
        .execute_batch("CREATE TABLE result(value); INSERT INTO result VALUES(42)")
        .unwrap();
    let file = writer.shared_file().unwrap();
    let reader = file.open_reader().unwrap();
    drop(writer);
    assert_eq!(sweep_temporary_directory(&directory).unwrap(), 1);
    assert!(file.open_reader().is_err(), "a retired name cannot reopen");
    let value: i64 = reader
        .connection()
        .query_row("SELECT value FROM result", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, 42, "an existing handle finishes after unlink");
}

#[test]
#[allow(clippy::disallowed_methods)] // Arrange replacement of private derived storage.
fn a_replaced_result_is_neither_reopened_nor_deleted_by_the_old_owner() {
    let scratch = norn_testkit::scratch::Scratch::new("replaced-result");
    let path = scratch.join("results.sqlite3");
    let writer = TemporaryDatabase::create(&path, 64, None).unwrap();
    let file = writer.shared_file().unwrap();
    drop(writer);
    std::fs::rename(&path, scratch.join("retired.sqlite3")).unwrap();
    std::fs::write(&path, b"a different file at the same name").unwrap();
    assert!(file.open_reader().is_err());
    drop(file);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"a different file at the same name"
    );
}

#[test]
#[allow(clippy::disallowed_methods)] // Observe this fixture's disposable-file lifetime.
fn a_completed_disposable_file_has_independent_readers_and_outlives_its_writer() {
    let scratch = norn_testkit::scratch::Scratch::new("shared-result-readers");
    let path = scratch.join("results").join("result.sqlite3");
    let writer = TemporaryDatabase::create(&path, 64, None).expect("private result file");
    writer
        .connection()
        .execute_batch(
            "CREATE TABLE result (key INTEGER PRIMARY KEY, value TEXT); \
         INSERT INTO result VALUES (1, 'first'), (2, 'second')",
        )
        .expect("complete result");
    let file = writer.shared_file().expect("reopenable storage");
    drop(writer);
    let first = file.open_reader().expect("first independent reader");
    let second = file.open_reader().expect("second independent reader");
    drop(file);
    for reader in [&first, &second] {
        let value: String = reader
            .connection()
            .query_row("SELECT value FROM result WHERE key = 2", [], |row| {
                row.get(0)
            })
            .expect("completed result survives writer close");
        assert_eq!(value, "second");
        assert!(
            reader
                .connection()
                .execute("DELETE FROM result", [])
                .is_err()
        );
        assert!(
            reader
                .connection()
                .execute_batch("CREATE TEMP TABLE escape(x)")
                .is_err()
        );
    }
    drop(first);
    assert!(path.exists(), "the last reader keeps the file");
    drop(second);
    assert!(!path.exists(), "the last reader removes disposable storage");
}

#[test]
fn a_temporary_results_capacity_refuses_without_affecting_another_result() {
    let small = TemporaryDatabase::open(64, Some(16 * 1024)).expect("a bounded scratch database");
    let other =
        TemporaryDatabase::open(64, Some(16 * 1024)).expect("an independent scratch database");
    for database in [&small, &other] {
        database
            .connection()
            .execute_batch("CREATE TABLE result (key INTEGER PRIMARY KEY, payload BLOB)")
            .expect("a result table");
    }
    other
        .connection()
        .execute("INSERT INTO result VALUES (1, x'01')", [])
        .expect("another result");
    let failure = small
        .connection()
        .execute("INSERT INTO result VALUES (1, zeroblob(32768))", [])
        .expect_err("the scratch page quota must refuse a larger result");
    assert_eq!(
        failure.sqlite_error_code(),
        Some(norn_db::rusqlite::ErrorCode::DiskFull)
    );
    let count: u64 = other
        .connection()
        .query_row("SELECT count(*) FROM result", [], |row| row.get(0))
        .expect("the independent result remains readable");
    assert_eq!(count, 1);
    drop(small);
    let reopened = TemporaryDatabase::open(64, Some(16 * 1024)).expect("a new temporary database");
    assert!(
        reopened
            .connection()
            .prepare("SELECT * FROM result")
            .is_err()
    );
    assert!(
        connect(std::path::Path::new("")).is_err(),
        "durable opens still require a named file"
    );
}

#[test]
#[allow(clippy::disallowed_methods)] // Arrange a replacement at the actual cleanup seam.
fn last_owner_cleanup_reports_failure_without_retaining_the_file() {
    use norn_db::TemporaryCleanupStatus;
    let scratch = norn_testkit::scratch::Scratch::new("temporary-cleanup-receipt");
    let path = scratch.join("result.sqlite3");
    let writer = TemporaryDatabase::create(&path, 64, None).unwrap();
    let receipt = writer.shared_file().unwrap().cleanup_receipt();
    assert!(matches!(receipt.status(), TemporaryCleanupStatus::Pending));
    std::fs::rename(&path, scratch.join("original.sqlite3")).unwrap();
    std::fs::write(&path, "replacement must survive").unwrap();
    drop(writer);
    assert!(matches!(
        receipt.status(),
        TemporaryCleanupStatus::Failed(_)
    ));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "replacement must survive"
    );
    let clean = TemporaryDatabase::create(&scratch.join("clean.sqlite3"), 64, None).unwrap();
    let receipt = clean.shared_file().unwrap().cleanup_receipt();
    drop(clean);
    assert!(matches!(receipt.status(), TemporaryCleanupStatus::Removed));
    assert!(!scratch.join("clean.sqlite3").exists());
}

#[test]
#[allow(clippy::disallowed_methods)] // Recognized orphan names are arranged at the filesystem seam.
fn a_sweep_removes_only_exclusive_names_and_their_necessary_sidecars() {
    let scratch = norn_testkit::scratch::Scratch::new("loading-sidecar-sweep");
    let directory = scratch.join("loading");
    std::fs::create_dir(&directory).unwrap();
    let stem = "result-0123456789abcdef0123456789abcdef.sqlite3";
    for suffix in ["", "-wal", "-shm", "-journal", "-unrecognized"] {
        std::fs::write(directory.join(format!("{stem}{suffix}")), "orphan").unwrap();
    }
    std::fs::write(directory.join("result-short.sqlite3-wal"), "keep").unwrap();
    std::fs::create_dir(directory.join("result-ffffffffffffffffffffffffffffffff.sqlite3")).unwrap();
    assert_eq!(sweep_temporary_directory(&directory).unwrap(), 4);
    assert!(directory.join(format!("{stem}-unrecognized")).exists());
    assert!(directory.join("result-short.sqlite3-wal").exists());
    assert!(
        directory
            .join("result-ffffffffffffffffffffffffffffffff.sqlite3")
            .is_dir()
    );
}

#[test]
#[allow(clippy::disallowed_methods)] // Force cleanup failure after exclusive creation, before setup returns.
fn failed_construction_preserves_both_the_operation_error_and_cleanup_evidence() {
    let scratch = norn_testkit::scratch::Scratch::new("failed-temporary-construction");
    let mut receipt = None;
    let failed =
        TemporaryDatabase::create_in_observed(&scratch.join("loading"), 0, None, |created| {
            std::fs::create_dir(&created.paths()[1]).unwrap();
            receipt = Some(created.clone());
        });
    assert!(
        matches!(failed, Err(norn_db::DbError::Sql { .. })),
        "original setup failure keeps its type"
    );
    let receipt = receipt.expect("receipt published at exclusive creation");
    assert!(matches!(
        receipt.status(),
        norn_db::TemporaryCleanupStatus::Failed(norn_db::DbError::Lifecycle { .. })
    ));
    assert!(
        receipt.paths()[0].exists(),
        "failed cleanup cannot claim removal"
    );
}

#[test]
#[allow(clippy::disallowed_methods)] // External main-file loss must not hide remaining sidecars.
fn missing_base_with_remaining_sidecars_is_not_successful_cleanup() {
    let scratch = norn_testkit::scratch::Scratch::new("missing-disposable-base");
    let path = scratch.join("result.sqlite3");
    let writer = TemporaryDatabase::create(&path, 64, None).unwrap();
    let receipt = writer.shared_file().unwrap().cleanup_receipt();
    std::fs::write(&receipt.paths()[1], "residue").unwrap();
    std::fs::remove_file(&path).unwrap();
    drop(writer);
    assert!(
        matches!(receipt.status(), norn_db::TemporaryCleanupStatus::Failed(_)),
        "removing only the main name is not evidence of complete cleanup"
    );
}

#[test]
#[cfg(unix)]
#[allow(clippy::disallowed_methods)] // Arrange links wholly within this test's scratch tree.
fn temporary_sweeps_never_follow_namespace_or_recognized_file_links() {
    let scratch = norn_testkit::scratch::Scratch::new("temporary-sweep-links");
    let outside = scratch.join("outside");
    let namespace = scratch.join("loading");
    std::fs::create_dir(&outside).unwrap();
    std::fs::create_dir(&namespace).unwrap();
    let name = "result-0123456789abcdef0123456789abcdef.sqlite3-wal";
    std::fs::write(outside.join(name), "keep").unwrap();
    std::os::unix::fs::symlink(outside.join(name), namespace.join(name)).unwrap();
    assert_eq!(sweep_temporary_directory(&namespace).unwrap(), 0);
    let alias = scratch.join("linked-loading");
    std::os::unix::fs::symlink(&outside, &alias).unwrap();
    assert!(sweep_temporary_directory(&alias).is_err());
    assert_eq!(std::fs::read_to_string(outside.join(name)).unwrap(), "keep");
}
