//! Removal and movement, which are the same kernel with different endings.

use norn_fs::{MoveRefusal, Refusal, move_document, vacate};

use crate::common::{
    Scratch, bytes_at, demand_unwritable, exists, hash, identity_at, set_mode, symlink,
};

/// A removal under a satisfied precondition takes the document and reports the
/// identity of what it took.
///
/// The identity is carried for the reason a write's is: the removal comes back as
/// a filesystem event, and an event Norn cannot recognize as its own is
/// re-derived for nothing.
#[test]
fn a_removal_under_its_precondition_reports_what_it_took() {
    let scratch = Scratch::new("vacate");
    let path = scratch.place("note.md", b"to be removed");
    let identity = identity_at(&path);

    let vacated = vacate(&path, hash(b"to be removed")).expect("a removal");

    assert!(!exists(&path));
    assert_eq!(vacated.removed.content_hash, hash(b"to be removed"));
    assert_eq!(vacated.removed.len, b"to be removed".len() as u64);
    assert_eq!((vacated.removed.dev, vacated.removed.ino), identity);
}

/// A removal bears a precondition for the reason a write does: the intent to
/// remove a document was formed from its content.
///
/// The forbidden shape is an unconditional unlink. A document somebody edited
/// after the plan was made is a document the plan no longer describes, and
/// removing it destroys work nobody looked at.
#[test]
fn a_removal_whose_precondition_moved_refuses_and_leaves_the_document() {
    let scratch = Scratch::new("vacate-drift");
    let path = scratch.place("note.md", b"what somebody else wrote");

    let refusal = vacate(&path, hash(b"what the caller read")).expect_err("a stale precondition");

    let Refusal::Drifted { observed, .. } = &refusal else {
        panic!("a stale precondition was not reported as drift: {refusal}");
    };
    assert_eq!(
        observed.as_ref().expect("the observed state").content_hash,
        hash(b"what somebody else wrote")
    );
    assert_eq!(bytes_at(&path), b"what somebody else wrote");
}

/// **The bar on a removal onto nothing.** A path with nothing at it is drift, not
/// a removal that succeeded.
///
/// The forbidden shape is reporting success — an idempotent removal. It records
/// an own-write for an event this process did not cause, so the suppression path
/// then absorbs somebody else's deletion, and the caller is told its plan ran when
/// what really happened is that the world moved under it.
#[test]
fn a_removal_onto_nothing_is_drift_rather_than_success() {
    let scratch = Scratch::new("vacate-absent");
    let path = scratch.at("gone.md");

    let refusal = vacate(&path, hash(b"what was there")).expect_err("a removal onto nothing");

    assert!(
        matches!(&refusal, Refusal::Drifted { observed: None, .. }),
        "{refusal}"
    );
    let rendered = refusal.to_string();
    assert!(rendered.contains("nothing is at"), "{rendered}");
}

/// A symbolic link is not a document, in either direction. A removal through one
/// would take a link and leave the document, or take a document nothing named.
#[test]
fn a_removal_of_a_symlink_is_refused() {
    let scratch = Scratch::new("vacate-symlink");
    let real = scratch.place("real.md", b"the target's bytes");
    let link = scratch.at("link.md");
    symlink(&real, &link);

    let refusal = vacate(&link, hash(b"the target's bytes")).expect_err("a symlinked path");
    assert_eq!(refusal, Refusal::SymlinkDestination { path: link.clone() });
    assert!(exists(&link));
    assert_eq!(bytes_at(&real), b"the target's bytes");
}

/// A removal the filesystem refuses is an environmental refusal, and the document
/// is still there.
#[test]
fn a_removal_the_filesystem_refuses_leaves_the_document() {
    let scratch = Scratch::new("vacate-eacces");
    let folder = scratch.directory("folder");
    let path = scratch.place("folder/note.md", b"bytes");
    set_mode(&folder, 0o500);
    demand_unwritable(&folder);

    let refusal = vacate(&path, hash(b"bytes")).expect_err("a removal nothing may make");

    assert!(
        matches!(
            &refusal,
            Refusal::Environment {
                operation: "removing",
                kind: std::io::ErrorKind::PermissionDenied,
                ..
            }
        ),
        "{refusal}"
    );
    set_mode(&folder, 0o755);
    assert_eq!(bytes_at(&path), b"bytes");
}

/// A move lands the content at the destination, takes the source, and reports
/// both legs.
#[test]
fn a_move_lands_the_destination_and_takes_the_source() {
    let scratch = Scratch::new("move");
    let source = scratch.place("from.md", b"the document");
    let destination = scratch.at("to.md");
    let was = identity_at(&source);

    let moved = move_document(
        &source,
        &destination,
        hash(b"the document"),
        scratch.shadows(),
    )
    .expect("a move");

    assert!(!exists(&source));
    assert_eq!(bytes_at(&destination), b"the document");
    assert_eq!(moved.created.content_hash, hash(b"the document"));
    assert_eq!(
        (moved.created.dev, moved.created.ino),
        identity_at(&destination)
    );
    assert_eq!((moved.vacated.dev, moved.vacated.ino), was);
}

/// **The bar on a move onto a taken name.** The destination leg is an exclusive
/// create, so a name that came into existence after any look refuses and neither
/// side is touched.
///
/// The forbidden shape is an existence precheck followed by a write, which is
/// what makes a move race-losable where a create is not: the two operations
/// would have different guarantees for the same question, and the weaker one
/// would be the one a rename verb reached for.
#[test]
fn a_move_onto_a_taken_name_touches_neither_side() {
    let scratch = Scratch::new("move-taken");
    let source = scratch.place("from.md", b"the document");
    let destination = scratch.at("to.md");
    // The caller's own look: nothing is there.
    assert!(!exists(&destination));
    // And then somebody else creates it.
    scratch.place("to.md", b"the racer's bytes");

    let refusal = move_document(
        &source,
        &destination,
        hash(b"the document"),
        scratch.shadows(),
    )
    .expect_err("a move onto a taken name");

    assert!(
        matches!(
            &refusal,
            MoveRefusal::Destination(Refusal::DestinationExists { .. })
        ),
        "{refusal}"
    );
    assert_eq!(bytes_at(&source), b"the document", "the source was touched");
    assert_eq!(
        bytes_at(&destination),
        b"the racer's bytes",
        "the racer was overwritten"
    );
}

/// A move whose source moved refuses on the source leg and writes nothing, so a
/// caller cannot be told a document was moved that nobody read.
#[test]
fn a_move_whose_source_moved_writes_nothing() {
    let scratch = Scratch::new("move-drift");
    let source = scratch.place("from.md", b"what somebody else wrote");
    let destination = scratch.at("to.md");

    let refusal = move_document(
        &source,
        &destination,
        hash(b"what the caller read"),
        scratch.shadows(),
    )
    .expect_err("a stale source precondition");

    assert!(
        matches!(&refusal, MoveRefusal::Source(Refusal::Drifted { .. })),
        "{refusal}"
    );
    assert!(
        !exists(&destination),
        "a refused move created the destination"
    );
    assert_eq!(bytes_at(&source), b"what somebody else wrote");
}

/// **The bar on the pair's residue.** A move whose source cannot be removed
/// leaves the document at both paths, names the leg that refused, and hands back
/// the identity of what it did publish.
///
/// The forbidden shape is reporting this as a move that did not happen. Half of
/// it happened, and a caller that treated the whole thing as a no-op would write
/// the destination again — or, worse, an ordering that removed the source first
/// would leave the document at *neither* path, which is the one residue nothing
/// can resolve.
#[test]
fn a_move_that_cannot_take_its_source_leaves_the_document_at_both_paths() {
    let scratch = Scratch::new("move-remains");
    let folder = scratch.directory("locked");
    let source = scratch.place("locked/from.md", b"the document");
    let destination = scratch.at("to.md");
    // Readable and traversable, so the source can be read; not writable, so its
    // directory entry cannot be removed.
    set_mode(&folder, 0o500);
    demand_unwritable(&folder);

    let refusal = move_document(
        &source,
        &destination,
        hash(b"the document"),
        scratch.shadows(),
    )
    .expect_err("a source whose removal is refused");

    let MoveRefusal::SourceRemains { created, refusal } = &refusal else {
        panic!("the leg that refused was not named: {refusal}");
    };
    assert_eq!(created.content_hash, hash(b"the document"));
    assert!(
        matches!(
            refusal,
            Refusal::Environment {
                operation: "removing",
                ..
            }
        ),
        "{refusal}"
    );

    set_mode(&folder, 0o755);
    assert_eq!(
        bytes_at(&source),
        b"the document",
        "the source was removed after all"
    );
    assert_eq!(
        bytes_at(&destination),
        b"the document",
        "the destination does not hold the document, so the residue is neither"
    );
}

/// A move onto a symbolic link is refused rather than followed, so a rename verb
/// cannot publish a document somewhere the caller did not name.
#[test]
fn a_move_onto_a_symlink_is_refused() {
    let scratch = Scratch::new("move-symlink");
    let source = scratch.place("from.md", b"the document");
    let elsewhere = scratch.place("elsewhere.md", b"somebody else's document");
    let destination = scratch.at("to.md");
    symlink(&elsewhere, &destination);

    let refusal = move_document(
        &source,
        &destination,
        hash(b"the document"),
        scratch.shadows(),
    )
    .expect_err("a move onto a link");

    assert!(
        matches!(
            &refusal,
            MoveRefusal::Destination(Refusal::SymlinkDestination { .. })
        ),
        "{refusal}"
    );
    assert_eq!(bytes_at(&elsewhere), b"somebody else's document");
    assert_eq!(bytes_at(&source), b"the document");
}
