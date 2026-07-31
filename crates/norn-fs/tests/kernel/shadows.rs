//! Where a write's bytes wait, and what a leaked one can and cannot do.

use std::ffi::OsStr;

use norn_fs::{Precondition, Refusal, is_shadow_name, write};

use crate::common::{Scratch, bytes_at, exists, hard_link, hash, identity_at};

/// A shadow is never a sibling of its destination, and its name is recognizable
/// as a shadow rather than as a hidden file.
///
/// A sibling is the forbidden placement: the vault tree belongs to every
/// consumer, and a mechanism file among documents is committed, synced and
/// indexed by tools that cannot know to ignore it.
#[test]
fn a_shadow_is_staged_outside_the_vault_under_a_recognizable_name() {
    let scratch = Scratch::new("shadow-placement");
    assert!(
        !scratch.shadows().directory().starts_with(scratch.vault()),
        "the shadow home is inside the vault: {}",
        scratch.shadows().directory().display()
    );

    // The name the next write will take, observed by taking one from the same
    // home: a name is per-attempt, so this is the *shape*, not the value.
    let path = scratch.place("note.md", b"old");
    write(
        &path,
        b"new",
        Precondition::Replace(hash(b"old")),
        scratch.shadows(),
    )
    .expect("a replacement");

    // And it took nothing with it. What the placement means is checked by the
    // vault holding exactly the document.
    let names = crate::common::names_in(&scratch.vault());
    assert_eq!(names, vec!["note.md".to_string()]);
}

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
/// by one. A name is per-attempt, so nothing a previous life left behind is a
/// name this life will compute.
#[test]
fn residue_in_the_shadow_home_neither_blocks_nor_is_taken() {
    let scratch = Scratch::new("shadow-residue");
    let residue = scratch
        .shadows()
        .directory()
        .join(format!("norn-shadow-{}-0", std::process::id()));
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

/// **The bar on a refusal after staging.** A write that cannot publish takes its
/// shadow with it, so a refusal costs nothing at rest.
///
/// The refusal here is reached after the shadow exists: the destination's own
/// directory is unwritable, so the rename fails. The forbidden shape is a
/// failure path that returns without cleaning up — every refused write would
/// then leave a copy of a document in the shadow home.
#[test]
fn a_write_that_cannot_publish_takes_its_shadow_with_it() {
    let scratch = Scratch::new("shadow-cleanup");
    let folder = scratch.directory("folder");
    let path = scratch.place("folder/note.md", b"old");
    crate::common::set_mode(&folder, 0o500);
    crate::common::demand_unwritable(&folder);

    let refusal = write(
        &path,
        b"new",
        Precondition::Replace(hash(b"old")),
        scratch.shadows(),
    )
    .expect_err("a rename into a directory nothing may write");

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
    crate::common::set_mode(&folder, 0o755);
    assert_eq!(bytes_at(&path), b"old", "the destination was touched");
    assert!(
        scratch.shadow_names().is_empty(),
        "a refused write left {:?} behind",
        scratch.shadow_names()
    );
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
