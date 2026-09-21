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

use crate::common::{Disarmed, Scratch, document, write_document};

/// The snapshot a case reads from: the handle's turn, taken where nothing
/// holds it, and the snapshot established on it.
fn a_snapshot(reader: &Arc<norn_store::SnapshotReader>) -> norn_store::Snapshot {
    reader
        .try_take()
        .expect("a handle nothing is reading holds its connection")
        .establish()
        .snapshot
        .expect("a snapshot")
}

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

/// **The mint reports what it ran against the database**, so a caller that
/// holds a lock across it can say what that lock paid for. The read-only open
/// runs two statements the database answers — the journal-mode read and the
/// store-epoch read — and the establishment a read runs afterwards is a third
/// that is not this mint's.
#[test]
fn a_mint_reports_the_statements_it_ran_against_the_database() {
    let scratch = Scratch::new("reader-mint-cost");
    let mut store = scratch.open();
    write_one(&mut store, "notes/first.md");

    let minted = store.open_reader();
    let reader = Arc::new(minted.reader.expect("a live store mints a reader"));
    assert_eq!(
        minted.statements, 2,
        "the mint reported a cost other than the two statements the read-only open runs"
    );
    assert_eq!(
        a_snapshot(&reader).counters().statements_executed(),
        1,
        "the establishment ran the mint's statements again instead of its own one"
    );
}

/// **An establishment that refuses reports what it ran**, and what it ran is
/// the statement that refused it: the transaction opened, the write-generation
/// read met a busy database, and the rollback that followed read nothing. A
/// caller that holds a lock across the attempt waited for all of it, so an
/// attempt that reported nothing would leave that wait invisible in exactly
/// the case it is longest.
///
/// The control is the read after it. The connection came back, so the next
/// establishment answers — which is only possible because the refused attempt
/// rolled its transaction back rather than leaving one open.
#[test]
fn an_establishment_that_refuses_reports_what_it_ran_and_gives_the_connection_back() {
    // The arm below is per-thread and one-shot, and the establishment it is
    // armed for is what consumes it. The guard is what puts it back on every
    // other path out of this case, an early return and a panic alike: the
    // seam's contract is that an arm a case leaves standing must not fail
    // whatever opens next on its thread.
    let _disarmed = Disarmed;
    let scratch = Scratch::new("reader-refused-establishment");
    let mut store = scratch.open();
    write_one(&mut store, "notes/first.md");
    let reader = Arc::new(
        store
            .open_reader()
            .reader
            .expect("a live store mints a reader"),
    );

    norn_store::induced_failure::fail_next_meta_read_as_busy();
    let refused = reader
        .try_take()
        .expect("a handle nothing is reading holds its connection")
        .establish();
    refused
        .snapshot
        .expect_err("a busy write-generation read established a snapshot");
    assert_eq!(
        refused.counters.statements_executed(),
        1,
        "the refused attempt reported something other than the statement that refused it"
    );
    assert_eq!(
        refused.counters.snapshots_opened(),
        1,
        "the refused attempt opened no transaction to run that statement in"
    );

    let answered = a_snapshot(&reader);
    assert_eq!(
        answered.counters().statements_executed(),
        1,
        "the read after the refusal ran something other than its own one statement"
    );
}

/// A reader is minted from a live store, and it answers under the reading that
/// store is at: the store's own epoch, and the last generation it committed.
#[test]
fn a_reader_answers_under_the_store_s_own_epoch_and_generation() {
    let scratch = Scratch::new("reader-reading");
    let mut store = scratch.open();
    let generation = write_one(&mut store, "notes/first.md");

    let reader = Arc::new(
        store
            .open_reader()
            .reader
            .expect("a live store mints a reader"),
    );
    assert_eq!(
        reader.epoch(),
        store.epoch(),
        "the reader reads another database"
    );
    // The handle names which database it reads and never where that database
    // is: a caller holding the path opens its own connection to it and reads
    // under no hold at all, which is the escape this type has no accessor and
    // no Debug field for.
    let rendered = format!("{reader:?}");
    assert!(
        !rendered.contains(
            store
                .path()
                .to_str()
                .expect("a scratch path is representable")
        ),
        "the handle's rendering hands out its database file: {rendered}"
    );

    let snapshot = a_snapshot(&reader);
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
    let reader = Arc::new(store.open_reader().reader.expect("a reader"));

    let snapshot = a_snapshot(&reader);
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
    let reader = Arc::new(store.open_reader().reader.expect("a reader"));

    let snapshot = a_snapshot(&reader);
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
    let after = a_snapshot(&reader);
    assert_eq!(
        after.reading().write_generation(),
        second,
        "a snapshot established after the write did not see it"
    );
}

/// **Concurrent reads of one entry serialize on the one connection.** The
/// handle hands out one turn at a time: a second read finds no turn to take
/// while the first holds it, and the wait it then pays outside the caller's
/// lock ends when the first read gives the connection back.
#[test]
fn a_second_read_waits_for_the_one_connection_and_takes_it_when_it_comes_back() {
    let scratch = Scratch::new("reader-contention");
    let mut store = scratch.open();
    write_one(&mut store, "notes/first.md");
    let reader = Arc::new(store.open_reader().reader.expect("a reader"));

    let held = a_snapshot(&reader);
    assert!(
        reader.try_take().is_none(),
        "a handle a read is answering on handed out a second turn"
    );

    let waiting = Arc::clone(&reader);
    let second = thread::spawn(move || {
        waiting
            .wait_for_the_connection()
            .establish()
            .snapshot
            .expect("a second snapshot")
    });
    // The second read cannot establish while the first holds the connection,
    // so the hand-back is what releases it.
    thread::sleep(std::time::Duration::from_millis(50));
    drop(held);
    let second = second.join().expect("the waiting read finished");
    assert!(
        second.reading().write_generation() > 0,
        "the read that waited for the connection answered under no reading"
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
    let reader = Arc::new(store.open_reader().reader.expect("a reader"));
    let snapshot = a_snapshot(&reader);

    drop(store);
    assert_eq!(
        snapshot.reading().write_generation(),
        generation,
        "the reading a live store answered under moved when that store closed"
    );
}
