//! The folders a create makes on its way to its name, and the empty folders a
//! removal leaves behind.

use std::path::{Path, PathBuf};

use norn_fs::{Refusal, Transition, remove_empty_folders};

use crate::common::{Scratch, bytes_at, exists, names_in, staged, symlink};

/// Staging makes no folder: a missing folder means the target is absent, and
/// the create stages with nothing new in the vault.
#[test]
fn staging_a_create_under_missing_folders_makes_none() {
    let scratch = Scratch::new("folders-staging");
    let staged = staged(
        scratch
            .stage("a/b/fresh.md", Transition::Create { content: b"fresh" })
            .expect("a create under missing folders stages"),
    );

    assert!(!exists(&scratch.at("a")), "staging made a folder");
    assert_eq!(scratch.shadow_names().len(), 1);
    norn_fs::discard(staged, scratch.shadows());
}

/// **The bar on made folders.** Publishing a create makes every missing folder
/// on the way, one level at a time, and reports the ones it made, shallowest
/// first, relative to the vault root — and none it found.
#[test]
fn a_create_makes_its_missing_folders_and_reports_them() {
    let scratch = Scratch::new("folders-made");
    scratch.directory("a");
    let published = scratch
        .stage_and_publish("a/b/c/fresh.md", Transition::Create { content: b"fresh" })
        .expect("a create under missing folders");

    assert_eq!(bytes_at(&scratch.at("a/b/c/fresh.md")), b"fresh");
    assert_eq!(
        published.made_folders,
        vec![PathBuf::from("a/b"), PathBuf::from("a/b/c")],
        "the folders reported are not the folders made"
    );
    assert!(published.durability.is_synced());
    assert!(scratch.shadow_names().is_empty());
}

/// A create whose folder exists makes none and reports none.
#[test]
fn a_create_into_an_existing_folder_reports_no_folder() {
    let scratch = Scratch::new("folders-none");
    scratch.directory("a");
    let published = scratch
        .stage_and_publish("a/fresh.md", Transition::Create { content: b"fresh" })
        .expect("a create into a folder");
    assert!(published.made_folders.is_empty(), "{published:?}");
}

/// A folder that became a file between the phases refuses the create, which
/// makes nothing past it and leaves no folder it made.
#[test]
fn a_create_whose_missing_folder_became_a_file_refuses_and_leaves_nothing() {
    let scratch = Scratch::new("folders-file");
    let staged = staged(
        scratch
            .stage("a/b/fresh.md", Transition::Create { content: b"fresh" })
            .expect("staged"),
    );
    scratch.directory("a");
    scratch.place("a/b", b"a file where a folder would be");

    let refusal = scratch
        .publish(staged)
        .expect_err("a folder that is a file");

    assert!(
        matches!(
            &refusal,
            Refusal::Environment {
                kind: std::io::ErrorKind::NotADirectory,
                ..
            }
        ),
        "{refusal}"
    );
    assert_eq!(names_in(&scratch.at("a")), vec!["b".to_string()]);
    assert!(scratch.shadow_names().is_empty());
}

/// A missing folder that became a link between the phases refuses the create,
/// and nothing is made or published through the link.
#[test]
fn a_create_whose_missing_folder_became_a_link_refuses() {
    let scratch = Scratch::new("folders-link");
    let staged = staged(
        scratch
            .stage("a/b/fresh.md", Transition::Create { content: b"fresh" })
            .expect("staged"),
    );
    let outside = scratch.vault().with_extension("outside");
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: a folder outside the vault.
    std::fs::create_dir_all(&outside).expect("a folder outside the vault");
    symlink(&outside, &scratch.at("a"));

    let refusal = scratch
        .publish(staged)
        .expect_err("a folder that is a link");

    assert!(
        matches!(&refusal, Refusal::LinkedAncestor { ancestor, .. } if *ancestor == scratch.at("a")),
        "{refusal}"
    );
    assert!(
        names_in(&outside).is_empty(),
        "something was made through the link"
    );
}

/// **The bar on emptying upward.** Removing empty folders takes the given
/// folder and each empty one above it, stops at the first that is not empty,
/// and reports what it removed, deepest first.
#[test]
fn empty_folders_are_removed_upward_until_one_is_not_empty() {
    let scratch = Scratch::new("folders-empty");
    scratch.directory("a/b/c");
    scratch.place("a/keep.md", b"a document keeps its folder");

    let removed =
        remove_empty_folders(&scratch.vault(), Path::new("a/b/c")).expect("empty folders removed");

    assert_eq!(
        removed.removed,
        vec![PathBuf::from("a/b/c"), PathBuf::from("a/b")]
    );
    assert!(removed.durability.is_synced());
    assert!(!exists(&scratch.at("a/b")));
    assert_eq!(
        bytes_at(&scratch.at("a/keep.md")),
        b"a document keeps its folder"
    );
}

/// The vault root is never removed, however empty it is.
#[test]
fn emptying_upward_never_removes_the_root() {
    let scratch = Scratch::new("folders-root");
    scratch.directory("a/b");

    let removed =
        remove_empty_folders(&scratch.vault(), Path::new("a/b")).expect("empty folders removed");

    assert_eq!(
        removed.removed,
        vec![PathBuf::from("a/b"), PathBuf::from("a")]
    );
    assert!(exists(&scratch.vault()), "the root was removed");
}

/// A folder already gone is where emptying starts from, not a refusal: the
/// folders above it are emptied as if it had been removed here.
#[test]
fn emptying_upward_from_a_folder_already_gone_empties_what_is_above() {
    let scratch = Scratch::new("folders-gone");
    scratch.directory("a");

    let removed =
        remove_empty_folders(&scratch.vault(), Path::new("a/b/c")).expect("empty folders removed");

    assert_eq!(removed.removed, vec![PathBuf::from("a")]);
}

/// Emptying upward descends through no link: a folder reached through a link
/// refuses, and what the link points at is untouched.
#[test]
fn emptying_upward_through_a_link_refuses() {
    let scratch = Scratch::new("folders-empty-link");
    let outside = scratch.vault().with_extension("outside");
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: a folder outside the vault.
    std::fs::create_dir_all(outside.join("empty")).expect("an empty folder outside the vault");
    symlink(&outside, &scratch.at("linked"));

    let refusal = remove_empty_folders(&scratch.vault(), Path::new("linked/empty"))
        .expect_err("a folder reached through a link");

    assert!(
        matches!(&refusal, Refusal::LinkedAncestor { ancestor, .. } if *ancestor == scratch.at("linked")),
        "{refusal}"
    );
    assert!(
        exists(&outside.join("empty")),
        "a folder outside the vault was removed"
    );
}

/// A path that is not a folder below the root refuses before anything is
/// removed.
#[test]
fn emptying_upward_from_a_path_that_leaves_the_root_is_refused() {
    let scratch = Scratch::new("folders-uncontained");
    scratch.directory("a");
    for relative in ["../a", "a/../a", "/a"] {
        let refusal = remove_empty_folders(&scratch.vault(), Path::new(relative))
            .expect_err("a path that leaves the root");
        assert!(
            matches!(
                &refusal,
                Refusal::Environment {
                    kind: std::io::ErrorKind::InvalidInput,
                    ..
                }
            ),
            "{relative}: {refusal}"
        );
    }
    assert!(exists(&scratch.at("a")));
}
