//! **The read-only snapshot handle**: what a reader is minted from, what it
//! answers under, and what concurrent reads of one entry pay.
//!
//! A reader is a second connection over one store's file, and every case here
//! reaches it the way a host does: mint one from a live store, establish the
//! snapshot a read answers from, and read what that snapshot was established
//! at. What a statement run on the snapshot returns is the read builders'
//! subject and not this suite's; what is pinned here is the handle.

use std::sync::Arc;
use std::thread;

use crate::common::{Scratch, document, write_document};

/// Write one document, and report the generation the store is at afterwards.
fn write_one(store: &mut norn_store::Store, text: &str) -> i64 {
    let mut request = store.begin_request();
    let outcome = write_document(
        &mut request,
        &document(text, &format!("hash-{text}"), "body"),
    );
    outcome
        .generation
        .expect("a changeset that wrote a document stamps a generation")
}

/// A reader is minted from a live store, and it answers under the reading that
/// store is at: the store's own epoch, and the last generation it committed.
#[test]
fn a_reader_answers_under_the_store_s_own_epoch_and_generation() {
    let scratch = Scratch::new("reader-reading");
    let mut store = scratch.open();
    let generation = write_one(&mut store, "notes/first.md");

    let reader = Arc::new(store.open_reader().expect("a live store mints a reader"));
    assert_eq!(
        reader.epoch(),
        store.epoch(),
        "the reader reads another database"
    );
    assert_eq!(reader.path(), store.path(), "the reader reads another file");

    let snapshot = Arc::clone(&reader).establish().expect("a snapshot");
    assert_eq!(snapshot.reading().epoch(), store.epoch());
    assert_eq!(
        snapshot.reading().write_generation(),
        generation,
        "the reading names a generation the store never committed"
    );
}

/// One read is one snapshot, established by one statement. A deferred `BEGIN`
/// takes no snapshot, so the statement that reads the store's generation is
/// both the establishment and the reading — and it is the only statement the
/// establishment runs.
#[test]
fn establishing_a_snapshot_costs_one_snapshot_and_one_statement() {
    let scratch = Scratch::new("reader-counters");
    let mut store = scratch.open();
    write_one(&mut store, "notes/first.md");
    let reader = Arc::new(store.open_reader().expect("a reader"));

    let snapshot = Arc::clone(&reader).establish().expect("a snapshot");
    let counters = snapshot.counters();
    assert_eq!(
        counters.snapshots_opened(),
        1,
        "a read opened more than one snapshot"
    );
    assert_eq!(
        counters.statements_executed(),
        1,
        "establishing the snapshot ran more than the establishing statement"
    );
    assert_eq!(
        counters.readings().collect::<Vec<_>>(),
        vec![("snapshots_opened", 1), ("statements_executed", 1)],
        "the reading carries another vocabulary"
    );
}

/// The snapshot is an instant. A write that lands after it was established is
/// not in it, and the read that follows the snapshot's end sees that write —
/// which is what says the first reading was a snapshot rather than a stale
/// value.
#[test]
fn a_snapshot_answers_at_the_instant_it_was_established() {
    let scratch = Scratch::new("reader-instant");
    let mut store = scratch.open();
    let first = write_one(&mut store, "notes/first.md");
    let reader = Arc::new(store.open_reader().expect("a reader"));

    let snapshot = Arc::clone(&reader).establish().expect("a snapshot");
    let second = write_one(&mut store, "notes/second.md");
    assert!(
        second > first,
        "the second changeset stamped no later generation"
    );
    assert_eq!(
        snapshot.reading().write_generation(),
        first,
        "the snapshot moved with a write that landed after it"
    );

    drop(snapshot);
    let after = Arc::clone(&reader).establish().expect("a second snapshot");
    assert_eq!(
        after.reading().write_generation(),
        second,
        "a snapshot established after the write did not see it"
    );
}

/// **Concurrent reads of one entry serialize on the one reader**, and the wait
/// that costs is counted rather than assumed. The reading a waiting read
/// carries is what mints more handles through this seam.
#[test]
fn a_second_read_waits_for_the_one_connection_and_reports_the_wait() {
    let scratch = Scratch::new("reader-contention");
    let mut store = scratch.open();
    write_one(&mut store, "notes/first.md");
    let reader = Arc::new(store.open_reader().expect("a reader"));

    let held = Arc::clone(&reader).establish().expect("a first snapshot");
    assert_eq!(held.waits(), 0, "an uncontended read waited");

    let waiting = Arc::clone(&reader);
    let second = thread::spawn(move || waiting.establish().expect("a second snapshot"));
    // The second read cannot establish while the first holds the connection,
    // so the hand-back is what releases it.
    thread::sleep(std::time::Duration::from_millis(50));
    drop(held);
    let second = second.join().expect("the waiting read finished");
    assert!(
        second.waits() >= 1,
        "the second read reported no wait for a connection it could not have had"
    );
}

/// A reader outlives the store it was minted from where a read is still
/// running: the entry lets its own hold go at a teardown, and the read goes on
/// answering from the handle it took.
#[test]
fn a_snapshot_answers_after_the_store_it_was_minted_from_is_gone() {
    let scratch = Scratch::new("reader-outlives");
    let mut store = scratch.open();
    let generation = write_one(&mut store, "notes/first.md");
    let reader = Arc::new(store.open_reader().expect("a reader"));
    let snapshot = Arc::clone(&reader).establish().expect("a snapshot");

    drop(store);
    assert_eq!(
        snapshot.reading().write_generation(),
        generation,
        "the reading a live store answered under moved when that store closed"
    );
}
