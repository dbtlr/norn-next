//! What a write asks before it does anything, and what it does when the answer
//! is no.
//!
//! Every case here ends with the destination byte-identical to what it held
//! when the call began, because that is the one thing every refusal promises.

use norn_fs::{Landed, Precondition, Refusal, write};

use crate::common::{Scratch, bytes_at, exists, hash, identity_at};

/// A precondition is checked against the bytes that are there **now**, not
/// against whatever the caller last saw.
///
/// The construction is the discrimination: the caller passes a hash the file
/// really did have, and the file has since moved on. A check against a
/// caller-supplied or index-cached snapshot would agree with the caller and
/// publish over somebody else's document.
#[test]
fn a_precondition_is_checked_against_the_bytes_that_are_there_now() {
    let scratch = Scratch::new("precondition-now");
    let path = scratch.place("note.md", b"what the caller read");
    let stale = hash(b"what the caller read");
    scratch.place("note.md", b"what somebody else wrote");

    let refusal = write(
        &path,
        b"what the caller composed",
        Precondition::Replace(stale),
        scratch.shadows(),
    )
    .expect_err("a stale precondition");

    let Refusal::Drifted {
        expected, observed, ..
    } = &refusal
    else {
        panic!("a stale precondition was not reported as drift: {refusal}");
    };
    assert_eq!(*expected, stale);
    let observed = observed.as_ref().expect("the observed state");
    assert_eq!(observed.content_hash, hash(b"what somebody else wrote"));
    assert_eq!(observed.len, b"what somebody else wrote".len() as u64);

    assert_eq!(
        bytes_at(&path),
        b"what somebody else wrote",
        "a refused write touched the destination"
    );
    assert!(
        scratch.shadow_names().is_empty(),
        "a refusal before staging staged something: {:?}",
        scratch.shadow_names()
    );
}

/// The refusal carries both hashes: the one the caller composed against and the
/// one that is there. One of them alone leaves the caller unable to say which
/// side moved.
#[test]
fn drift_names_the_expected_and_the_observed_hash() {
    let scratch = Scratch::new("drift-both-hashes");
    let path = scratch.place("note.md", b"theirs");
    let refusal = write(
        &path,
        b"ours",
        Precondition::Replace(hash(b"mine")),
        scratch.shadows(),
    )
    .expect_err("a precondition nothing satisfies");

    let rendered = refusal.to_string();
    assert!(rendered.contains(&hash(b"mine").to_hex()), "{rendered}");
    assert!(rendered.contains(&hash(b"theirs").to_hex()), "{rendered}");
}

/// A hash precondition on a path with nothing at it is drift with nothing to
/// describe, not a create.
///
/// The forbidden shape is treating absence as permission. A caller composed its
/// content from a document somebody has since removed, and a write that landed
/// anyway would resurrect a deleted document under a plan nobody re-made.
#[test]
fn a_hash_precondition_onto_nothing_is_drift() {
    let scratch = Scratch::new("drift-onto-nothing");
    let path = scratch.at("gone.md");
    let refusal = write(
        &path,
        b"ours",
        Precondition::Replace(hash(b"what was there")),
        scratch.shadows(),
    )
    .expect_err("a precondition onto nothing");

    assert!(
        matches!(&refusal, Refusal::Drifted { observed: None, .. }),
        "{refusal}"
    );
    assert!(!exists(&path), "a refused write created the destination");
}

/// A create with nothing at the destination lands its content and reports the
/// identity of what it published.
#[test]
fn a_create_onto_nothing_lands_and_reports_its_identity() {
    let scratch = Scratch::new("create-lands");
    let path = scratch.at("fresh.md");
    let landed = write(
        &path,
        b"fresh bytes",
        Precondition::Create,
        scratch.shadows(),
    )
    .expect("a create onto nothing");

    assert!(landed.wrote());
    let state = landed.post_state();
    assert_eq!(bytes_at(&path), b"fresh bytes");
    assert_eq!(state.content_hash, hash(b"fresh bytes"));
    assert_eq!(state.len, b"fresh bytes".len() as u64);
    assert_eq!((state.dev, state.ino), identity_at(&path));
}

/// **The bar on exclusive create.** A destination that came into existence
/// after any friendly look refuses, and the racer's bytes survive.
///
/// The forbidden shape is a precheck: an existence test followed by a write is
/// two moments, and anything that arrives between them is overwritten. The
/// exclusive open is one moment, and this case is what a precheck would get
/// wrong — the file is placed *after* the caller decided to create.
#[test]
fn a_create_onto_a_racers_document_refuses_and_leaves_it() {
    let scratch = Scratch::new("create-race");
    let path = scratch.at("note.md");
    // The caller's own look: nothing is there.
    assert!(!exists(&path));
    // And then somebody else creates it.
    scratch.place("note.md", b"the racer's bytes");

    let refusal = write(&path, b"ours", Precondition::Create, scratch.shadows())
        .expect_err("a create onto a taken name");

    assert_eq!(refusal, Refusal::DestinationExists { path: path.clone() });
    assert_eq!(
        bytes_at(&path),
        b"the racer's bytes",
        "a refused create overwrote the racer"
    );
}

/// A create refuses a taken name whatever is at it — a document, an empty file,
/// or a directory — and each refusal is the same typed one rather than an
/// operating-system message.
#[test]
fn a_create_refuses_every_taken_name_the_same_way() {
    let scratch = Scratch::new("create-taken");
    let document = scratch.place("note.md", b"bytes");
    let empty = scratch.place("empty.md", b"");
    let directory = scratch.directory("folder.md");

    for taken in [&document, &empty, &directory] {
        let refusal = write(taken, b"ours", Precondition::Create, scratch.shadows())
            .expect_err("a create onto a taken name");
        assert_eq!(
            refusal,
            Refusal::DestinationExists {
                path: taken.to_path_buf()
            },
            "{} refused as something other than a taken name",
            taken.display()
        );
    }
    assert_eq!(bytes_at(&document), b"bytes");
    assert_eq!(bytes_at(&empty), b"");
}

/// **The bar on a write that changes nothing.** Content identical to what is
/// there is a distinct outcome that publishes nothing.
///
/// The forbidden shape is reporting it as written. That primes a suppression
/// path for a filesystem event that is never coming, which then absorbs the next
/// foreign change to that document — and it costs a new inode, a shadow and a
/// rename for a document that did not change.
#[test]
fn content_identical_to_what_is_there_publishes_nothing() {
    let scratch = Scratch::new("unchanged");
    let path = scratch.place("note.md", b"already right");
    let before = identity_at(&path);

    let landed = write(
        &path,
        b"already right",
        Precondition::Replace(hash(b"already right")),
        scratch.shadows(),
    )
    .expect("a write that changes nothing");

    assert!(
        matches!(landed, Landed::Unchanged(_)),
        "{landed:?} was reported for content that did not change"
    );
    assert!(!landed.wrote());
    assert_eq!(landed.post_state().content_hash, hash(b"already right"));
    assert_eq!(
        (landed.post_state().dev, landed.post_state().ino),
        before,
        "an unchanged write reported an identity the destination does not have"
    );
    assert_eq!(
        identity_at(&path),
        before,
        "an unchanged write replaced the file with a copy of itself"
    );
    assert!(
        scratch.shadow_names().is_empty(),
        "an unchanged write staged a shadow: {:?}",
        scratch.shadow_names()
    );
}

/// The unchanged outcome is only reachable under a precondition that was
/// satisfied. Identical content behind a *stale* hash is still drift: the caller
/// composed against a world that has moved, and that its answer happens to
/// match is not evidence that its reasoning does.
#[test]
fn identical_content_behind_a_stale_precondition_is_still_drift() {
    let scratch = Scratch::new("unchanged-stale");
    let path = scratch.place("note.md", b"what is there");
    let refusal = write(
        &path,
        b"what is there",
        Precondition::Replace(hash(b"what the caller read")),
        scratch.shadows(),
    )
    .expect_err("a stale precondition");
    assert!(matches!(refusal, Refusal::Drifted { .. }), "{refusal}");
}

/// A replacement lands, publishes exactly the composed bytes, and reports an
/// identity that matches what is at the path.
#[test]
fn a_replacement_lands_and_reports_what_it_published() {
    let scratch = Scratch::new("replace-lands");
    let path = scratch.place("note.md", b"old");
    let landed = write(
        &path,
        b"new bytes",
        Precondition::Replace(hash(b"old")),
        scratch.shadows(),
    )
    .expect("a replacement");

    assert!(landed.wrote());
    let state = landed.post_state();
    assert_eq!(bytes_at(&path), b"new bytes");
    assert_eq!(state.content_hash, hash(b"new bytes"));
    assert_eq!(state.len, b"new bytes".len() as u64);
    assert_eq!((state.dev, state.ino), identity_at(&path));
}

/// A destination that is a symbolic link is refused, whether the link resolves
/// or dangles.
///
/// The two are opposite mistakes and neither is a document write: reading
/// through a resolving link would take bytes from one place and publish at
/// another, and a rename onto a dangling one would replace the link itself. So
/// the refusal keys on being a link and asks nothing about the target.
#[test]
fn a_symlinked_destination_is_refused_whether_or_not_it_resolves() {
    let scratch = Scratch::new("symlink");
    let real = scratch.place("real.md", b"the target's bytes");
    let resolving = scratch.at("resolving.md");
    crate::common::symlink(&real, &resolving);
    let dangling = scratch.at("dangling.md");
    crate::common::symlink(&scratch.at("nothing.md"), &dangling);

    for link in [&resolving, &dangling] {
        for precondition in [
            Precondition::Create,
            Precondition::Replace(hash(b"the target's bytes")),
        ] {
            let refusal = write(link, b"ours", precondition, scratch.shadows())
                .expect_err("a symlinked destination");
            assert_eq!(
                refusal,
                Refusal::SymlinkDestination {
                    path: link.to_path_buf()
                },
                "{} was refused as something other than a link",
                link.display()
            );
        }
    }
    assert_eq!(
        bytes_at(&real),
        b"the target's bytes",
        "a write went through a link"
    );
    assert!(!exists(&scratch.at("nothing.md")));
}

/// An unreadable destination is an environmental refusal, never drift and never
/// absence.
///
/// The distinction is the whole point: absence would let a create proceed and
/// drift would send a caller re-planning against a document it cannot see. A
/// permission that was revoked is the machine's problem and says so.
#[test]
fn an_unreadable_destination_is_an_environmental_refusal() {
    let scratch = Scratch::new("unreadable");
    let path = scratch.place("note.md", b"bytes nobody can read");
    crate::common::set_mode(&path, 0o000);
    crate::common::demand_unreadable(&path);

    let refusal = write(
        &path,
        b"ours",
        Precondition::Replace(hash(b"bytes nobody can read")),
        scratch.shadows(),
    )
    .expect_err("an unreadable destination");

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
}
