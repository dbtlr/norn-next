//! Opening a store, and the database-side heal rung.
//!
//! Rung 3 is *discard and rebuild*, and it has three triggers: a store schema
//! that is not this build's, which pre-release means the DDL was edited; a schema
//! that no longer holds what the statement list created; and damage the lower
//! rungs cannot resolve. All are reached here, along with the cases that must
//! **not** reach it — a broken environment, a busy database, a name that is not a
//! file — where discarding a sound database would destroy work to fix nothing.

use std::path::{Path, PathBuf};

use norn_store::{OpenOutcome, RebuildReason, Store, StoreError, StoreMode, ddl, induced_failure};

use crate::common::{Scratch, document, path};

/// Write bytes at `path`, preparing its parent.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging a file a store has to judge.
fn write_file(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("a scratch directory");
    }
    std::fs::write(path, bytes).expect("writing a scratch file");
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: the bytes a store wrote, to damage them.
fn read_file(path: &Path) -> Vec<u8> {
    std::fs::read(path).expect("reading a scratch file")
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: whether a store's file survived.
fn exists(path: &Path) -> bool {
    std::fs::metadata(path).is_ok()
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// The first open creates, the second reuses, and the parent directory is
/// prepared rather than required.
#[test]
fn a_first_open_creates_and_a_second_reuses() {
    let scratch = Scratch::new("create");
    let database = scratch.database();
    assert!(!exists(&database), "the file exists before any open");

    let store = Store::open(&database).expect("creating a store");
    assert_eq!(*store.open_outcome(), OpenOutcome::Created);
    assert_eq!(store.mode(), StoreMode::Durable);
    assert_eq!(store.path(), database.as_path());
    let recorded = store.recorded_store_schema().expect("the recorded schema");
    assert_eq!(recorded.version, Some(ddl::STORE_SCHEMA_VERSION));
    assert_eq!(recorded.ddl_fingerprint, Some(ddl::fingerprint()));
    assert_eq!(
        recorded.schema_digest,
        Some(store.schema_digest().expect("the schema digest")),
        "a database records the schema it was created holding"
    );
    store.verify_integrity().expect("a store it just created");
    drop(store);

    assert!(exists(&database));
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
    assert!(ddl::statements().len() > 20, "the statement list is short");
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
    induced_failure::record_store_schema_out_of_band(
        &mut store,
        ddl::STORE_SCHEMA_VERSION,
        "0000000000000000",
    )
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
    let recorded = rebuilt
        .recorded_store_schema()
        .expect("the recorded schema");
    assert_eq!(recorded.version, Some(ddl::STORE_SCHEMA_VERSION));
    assert_eq!(recorded.ddl_fingerprint, Some(ddl::fingerprint()));
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
    induced_failure::record_store_schema_out_of_band(&mut store, 7, &ddl::fingerprint())
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

/// **The fingerprint is compared against a self-report, so it cannot see a
/// dropped object.** The schema digest is what does: an index removed behind the
/// store's back leaves the version and the fingerprint intact while the database
/// can no longer keep the promise the index carried, and the next open rebuilds.
#[test]
fn a_schema_that_no_longer_holds_what_it_was_created_with_is_rebuilt_from_zero() {
    let scratch = Scratch::new("dropped-index");
    let database = scratch.database();
    let subject = path("docs/norn/glossary.md");

    let mut store = Store::open(&database).expect("creating a store");
    store
        .begin_request()
        .upsert_document(&document(subject.as_str(), "hash-1", "a body\n"))
        .expect("writing a document");
    let recorded = store.recorded_store_schema().expect("the recorded schema");

    // The unique index on `path` is what makes a path one document. Without it a
    // re-derivation inserts a second row rather than updating the first.
    induced_failure::execute_out_of_band(&mut store, "DROP INDEX documents_path")
        .expect("dropping an index");
    assert_eq!(
        store.recorded_store_schema().expect("the recorded schema"),
        recorded,
        "dropping an index moved the recorded pair, so it was not blind to one after all"
    );
    assert_ne!(
        store.schema_digest().expect("the schema digest"),
        recorded.schema_digest.clone().expect("a recorded digest"),
        "the digest of the schema in front of the reader did not move"
    );
    drop(store);

    let mut rebuilt = Store::open(&database).expect("reopening a store");
    match rebuilt.open_outcome() {
        OpenOutcome::RebuiltFromZero(RebuildReason::Damaged { detail }) => {
            assert!(detail.contains("changed or removed"), "{detail}");
        }
        other => panic!("the store opened as {other:?} rather than rebuilding"),
    }
    assert_eq!(
        rebuilt
            .begin_request()
            .stored_document(&subject)
            .expect("reading a document"),
        None
    );
    rebuilt.verify_integrity().expect("a rebuilt store");
}

/// The other trigger, and the one that outlives the pre-release build: a file
/// that is not a database at all.
#[test]
fn a_file_that_is_not_a_database_is_rebuilt_from_zero() {
    let scratch = Scratch::new("garbage");
    let database = scratch.database();
    write_file(&database, b"this is not a database\n");

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
    let mut bytes = read_file(&database);
    for byte in bytes.iter_mut().skip(100) {
        *byte = 0x5a;
    }
    write_file(&database, &bytes);

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
    let blocker = scratch.database();
    write_file(&blocker, b"a file where a directory would go\n");

    let error = Store::open(blocker.join("store.sqlite3")).expect_err("a hostile environment");
    let StoreError::Lifecycle { operation, .. } = &error else {
        panic!("the environment failed as {error:?} rather than as a lifecycle refusal");
    };
    assert!(operation.contains("preparing"), "{operation}");
}

/// The same rule for a database somebody else holds. A pinned-scalar read that
/// reports the database busy says nothing about the stored state, so it is
/// reported — and the database it could not read is still there afterwards, with
/// everything that was in it.
#[test]
fn a_database_that_reports_itself_busy_is_refused_rather_than_rebuilt() {
    let scratch = Scratch::new("busy");
    let database = scratch.database();
    let subject = path("docs/norn/glossary.md");

    let mut store = Store::open(&database).expect("creating a store");
    store
        .begin_request()
        .upsert_document(&document(subject.as_str(), "hash-1", "a body\n"))
        .expect("writing a document");
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
            .stored_document(&subject)
            .expect("reading a document")
            .is_some(),
        "the refusal discarded a database it could not even read"
    );
}

/// A name SQLite reads as something other than the file it spells is a caller
/// error, and it is refused as one. It is emphatically not a verdict about
/// stored state: the file-lifecycle operations would otherwise be removing a
/// path nothing was ever written to and reporting success.
#[test]
fn a_name_that_is_not_a_file_is_refused_as_a_caller_error() {
    for name in [":memory:", ""] {
        let error = Store::open(name).expect_err("a name that is not a file");
        let StoreError::Lifecycle { operation, .. } = &error else {
            panic!("`{name}` was refused as {error:?} rather than as a lifecycle refusal");
        };
        assert!(operation.contains("opening"), "{operation}");
    }
}

/// **URI filenames are off**, so a path that looks like one is a filename. A
/// store whose path begins `file:` is a file called that, which is what makes
/// removing it at rung 3 remove the database that was written.
#[test]
fn a_path_that_looks_like_a_uri_is_a_filename() {
    let scratch = Scratch::new("uri");
    let uri_shaped = scratch
        .database()
        .parent()
        .expect("a parent directory")
        .join("file:store.sqlite3?mode=memory&cache=shared");

    let store = Store::open(&uri_shaped).expect("creating a store at a URI-shaped path");
    assert_eq!(*store.open_outcome(), OpenOutcome::Created);
    drop(store);
    assert!(
        exists(&uri_shaped),
        "the database went somewhere other than the path it was given"
    );

    let store = Store::open(&uri_shaped).expect("reopening a store");
    assert_eq!(*store.open_outcome(), OpenOutcome::Reused);
    store.discard().expect("discarding a store");
    assert!(!exists(&uri_shaped), "the discard removed another file");
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
    assert!(exists(&database));
    store.close().expect("closing a throwaway store");
    assert!(!exists(&database), "close left the file behind");

    let store = Store::open_throwaway(&database).expect("creating a throwaway store");
    assert!(exists(&database));
    drop(store);
    assert!(!exists(&database), "a drop left the file behind");
}

/// **A throwaway open over a durable store is refused.** Adopting it would arm a
/// teardown that deletes a registered vault's whole derived state when the store
/// drops, with nothing said about it.
#[test]
fn a_throwaway_store_refuses_to_adopt_a_durable_one() {
    let scratch = Scratch::new("throwaway-over-durable");
    let database = scratch.database();
    let subject = path("docs/norn/glossary.md");

    let mut durable = Store::open(&database).expect("creating a durable store");
    durable
        .begin_request()
        .upsert_document(&document(subject.as_str(), "hash-1", "a body\n"))
        .expect("writing a document");
    drop(durable);

    let error = Store::open_throwaway(&database).expect_err("a throwaway over a durable store");
    let StoreError::Lifecycle { operation, .. } = &error else {
        panic!("it was refused as {error:?} rather than as a lifecycle refusal");
    };
    assert!(operation.contains("throwaway"), "{operation}");

    assert!(exists(&database), "the refused open removed the file");
    let mut reopened = Store::open(&database).expect("reopening the durable store");
    assert!(
        reopened
            .begin_request()
            .stored_document(&subject)
            .expect("reading a document")
            .is_some(),
        "the refused open took the durable store's derived state with it"
    );
}

/// The other direction is adoption. The leftovers of a throwaway store are
/// rebuildable derived state at a path a durable store was asked for, so the
/// durable store takes the file over — and it is a durable store afterwards.
#[test]
fn a_durable_store_adopts_the_leftovers_of_a_throwaway_one() {
    let scratch = Scratch::new("durable-over-throwaway");
    let database = scratch.database();

    // A process that died rather than closing leaves the file behind.
    let leftover = Store::open_throwaway(&database).expect("creating a throwaway store");
    std::mem::forget(leftover);

    let adopted = Store::open(&database).expect("adopting the leftovers");
    assert_eq!(*adopted.open_outcome(), OpenOutcome::Reused);
    drop(adopted);
    assert!(exists(&database), "the adopting store tore the file down");

    Store::open_throwaway(&database)
        .expect_err("a throwaway open over what is now a durable store");
}

/// A durable store's file does outlive it, which is the whole difference.
#[test]
fn a_durable_store_keeps_its_file() {
    let scratch = Scratch::new("durable");
    let database = scratch.database();
    let store = Store::open(&database).expect("creating a store");
    store.close().expect("closing a store");
    assert!(exists(&database));
}

/// Rung 3 reached deliberately: the store discards its own file, and the next
/// open builds one from the statement list. Every sidecar a journal leaves goes
/// with it — a rebuilt database beside a stale journal is a database with
/// somebody else's committed pages in it.
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
    // A rollback journal beside a store this build did not write, which is the
    // kind of file rung 3 is reached for.
    write_file(&sidecar(&database, "-journal"), b"a stale rollback journal");
    store.discard().expect("discarding a store");

    for suffix in ["", "-wal", "-shm", "-journal"] {
        let candidate = sidecar(&database, suffix);
        assert!(
            !exists(&candidate),
            "`{}` survived the discard",
            candidate.display()
        );
    }

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
