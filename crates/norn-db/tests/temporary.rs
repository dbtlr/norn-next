//! Scratch databases enforce their own capacity without weakening durable opens.

use norn_db::{TemporaryDatabase, connect};

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
