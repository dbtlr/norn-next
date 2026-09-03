//! Conditions the machine imposes on a store, and the line between them and
//! damage.
//!
//! Three of this crate's claims are about failures that are not the
//! database's fault and must never be answered as though they were. A **full
//! disk** stops an increment from completing and says nothing about the pages
//! already at rest, so it is refused and nothing is discarded. A **database
//! held by somebody else** is the same shape at the open: a busy pinned-scalar
//! read is refused, and the database it could not read is intact afterwards.
//! A **store schema that cannot be written** is the other end: a creation is
//! what rung 3 resolves damage *with*, so a creation that itself meets a
//! corrupt database is the one state no rung above it can resolve, and it is
//! typed as damage at the statement that met it.
//!
//! The cases live in a binary of their own because most arrangements they use
//! are process-wide — a page cap is read by every connection opened after it,
//! and a store creation armed to fail is armed for whichever creation runs
//! next. Sharing a process with the rest of the suite would arm them for cases
//! that never asked. The busy arm is per-thread and one-shot rather than
//! process-wide; it sits with these cases because it states the same kind of
//! claim. Inside this binary they take one lock and disarm on the way out.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use norn_store::{
    Change, DocumentFacts, DocumentPath, IncrementProvenance, OpenOutcome, Store, StoreError, ddl,
    induced_failure,
};
use norn_testkit::scratch::Scratch as TestkitScratch;

/// One case at a time: every arrangement here is process-wide.
fn serially() -> MutexGuard<'static, ()> {
    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    GUARD
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Everything back to what a store opened in an ordinary process meets.
struct Disarmed;

impl Drop for Disarmed {
    fn drop(&mut self) {
        induced_failure::disarm();
    }
}

/// A store's database under a directory that lasts one case.
///
/// The naming and the removal are [`norn_testkit::scratch::Scratch`]'s; what
/// this adds is where a store's file sits inside the tree and how one is
/// opened.
struct Scratch {
    root: TestkitScratch,
}

impl Scratch {
    fn new(label: &str) -> Scratch {
        Scratch {
            root: TestkitScratch::new(&format!("norn-store-{label}")),
        }
    }

    fn database(&self) -> PathBuf {
        self.root.join("derived").join("store.sqlite3")
    }

    fn open(&self) -> Store {
        Store::open(self.database()).expect("opening a store")
    }
}

/// One document's facts, spelled once.
fn document(path: &str, hash: &str, body: &str) -> DocumentFacts {
    DocumentFacts::new(
        DocumentPath::new(path).expect("a document path"),
        hash.to_string(),
        body.to_string(),
        body.len() as u64,
    )
}

/// Write one document as a changeset of its own.
fn write_document(store: &mut Store, facts: DocumentFacts) {
    store
        .begin_request()
        .apply_increment(IncrementProvenance::Derived, [Change::Upsert(facts)])
        .expect("applying a document upsert");
}

/// **A full disk refuses the increment and is not damage.**
///
/// The condition is met by the engine rather than described to it: the
/// connection is held to the page count the database already has, so the next
/// statement that must grow the file reports `SQLITE_FULL` — the same code a
/// disk with no room left produces. The refusal must not carry damage, because
/// a damaged verdict authorizes discarding the database, and discarding a sound
/// one to fix a full disk destroys work to fix nothing.
///
/// The second half is the recovery: clearing the condition and applying the
/// same increment lands it, so what was refused was the environment rather than
/// the work.
#[test]
fn a_full_disk_refuses_the_increment_and_is_never_typed_as_damage() {
    let _serial = serially();
    let _disarmed = Disarmed;
    let scratch = Scratch::new("full-disk");
    let standing = "notes/standing.md";

    let mut store = scratch.open();
    write_document(
        &mut store,
        document(standing, "hash-1", "the row that was already there\n"),
    );
    let held = held_pages(&mut store);
    drop(store);

    induced_failure::cap_the_pages(held);
    let mut store = scratch.open();
    let error = store
        .begin_request()
        .apply_increment(IncrementProvenance::Derived, growth())
        .expect_err("an increment with no room to land in");
    assert_eq!(
        error.damage(),
        None,
        "a full disk was typed as damage, which authorizes discarding the database: {error:?}"
    );
    assert!(
        error.to_string().contains("full"),
        "the refusal does not say the disk is full: {error}"
    );
    assert!(
        store
            .begin_request()
            .stored_document(&DocumentPath::new(standing).expect("a document path"))
            .expect("reading the row that was already there")
            .is_some(),
        "the refused increment took a row that had already committed"
    );
    drop(store);

    induced_failure::uncap_the_pages();
    let mut store = scratch.open();
    store
        .begin_request()
        .apply_increment(IncrementProvenance::Derived, growth())
        .expect("the same increment, with room for it");
    assert!(
        store
            .begin_request()
            .stored_document(&DocumentPath::new("notes/grown-0.md").expect("a document path"))
            .expect("reading a row the second attempt landed")
            .is_some()
    );
}

/// **A database somebody else holds meets the same rule as a full disk.**
///
/// A pinned-scalar read that reports the database busy says nothing about the
/// stored state, so an open that meets one is refused rather than sent to rung
/// 3: rebuilding from zero in response would discard a sound database to fix a
/// condition that clears on its own. The database an open could not even read
/// is still there afterwards, with everything that was in it.
#[test]
fn a_database_that_reports_itself_busy_is_refused_rather_than_rebuilt() {
    let _serial = serially();
    let _disarmed = Disarmed;
    let scratch = Scratch::new("busy");
    let database = scratch.database();
    let subject = "docs/norn/glossary.md";

    let mut store = scratch.open();
    write_document(&mut store, document(subject, "hash-1", "a body\n"));
    drop(store);

    induced_failure::fail_next_meta_read_as_busy();
    let error = Store::open(&database).expect_err("a busy database");
    assert!(
        matches!(error, StoreError::Sql { .. }),
        "a busy database was reported as {error:?} rather than as a refused operation"
    );

    let mut reopened = Store::open(&database).expect("reopening a store");
    assert_eq!(*reopened.open_outcome(), OpenOutcome::Reused);
    assert!(
        reopened
            .begin_request()
            .stored_document(&DocumentPath::new(subject).expect("a document path"))
            .expect("reading a document")
            .is_some(),
        "the refusal discarded a database it could not even read"
    );
}

/// **A store schema that meets a corrupt database is damage.**
///
/// Rung 3 resolves damage by discarding the file and running the statement list
/// again, so this is the one failure above the top of the ladder: the act that
/// *is* the resolution met the condition it was resolving. Typing it as
/// anything but damage would send a host back to a lower rung to meet it again.
#[test]
fn a_store_schema_that_meets_a_corrupt_database_is_damage() {
    let _serial = serially();
    let _disarmed = Disarmed;
    let scratch = Scratch::new("create-corrupt");

    induced_failure::corrupt_the_next_store_creation_at(&scratch.database());
    let error = Store::open(scratch.database()).expect_err("a store schema that met corruption");
    assert!(
        error.damage().is_some(),
        "a store schema that met a corrupt database reported {error:?}"
    );

    // One-shot: the arm is taken by the statement that met it, so the next
    // creation is an ordinary one and the case leaves nothing armed behind it.
    let mut store = Store::open(scratch.database()).expect("creating a store afterwards");
    write_document(&mut store, document("notes/after.md", "hash-1", "a body\n"));
}

/// **A store schema refused at a statement names the statement.**
///
/// A read-only database is not damage — it says nothing about the file's
/// contents — so it reports as the refused operation, and what it has to carry
/// is *which* of the statement list's statements it stopped at. A creation is
/// the one place in this crate where one refusal can come from any of a dozen
/// statements, and a message that named none of them leaves the list to be
/// bisected by hand.
#[test]
fn a_store_schema_refused_at_a_statement_names_the_statement() {
    let _serial = serially();
    let _disarmed = Disarmed;
    let scratch = Scratch::new("create-refused");
    let first = ddl::statements()
        .first()
        .and_then(|statement| statement.lines().next().map(str::to_string))
        .expect("the statement list is not empty");

    induced_failure::refuse_the_next_store_creation_at(&scratch.database());
    let error = Store::open(scratch.database()).expect_err("a store schema that was refused");
    assert!(
        error.damage().is_none(),
        "a read-only database was typed as damage: {error:?}"
    );
    let StoreError::Sql { operation, message } = &error else {
        panic!("a refused creation reported {error:?} rather than a refused operation");
    };
    assert!(operation.contains("store schema"), "{operation}");
    assert!(
        message.contains(&first),
        "the refusal names no statement: {message}"
    );
}

/// The changesets this crate's own suite tears are counted, and the count is
/// what a case arms a tear against. A count that never moved would make every
/// arm either fire immediately or never.
#[test]
fn every_committed_changeset_is_counted() {
    let _serial = serially();
    let _disarmed = Disarmed;
    let scratch = Scratch::new("counted");
    let mut store = scratch.open();

    let before = induced_failure::changesets_committed();
    write_document(
        &mut store,
        document("notes/counted.md", "hash-1", "a body\n"),
    );
    assert_eq!(induced_failure::changesets_committed(), before + 1);

    // An empty changeset opens no transaction and commits nothing, so it is not
    // a boundary anything can be torn at.
    store
        .begin_request()
        .apply_increment(IncrementProvenance::Derived, [])
        .expect("an empty changeset");
    assert_eq!(induced_failure::changesets_committed(), before + 1);
}

/// The page count a database is holding, read through the engine.
fn held_pages(store: &mut Store) -> u64 {
    induced_failure::page_count(store).expect("reading the page count")
}

/// Documents a database that may not grow cannot hold.
///
/// Enough of them, and each big enough, that no arrangement of free space
/// inside the pages already allocated can take them: the increment has to grow
/// the file, which is the condition the cap makes impossible.
fn growth() -> Vec<Change> {
    (0..64)
        .map(|index| {
            let body =
                "a body no page has room left for beside sixty-three of its kind\n".repeat(16);
            Change::Upsert(document(
                &format!("notes/grown-{index}.md"),
                &format!("hash-grown-{index}"),
                &body,
            ))
        })
        .collect()
}
