//! Where a write's bytes wait, and what a leaked one can and cannot do.

use std::ffi::OsStr;

use norn_fs::{Precondition, Refusal, is_shadow_name, write};

use crate::common::{Scratch, bytes_at, exists, hard_link, hash, identity_at};

/// **The bar on cleanup after a success.** A landed write leaves no shadow.
#[test]
fn a_landed_write_leaves_no_shadow() {
    let scratch = Scratch::new("shadow-gone");
    let path = scratch.place("note.md", b"old");
    for round in 0..5 {
        let previous = format!("{round}");
        let next = format!("{}", round + 1);
        let expected = if round == 0 {
            hash(b"old")
        } else {
            hash(previous.as_bytes())
        };
        write(
            &path,
            next.as_bytes(),
            Precondition::Replace(expected),
            scratch.shadows(),
        )
        .expect("a replacement");
        assert!(
            scratch.shadow_names().is_empty(),
            "round {round} left {:?} behind",
            scratch.shadow_names()
        );
    }
    assert_eq!(bytes_at(&path), b"5");
}

/// **The bar on a leaked, aliased shadow.** A shadow left behind by a dead
/// writer — even one that had become a second name for the live document — is
/// never reopened, so a later write cannot reach the document through it.
///
/// The hazard is manufactured directly rather than raced for: the leak is a hard
/// link, placed in the shadow home under the name a stem-derived scheme would
/// compute for this destination. The forbidden shape is exactly that scheme with
/// truncating semantics — a later write to the same document recomputes the name,
/// opens it, and truncates the live document through the shared inode before it
/// has looked at the destination at all.
///
/// The residue is also what the mode-preservation boundary admits it cannot
/// carry: a foreign hard link keeps the old inode and therefore the old content.
/// That is asserted here rather than hoped for.
#[test]
fn a_leaked_aliased_shadow_is_never_reopened() {
    let scratch = Scratch::new("shadow-alias");
    let path = scratch.place("note.md", b"live bytes");
    let live = identity_at(&path);

    // Two leaks: the name a stem-derived scheme would compute, and one in this
    // crate's own shape left by a process that is gone. Both are second names
    // for the live document.
    let stem_derived = scratch.shadows().directory().join(".note.md.tmp");
    let shadow_shaped = scratch.shadows().directory().join("norn-shadow-1-0");
    hard_link(&path, &stem_derived);
    hard_link(&path, &shadow_shaped);
    assert_eq!(identity_at(&stem_derived), live);
    assert_eq!(identity_at(&shadow_shaped), live);

    // A create refuses without having touched anything.
    let refusal = write(&path, b"ours", Precondition::Create, scratch.shadows())
        .expect_err("a create onto a taken name");
    assert_eq!(refusal, Refusal::DestinationExists { path: path.clone() });
    assert_eq!(bytes_at(&path), b"live bytes");
    assert_eq!(bytes_at(&stem_derived), b"live bytes");

    // And a replacement lands without having touched them either.
    write(
        &path,
        b"published bytes",
        Precondition::Replace(hash(b"live bytes")),
        scratch.shadows(),
    )
    .expect("a replacement");

    assert_eq!(bytes_at(&path), b"published bytes");
    assert_ne!(
        identity_at(&path),
        live,
        "the replacement wrote in place instead of publishing a new file"
    );
    for leak in [&stem_derived, &shadow_shaped] {
        assert_eq!(
            bytes_at(leak),
            b"live bytes",
            "{} was reopened by a later write",
            leak.display()
        );
        assert_eq!(identity_at(leak), live);
    }
}

/// A shadow home already holding residue neither blocks a write nor is consumed
/// by one.
///
/// The residue's sequence number is one no run of this process reaches, so the
/// case is deterministically about residue that is *not* in the way — whether it
/// runs alone or beside every other case in the binary. A name that might or
/// might not be the one the next write computes would make this two different
/// cases depending on how many shadows the process had already staged, and the
/// collision itself is a claim of its own with a bar of its own.
#[test]
fn residue_in_the_shadow_home_neither_blocks_nor_is_taken() {
    let scratch = Scratch::new("shadow-residue");
    let residue = scratch
        .shadows()
        .directory()
        .join(format!("norn-shadow-{}-999999999999", std::process::id()));
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: a dead writer's residue.
    std::fs::write(&residue, b"a dead writer's bytes").expect("residue");

    let path = scratch.place("note.md", b"old");
    write(
        &path,
        b"new",
        Precondition::Replace(hash(b"old")),
        scratch.shadows(),
    )
    .expect("a replacement over a dead writer's residue");

    assert_eq!(bytes_at(&path), b"new");
    assert_eq!(
        bytes_at(&residue),
        b"a dead writer's bytes",
        "a write took a name a previous life had already taken"
    );
    assert!(is_shadow_name(residue.file_name().expect("a name")));
}

/// **The bar on recognizing a shadow.** Suppression, the walk and the sweep all
/// key on the predicate, and it admits our names and refuses everything shaped
/// like them.
///
/// The forbidden shape is a coarse hidden-entry filter. It hides a user's
/// genuinely dot-prefixed document from the index as well, and gives a sweep no
/// way to tell our residue from a file somebody put there on purpose.
#[test]
fn the_predicate_is_what_recognizes_a_shadow() {
    let scratch = Scratch::new("shadow-predicate");
    // Names the vault may legitimately hold, none of which is a shadow.
    for theirs in [
        ".hidden.md",
        ".note.md.tmp",
        "norn-shadow-notes.md",
        "norn-shadow.md",
        "note.md",
    ] {
        let path = scratch.place(theirs, b"somebody's file");
        assert!(
            !is_shadow_name(path.file_name().expect("a name")),
            "{theirs} would be taken for a shadow"
        );
        assert!(exists(&path));
    }
    assert!(is_shadow_name(OsStr::new("norn-shadow-1-0")));
}

/// A shadow that cannot be staged at all refuses before the destination is
/// touched, and the refusal names the shadow home rather than the document.
///
/// A shadow home that has become unwritable is a machine problem, and it reads as
/// one: the destination is exactly as it was, and nothing about the caller's plan
/// was wrong. The clause about a *removal* that cannot happen is checked in the
/// crate's own suite, where both the swap and the cleanup can be made to fail in
/// one call.
#[test]
fn a_shadow_that_cannot_be_staged_refuses_before_the_destination_is_touched() {
    let scratch = Scratch::new("shadow-blocked");
    let path = scratch.place("note.md", b"old");
    crate::common::set_mode(scratch.shadows().directory(), 0o500);
    crate::common::demand_unwritable(scratch.shadows().directory());

    let refusal = write(
        &path,
        b"new",
        Precondition::Replace(hash(b"old")),
        scratch.shadows(),
    )
    .expect_err("a shadow home nothing may write into");

    assert!(
        matches!(
            &refusal,
            Refusal::Environment {
                operation: "creating",
                kind: std::io::ErrorKind::PermissionDenied,
                ..
            }
        ),
        "{refusal}"
    );
    crate::common::set_mode(scratch.shadows().directory(), 0o755);
    assert_eq!(bytes_at(&path), b"old");
    assert!(scratch.shadow_names().is_empty());
}
