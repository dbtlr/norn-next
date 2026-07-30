//! Opening a store, and the database-side heal rung.
//!
//! Rung 3 is *discard and rebuild*, and it has two triggers with different
//! lifetimes: a store schema that is not this build's, which pre-release means
//! the DDL was edited, and damage the lower rungs cannot resolve. Both are
//! reached here, along with the case that must **not** reach it — a broken
//! environment, where discarding a sound database would destroy work to fix
//! nothing.

mod common;

use common::{Scratch, document, path};
use norn_store::{OpenOutcome, RebuildReason, Store, StoreError, StoreMode, ddl};

/// The first open creates, the second reuses, and the parent directory is
/// prepared rather than required.
#[test]
fn a_first_open_creates_and_a_second_reuses() {
    let scratch = Scratch::new("create");
    let database = scratch.database();
    assert!(
        !scratch.exists(&database),
        "the file exists before any open"
    );

    let store = Store::open(&database).expect("creating a store");
    assert_eq!(*store.open_outcome(), OpenOutcome::Created);
    assert_eq!(store.mode(), StoreMode::Durable);
    assert_eq!(store.path(), database.as_path());
    assert_eq!(
        store.recorded_store_schema().expect("the recorded schema"),
        (Some(ddl::STORE_SCHEMA_VERSION), Some(ddl::fingerprint()))
    );
    store.verify_integrity().expect("a store it just created");
    drop(store);

    assert!(scratch.exists(&database));
    let reopened = Store::open(&database).expect("reopening a store");
    assert_eq!(*reopened.open_outcome(), OpenOutcome::Reused);
}

/// A store schema version is pinned at 1 and the fingerprint is the digest of
/// the whole statement list, so both are readable facts rather than claims.
#[test]
fn the_store_schema_version_is_pinned_and_the_fingerprint_is_the_statement_list() {
    assert_eq!(ddl::STORE_SCHEMA_VERSION, 1);

    let fingerprint = ddl::fingerprint();
    assert_eq!(
        fingerprint.len(),
        16,
        "`{fingerprint}` is not 16 hex digits"
    );
    assert!(
        fingerprint.chars().all(|digit| digit.is_ascii_hexdigit()),
        "`{fingerprint}` is not hex"
    );
    assert_eq!(fingerprint, ddl::fingerprint(), "it is not deterministic");
    assert!(
        ddl::statements().count() > 20,
        "the statement list is short"
    );
}

/// **The pre-release rung-3 trigger.** A database recording a store schema this
/// build did not write is discarded and rebuilt, and what was in it goes with
/// it: the derived state was derived under a shape that no longer exists.
#[test]
fn a_ddl_fingerprint_this_build_did_not_write_is_rebuilt_from_zero() {
    let scratch = Scratch::new("fingerprint");
    let database = scratch.database();
    let document_path = path("docs/norn/glossary.md");

    let mut store = Store::open(&database).expect("creating a store");
    store
        .begin_request()
        .upsert_document(&document(document_path.as_str(), "hash-1", "a body\n"))
        .expect("writing a document");
    store
        .record_store_schema(ddl::STORE_SCHEMA_VERSION, "0000000000000000")
        .expect("recording a store schema this build did not write");
    drop(store);

    let mut rebuilt = Store::open(&database).expect("reopening a store");
    match rebuilt.open_outcome() {
        OpenOutcome::RebuiltFromZero(RebuildReason::DdlFingerprint { expected, found }) => {
            assert_eq!(*expected, ddl::fingerprint());
            assert_eq!(found.as_deref(), Some("0000000000000000"));
        }
        other => panic!("the store opened as {other:?} rather than rebuilding"),
    }
    assert_eq!(
        rebuilt
            .recorded_store_schema()
            .expect("the recorded schema"),
        (Some(ddl::STORE_SCHEMA_VERSION), Some(ddl::fingerprint()))
    );
    assert_eq!(
        rebuilt
            .begin_request()
            .stored_document(&document_path)
            .expect("reading a document"),
        None,
        "a rebuild from zero kept derived state from the shape it discarded"
    );
    rebuilt.verify_integrity().expect("a rebuilt store");
}

/// The version branch of the same trigger. It does not move through the
/// pre-release build, which is exactly why a database claiming another version
/// is not a database this build reads.
#[test]
fn a_store_schema_version_that_is_not_pinned_is_rebuilt_from_zero() {
    let scratch = Scratch::new("version");
    let database = scratch.database();

    let mut store = Store::open(&database).expect("creating a store");
    store
        .record_store_schema(7, &ddl::fingerprint())
        .expect("recording another version");
    drop(store);

    let rebuilt = Store::open(&database).expect("reopening a store");
    match rebuilt.open_outcome() {
        OpenOutcome::RebuiltFromZero(RebuildReason::StoreSchemaVersion { expected, found }) => {
            assert_eq!(*expected, ddl::STORE_SCHEMA_VERSION);
            assert_eq!(*found, Some(7));
        }
        other => panic!("the store opened as {other:?} rather than rebuilding"),
    }
}

/// The other trigger, and the one that outlives the pre-release build: a file
/// that is not a database at all.
#[test]
fn a_file_that_is_not_a_database_is_rebuilt_from_zero() {
    let scratch = Scratch::new("garbage");
    let database = scratch.write("derived/store.sqlite3", b"this is not a database\n");

    let store = Store::open(&database).expect("opening over a file that is not a database");
    match store.open_outcome() {
        OpenOutcome::RebuiltFromZero(RebuildReason::Damaged { detail }) => {
            assert!(!detail.is_empty(), "the damage was not named");
        }
        other => panic!("the store opened as {other:?} rather than rebuilding"),
    }
    store.verify_integrity().expect("a rebuilt store");
}

/// A store whose pages have been overwritten is damaged rather than merely
/// out of date, and the same rung resolves it.
#[test]
fn a_store_whose_pages_were_overwritten_is_rebuilt_from_zero() {
    let scratch = Scratch::new("corrupt");
    let database = scratch.database();

    let mut store = Store::open(&database).expect("creating a store");
    store
        .begin_request()
        .upsert_document(&document("glossary.md", "hash-1", "a body\n"))
        .expect("writing a document");
    drop(store);

    // The header stays a SQLite header and the schema pages behind it do not,
    // which is the shape corruption takes: the file opens and then cannot be
    // read.
    let mut bytes = scratch.read(&database);
    for byte in bytes.iter_mut().skip(100) {
        *byte = 0x5a;
    }
    scratch.write("derived/store.sqlite3", &bytes);

    let store = Store::open(&database).expect("opening over a corrupt database");
    match store.open_outcome() {
        OpenOutcome::RebuiltFromZero(RebuildReason::Damaged { detail }) => {
            assert!(!detail.is_empty(), "the damage was not named");
        }
        other => panic!("the store opened as {other:?} rather than rebuilding"),
    }
    store.verify_integrity().expect("a rebuilt store");
}

/// **Rung 3 is for damaged state, never for a hostile environment.** A parent
/// that cannot be prepared is reported, and nothing is discarded — there is
/// nothing wrong with the stored state to discard.
#[test]
fn an_environment_that_cannot_hold_a_database_is_refused_rather_than_rebuilt() {
    let scratch = Scratch::new("hostile");
    let blocker = scratch.write("blocker", b"a file where a directory would go\n");

    let error = Store::open(blocker.join("store.sqlite3")).expect_err("a hostile environment");
    let StoreError::Lifecycle { operation, .. } = &error else {
        panic!("the environment failed as {error:?} rather than as a lifecycle refusal");
    };
    assert!(operation.contains("preparing"), "{operation}");
}

/// A throwaway store's file does not outlive it, whether it is closed or
/// dropped. Disposable derivation is what an unregistered root gets, and it
/// would otherwise accumulate on disk.
#[test]
fn a_throwaway_store_tears_its_file_down() {
    let scratch = Scratch::new("throwaway");
    let database = scratch.database();

    let store = Store::open_throwaway(&database).expect("creating a throwaway store");
    assert_eq!(store.mode(), StoreMode::Throwaway);
    assert!(scratch.exists(&database));
    store.close().expect("closing a throwaway store");
    assert!(!scratch.exists(&database), "close left the file behind");

    let store = Store::open_throwaway(&database).expect("creating a throwaway store");
    assert!(scratch.exists(&database));
    drop(store);
    assert!(!scratch.exists(&database), "a drop left the file behind");
}

/// A durable store's file does outlive it, which is the whole difference.
#[test]
fn a_durable_store_keeps_its_file() {
    let scratch = Scratch::new("durable");
    let database = scratch.database();
    let store = Store::open(&database).expect("creating a store");
    store.close().expect("closing a store");
    assert!(scratch.exists(&database));
}

/// Rung 3 reached deliberately: the store discards its own file, and the next
/// open builds one from the statement list.
#[test]
fn a_discarded_store_is_gone_and_the_next_open_creates() {
    let scratch = Scratch::new("discard");
    let database = scratch.database();
    let document_path = path("glossary.md");

    let mut store = Store::open(&database).expect("creating a store");
    store
        .begin_request()
        .upsert_document(&document(document_path.as_str(), "hash-1", "a body\n"))
        .expect("writing a document");
    store.discard().expect("discarding a store");
    assert!(
        !scratch.exists(&database),
        "the discarded file is still there"
    );

    let mut created = Store::open(&database).expect("creating a store again");
    assert_eq!(*created.open_outcome(), OpenOutcome::Created);
    assert_eq!(
        created
            .begin_request()
            .stored_document(&document_path)
            .expect("reading a document"),
        None
    );
}
