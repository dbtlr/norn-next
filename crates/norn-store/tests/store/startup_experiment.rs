//! Disposable startup experiments keep partial derivation distinct from final findings.

use norn_store::{Change, ContentModel, IncrementProvenance};

use crate::common::{Scratch, path};
use crate::health::derived;

#[test]
fn deferred_startup_does_not_file_a_temporary_broken_link() {
    let scratch = Scratch::new("startup-deferred-health");
    let mut store = scratch.open();
    store
        .begin_request()
        .apply_startup_increment(
            [Change::Upsert(derived("first.md", "[[later]]"))],
            &[],
            &ContentModel::none(),
        )
        .unwrap();
    assert!(
        store
            .begin_request()
            .stored_findings(&path("first.md"))
            .unwrap()
            .is_empty(),
        "a partial import must not file a broken link before its target is loaded"
    );
}

#[test]
fn final_health_uses_all_targets_and_matches_an_ordinary_build() {
    let scratch = Scratch::new("startup-final-health");
    let ordinary_scratch = Scratch::new("startup-normal-health");
    let mut staged = scratch.open();
    let mut ordinary = ordinary_scratch.open();
    for facts in [
        derived("first.md", "[[later]] [[missing]]"),
        derived("later.md", "A target that arrives in a later changeset."),
    ] {
        staged
            .begin_request()
            .apply_startup_increment([Change::Upsert(facts.clone())], &[], &ContentModel::none())
            .unwrap();
        ordinary
            .begin_request()
            .apply_increment(
                IncrementProvenance::Derived,
                [Change::Upsert(facts)],
                &[],
                &ContentModel::none(),
            )
            .unwrap();
    }
    staged
        .begin_request()
        .finish_startup_health(&ContentModel::none())
        .unwrap();
    let staged_findings = staged
        .begin_request()
        .stored_findings(&path("first.md"))
        .unwrap();
    let ordinary_findings = ordinary
        .begin_request()
        .stored_findings(&path("first.md"))
        .unwrap();
    assert_eq!(staged_findings.len(), 1);
    assert_eq!(staged_findings[0].kind, ordinary_findings[0].kind);
    assert_eq!(staged_findings[0].target, ordinary_findings[0].target);
    assert_eq!(staged_findings[0].ordinal, ordinary_findings[0].ordinal);
    assert_eq!(
        staged_findings[0].class_keys,
        ordinary_findings[0].class_keys
    );
    assert_eq!(
        staged_findings[0].candidates,
        ordinary_findings[0].candidates
    );
    staged.verify_integrity().unwrap();
}

#[test]
fn metadata_catalogue_respects_exact_paths_and_ambiguous_slugs() {
    use norn_store::{AmbiguityIgnore, startup_experiment::Catalogue};
    let scratch = Scratch::new("startup-catalogue");
    let mut catalogue = Catalogue::open(scratch.database().parent().unwrap()).unwrap();
    let ignore = AmbiguityIgnore::none();
    for name in [
        "folder/alpha.md",
        "alpha.md",
        "one/project.md",
        "two/Project.md",
    ] {
        catalogue.record_document(&path(name), &ignore).unwrap();
    }
    assert!(catalogue.candidates("alpha", &ignore).unwrap().is_none());
    catalogue.finish_inventory().unwrap();
    let exact = catalogue.candidates("alpha", &ignore).unwrap().unwrap();
    assert_eq!(exact.paths, ["alpha.md"]);
    assert_eq!(exact.total, 1);
    let ambiguous = catalogue.candidates("project", &ignore).unwrap().unwrap();
    assert_eq!(ambiguous.total, 2);
    assert_eq!(ambiguous.paths, ["one/project.md", "two/Project.md"]);
    assert_eq!(
        catalogue
            .candidates("absent", &ignore)
            .unwrap()
            .unwrap()
            .total,
        0
    );
}

#[test]
fn catalogue_includes_vault_files_in_document_ambiguity() {
    use norn_store::{AmbiguityIgnore, startup_experiment::Catalogue};
    let scratch = Scratch::new("startup-catalogue-files");
    let mut catalogue = Catalogue::open(scratch.database().parent().unwrap()).unwrap();
    let ignore = AmbiguityIgnore::none();
    catalogue
        .record_document(&path("photo.png.md"), &ignore)
        .unwrap();
    catalogue.record_file(&path("photo.png"), &ignore).unwrap();
    catalogue.finish_inventory().unwrap();
    assert_eq!(
        catalogue
            .candidates("photo.png", &ignore)
            .unwrap()
            .unwrap()
            .total,
        2
    );
}

#[test]
fn initial_cache_restores_the_live_writer_setting_including_its_sign() {
    use norn_store::startup_experiment::WriterCacheSize;
    let scratch = norn_testkit::scratch::Scratch::under(
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")),
        "cache-live-writer",
    )
    .unwrap();
    let mut store = norn_store::Store::open(
        scratch.join("store.sqlite3"),
        norn_store::StoredPathOrder::Sensitive,
        crate::common::DERIVATION,
    )
    .unwrap();
    // A positive page count proves restoration does not assume a KiB default.
    norn_store::induced_failure::execute_out_of_band(&mut store, "PRAGMA main.cache_size=37")
        .unwrap();
    let window = store
        .begin_request()
        .start_initial_import_cache(WriterCacheSize::new(8192).unwrap(), true)
        .unwrap();
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Upsert(derived("note.md", "[[missing]]"))],
            &[],
            &ContentModel::none(),
        )
        .unwrap();
    let receipt = store
        .begin_request()
        .restore_initial_import_cache(&window)
        .unwrap();
    assert_eq!(receipt.before, 37);
    assert_eq!(receipt.during, Some(-8192));
    assert_eq!(receipt.after, Some(37));
    assert!(!receipt.reclamation_verified);
    store.verify_integrity().unwrap();
}
