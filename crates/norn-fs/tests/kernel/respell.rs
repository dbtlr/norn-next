//! A case-only rename on a root that folds case: the respell.
//!
//! The cases about what a respell does are stated over a root that folds, and
//! they run only where the scratch root is detected to fold — which a default
//! APFS volume does and a Linux temporary directory does not. They are gated on
//! the detected behavior rather than on the platform, so a folding volume
//! anywhere runs them and a case-sensitive one anywhere skips them. The cases
//! about what a respell refuses run everywhere.

use std::path::Path;

use norn_fs::{
    AfterState, CaseSensitivity, PathNormalizer, Refusal, Staging, Transition, confirm_landed,
};

use crate::common::{Scratch, bytes_at, found, hash, identity_at, names_in, staged, wrote};

/// The case behavior the scratch vault's root proves.
fn root_case(scratch: &Scratch) -> CaseSensitivity {
    PathNormalizer::detect(&scratch.vault())
        .expect("the scratch root's case behavior")
        .case_sensitivity()
}

/// Whether the scratch root folds, saying so where a case is skipped for it.
fn folds(scratch: &Scratch, case: &str) -> bool {
    let folds = root_case(scratch) == CaseSensitivity::Insensitive;
    if !folds {
        eprintln!("{case}: skipped, the scratch root does not fold case");
    }
    folds
}

/// A respell of `note.md` to `Note.md`.
fn respell(before: &[u8], content: Option<&'static [u8]>) -> Transition<'static> {
    Transition::Respell {
        to: Path::new("Note.md"),
        before: hash(before),
        content,
    }
}

/// A respell changes only the ASCII case of the final name: another folder,
/// another name, the same spelling, or a case change outside ASCII is refused
/// before anything is read.
#[test]
fn a_respell_that_changes_more_than_ascii_case_is_refused() {
    let scratch = Scratch::new("respell-names");
    scratch.place("a/note.md", b"old");
    for (from, to) in [
        ("a/note.md", "b/Note.md"),
        ("a/note.md", "a/Nope.md"),
        ("a/note.md", "a/note.md"),
        ("a/note.md", "A/note.md"),
        ("a/\u{e9}.md", "a/\u{c9}.md"),
    ] {
        let refusal = scratch
            .stage(
                from,
                Transition::Respell {
                    to: Path::new(to),
                    before: hash(b"old"),
                    content: None,
                },
            )
            .expect_err("a respell that is not a case-only rename");
        assert!(
            matches!(&refusal, Refusal::InvalidRequest { .. }),
            "{from} -> {to}: {refusal}"
        );
    }
    assert!(scratch.shadow_names().is_empty());
}

/// **The bar on a root that does not fold.** A respell on a root that tells
/// the two spellings apart is refused, and nothing is staged.
///
/// There the two spellings are two names, so a rename from one to the other
/// is a move and is planned as one. The forbidden shape is a plain rename:
/// where the new spelling already names another document, it replaces it.
#[test]
fn a_respell_on_a_root_that_does_not_fold_is_refused() {
    let scratch = Scratch::new("respell-sensitive");
    if root_case(&scratch) != CaseSensitivity::Sensitive {
        eprintln!("a_respell_on_a_root_that_does_not_fold_is_refused: skipped, the root folds");
        return;
    }
    scratch.place("note.md", b"old");

    for content in [None, Some(&b"new"[..])] {
        let refusal = scratch
            .stage("note.md", respell(b"old", content))
            .expect_err("a respell on a root that does not fold");
        assert_eq!(
            refusal,
            Refusal::NotCaseFolding {
                path: scratch.at("note.md")
            }
        );
    }

    // And where both spellings are names of their own.
    scratch.place("Note.md", b"another document");
    let refusal = scratch
        .stage("note.md", respell(b"old", None))
        .expect_err("a respell onto another document");
    assert!(
        matches!(refusal, Refusal::NotCaseFolding { .. }),
        "{refusal}"
    );
    assert_eq!(bytes_at(&scratch.at("Note.md")), b"another document");
    assert!(scratch.shadow_names().is_empty());
}

/// A respell with no new content renames the file in place: the same file,
/// under the new spelling, and no shadow.
#[test]
fn a_respell_without_new_content_renames_the_same_file() {
    let scratch = Scratch::new("respell-rename");
    if !folds(
        &scratch,
        "a_respell_without_new_content_renames_the_same_file",
    ) {
        return;
    }
    let path = scratch.place("note.md", b"old");
    let before = identity_at(&path);

    let staged = staged(
        scratch
            .stage("note.md", respell(b"old", None))
            .expect("staged"),
    );
    assert!(
        scratch.shadow_names().is_empty(),
        "a rename staged a shadow"
    );
    let published = wrote(scratch.publish(staged).expect("a respell"));

    assert_eq!(names_in(&scratch.vault()), vec!["Note.md".to_string()]);
    assert_eq!(identity_at(&scratch.at("Note.md")), before);
    assert!(
        matches!(published.after, AfterState::Present(state) if state.content_hash == hash(b"old"))
    );
    assert!(published.durability.is_synced());
}

/// A respell with new content publishes it under the old spelling and then
/// renames: the new spelling holds the new content, and no shadow is left.
#[test]
fn a_respell_with_new_content_lands_it_under_the_new_spelling() {
    let scratch = Scratch::new("respell-content");
    if !folds(
        &scratch,
        "a_respell_with_new_content_lands_it_under_the_new_spelling",
    ) {
        return;
    }
    scratch.place("note.md", b"old");

    let staged = staged(
        scratch
            .stage("note.md", respell(b"old", Some(b"new")))
            .expect("staged"),
    );
    assert_eq!(scratch.shadow_names().len(), 1);
    let _ = wrote(scratch.publish(staged).expect("a respell"));

    assert_eq!(names_in(&scratch.vault()), vec!["Note.md".to_string()]);
    assert_eq!(bytes_at(&scratch.at("Note.md")), b"new");
    assert!(scratch.shadow_names().is_empty());
}

/// **The bar on a respell a crash interrupted halfway.** The old spelling at
/// the new content stages with no shadow, and publication finishes the rename.
#[test]
fn a_respell_found_halfway_finishes_the_rename() {
    let scratch = Scratch::new("respell-halfway");
    if !folds(&scratch, "a_respell_found_halfway_finishes_the_rename") {
        return;
    }
    scratch.place("note.md", b"new");

    let staged = staged(
        scratch
            .stage("note.md", respell(b"old", Some(b"new")))
            .expect("a halfway respell stages"),
    );
    assert!(
        scratch.shadow_names().is_empty(),
        "a halfway respell staged a shadow"
    );
    let _ = wrote(scratch.publish(staged).expect("the rename finishes"));

    assert_eq!(names_in(&scratch.vault()), vec!["Note.md".to_string()]);
    assert_eq!(bytes_at(&scratch.at("Note.md")), b"new");
}

/// A respell whose new spelling already holds the after-state stages as
/// landed, and the landing is confirmed.
#[test]
fn a_respell_already_landed_stages_as_landed() {
    let scratch = Scratch::new("respell-landed");
    if !folds(&scratch, "a_respell_already_landed_stages_as_landed") {
        return;
    }
    scratch.place("Note.md", b"new");

    let Staging::Landed(landed) = scratch
        .stage("note.md", respell(b"old", Some(b"new")))
        .expect("a landed respell")
    else {
        panic!("a landed respell staged");
    };
    let confirmed = confirm_landed(&scratch.vault(), &landed).expect("confirmed");
    assert!(matches!(confirmed.after, AfterState::Present(_)));
    assert!(confirmed.durability.is_synced());
}

/// A respell another writer finished between the phases is found landed.
#[test]
fn a_respell_another_writer_finished_is_found() {
    let scratch = Scratch::new("respell-found");
    if !folds(&scratch, "a_respell_another_writer_finished_is_found") {
        return;
    }
    scratch.place("note.md", b"old");
    let staged = staged(
        scratch
            .stage("note.md", respell(b"old", None))
            .expect("staged"),
    );
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: playing the foreign writer.
    std::fs::rename(scratch.at("note.md"), scratch.at("Note.md")).expect("a foreign respell");

    let _ = found(
        scratch
            .publish(staged)
            .expect("a landing another writer made"),
    );
    assert_eq!(names_in(&scratch.vault()), vec!["Note.md".to_string()]);
}

/// A respell over other bytes, or over a third spelling, is drift.
#[test]
fn a_respell_at_neither_state_is_drift() {
    let scratch = Scratch::new("respell-drift");
    if !folds(&scratch, "a_respell_at_neither_state_is_drift") {
        return;
    }
    for (spelling, content) in [("note.md", &b"theirs"[..]), ("NOTE.md", &b"old"[..])] {
        let path = scratch.place(spelling, content);
        let refusal = scratch
            .stage("note.md", respell(b"old", Some(b"new")))
            .expect_err("a respell at neither state");
        assert!(
            matches!(refusal, Refusal::Drifted { .. }),
            "{spelling}: {refusal}"
        );
        #[allow(clippy::disallowed_methods)] // Harness scaffolding: clearing the case's file.
        std::fs::remove_file(path).expect("clearing");
    }
    assert!(scratch.shadow_names().is_empty());
}

/// **A landed respell is the new spelling.** A respell another writer moved
/// back to its old spelling after staging found it landed is not confirmed.
#[test]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: playing the foreign writer.
fn a_respell_reverted_to_its_old_spelling_is_not_confirmed() {
    let scratch = Scratch::new("respell-reverted");
    if !folds(
        &scratch,
        "a_respell_reverted_to_its_old_spelling_is_not_confirmed",
    ) {
        return;
    }
    scratch.place("Note.md", b"old");
    let Staging::Landed(landed) = scratch
        .stage("note.md", respell(b"old", None))
        .expect("a landed respell")
    else {
        panic!("a landed respell staged");
    };
    std::fs::rename(scratch.at("Note.md"), scratch.at("note.md")).expect("a foreign respell back");

    let refusal = confirm_landed(&scratch.vault(), &landed).expect_err("a reverted respell");
    assert!(matches!(refusal, Refusal::Drifted { .. }), "{refusal}");
}

/// **On a root that folds, a name is the spelling its folder lists.** A
/// create whose content stands under another spelling is not landed — the
/// name is taken, and not by this create — and a replace or a remove of a
/// spelling the folder no longer lists is drift.
#[test]
fn a_target_under_another_spelling_is_not_this_target() {
    let scratch = Scratch::new("respell-other-spelling");
    if !folds(
        &scratch,
        "a_target_under_another_spelling_is_not_this_target",
    ) {
        return;
    }
    scratch.place("Note.md", b"old");

    let create = scratch
        .stage("note.md", Transition::Create { content: b"old" })
        .expect_err("a create whose content stands under another spelling");
    assert!(
        matches!(create, Refusal::DestinationExists { .. }),
        "{create}"
    );
    for transition in [
        Transition::Replace {
            before: hash(b"old"),
            content: b"new",
        },
        Transition::Remove {
            before: hash(b"old"),
        },
    ] {
        let refusal = scratch
            .stage("note.md", transition)
            .expect_err("a target the folder lists under another spelling");
        assert!(matches!(refusal, Refusal::Drifted { .. }), "{refusal}");
    }
    assert_eq!(names_in(&scratch.vault()), vec!["Note.md".to_string()]);
    assert!(scratch.shadow_names().is_empty());
}
