//! Where a staged target's bytes wait, what a leaked shadow can and cannot do,
//! and what staging holds on to between the phases.

use std::ffi::OsStr;

use norn_fs::{Refusal, Transition, is_shadow_name};

use crate::common::{Scratch, bytes_at, exists, hard_link, hash, identity_at, staged};

/// **The bar on cleanup after a success.** A landed publication leaves no
/// shadow.
#[test]
fn a_landed_publication_leaves_no_shadow() {
    let scratch = Scratch::new("shadow-gone");
    let path = scratch.place("note.md", b"0");
    for round in 0..5 {
        let previous = format!("{round}");
        let next = format!("{}", round + 1);
        let _ = scratch
            .stage_and_publish(
                "note.md",
                Transition::Replace {
                    before: hash(previous.as_bytes()),
                    content: next.as_bytes(),
                },
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
/// The hazard is manufactured directly: the leak is a hard link placed in the
/// shadow home under the name a stem-derived scheme would compute, and under a
/// name in this crate's own shape. A foreign hard link keeps the old inode and
/// therefore the old content, which is asserted rather than hoped for.
#[test]
fn a_leaked_aliased_shadow_is_never_reopened() {
    let scratch = Scratch::new("shadow-alias");
    let path = scratch.place("note.md", b"live bytes");
    let live = identity_at(&path);

    let stem_derived = scratch.shadows().directory().join(".note.md.tmp");
    let shadow_shaped = scratch.shadows().directory().join("norn-shadow-1-0");
    hard_link(&path, &stem_derived);
    hard_link(&path, &shadow_shaped);

    let _ = scratch
        .stage_and_publish(
            "note.md",
            Transition::Replace {
                before: hash(b"live bytes"),
                content: b"published bytes",
            },
        )
        .expect("a replacement");

    assert_eq!(bytes_at(&path), b"published bytes");
    assert_ne!(identity_at(&path), live);
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

/// A shadow home already holding residue neither blocks a publication nor is
/// consumed by one.
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
    let _ = scratch
        .stage_and_publish(
            "note.md",
            Transition::Replace {
                before: hash(b"old"),
                content: b"new",
            },
        )
        .expect("a replacement over a dead writer's residue");

    assert_eq!(bytes_at(&path), b"new");
    assert_eq!(bytes_at(&residue), b"a dead writer's bytes");
    assert!(is_shadow_name(residue.file_name().expect("a name")));
}

/// **The bar on recognizing a shadow.** The predicate admits our names and
/// refuses everything shaped like them.
#[test]
fn the_predicate_is_what_recognizes_a_shadow() {
    let scratch = Scratch::new("shadow-predicate");
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

/// A shadow that cannot be staged at all refuses staging, and the refusal names
/// the shadow home rather than the document.
#[test]
fn a_shadow_that_cannot_be_staged_refuses_staging() {
    let scratch = Scratch::new("shadow-blocked");
    let path = scratch.place("note.md", b"old");
    crate::common::set_mode(scratch.shadows().directory(), 0o500);
    crate::common::demand_unwritable(scratch.shadows().directory());

    let refusal = scratch
        .stage(
            "note.md",
            Transition::Replace {
                before: hash(b"old"),
                content: b"new",
            },
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

/// Discarding a staged target removes its shadow and publishes nothing.
#[test]
fn a_discarded_target_leaves_no_shadow_and_publishes_nothing() {
    let scratch = Scratch::new("shadow-discard");
    let path = scratch.place("note.md", b"old");
    let staged = staged(
        scratch
            .stage(
                "note.md",
                Transition::Replace {
                    before: hash(b"old"),
                    content: b"new",
                },
            )
            .expect("staged"),
    );
    assert_eq!(scratch.shadow_names().len(), 1);

    scratch.discard(staged);

    assert!(
        scratch.shadow_names().is_empty(),
        "a discard left the shadow"
    );
    assert_eq!(bytes_at(&path), b"old");
}

/// **The bar on what staging holds.** No descriptor staging opened survives
/// it: not the root, not a folder, not the target, not the shadow.
///
/// Read off this process's descriptor table, keyed on where each descriptor
/// leads, so a descriptor another case in this binary holds is never counted.
/// The forbidden shape is a staged target that carries an open handle into
/// publication: a plan of many targets would hold one per target, and a
/// handle carried across the phases is exactly what lets publication skip
/// asking the filesystem again.
#[cfg(target_os = "linux")]
#[test]
fn no_descriptor_survives_staging() {
    let scratch = Scratch::new("shadow-no-handle");
    scratch.place("folder/note.md", b"old");
    scratch.place("folder/gone.md", b"going");
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the spelling the kernel reports.
    let tree = std::fs::canonicalize(scratch.vault().parent().expect("the vault sits in a tree"))
        .expect("the tree's canonical spelling");
    // The control: a descriptor held under the tree is one the reading sees,
    // so an empty answer below is not an unreadable table.
    {
        #[allow(clippy::disallowed_methods, clippy::disallowed_types)]
        // Harness scaffolding: the control descriptor.
        let _held = std::fs::File::open(scratch.at("folder/note.md")).expect("a held file");
        assert_eq!(
            descriptors_under(&tree).len(),
            1,
            "the reading does not see a held file"
        );
    }

    let staged = [
        staged(
            scratch
                .stage(
                    "folder/note.md",
                    Transition::Replace {
                        before: hash(b"old"),
                        content: b"new",
                    },
                )
                .expect("a replacement stages"),
        ),
        staged(
            scratch
                .stage("folder/fresh.md", Transition::Create { content: b"fresh" })
                .expect("a create stages"),
        ),
        staged(
            scratch
                .stage(
                    "folder/gone.md",
                    Transition::Remove {
                        before: hash(b"going"),
                    },
                )
                .expect("a removal stages"),
        ),
    ];

    let held = descriptors_under(&tree);
    assert!(
        held.is_empty(),
        "staging left descriptors open under the tree: {held:?}"
    );
    for staged in staged {
        scratch.discard(staged);
    }
}

/// Every descriptor this process holds that leads to a name under `tree`.
#[cfg(target_os = "linux")]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: reading this process's descriptor table.
fn descriptors_under(tree: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir("/proc/self/fd")
        .expect("this process's descriptor table")
        .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
        .filter(|target| target.starts_with(tree))
        .collect()
}
