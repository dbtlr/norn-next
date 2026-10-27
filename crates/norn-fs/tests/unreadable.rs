//! Path failures through the public, process-armed filesystem seam.

#![allow(clippy::disallowed_methods)] // Fixtures arrange and inspect their own filesystem tree.

use std::path::Path;
use std::time::Duration;

use norn_fs::{Opened, Refusal, UnreadKind, Vault, WalkError, WalkFact};
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

#[test]
fn the_inventory_records_a_directory_it_cannot_list_and_continues() {
    run_directory_case("inventory", "denied", "folder");
    run_directory_case("inventory", "failing", "folder");
}

fn run_directory_case(reader: &str, answer: &str, at: &str) {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), "unlistable").unwrap();
    let root = sandbox.work_dir();
    std::fs::create_dir(root.join("folder")).unwrap();
    std::fs::write(root.join("folder/note.md"), b"body").unwrap();
    std::fs::write(root.join("z.md"), b"control").unwrap();
    if matches!(reader, "link" | "readlink" | "link-stat") {
        std::os::unix::fs::symlink("folder/note.md", root.join("a-link")).unwrap();
    }
    let stage = if reader == "page-directory" {
        "page"
    } else {
        "list"
    };
    let hits = root.join("hits");
    let outcome = Run::new(&sandbox, std::env::current_exe().unwrap())
        .args(["--exact", "the_child_reads_one_directory", "--nocapture"])
        .deadline(Duration::from_secs(30))
        .env("NORN_FS_DIRECTORY_READER", reader)
        .env("NORN_FS_UNREADABLE_ROOT", &root)
        .env("NORN_FS_UNREADABLE_ANSWER", answer)
        .env("NORN_FS_WALK_ARMED_STAGES", format!("{stage}={answer}"))
        .env("NORN_FS_WALK_REACH", "directory")
        .env("NORN_FS_WALK_LIST_AT", at)
        .env("NORN_FS_ARM_HITS", &hits)
        .wait()
        .unwrap();
    outcome.assert_success();
    assert_eq!(
        std::fs::read_to_string(hits).unwrap(),
        format!("seam=norn-fs/walk stage={stage} answer={answer}\n")
    );
}

#[test]
fn the_child_reads_one_directory() {
    let Ok(reader) = std::env::var("NORN_FS_DIRECTORY_READER") else {
        return;
    };
    let root = std::env::var_os("NORN_FS_UNREADABLE_ROOT").unwrap();
    let root = Path::new(&root);
    let errno = match std::env::var("NORN_FS_UNREADABLE_ANSWER").unwrap().as_str() {
        "denied" => libc::EACCES,
        "failing" => libc::EIO,
        "exhausted" => libc::EMFILE,
        _ => unreachable!(),
    };
    match reader.as_str() {
        "inventory" => {
            let mut inventory = norn_fs::walk::inventory::InventoryWalk::new(root, &[]).unwrap();
            let mut names = Vec::new();
            loop {
                let page = inventory.advance(1).unwrap();
                names.extend(page.records.into_iter().map(|record| record.value));
                if page.complete {
                    break;
                }
            }
            names.sort();
            assert_eq!(names, [b"folder".to_vec(), b"z.md".to_vec()]);
        }
        "page-directory" => {
            let mut walk = norn_fs::walk(root, &[]).unwrap();
            let WalkFact::Unreadable(fact) = walk.next().unwrap().unwrap() else {
                panic!("a stat-denied directory name");
            };
            assert_eq!(fact.path().as_path(), Path::new("folder"));
            assert_eq!(fact.kind(), UnreadKind::Unknown);
            assert_eq!(fact.cause().raw_os_error(), Some(errno));
            assert_eq!(
                walk.next().unwrap().unwrap().path().as_path(),
                Path::new("z.md")
            );
            assert!(walk.next().is_none());
        }
        "walk" | "subtree" => {
            let answer = if reader == "walk" {
                norn_fs::walk(root, &[])
            } else {
                norn_fs::walk_subtree(root, Path::new("folder"), &[])
            };
            let mut walk = match answer {
                Ok(walk) => walk,
                Err(WalkError::Environment { source, .. }) => {
                    assert_eq!(errno, libc::EMFILE);
                    assert_eq!(source.raw_os_error(), Some(errno));
                    return;
                }
                Err(error) => panic!("{error}"),
            };
            assert_directory_fact(walk.next().unwrap(), errno);
            if errno == libc::EMFILE {
                assert!(walk.next().is_none());
            } else if reader == "walk" {
                assert_eq!(
                    walk.next().unwrap().unwrap().path().as_path(),
                    Path::new("z.md")
                );
                assert!(walk.next().is_none());
            } else {
                assert!(walk.next().is_none());
            }
        }
        "reach" => {
            let answer = Vault::open(root, &[])
                .unwrap()
                .reach_or_unreadable(Path::new("folder/note.md"));
            if errno == libc::EMFILE {
                assert!(
                    matches!(answer, Err(WalkError::Environment { source, .. }) if source.raw_os_error() == Some(errno))
                );
            } else {
                let fact = answer.unwrap().expect_err("the ancestor answers");
                assert_directory_fact(Ok(WalkFact::Unreadable(fact)), errno);
            }
        }
        "fact" | "fact-directory" | "inventory-replay" => {
            let answer = if reader == "inventory-replay" {
                norn_fs::walk::inventory::InventoryWalk::new(root, &[])
                    .unwrap()
                    .replay(b"folder")
            } else {
                let path = if reader == "fact" {
                    "folder/note.md"
                } else {
                    "folder"
                };
                Vault::open(root, &[]).unwrap().fact(Path::new(path))
            };
            assert_directory_fact(answer, errno);
        }
        "whole" => assert_folder_opened(
            norn_fs::read_optional_and_hash(root, Path::new("folder/note.md")),
            errno,
        ),
        "stream" => assert_folder_opened(
            norn_fs::stream_optional_and_hash(root, Path::new("folder/note.md")),
            errno,
        ),
        "root" => {
            let mut walk = norn_fs::walk(root, &[]).unwrap();
            assert!(
                matches!(walk.next().unwrap(), Err(WalkError::Environment { source, .. }) if source.raw_os_error() == Some(errno))
            );
            assert!(walk.next().is_none());
        }
        "inventory-root" => {
            let answer = norn_fs::walk::inventory::InventoryWalk::new(root, &[]);
            let error = match answer {
                Err(error) => error,
                Ok(mut inventory) => inventory.advance(1).err().expect("root stream refuses"),
            };
            assert!(
                matches!(error, WalkError::Environment { source, .. } if source.raw_os_error() == Some(errno))
            );
        }
        "inventory-resource" => {
            let mut inventory = norn_fs::walk::inventory::InventoryWalk::new(root, &[]).unwrap();
            let error = loop {
                match inventory.advance(1) {
                    Err(error) => break error,
                    Ok(page) => assert!(!page.complete, "exhaustion was ignored"),
                }
            };
            assert!(
                matches!(error, WalkError::Environment { source, .. } if source.raw_os_error() == Some(errno))
            );
        }
        "link" | "readlink" | "link-stat" => {
            let mut walk = norn_fs::walk(root, &[]).unwrap();
            let answer = walk.next().unwrap();
            if errno == libc::EMFILE {
                assert!(
                    matches!(answer, Err(WalkError::Environment { source, .. }) if source.raw_os_error() == Some(errno))
                );
                assert!(walk.next().is_none());
            } else {
                assert!(matches!(answer, Ok(WalkFact::Skipped(skip))
                    if skip.path().as_path() == Path::new("a-link")
                        && skip.reason() == norn_fs::SkipReason::SymbolicLink(norn_fs::LinkKind::Unreachable)));
                assert!(walk.all(|fact| fact.is_ok()));
            }
        }
        _ => panic!("unknown directory reader {reader}"),
    }
}

fn assert_directory_fact(answer: Result<WalkFact, WalkError>, errno: i32) {
    if errno == libc::EMFILE {
        assert!(
            matches!(answer, Err(WalkError::Environment { source, .. }) if source.raw_os_error() == Some(errno))
        );
    } else {
        let WalkFact::Unreadable(fact) = answer.unwrap() else {
            panic!("an unreadable directory fact");
        };
        assert_eq!(fact.path().as_path(), Path::new("folder"));
        assert_eq!(fact.kind(), UnreadKind::Directory);
        assert_eq!(fact.cause().raw_os_error(), Some(errno));
    }
}

fn assert_folder_opened<T>(answer: Result<Opened<T>, Refusal>, errno: i32) {
    if errno == libc::EMFILE {
        assert!(
            matches!(answer, Err(Refusal::Environment { raw_os_error, .. }) if raw_os_error == Some(errno))
        );
    } else {
        let Ok(Opened::UnreadableFolder {
            folder,
            operation,
            cause,
        }) = answer
        else {
            panic!("an unreadable folder");
        };
        assert_eq!(folder, Path::new("folder"));
        assert_eq!(operation, "opening");
        assert_eq!(cause.raw_os_error(), Some(errno));
    }
}

#[test]
fn public_directory_readers_answer_at_the_failed_folder() {
    for reader in [
        "walk",
        "subtree",
        "reach",
        "fact",
        "fact-directory",
        "inventory-replay",
        "whole",
        "stream",
    ] {
        for answer in ["denied", "failing", "exhausted"] {
            run_directory_case(reader, answer, "folder");
        }
    }
}

#[test]
fn an_exhausted_directory_open_still_refuses() {
    run_directory_case("inventory-resource", "exhausted", "folder");
}

#[test]
fn root_listing_failures_still_refuse() {
    for reader in ["root", "inventory-root"] {
        for answer in ["denied", "failing", "exhausted"] {
            run_directory_case(reader, answer, "");
        }
    }
}

#[test]
fn a_link_into_a_denied_directory_is_refused_as_unreachable() {
    for (reader, at) in [
        ("link", "folder"),
        ("readlink", "a-link"),
        ("link-stat", "folder/note.md"),
    ] {
        for answer in ["denied", "failing", "exhausted"] {
            run_directory_case(reader, answer, at);
        }
    }
}

#[test]
fn a_page_denied_directory_name_has_unknown_kind_and_the_walk_continues() {
    run_directory_case("page-directory", "denied", "folder");
}
