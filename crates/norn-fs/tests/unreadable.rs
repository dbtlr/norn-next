//! Document failures through the public, process-armed filesystem seam.

#![allow(clippy::disallowed_methods)] // Fixtures arrange and inspect their own filesystem tree.

use std::path::Path;
use std::time::Duration;

use norn_fs::{Opened, Refusal, Vault, WalkError, WalkFact};
use norn_testkit::process::{Run, Sandbox};

const ROLE: &str = "NORN_FS_UNREADABLE_READER";

#[test]
fn path_targeted_document_denial_repeats_until_the_guard_drops() {
    let scratch = norn_testkit::scratch::Scratch::new("fs-targeted-denial");
    let note = scratch.join("note.md");
    std::fs::write(&note, b"unchanged bytes").unwrap();
    let denied = norn_fs::deny_document_opens(&note);
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                for _ in 0..3 {
                    assert_opened(
                        norn_fs::read_optional_and_hash(scratch.root(), Path::new("note.md")),
                        Some(libc::EACCES),
                    );
                    assert_fact(
                        Vault::open(scratch.root(), &[])
                            .unwrap()
                            .fact(Path::new("note.md")),
                        Some(libc::EACCES),
                    );
                }
            })
            .join()
            .unwrap();
    });
    let exhausted = norn_fs::exhaust_document_opens(&note);
    assert_opened(
        norn_fs::read_optional_and_hash(scratch.root(), Path::new("note.md")),
        Some(libc::EMFILE),
    );
    drop(exhausted);
    assert_opened(
        norn_fs::read_optional_and_hash(scratch.root(), Path::new("note.md")),
        Some(libc::EACCES),
    );
    drop(denied);
    let Opened::Read(read) =
        norn_fs::read_optional_and_hash(scratch.root(), Path::new("note.md")).unwrap()
    else {
        panic!("the path reads once the guard drops");
    };
    assert_eq!(read.bytes(), b"unchanged bytes");
}

#[test]
fn a_failing_final_name_stat_is_an_unreadable_answer_to_a_reach_that_asks_for_it() {
    let scratch = norn_testkit::scratch::Scratch::new("fs-failing-stat");
    let note = scratch.join("note.md");
    std::fs::write(&note, b"body").unwrap();
    let vault = Vault::open(scratch.root(), &[]).unwrap();
    let failing = norn_fs::fail_document_stats(&note);
    let unreadable = vault
        .reach_or_unreadable(Path::new("note.md"))
        .unwrap()
        .expect_err("the stat of the final name fails");
    assert_eq!(unreadable.path().as_path(), Path::new("note.md"));
    assert_eq!(unreadable.cause().raw_os_error(), Some(libc::EIO));
    assert!(matches!(
        vault.reach(Path::new("note.md")),
        Err(WalkError::Environment { .. })
    ));
    drop(failing);
    assert!(matches!(
        vault.reach_or_unreadable(Path::new("note.md")).unwrap(),
        Ok(norn_fs::Reach::Stands { .. })
    ));
}

#[test]
fn path_targeted_descriptor_exhaustion_crosses_threads_and_ends_with_its_guard() {
    let scratch = norn_testkit::scratch::Scratch::new("fs-targeted-open");
    let note = scratch.join("zz.md");
    std::fs::write(&note, b"body").unwrap();
    std::fs::write(scratch.join("other.md"), b"other").unwrap();
    assert_opened(
        norn_fs::read_optional_and_hash(scratch.root(), Path::new("zz.md")),
        None,
    );

    let first = norn_fs::exhaust_document_opens(&note);
    let second = norn_fs::exhaust_document_opens(&note);
    drop(first);
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                assert_opened(
                    norn_fs::read_optional_and_hash(scratch.root(), Path::new("zz.md")),
                    Some(libc::EMFILE),
                );
                assert_opened(
                    norn_fs::stream_optional_and_hash(scratch.root(), Path::new("zz.md")),
                    Some(libc::EMFILE),
                );
                assert_fact(
                    Vault::open(scratch.root(), &[])
                        .unwrap()
                        .fact(Path::new("zz.md")),
                    Some(libc::EMFILE),
                );
                assert_opened(
                    norn_fs::read_optional_and_hash(scratch.root(), Path::new("other.md")),
                    None,
                );
            })
            .join()
            .unwrap();
    });
    drop(second);
    assert_opened(
        norn_fs::read_optional_and_hash(scratch.root(), Path::new("zz.md")),
        None,
    );
}

#[test]
fn public_document_readers_classify_process_armed_failures() {
    for reader in ["whole", "stream", "file", "fact", "page"] {
        for answer in ["denied", "failing", "exhausted", "unarmed"] {
            let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), "unreadable")
                .expect("a sandbox");
            let root = sandbox.work_dir();
            std::fs::write(root.join("note.md"), b"body").unwrap();
            std::fs::write(root.join("z-control.toml"), b"control").unwrap();
            let hits = root.join("hits");
            let stage = if reader == "page" { "page" } else { "open" };
            let mut run = Run::new(&sandbox, std::env::current_exe().unwrap())
                .args(["--exact", "the_child_reads_one_document", "--nocapture"])
                .deadline(Duration::from_secs(30))
                .env(ROLE, reader)
                .env("NORN_FS_UNREADABLE_ROOT", &root)
                .env("NORN_FS_UNREADABLE_ANSWER", answer)
                .env("NORN_FS_ARM_HITS", &hits);
            if answer != "unarmed" {
                run = run.env("NORN_FS_WALK_ARMED_STAGES", format!("{stage}={answer}"));
            }
            let outcome = run.wait().unwrap();
            outcome.assert_success();
            if answer == "unarmed" {
                assert!(!hits.exists(), "an unarmed read recorded a failure");
            } else {
                assert_eq!(
                    std::fs::read_to_string(hits).unwrap(),
                    format!("seam=norn-fs/walk stage={stage} answer={answer}\n"),
                    "{reader}/{answer}"
                );
            }
        }
    }
}

#[test]
fn the_child_reads_one_document() {
    let Ok(reader) = std::env::var(ROLE) else {
        return;
    };
    let root = std::env::var_os("NORN_FS_UNREADABLE_ROOT").unwrap();
    let root = Path::new(&root);
    let errno = match std::env::var("NORN_FS_UNREADABLE_ANSWER").unwrap().as_str() {
        "denied" => Some(libc::EACCES),
        "failing" => Some(libc::EIO),
        "exhausted" => Some(libc::EMFILE),
        "unarmed" => None,
        _ => unreachable!(),
    };
    // A schema read must neither take a document's arm nor become an unknown answer.
    assert_eq!(
        norn_fs::read_and_hash(root, Path::new("z-control.toml"))
            .unwrap()
            .bytes(),
        b"control"
    );
    match reader.as_str() {
        "whole" => assert_opened(
            norn_fs::read_optional_and_hash(root, Path::new("note.md")),
            errno,
        ),
        "stream" => assert_opened(
            norn_fs::stream_optional_and_hash(root, Path::new("note.md")),
            errno,
        ),
        "file" => {
            let WalkFact::File(file) = norn_fs::walk(root, &[]).unwrap().next().unwrap().unwrap()
            else {
                panic!("a file fact")
            };
            let answer = file.read_optional();
            match answer {
                Err(WalkError::Environment { source, .. }) => {
                    assert_eq!(errno, Some(libc::EMFILE));
                    assert_eq!(source.raw_os_error(), errno);
                }
                Ok(answer) => assert_opened(Ok(answer), errno),
                Err(error) => panic!("{error}"),
            }
        }
        "fact" => assert_fact(
            Vault::open(root, &[]).unwrap().fact(Path::new("note.md")),
            errno,
        ),
        "page" => {
            let mut walk = norn_fs::walk(root, &[]).unwrap();
            assert_fact(walk.next().unwrap(), errno);
            if errno == Some(libc::EMFILE) {
                assert!(walk.next().is_none(), "resource failure is terminal");
            } else {
                assert_eq!(
                    walk.next().unwrap().unwrap().path().as_path(),
                    Path::new("z-control.toml")
                );
                assert!(walk.next().is_none());
            }
        }
        _ => unreachable!(),
    }
}

fn assert_opened<T>(answer: Result<Opened<T>, Refusal>, errno: Option<i32>) {
    match (errno, answer) {
        (None, Ok(Opened::Read(_))) => {}
        (Some(libc::EMFILE), Err(Refusal::Environment { raw_os_error, .. })) => {
            assert_eq!(raw_os_error, errno);
        }
        (Some(_), Ok(Opened::Unreadable { operation, cause })) => {
            assert_ne!(errno, Some(libc::EMFILE));
            assert_eq!(operation, "opening");
            assert_eq!(cause.raw_os_error(), errno);
        }
        _ => panic!("unexpected document-read result for errno {errno:?}"),
    }
}

fn assert_fact(answer: Result<WalkFact, WalkError>, errno: Option<i32>) {
    match (errno, answer) {
        (None, Ok(WalkFact::File(_))) => {}
        (Some(libc::EMFILE), Err(WalkError::Environment { source, .. })) => {
            assert_eq!(source.raw_os_error(), errno);
        }
        (Some(_), Ok(WalkFact::Unreadable(fact))) => {
            assert_ne!(errno, Some(libc::EMFILE));
            assert_eq!(fact.path().as_path(), Path::new("note.md"));
            assert_eq!(fact.cause().raw_os_error(), errno);
        }
        (_, answer) => panic!("unexpected fact for errno {errno:?}: {answer:?}"),
    }
}
