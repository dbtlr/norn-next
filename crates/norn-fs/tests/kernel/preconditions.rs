//! What staging asks of a target before anything is published, and what it
//! does when the answer is no.
//!
//! Every refusal here ends with the target byte-identical to what it held when
//! the call began and with no shadow left behind: a staging refusal is found
//! before publication, so it writes nothing.

use norn_fs::{Refusal, Staging, Transition};

use crate::common::{Scratch, bytes_at, exists, hash, identity_at, staged};

/// A before-state is checked against the bytes that are there **now**, not
/// against whatever the caller last saw.
///
/// The construction is the discrimination: the caller passes a hash the file
/// really did have, and the file has since moved on. A check against a
/// caller-supplied or index-cached snapshot would agree with the caller and
/// publish over somebody else's document.
#[test]
fn a_before_state_is_checked_against_the_bytes_that_are_there_now() {
    let scratch = Scratch::new("precondition-now");
    let path = scratch.place("note.md", b"what the caller read");
    let stale = hash(b"what the caller read");
    scratch.place("note.md", b"what somebody else wrote");

    for transition in [
        Transition::Replace {
            before: stale,
            content: b"what the caller composed",
        },
        Transition::Remove { before: stale },
    ] {
        let refusal = scratch
            .stage("note.md", transition)
            .expect_err("a stale before-state");

        let Refusal::Drifted {
            expected, observed, ..
        } = &refusal
        else {
            panic!("a stale before-state was not reported as drift: {refusal}");
        };
        assert_eq!(*expected, stale);
        let observed = observed.as_ref().expect("the observed state");
        assert_eq!(observed.content_hash, hash(b"what somebody else wrote"));
        assert_eq!(observed.len, b"what somebody else wrote".len() as u64);
    }

    assert_eq!(bytes_at(&path), b"what somebody else wrote");
    assert!(
        scratch.shadow_names().is_empty(),
        "a staging refusal left a shadow: {:?}",
        scratch.shadow_names()
    );
}

/// A hash before-state on a path with nothing at it is drift with nothing to
/// describe when the transition means to write there, not a create.
///
/// The forbidden shape is treating absence as permission. A caller composed its
/// content from a document somebody has since removed, and a replacement that
/// landed anyway would resurrect a deleted document under a plan nobody re-made.
#[test]
fn a_replacement_onto_nothing_is_drift() {
    let scratch = Scratch::new("drift-onto-nothing");
    for relative in ["gone.md", "absent-folder/gone.md"] {
        let refusal = scratch
            .stage(
                relative,
                Transition::Replace {
                    before: hash(b"what was there"),
                    content: b"ours",
                },
            )
            .expect_err("a replacement onto nothing");

        assert!(
            matches!(&refusal, Refusal::Drifted { observed: None, .. }),
            "{relative}: {refusal}"
        );
        assert!(!exists(&scratch.at(relative)));
    }
    assert!(scratch.shadow_names().is_empty());
}

/// **The bar on a removal re-sent after it landed.** A removal whose target is
/// already gone stages as landed, because absence is the removal's after-state.
///
/// The forbidden shape is refusing it as drift: a resolved plan re-sent after a
/// crash would then refuse on its own progress, and never finish.
#[test]
fn a_removal_whose_target_is_gone_stages_as_landed() {
    let scratch = Scratch::new("remove-landed");
    for relative in ["gone.md", "absent-folder/gone.md"] {
        let staging = scratch
            .stage(
                relative,
                Transition::Remove {
                    before: hash(b"what was there"),
                },
            )
            .expect("a removal already landed");
        assert!(
            matches!(&staging, Staging::Landed(landed) if landed.path() == std::path::Path::new(relative)),
            "{relative}: {staging:?}"
        );
    }
}

/// **The bar on a re-sent write.** A create or a replacement whose target
/// already holds its after-state stages as landed and stages no shadow.
///
/// The forbidden shape is staging it again. For a create that is a refusal —
/// the name is taken — on a write that already happened; for a replacement it
/// is drift on the same grounds; and publishing identical bytes anyway would
/// cost a new inode and a filesystem event for a document that did not change.
#[test]
fn a_target_at_its_after_state_stages_as_landed_with_no_shadow() {
    let scratch = Scratch::new("already-landed");
    let path = scratch.place("note.md", b"the after-state");
    let before = identity_at(&path);

    for transition in [
        Transition::Create {
            content: b"the after-state",
        },
        Transition::Replace {
            before: hash(b"the before-state"),
            content: b"the after-state",
        },
        // A replacement whose content is what it read is landed too.
        Transition::Replace {
            before: hash(b"the after-state"),
            content: b"the after-state",
        },
    ] {
        let staging = scratch
            .stage("note.md", transition)
            .expect("a target at its after-state");
        assert!(matches!(staging, Staging::Landed(_)), "{staging:?}");
    }

    assert_eq!(identity_at(&path), before, "a landed target was replaced");
    assert!(
        scratch.shadow_names().is_empty(),
        "a landed target staged a shadow: {:?}",
        scratch.shadow_names()
    );
}

/// A create whose name holds something other than its content refuses as a
/// taken name, whatever is there — a document, an empty file, or a directory.
#[test]
fn a_create_refuses_every_taken_name_the_same_way() {
    let scratch = Scratch::new("create-taken");
    let document = scratch.place("note.md", b"bytes");
    let empty = scratch.place("empty.md", b"");
    let directory = scratch.directory("folder.md");

    for (relative, taken) in [
        ("note.md", &document),
        ("empty.md", &empty),
        ("folder.md", &directory),
    ] {
        let refusal = scratch
            .stage(relative, Transition::Create { content: b"ours" })
            .expect_err("a create onto a taken name");
        assert_eq!(
            refusal,
            Refusal::DestinationExists {
                path: taken.to_path_buf()
            },
            "{relative} refused as something other than a taken name"
        );
    }
    assert_eq!(bytes_at(&document), b"bytes");
    assert_eq!(bytes_at(&empty), b"");
    assert!(scratch.shadow_names().is_empty());
}

/// A create onto nothing stages a shadow holding exactly its content, and
/// the target stays absent until publication.
#[test]
fn a_create_stages_its_content_and_publishes_nothing_yet() {
    let scratch = Scratch::new("create-staged");
    let staged = staged(
        scratch
            .stage(
                "fresh.md",
                Transition::Create {
                    content: b"fresh bytes",
                },
            )
            .expect("a create onto nothing"),
    );

    assert_eq!(staged.path(), std::path::Path::new("fresh.md"));
    assert!(!exists(&scratch.at("fresh.md")), "staging published");
    assert_eq!(bytes_at(&scratch.only_shadow()), b"fresh bytes");
}

/// A target that is a symbolic link is refused on every transition, whether
/// the link resolves or dangles.
///
/// The two are opposite mistakes and neither is a document write: reading
/// through a resolving link would take bytes from one place and publish at
/// another, and a rename onto a dangling one would replace the link itself. So
/// the refusal keys on being a link and asks nothing about the target.
#[test]
fn a_symlinked_target_is_refused_whether_or_not_it_resolves() {
    let scratch = Scratch::new("symlink");
    let real = scratch.place("real.md", b"the target's bytes");
    crate::common::symlink(&real, &scratch.at("resolving.md"));
    crate::common::symlink(&scratch.at("nothing.md"), &scratch.at("dangling.md"));

    for link in ["resolving.md", "dangling.md"] {
        for transition in [
            Transition::Create { content: b"ours" },
            Transition::Replace {
                before: hash(b"the target's bytes"),
                content: b"ours",
            },
            Transition::Remove {
                before: hash(b"the target's bytes"),
            },
        ] {
            let refusal = scratch
                .stage(link, transition)
                .expect_err("a symlinked target");
            assert_eq!(
                refusal,
                Refusal::SymlinkDestination {
                    path: scratch.at(link)
                },
                "{link} was refused as something other than a link"
            );
        }
    }
    assert_eq!(bytes_at(&real), b"the target's bytes");
    assert!(!exists(&scratch.at("nothing.md")));
    assert!(scratch.shadow_names().is_empty());
}

/// An unreadable target is an environmental refusal, never drift and never
/// absence.
///
/// The distinction is the whole point: absence would let a create proceed and
/// drift would send a caller re-planning against a document it cannot see. A
/// permission that was revoked is the machine's problem and says so.
#[test]
fn an_unreadable_target_is_an_environmental_refusal() {
    let scratch = Scratch::new("unreadable");
    let path = scratch.place("note.md", b"bytes nobody can read");
    crate::common::set_mode(&path, 0o000);
    crate::common::demand_unreadable(&path);

    let refusal = scratch
        .stage(
            "note.md",
            Transition::Replace {
                before: hash(b"bytes nobody can read"),
                content: b"ours",
            },
        )
        .expect_err("an unreadable target");

    assert!(
        matches!(
            &refusal,
            Refusal::Environment {
                kind: std::io::ErrorKind::PermissionDenied,
                ..
            }
        ),
        "{refusal}"
    );
    crate::common::set_mode(&path, 0o644);
    assert_eq!(bytes_at(&path), b"bytes nobody can read");
    assert!(scratch.shadow_names().is_empty());
}

/// **The bar on a name that is not a regular file.** A pipe at a document's
/// name is refused, and refused without waiting: opening a FIFO for reading
/// holds the caller until somebody writes to the pipe.
#[test]
fn a_pipe_at_a_target_refuses_without_waiting() {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let scratch = Scratch::new("fifo");
        let made = std::process::Command::new("mkfifo")
            .arg(scratch.at("note.md"))
            .status()
            .expect("run mkfifo");
        assert!(made.success(), "mkfifo failed");
        let refusal = scratch
            .stage(
                "note.md",
                Transition::Replace {
                    before: hash(b"old"),
                    content: b"new",
                },
            )
            .expect_err("a pipe is not a document to replace");
        let _ = sender.send(refusal);
    });

    let refusal = receiver
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("staging never returned: a pipe at the target held it inside open");
    assert!(
        matches!(&refusal, Refusal::NotRegularFile { .. }),
        "{refusal}"
    );
}
