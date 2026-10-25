//! The first-index name catalogue answers exact paths, ambiguity and order as a read resolves them.

use crate::common::{Scratch, path};

#[test]
fn metadata_catalogue_respects_exact_paths_and_ambiguous_slugs() {
    use norn_store::{AmbiguityIgnore, loading::Catalogue};
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
    use norn_store::{AmbiguityIgnore, loading::Catalogue};
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
fn catalogue_answers_unchanged_after_many_rows_cross_pages() {
    use norn_store::{AmbiguityIgnore, loading::Catalogue};
    let scratch = Scratch::new("startup-catalogue-pages");
    let mut catalogue = Catalogue::open(scratch.database().parent().unwrap()).unwrap();
    let ignore = AmbiguityIgnore::none();
    // Enough rows to overflow the catalogue's page cache several times over.
    for number in 0..20_000 {
        catalogue
            .record_document(&path(&format!("fill/{number:05}.md")), &ignore)
            .unwrap();
    }
    for directory in 0..7 {
        let name = if directory % 2 == 0 { "Same" } else { "same" };
        catalogue
            .record_document(&path(&format!("d{directory}/{name}.md")), &ignore)
            .unwrap();
    }
    catalogue
        .record_document(&path("00042.md"), &ignore)
        .unwrap();
    catalogue
        .record_document(&path("only/Unique.md"), &ignore)
        .unwrap();
    catalogue
        .record_file(&path("assets/Unique.md.png"), &ignore)
        .unwrap();
    assert!(catalogue.candidates("same", &ignore).unwrap().is_none());
    catalogue.finish_inventory().unwrap();

    let ambiguous = catalogue.candidates("SAME", &ignore).unwrap().unwrap();
    assert_eq!(ambiguous.total, 7);
    assert_eq!(
        ambiguous.paths,
        [
            "d0/Same.md",
            "d1/same.md",
            "d2/Same.md",
            "d3/same.md",
            "d4/Same.md"
        ]
    );
    let exact = catalogue.candidates("00042", &ignore).unwrap().unwrap();
    assert_eq!(
        (exact.total, exact.paths),
        (1, vec!["00042.md".to_string()])
    );
    let suffix = catalogue
        .candidates("fill/19999", &ignore)
        .unwrap()
        .unwrap();
    assert_eq!(
        (suffix.total, suffix.paths),
        (1, vec!["fill/19999.md".to_string()])
    );
    let unique = catalogue.candidates("unique", &ignore).unwrap().unwrap();
    assert_eq!(unique.total, 1);
    assert_eq!(unique.paths, ["only/Unique.md"]);
}

#[test]
fn catalogue_refuses_a_second_record_of_the_same_path() {
    use norn_store::{AmbiguityIgnore, loading::Catalogue};
    let scratch = Scratch::new("startup-catalogue-duplicate");
    let mut catalogue = Catalogue::open(scratch.database().parent().unwrap()).unwrap();
    let ignore = AmbiguityIgnore::none();
    catalogue
        .record_document(&path("a/note.md"), &ignore)
        .unwrap();
    assert!(
        catalogue
            .record_document(&path("a/note.md"), &ignore)
            .is_err()
    );
}
