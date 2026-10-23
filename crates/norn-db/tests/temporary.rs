//! Scratch databases enforce their own capacity without weakening durable opens.

use norn_db::{TemporaryDatabase, connect};

#[test]
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
