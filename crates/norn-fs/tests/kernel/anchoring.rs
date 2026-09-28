//! Where a staged or published target is reached from: the vault root, one
//! folder at a time, through no link.

use std::path::Path;

use norn_fs::{Refusal, Transition};

use crate::common::{Scratch, bytes_at, exists, hash, staged, swap_folder_for_link, symlink};

/// Every kind of transition, over a document that holds `b"old"`.
fn every_kind() -> [Transition<'static>; 3] {
    [
        Transition::Create { content: b"fresh" },
        Transition::Replace {
            before: hash(b"old"),
            content: b"new",
        },
        Transition::Remove {
            before: hash(b"old"),
        },
    ]
}

/// The name each kind in [`every_kind`] is aimed at below a folder: a create
/// needs a name nothing is at.
fn name_for(transition: &Transition<'_>) -> &'static str {
    match transition {
        Transition::Create { .. } => "fresh.md",
        _ => "note.md",
    }
}

/// **The bar on a linked folder at staging.** A target reached through a
/// folder that is a symbolic link refuses staging for every kind, naming the
/// link, and nothing is staged or written through it.
///
/// The forbidden shape is a descent the kernel resolves in one call:
/// `O_NOFOLLOW` binds only the last name, so a link in the middle of the path
/// would carry a write out of the vault and into wherever it points.
#[test]
fn a_linked_folder_refuses_staging_for_every_kind() {
    let scratch = Scratch::new("anchor-stage-link");
    let outside = scratch.vault().with_extension("outside");
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: a folder outside the vault.
    std::fs::create_dir_all(&outside).expect("a folder outside the vault");
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: a document outside the vault.
    std::fs::write(outside.join("note.md"), b"old").expect("a document outside the vault");
    symlink(&outside, &scratch.at("linked"));

    for transition in every_kind() {
        let relative = format!("linked/{}", name_for(&transition));
        let refusal = scratch
            .stage(&relative, transition)
            .expect_err("a target below a linked folder");
        assert_eq!(
            refusal,
            Refusal::LinkedAncestor {
                path: scratch.at(&relative),
                ancestor: scratch.at("linked"),
            },
            "{relative}"
        );
    }
    assert_eq!(bytes_at(&outside.join("note.md")), b"old");
    assert!(!exists(&outside.join("fresh.md")));
    assert!(scratch.shadow_names().is_empty());
}

/// **The bar on a linked folder at publication.** A folder swapped for a link
/// between the phases refuses publication for every kind, and nothing is
/// published through the link.
///
/// The link here points at the folder's own contents, so every name below it
/// still resolves and still hashes as staging saw it. Only a descent that
/// refuses links tells the difference, which is the claim.
#[test]
fn a_folder_swapped_for_a_link_refuses_publication_for_every_kind() {
    let scratch = Scratch::new("anchor-publish-link");
    scratch.place("folder/note.md", b"old");
    let staged = every_kind().map(|transition| {
        let relative = format!("folder/{}", name_for(&transition));
        (
            relative.clone(),
            staged(scratch.stage(&relative, transition).expect("staged")),
        )
    });
    swap_folder_for_link(&scratch.at("folder"));

    for (relative, staged) in staged {
        let refusal = scratch
            .publish(staged)
            .expect_err("a target below a folder swapped for a link");
        assert_eq!(
            refusal,
            Refusal::LinkedAncestor {
                path: scratch.at(&relative),
                ancestor: scratch.at("folder"),
            },
            "{relative}"
        );
    }
    let real = scratch.at("folder.real");
    assert_eq!(bytes_at(&real.join("note.md")), b"old");
    assert!(!exists(&real.join("fresh.md")));
    assert!(
        scratch.shadow_names().is_empty(),
        "a refused publication left its shadow"
    );
}

/// A folder in the path that is a file refuses staging as the environment
/// naming a name that is not a folder, rather than reading as absence.
///
/// The forbidden shape is absence: a create would stage a document that can
/// never be published, and a removal would report landed over a tree it never
/// looked into.
#[test]
fn a_folder_that_is_a_file_refuses_staging() {
    let scratch = Scratch::new("anchor-file-parent");
    scratch.place("folder", b"a file where a folder would be");

    for transition in every_kind() {
        let relative = format!("folder/{}", name_for(&transition));
        let refusal = scratch
            .stage(&relative, transition)
            .expect_err("a target below a file");
        assert!(
            matches!(
                &refusal,
                Refusal::Environment {
                    kind: std::io::ErrorKind::NotADirectory,
                    ..
                }
            ),
            "{relative}: {refusal}"
        );
    }
    assert!(scratch.shadow_names().is_empty());
}

/// A path that is not a name below the vault root refuses before anything is
/// opened: a parent component, an absolute path, or nothing at all.
#[test]
fn a_path_that_leaves_the_root_is_refused() {
    let scratch = Scratch::new("anchor-uncontained");
    scratch.place("note.md", b"old");
    let escape = format!(
        "../{}/note.md",
        scratch
            .vault()
            .file_name()
            .expect("a root name")
            .to_string_lossy()
    );
    let absolute = scratch.at("note.md").to_string_lossy().into_owned();

    for relative in [escape.as_str(), absolute.as_str(), "", "folder/../note.md"] {
        for transition in every_kind() {
            let refusal = norn_fs::stage(
                &scratch.vault(),
                Path::new(relative),
                transition,
                scratch.shadows(),
            )
            .expect_err("a path that leaves the root");
            assert!(
                matches!(
                    &refusal,
                    Refusal::Environment {
                        kind: std::io::ErrorKind::InvalidInput,
                        ..
                    }
                ),
                "{relative:?}: {refusal}"
            );
        }
    }
    assert_eq!(bytes_at(&scratch.at("note.md")), b"old");
    assert!(scratch.shadow_names().is_empty());
}

/// The root itself is resolved as it is spelled: a vault reached through a
/// linked root stages and publishes like any other.
///
/// The root is the boundary rather than a name inside it, so refusing a link
/// there would refuse every vault a person keeps behind one.
#[test]
fn a_root_spelled_through_a_link_is_the_boundary() {
    let scratch = Scratch::new("anchor-linked-root");
    scratch.place("note.md", b"old");
    let spelled = scratch.vault().with_extension("link");
    symlink(&scratch.vault(), &spelled);

    let staged = staged(
        norn_fs::stage(
            &spelled,
            Path::new("note.md"),
            Transition::Replace {
                before: hash(b"old"),
                content: b"new",
            },
            scratch.shadows(),
        )
        .expect("a root reached through a link"),
    );
    norn_fs::publish(&spelled, staged, scratch.shadows()).expect("published");
    assert_eq!(bytes_at(&scratch.at("note.md")), b"new");
}
