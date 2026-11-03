//! The store's verification in its two halves: the read-only checks, which
//! run over one snapshot of a read connection of their own, and the full-text
//! check, which is a write and runs on the store's writer.
//!
//! The whole verification a from-zero build closes with is both halves, and
//! what the halves report together is what it reports: the same verdict on a
//! sound store and the same damage, named the same way, on a store damaged in
//! each way the verification looks for. The read-only half can be stopped
//! from another thread, and a stopped verification reaches no verdict.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::common::{
    Scratch, document, document_with_every_fact, path, path_names_no_document_in_class,
    record_death, unread_block, violation, write_document,
};
use norn_store::{
    Provenance, ReadOnlyVerification, Store, StoreError, StoredPathOrder, induced_failure,
};
use norn_wire::{FindingKind, Severity};

/// The store's verification as its two halves run it, in the order the
/// whole verification asks: the read-only half's verdict, then the full-text
/// check's. A stopped read-only half is a failure of the case: nothing here
/// stops it.
fn verified_in_halves(store: &Store) -> Result<(), StoreError> {
    match store.verification_source().verify(|| {}) {
        ReadOnlyVerification::Sound => store.verify_full_text(),
        ReadOnlyVerification::Failed(error) => Err(error),
        ReadOnlyVerification::Interrupted => panic!("a verification nothing stopped was stopped"),
    }
}

/// A store holding a document with every fact, a death, a link-health
/// finding, a rule finding citing a set, and a content finding beside a
/// quarantined place: every table a check of the verification reads holds a
/// row for an arrangement to damage.
fn a_store_with_a_row_for_every_check(scratch: &Scratch) -> Store {
    let mut store = scratch.open();
    let subject = path("docs/norn/glossary.md");
    let mut request = store.begin_request();
    write_document(
        &mut request,
        &document_with_every_fact(subject.as_str(), "hash-1"),
    );
    record_death(&mut request, &path("gone.md"), Provenance::HealPrune);
    request
        .record_finding(&path_names_no_document_in_class(
            subject.as_str(),
            "glossary",
            "glossary/",
            &[subject.as_str()],
            1,
        ))
        .expect("recording a link-health finding");
    let mut citing = unread_block("a.md");
    citing.kind = FindingKind::NotOneOf;
    citing.severity = Severity::Warning;
    citing.target = Some("status".to_string());
    citing.rules = ["open".to_string(), "tasks".to_string()]
        .into_iter()
        .collect();
    citing.value = Some("someday".to_string());
    request
        .record_finding(&citing)
        .expect("recording a rule finding");
    request
        .record_finding(&violation("q.md"))
        .expect("recording a content finding");
    store
}

/// Each way the whole verification's own cases damage a store, one per
/// check and each damaging nothing a second check reads, so the first
/// failure is the one failure; with the half that meets it: `true` where it is the full-text
/// check, which is the only check the read-only half leaves out.
const DAMAGE: &[(&str, bool)] = &[
    (
        "INSERT INTO documents_fts (rowid, body) VALUES (99999, 'a body no document holds')",
        true,
    ),
    (
        "DROP TRIGGER documents_fts_delete; DELETE FROM documents",
        true,
    ),
    (
        "PRAGMA foreign_keys = OFF;
         INSERT INTO document_trigrams (trigram, document) VALUES ('zzz', 999999999);
         PRAGMA foreign_keys = ON",
        false,
    ),
    ("UPDATE documents SET frontmatter = 'not json'", false),
    ("UPDATE links SET family = 'embed'", false),
    ("UPDATE document_tags SET source = 'inferred'", false),
    ("UPDATE tombstones SET provenance = 'rebuild-drop'", false),
    ("UPDATE findings SET severity = 'urgent'", false),
    ("UPDATE finding_rules SET position = 3", false),
    (
        "UPDATE finding_rules SET rule = 'closed' WHERE rule = 'tasks'",
        false,
    ),
    ("UPDATE rule_sets SET rules = 'other'", false),
    (
        "INSERT INTO tombstones (
             path, last_content_hash, provenance, generation, recorded_at,
             suffix_key, path_key
         ) VALUES (
             'docs/norn/glossary.md', 'hash-0', 'heal-prune', 1, 0,
             'glossary/norn/docs/', 'docs/norn/glossary.md'
         )",
        false,
    ),
    (
        "INSERT INTO content_quarantines (path, content_hash) VALUES ('elsewhere.md', 'hash-1')",
        false,
    ),
    (
        "UPDATE documents SET body_hash = 'not the hash of anything'",
        false,
    ),
    (
        "UPDATE documents SET frontmatter_projection_hash = 'not the hash of anything'",
        false,
    ),
];

/// **The halves report what the whole verification reports.** On a sound
/// store both are sound; on a store damaged in each way the verification's
/// own cases damage one, both report the same damage in the same words, and
/// the half that reports it is the one whose check it is.
#[test]
fn the_two_halves_report_exactly_what_the_whole_verification_reports() {
    let scratch = Scratch::new("halves-sound");
    let store = a_store_with_a_row_for_every_check(&scratch);
    store.verify_integrity().expect("a store just written to");
    verified_in_halves(&store).expect("the halves of a sound store");

    for (index, (arrange, full_text)) in DAMAGE.iter().enumerate() {
        let scratch = Scratch::new(&format!("halves-{index}"));
        let mut store = a_store_with_a_row_for_every_check(&scratch);
        induced_failure::execute_out_of_band(&mut store, arrange)
            .unwrap_or_else(|error| panic!("`{arrange}` was refused: {error:?}"));

        let whole = store.verify_integrity();
        assert!(
            matches!(whole, Err(StoreError::Damaged { .. })),
            "`{arrange}`: the whole verification reported {whole:?}"
        );
        assert_eq!(
            format!("{:?}", verified_in_halves(&store)),
            format!("{whole:?}"),
            "`{arrange}`: the halves reported otherwise than the whole"
        );
        let read_only = store.verification_source().verify(|| {});
        if *full_text {
            assert!(
                matches!(read_only, ReadOnlyVerification::Sound),
                "`{arrange}`: the read-only half reported {read_only:?}"
            );
        } else {
            assert!(
                matches!(&read_only, ReadOnlyVerification::Failed(error) if error.damage().is_some()),
                "`{arrange}`: the read-only half reported {read_only:?}"
            );
            store
                .verify_full_text()
                .unwrap_or_else(|error| panic!("`{arrange}`: the full-text check said {error:?}"));
        }
    }
}

/// **The full-text check keeps its classification.** It is a write, so on a
/// connection that cannot write it fails, and the failure is the refused
/// operation it was, never damage.
#[test]
fn a_full_text_check_that_cannot_write_is_refused_rather_than_called_damage() {
    let scratch = Scratch::new("full-text-read-only");
    let mut store = a_store_with_a_row_for_every_check(&scratch);
    induced_failure::execute_out_of_band(&mut store, "PRAGMA query_only = ON")
        .expect("making the connection read-only");
    let error = store
        .verify_full_text()
        .expect_err("a full-text check that cannot write");
    let StoreError::Sql { operation, .. } = &error else {
        panic!("it was reported as {error:?} rather than as a refused operation");
    };
    assert!(operation.contains("full-text index"), "{operation}");
    assert_eq!(error.damage(), None);
}

/// **A source outlives nothing it was minted from.** Minted from one store
/// lifetime, it reaches no verdict about the store a rebuild put at the same
/// path, and the refusal is not damage: a verdict of damage would discard the
/// rebuilt store for the state of the one before it.
#[test]
fn a_source_over_a_store_that_was_replaced_is_refused_and_not_damage() {
    let scratch = Scratch::new("halves-replaced");
    let store = a_store_with_a_row_for_every_check(&scratch);
    let source = store.verification_source();
    let _rebuilt = store
        .discard_and_reopen(StoredPathOrder::Sensitive)
        .expect("a rebuild of the store");
    let ReadOnlyVerification::Failed(error) = source.verify(|| {}) else {
        panic!("a source over a replaced store reached a verdict");
    };
    assert_eq!(error.damage(), None, "{error:?}");
}

/// **The read-only half holds one snapshot, and the writer is never held
/// behind it.** Held open between two of its checks, the verification lets a
/// write commit and reads none of it: the damage the write leaves stands
/// after the snapshot the verification reads.
#[test]
fn a_write_commits_while_the_read_only_half_is_in_flight() {
    let scratch = Scratch::new("halves-writer");
    let mut store = a_store_with_a_row_for_every_check(&scratch);
    // Held at its second point: the first check has run, so the snapshot
    // every later check reads is established.
    let source = store.verification_source();
    let (entered, entry) = mpsc::channel();
    let (resume, resumed) = mpsc::channel::<()>();
    let verifying = thread::spawn(move || {
        let mut calls = 0;
        source.verify(move || {
            calls += 1;
            if calls == 2 {
                entered.send(()).unwrap();
                resumed.recv_timeout(Duration::from_secs(30)).unwrap();
            }
        })
    });
    entry.recv_timeout(Duration::from_secs(15)).unwrap();
    write_document(
        &mut store.begin_request(),
        &document("arrived.md", "hash-2", "a body\n"),
    );
    induced_failure::execute_out_of_band(&mut store, "UPDATE findings SET severity = 'urgent'")
        .expect("a write while the verification holds its snapshot");
    resume.send(()).unwrap();
    let verdict = verifying.join().unwrap();
    assert!(
        matches!(verdict, ReadOnlyVerification::Sound),
        "the verification read past its snapshot: {verdict:?}"
    );
    assert!(matches!(
        store.verification_source().verify(|| {}),
        ReadOnlyVerification::Failed(StoreError::Damaged { .. })
    ));
}

/// **A stopped verification reaches no verdict**, wherever the stop finds
/// it: before it opens, between two of its checks, or between two of the
/// rows it recomputes. The store is damaged where only the last check looks,
/// so a verification that ran on past the stop would report damage.
#[test]
fn a_stopped_verification_reaches_no_verdict_wherever_it_stops() {
    let scratch = Scratch::new("halves-stopped");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    for at in 0..8 {
        write_document(
            &mut request,
            &document(&format!("{at}.md"), "hash-1", "a body\n"),
        );
    }
    induced_failure::execute_out_of_band(
        &mut store,
        "UPDATE documents SET body_hash = 'not the hash of anything' WHERE path = '7.md'",
    )
    .unwrap();
    let mut points = 0;
    store.verification_source().verify(|| points += 1);
    assert!(points > 8, "the verification stops at {points} points");

    let source = store.verification_source();
    source.stop_handle().stop();
    assert!(matches!(
        source.verify(|| {}),
        ReadOnlyVerification::Interrupted
    ));

    for stop_at in [1, points / 2, points] {
        let source = store.verification_source();
        let stop = source.stop_handle();
        let mut reached = 0;
        let mut after = 0;
        let verdict = source.verify(|| {
            reached += 1;
            if reached > stop_at {
                after += 1;
            }
            if reached == stop_at {
                stop.stop();
            }
        });
        assert!(
            matches!(verdict, ReadOnlyVerification::Interrupted),
            "stopped at {stop_at} of {points}: {verdict:?}"
        );
        assert_eq!(
            after, 0,
            "the verification went on past a stop at {stop_at}"
        );
    }
}

/// **A stop meets a statement in flight.** A verification over a store large
/// enough that its page check takes a while is stopped from another thread
/// while that check runs, and returns without a verdict long before the
/// check would have ended: the statement itself is stopped, rather than the
/// verification at the next point between two checks.
#[test]
fn a_stop_from_another_thread_interrupts_the_statement_in_flight() {
    let scratch = Scratch::new("halves-interrupted");
    let mut store = scratch.open();
    let body = "a body long enough to fill pages of the database\n".repeat(64);
    let mut request = store.begin_request();
    for at in 0..2_000 {
        write_document(
            &mut request,
            &document(&format!("{at}.md"), "hash-1", &body),
        );
    }
    // The first check is the page check: time it as the verification runs it.
    let mut marks = Vec::new();
    assert!(matches!(
        store
            .verification_source()
            .verify(|| marks.push(std::time::Instant::now())),
        ReadOnlyVerification::Sound
    ));
    let page_check = marks[1] - marks[0];
    assert!(
        page_check >= Duration::from_millis(10),
        "a page check of {page_check:?} is too short to be stopped inside"
    );

    let source = store.verification_source();
    let stop = source.stop_handle();
    let (entered, entry) = mpsc::channel();
    let verifying = thread::spawn(move || {
        let mut first = true;
        source.verify(move || {
            if std::mem::take(&mut first) {
                entered.send(std::time::Instant::now()).unwrap();
            }
        })
    });
    let began = entry.recv_timeout(Duration::from_secs(15)).unwrap();
    thread::sleep(page_check / 10);
    stop.stop();
    let verdict = verifying.join().unwrap();
    let took = began.elapsed();
    assert!(
        matches!(verdict, ReadOnlyVerification::Interrupted),
        "{verdict:?}"
    );
    assert!(
        took < page_check / 2,
        "a stop {:?} into a {page_check:?} page check returned after {took:?}",
        page_check / 10
    );
}
