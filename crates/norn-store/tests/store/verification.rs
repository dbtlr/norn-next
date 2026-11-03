//! The store's periodic verification through a [`VerificationSource`]: the
//! read-only checks over one snapshot of a read connection of their own, then
//! a copy of the store written through another read-only connection, and the
//! full-text check — the one check that writes — on the copy. Nothing in it
//! holds the store's writer.
//!
//! What it reports is what the whole verification a from-zero build closes
//! with reports: the same verdict on a sound store, and the same damage,
//! named the same way, on a store damaged in each way the verification looks
//! for. It can be stopped from another thread wherever it is, a stopped
//! verification reaches no verdict, and its copy is gone on every way out.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::common::{
    Scratch, document, document_with_every_fact, path, path_names_no_document_in_class,
    record_death, unread_block, violation, write_document,
};
use norn_store::{
    Provenance, Store, StoreError, StoredPathOrder, Verification, VerificationPoint,
    induced_failure,
};
use norn_wire::{FindingKind, Severity};

/// Where a source minted from the store at `scratch` writes its copy: beside
/// the store, in the directory that holds it.
fn copy_of(scratch: &Scratch) -> PathBuf {
    scratch.database().with_extension("verification-copy")
}

/// Whether anything of a copy at `copy` is left: the file or a journal's.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging and finding the verification's copy.
fn copy_left(copy: &Path) -> bool {
    ["", "-wal", "-shm", "-journal"].iter().any(|suffix| {
        let mut name = copy.as_os_str().to_os_string();
        name.push(suffix);
        PathBuf::from(name).exists()
    })
}

/// The source's verdict as the whole verification spells one. A stop is a
/// failure of the case: nothing here stops it.
fn verified_through_a_source(store: &Store) -> Result<(), StoreError> {
    match store.verification_source().verify(|_| {}) {
        Verification::Sound => Ok(()),
        Verification::Failed(error) => Err(error),
        Verification::Interrupted => panic!("a verification nothing stopped was stopped"),
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
/// failure is the one failure; `true` where the check that meets it is the
/// full-text check, which the source runs on its copy.
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

/// **The verification through a source reports what the whole verification
/// reports.** On a sound store both are sound; on a store damaged in each way
/// the verification's own cases damage one, both report the same damage in
/// the same words — the full-text check's on the copy included — and the
/// copy is gone afterwards either way.
#[test]
fn a_source_reports_exactly_what_the_whole_verification_reports() {
    let scratch = Scratch::new("through-source-sound");
    let store = a_store_with_a_row_for_every_check(&scratch);
    store.verify_integrity().expect("a store just written to");
    verified_through_a_source(&store).expect("a source over a sound store");
    assert!(
        !copy_left(&copy_of(&scratch)),
        "a sound verification left its copy"
    );

    for (index, (arrange, full_text)) in DAMAGE.iter().enumerate() {
        let scratch = Scratch::new(&format!("through-source-{index}"));
        let mut store = a_store_with_a_row_for_every_check(&scratch);
        induced_failure::execute_out_of_band(&mut store, arrange)
            .unwrap_or_else(|error| panic!("`{arrange}` was refused: {error:?}"));

        let whole = store.verify_integrity();
        assert!(
            matches!(whole, Err(StoreError::Damaged { .. })),
            "`{arrange}`: the whole verification reported {whole:?}"
        );
        let through = verified_through_a_source(&store);
        assert_eq!(
            format!("{through:?}"),
            format!("{whole:?}"),
            "`{arrange}`: the source reported otherwise than the whole"
        );
        if *full_text {
            assert!(
                format!("{through:?}").contains("full-text index"),
                "{through:?}"
            );
        }
        assert!(
            !copy_left(&copy_of(&scratch)),
            "`{arrange}`: the copy was left"
        );
    }
}

/// **A source outlives nothing it was minted from.** Minted from one store
/// lifetime, it reaches no verdict about the store a rebuild put at the same
/// path, and the refusal is not damage: a verdict of damage would discard the
/// rebuilt store for the state of the one before it.
#[test]
fn a_source_over_a_store_that_was_replaced_is_refused_and_not_damage() {
    let scratch = Scratch::new("source-replaced");
    let store = a_store_with_a_row_for_every_check(&scratch);
    let source = store.verification_source();
    let _rebuilt = store
        .discard_and_reopen(StoredPathOrder::Sensitive)
        .expect("a rebuild of the store");
    let Verification::Failed(error) = source.verify(|_| {}) else {
        panic!("a source over a replaced store reached a verdict");
    };
    assert_eq!(error.damage(), None, "{error:?}");
}

/// A store replaced at `scratch`'s path by a rebuild, holding one document,
/// then damaged behind the store's back by `damage`.
fn replaced_and_damaged(store: Store, damage: &str) -> Store {
    let mut rebuilt = store
        .discard_and_reopen(StoredPathOrder::Sensitive)
        .expect("a rebuild of the store");
    write_document(
        &mut rebuilt.begin_request(),
        &document("x.md", "hash-1", "a body\n"),
    );
    induced_failure::execute_out_of_band(&mut rebuilt, damage).expect("damaging the rebuilt store");
    rebuilt
}

/// **Each connection a verification opens is held to the source's
/// lifetime.** A store replaced before the verification opens, and damaged
/// where the read-only checks look, is refused at the read-only checks'
/// connection: their verdict would otherwise be damage about a store the
/// source was never minted from.
#[test]
fn a_store_replaced_before_the_read_only_checks_is_refused_there() {
    let scratch = Scratch::new("source-replaced-before-checks");
    let store = a_store_with_a_row_for_every_check(&scratch);
    let source = store.verification_source();
    let _rebuilt = replaced_and_damaged(store, "UPDATE documents SET body_hash = 'not a hash'");
    let Verification::Failed(error) = source.verify(|_| {}) else {
        panic!("a source over a replaced store reached a verdict");
    };
    assert_eq!(error.damage(), None, "{error:?}");
}

/// **The copy is held to the source's lifetime too.** A store replaced
/// between the read-only checks and the copy, and damaged where only the
/// copy's full-text check looks, is refused at the copy connection: a copy
/// of the replacement would be judged damaged for the source's lifetime.
#[test]
fn a_store_replaced_between_the_checks_and_the_copy_is_refused_there() {
    let scratch = Scratch::new("source-replaced-at-copy");
    let store = a_store_with_a_row_for_every_check(&scratch);
    let source = store.verification_source();
    let (entered, entry) = mpsc::channel();
    let (resume, resumed) = mpsc::channel::<()>();
    let verifying = thread::spawn(move || {
        source.verify(move |at| {
            if at == VerificationPoint::Copy {
                entered.send(()).unwrap();
                resumed.recv_timeout(Duration::from_secs(30)).unwrap();
            }
        })
    });
    entry
        .recv_timeout(Duration::from_secs(15))
        .expect("the verification to reach its copy");
    let _rebuilt = replaced_and_damaged(
        store,
        "INSERT INTO documents_fts (rowid, body) VALUES (99999, 'a body no document holds')",
    );
    resume.send(()).unwrap();
    let Verification::Failed(error) = verifying.join().unwrap() else {
        panic!("a source over a replaced store reached a verdict");
    };
    assert_eq!(error.damage(), None, "{error:?}");
}

/// **A write commits wherever the verification is.** Held with its snapshot
/// open, before its copy and with its copy written, the verification lets a
/// write commit each time, and reads none of it: the damage each write leaves
/// is where only the snapshot already taken looks — a row the read-only
/// checks read, or the index the copy's check reads.
#[test]
fn a_write_commits_wherever_the_verification_is() {
    const A_ROW: &str = "UPDATE findings SET severity = 'urgent'";
    const THE_INDEX: &str =
        "INSERT INTO documents_fts (rowid, body) VALUES (99999, 'a body no document holds')";
    for (held_at, nth, damage) in [
        (VerificationPoint::Check, 2, A_ROW),
        (VerificationPoint::Copy, 1, A_ROW),
        (VerificationPoint::CopyCheck, 1, THE_INDEX),
    ] {
        let scratch = Scratch::new("source-writer");
        let mut store = a_store_with_a_row_for_every_check(&scratch);
        let source = store.verification_source();
        let (entered, entry) = mpsc::channel();
        let (resume, resumed) = mpsc::channel::<()>();
        let verifying = thread::spawn(move || {
            let mut seen = 0;
            source.verify(move |at| {
                if at == held_at {
                    seen += 1;
                    if seen == nth {
                        entered.send(()).unwrap();
                        resumed.recv_timeout(Duration::from_secs(30)).unwrap();
                    }
                }
            })
        });
        entry.recv_timeout(Duration::from_secs(15)).unwrap();
        write_document(
            &mut store.begin_request(),
            &document("arrived.md", "hash-2", "a body\n"),
        );
        induced_failure::execute_out_of_band(&mut store, damage)
            .expect("a write while the verification is held");
        resume.send(()).unwrap();
        let verdict = verifying.join().unwrap();
        assert!(
            matches!(verdict, Verification::Sound),
            "held at {held_at:?}: the verification read past its snapshot: {verdict:?}"
        );
        assert!(matches!(
            store.verification_source().verify(|_| {}),
            Verification::Failed(StoreError::Damaged { .. })
        ));
    }
}

/// **A stopped verification reaches no verdict**, wherever the stop finds
/// it: before it opens, at any point of its read-only checks or the rows it
/// recomputes, before its copy, or before the copy's check. The store is
/// damaged where only the copy's check looks, so a verification that ran on
/// past the stop would report damage; and the copy is gone however it ended.
#[test]
fn a_stopped_verification_reaches_no_verdict_wherever_it_stops() {
    let scratch = Scratch::new("source-stopped");
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
        "INSERT INTO documents_fts (rowid, body) VALUES (99999, 'a body no document holds')",
    )
    .unwrap();
    let mut points = Vec::new();
    let verdict = store.verification_source().verify(|at| points.push(at));
    assert!(matches!(
        verdict,
        Verification::Failed(StoreError::Damaged { .. })
    ));
    for kind in [
        VerificationPoint::Check,
        VerificationPoint::Row,
        VerificationPoint::Copy,
        VerificationPoint::CopyCheck,
    ] {
        assert!(
            points.contains(&kind),
            "the verification met no {kind:?} point"
        );
    }

    let source = store.verification_source();
    source.stop_handle().stop();
    assert!(matches!(source.verify(|_| {}), Verification::Interrupted));

    for stop_at in 1..=points.len() {
        let source = store.verification_source();
        let stop = source.stop_handle();
        let mut reached = 0;
        let mut after = 0;
        let verdict = source.verify(|_| {
            reached += 1;
            if reached > stop_at {
                after += 1;
            }
            if reached == stop_at {
                stop.stop();
            }
        });
        assert!(
            matches!(verdict, Verification::Interrupted),
            "stopped at {:?}, point {stop_at} of {}: {verdict:?}",
            points[stop_at - 1],
            points.len()
        );
        assert_eq!(
            after, 0,
            "the verification went on past a stop at {stop_at}"
        );
        assert!(
            !copy_left(&copy_of(&scratch)),
            "a stop at {stop_at} left the copy"
        );
    }
}

/// A store large enough that each step of its verification takes a while,
/// and how long each of the three statements a stop is aimed at took in a
/// verification nothing stopped: the page check, the copy and the copy's
/// full-text check, each from the point before it to the point after it.
fn a_store_with_long_steps(
    scratch: &Scratch,
) -> (Store, [(VerificationPoint, usize, Duration); 3]) {
    let mut store = scratch.open();
    let body = "a body long enough to fill pages of the database\n".repeat(64);
    let mut request = store.begin_request();
    for at in 0..2_000 {
        write_document(
            &mut request,
            &document(&format!("{at}.md"), "hash-1", &body),
        );
    }
    let mut marks = Vec::new();
    assert!(matches!(
        store
            .verification_source()
            .verify(|at| marks.push((at, Instant::now()))),
        Verification::Sound
    ));
    let ended = Instant::now();
    let at = |kind: VerificationPoint| marks.iter().position(|(seen, _)| *seen == kind).unwrap();
    let page_check = marks[1].1 - marks[0].1;
    let copy = marks[at(VerificationPoint::CopyCheck)].1 - marks[at(VerificationPoint::Copy)].1;
    let copy_check = ended - marks[at(VerificationPoint::CopyCheck)].1;
    let steps = [
        (VerificationPoint::Check, 1, page_check),
        (VerificationPoint::Copy, 1, copy),
        (VerificationPoint::CopyCheck, 1, copy_check),
    ];
    for (kind, _, took) in steps {
        assert!(
            took >= Duration::from_millis(10),
            "the statement after {kind:?} took {took:?}, too short to be stopped inside"
        );
    }
    (store, steps)
}

/// **A stop meets the statement in flight**: the page check, the copy and the
/// copy's full-text check alike. Stopped from another thread a tenth of the
/// way into each, the verification returns without a verdict long before the
/// statement would have ended, and the copy is gone.
#[test]
fn a_stop_from_another_thread_interrupts_the_statement_in_flight() {
    let scratch = Scratch::new("source-interrupted");
    let (store, steps) = a_store_with_long_steps(&scratch);
    for (kind, nth, took) in steps {
        let source = store.verification_source();
        let stop = source.stop_handle();
        let (entered, entry) = mpsc::channel();
        let verifying = thread::spawn(move || {
            let mut seen = 0;
            source.verify(move |at| {
                if at == kind {
                    seen += 1;
                    if seen == nth {
                        entered.send(Instant::now()).unwrap();
                    }
                }
            })
        });
        let began = entry.recv_timeout(Duration::from_secs(15)).unwrap();
        thread::sleep(took / 10);
        stop.stop();
        let verdict = verifying.join().unwrap();
        let returned = began.elapsed();
        assert!(
            matches!(verdict, Verification::Interrupted),
            "{kind:?}: {verdict:?}"
        );
        assert!(
            returned < took / 2,
            "a stop {:?} into the {took:?} statement after {kind:?} returned after {returned:?}",
            took / 10
        );
        assert!(
            !copy_left(&copy_of(&scratch)),
            "{kind:?}: the stop left the copy"
        );
    }
}

/// **The copy goes with an unwind too.** A verification that unwinds with its
/// copy written leaves no copy behind it.
#[test]
fn an_unwind_out_of_a_verification_leaves_no_copy() {
    let scratch = Scratch::new("source-unwind");
    let store = a_store_with_a_row_for_every_check(&scratch);
    let source = store.verification_source();
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        source.verify(|at| {
            if at == VerificationPoint::CopyCheck {
                panic!("an unwind with the copy written");
            }
        })
    }));
    assert!(unwound.is_err());
    assert!(!copy_left(&copy_of(&scratch)), "the unwind left the copy");
}

/// **A copy a crash left is not this verification's copy.** Whatever stands
/// at the copy's place — a file that is no database, a journal beside it —
/// is cleared before the copy is written, so it is never what the full-text
/// check reads; and the clearing an attach runs removes it outright.
#[test]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging and finding the verification's copy.
fn a_copy_a_crash_left_is_cleared_before_the_next_copy() {
    let scratch = Scratch::new("source-leftover");
    let store = a_store_with_a_row_for_every_check(&scratch);
    let copy = copy_of(&scratch);
    let mut journal = copy.as_os_str().to_os_string();
    journal.push("-journal");
    std::fs::write(&copy, b"not a database").unwrap();
    std::fs::write(&journal, b"not a journal").unwrap();
    verified_through_a_source(&store).expect("a sound store with a leftover copy beside it");
    assert!(!copy_left(&copy));

    std::fs::write(&copy, b"not a database").unwrap();
    store
        .verification_source()
        .discard_copy()
        .expect("clearing a leftover copy");
    assert!(!copy_left(&copy));

    std::fs::write(&copy, b"not a database").unwrap();
    let path = store.path().to_path_buf();
    store.close().unwrap();
    Store::discard_at(&path).expect("discarding the derived state");
    assert!(
        !copy_left(&copy),
        "discarding the store left its verification's copy"
    );
}

/// **A copy the environment refuses is a refusal, never damage.** Where the
/// copy's place cannot be cleared — something that is not a file stands
/// there — the verification is refused, and the store it never finished
/// checking is not called damaged.
#[test]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging and finding the verification's copy.
fn a_copy_its_place_refuses_is_refused_rather_than_called_damage() {
    let scratch = Scratch::new("source-copy-refused");
    let store = a_store_with_a_row_for_every_check(&scratch);
    std::fs::create_dir_all(copy_of(&scratch).join("occupied")).unwrap();
    let Verification::Failed(error) = store.verification_source().verify(|_| {}) else {
        panic!("a verification whose copy was refused reached a verdict");
    };
    assert_eq!(error.damage(), None, "{error:?}");
}

/// **A copy whose place cannot be cleared never fails a discard or a
/// rebuild.** The copy is derived state the next copy or attach clears
/// again, and a verification over an occupied place is refused, so a discard
/// that stopped at it would trade a leftover file for a vault with no store:
/// the store goes, the rebuild reopens it, and the obstruction stays.
#[test]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging the verification's copy.
fn a_copy_whose_place_cannot_be_cleared_never_fails_a_discard_or_a_rebuild() {
    let scratch = Scratch::new("source-discard-obstructed");
    let store = a_store_with_a_row_for_every_check(&scratch);
    std::fs::create_dir_all(copy_of(&scratch).join("occupied")).unwrap();
    let rebuilt = store
        .discard_and_reopen(StoredPathOrder::Sensitive)
        .expect("a rebuild over an obstructed copy");
    drop(rebuilt);

    let store = scratch.open();
    store.discard().expect("a discard over an obstructed copy");
    assert!(
        !scratch.database().exists(),
        "the discard left the store itself"
    );
}

/// **A store whose directory's name is not UTF-8 is verified like any
/// other.** The data directory is read from the environment as raw bytes,
/// and the copy is written beside the store at exactly those bytes: a copy
/// spelled lossily would be refused every time, and damage would never be
/// found. Gated to the platforms whose filesystems take such a name.
#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn a_store_whose_directory_is_not_utf8_is_verified() {
    use std::os::unix::ffi::OsStrExt;

    let scratch = norn_testkit::scratch::Scratch::new("norn-store-source-not-utf8");
    let database = scratch
        .join(std::ffi::OsStr::from_bytes(b"derived-\xff"))
        .join("store.sqlite3");
    let mut store = Store::open(
        &database,
        StoredPathOrder::Sensitive,
        crate::common::DERIVATION,
    )
    .expect("opening a store");
    write_document(
        &mut store.begin_request(),
        &document("a.md", "hash-1", "a body\n"),
    );
    verified_through_a_source(&store).expect("a sound store");
    induced_failure::execute_out_of_band(
        &mut store,
        "INSERT INTO documents_fts (rowid, body) VALUES (99999, 'a body no document holds')",
    )
    .expect("damaging the full-text index");
    let error = verified_through_a_source(&store).expect_err("a damaged store");
    assert!(error.damage().is_some(), "{error:?}");
}

/// **Every discard takes the verification's copy with the store.** A copy a
/// crash left beside the store is derived state of the store discarded, so
/// a rebuild's discard removes it as the discard of a vault no longer
/// served does, and nothing is left to clear after it.
#[test]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging and finding the verification's copy.
fn a_discard_removes_the_verifications_copy() {
    let scratch = Scratch::new("source-discard");
    let copy = copy_of(&scratch);

    let store = a_store_with_a_row_for_every_check(&scratch);
    std::fs::write(&copy, b"not a database").unwrap();
    store.discard().expect("discarding the store");
    assert!(!copy_left(&copy), "a discard left the verification's copy");

    let store = scratch.open();
    std::fs::write(&copy, b"not a database").unwrap();
    let rebuilt = store
        .discard_and_reopen(StoredPathOrder::Sensitive)
        .expect("a rebuild of the store");
    assert!(!copy_left(&copy), "a rebuild left the verification's copy");
    drop(rebuilt);

    std::fs::write(scratch.database(), b"not a database").unwrap();
    std::fs::write(&copy, b"not a database").unwrap();
    let reopened = scratch.open();
    assert!(
        matches!(
            reopened.open_outcome(),
            norn_store::OpenOutcome::RebuiltFromZero(_)
        ),
        "{:?}",
        reopened.open_outcome()
    );
    assert!(
        !copy_left(&copy),
        "an open's rebuild left the verification's copy"
    );
}
