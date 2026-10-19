//! The one resolver: which documents a target names under the comparison key
//! and the places its schema ignores, read the same
//! way by a find's `resolves` part, by a class read, and by the finding a
//! producer files about the class.

use std::sync::Arc;

use std::collections::BTreeSet;

use norn_store::{
    AmbiguityIgnore, CandidateFact, Change, ClassKey, ContentModel, DerivedFinding,
    ExplainedStatement, FindingFacts, IncrementProvenance, Provenance, SnapshotReader, Store,
    StoreError, StoredPathOrder, SuffixKey, TargetClass,
};
use norn_wire::{FindParams, FindingKind, Pattern, Predicate, ResolutionTarget, Severity};

use crate::common::{Scratch, document, path, record_death, write_document, write_documents};

/// The fingerprint the suite's schema is pinned under.
const SCHEMA: &str = "resolve-schema";

use StoredPathOrder::{AsciiCaseInsensitive as Folding, Sensitive};

/// A store over a root proven to have one case behaviour, holding documents at
/// `paths`, its schema pinned, and a read handle over it.
struct Vault {
    _scratch: Scratch,
    store: Store,
    reader: Arc<SnapshotReader>,
    documents: Vec<norn_store::DocumentFacts>,
}

impl Vault {
    fn holding(label: &str, order: StoredPathOrder, paths: &[&str]) -> Self {
        let scratch = Scratch::new(label);
        let mut store = Store::open(scratch.database(), order, crate::common::DERIVATION)
            .expect("opening a store");
        store
            .begin_request()
            .pin_vault_schema(SCHEMA.as_bytes(), SCHEMA)
            .expect("pinning the suite's schema");
        let documents: Vec<_> = paths
            .iter()
            .enumerate()
            .map(|(index, at)| document(at, &format!("hash-{index}"), "a body\n"))
            .collect();
        write_documents(&mut store.begin_request(), &documents);
        let reader = Arc::new(store.open_reader().reader.expect("a reader"));
        Vault {
            _scratch: scratch,
            store,
            reader,
            documents,
        }
    }

    /// The paths a find's `resolves` part keeps on this vault's root, the
    /// schema ignoring `ignored`, in path order.
    fn resolves(&mut self, target: &str, ignored: &[&str]) -> Vec<String> {
        self.resolves_beside(target, ignored, &[])
    }

    /// The paths a find keeps that has a `resolves` part for `target` beside
    /// `others`, the schema ignoring `ignored`, in path order.
    fn resolves_beside(
        &mut self,
        target: &str,
        ignored: &[&str],
        others: &[Predicate],
    ) -> Vec<String> {
        let (declared, params) = self.resolving(target, ignored, others);
        let snapshot = self.snapshot();
        let found = snapshot
            .find(&params, &declared)
            .unwrap_or_else(|refusal| panic!("a find resolving `{target}`: {refusal}"));
        assert!(found.unsatisfied.is_empty(), "{:?}", found.unsatisfied);
        found
            .rows
            .iter()
            .map(|row| row.path.as_str().to_string())
            .collect()
    }

    /// The text of every statement a find with a `resolves` part for `target`
    /// beside `others` ran, the schema ignoring `ignored`.
    fn resolving_statements(
        &mut self,
        target: &str,
        ignored: &[&str],
        others: &[Predicate],
    ) -> Vec<String> {
        let (declared, params) = self.resolving(target, ignored, others);
        self.snapshot()
            .find_plans(&params, &declared)
            .unwrap_or_else(|refusal| panic!("a find resolving `{target}`: {refusal}"))
            .into_iter()
            .map(|plan| plan.plan.sql)
            .collect()
    }

    /// The declaration ignoring `ignored`, with the vault healed under it, and
    /// the find whose parts are a `resolves` part for `target` and `others`.
    fn resolving(
        &mut self,
        target: &str,
        ignored: &[&str],
        others: &[Predicate],
    ) -> (ContentModel, FindParams) {
        let declared = ignored
            .iter()
            .fold(ContentModel::under(SCHEMA), |declared, glob| {
                declared.declare_ambiguity_ignore(Pattern::parse(glob).expect("a glob"))
            });
        self.rederive(&declared);
        let params = FindParams::new(address()).with_predicates(
            std::iter::once(Predicate::resolves(
                ResolutionTarget::new(target).expect("a target"),
            ))
            .chain(others.iter().cloned()),
        );
        (declared, params)
    }

    fn snapshot(&self) -> norn_store::Snapshot {
        let snapshot = self
            .reader
            .try_take()
            .expect("a free handle")
            .establish()
            .expect("a snapshot");
        assert_eq!(snapshot.path_order(), self.store.path_order());
        snapshot
    }

    fn rederive(&mut self, declared: &ContentModel) {
        self.store
            .begin_request()
            .apply_increment(
                IncrementProvenance::Derived,
                self.documents.iter().cloned().map(Change::Upsert),
                &[],
                declared,
            )
            .expect("healing rows under the reading's declaration");
    }

    /// `target` compiled on this vault's store, the schema ignoring `ignored`.
    fn target_class(&mut self, target: &str, ignored: &[&str]) -> TargetClass {
        let declared = ignored
            .iter()
            .fold(ContentModel::under(SCHEMA), |declared, glob| {
                declared.declare_ambiguity_ignore(Pattern::parse(glob).expect("a glob"))
            });
        self.rederive(&declared);
        self.store
            .begin_request()
            .target_class(target, &ignoring(ignored))
            .expect("a suffix target")
    }

    /// The class the same resolution reads through the store's class read, in
    /// path order.
    fn class(&mut self, resolution: &TargetClass) -> Vec<String> {
        let mut paths: Vec<String> = self
            .store
            .begin_request()
            .suffix_candidates(resolution)
            .expect("reading a class")
            .iter()
            .map(|at| at.as_str().to_string())
            .collect();
        paths.sort();
        paths
    }
}

fn address() -> norn_wire::VaultAddress {
    norn_wire::VaultAddress::name(norn_wire::VaultName::new("resolve").expect("a vault name"))
}

fn ignoring(globs: &[&str]) -> AmbiguityIgnore {
    AmbiguityIgnore::new(
        globs
            .iter()
            .map(|glob| Pattern::parse(glob).expect("a glob")),
    )
}

fn strings(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|at| at.to_string()).collect()
}

/// Case variants share one comparison class. An exact spelling does not
/// exclude another candidate whose name differs only in case.
#[test]
fn case_variants_share_one_class_without_exact_spelling_precedence() {
    let mut vault = Vault::holding(
        "resolve-folded",
        Folding,
        &["a/Foo.md", "b/foo.md", "c/food.md"],
    );
    for target in ["Foo", "foo", "FOO", "Foo#Heading"] {
        assert_eq!(
            vault.resolves(target, &[]),
            strings(&["a/Foo.md", "b/foo.md"]),
            "`{target}` on a folding root"
        );
    }
}

/// A case-sensitive filesystem still resolves case variants through the
/// shared comparison key, without exact-spelling precedence.
#[test]
fn a_case_sensitive_filesystem_still_uses_the_shared_comparison_key() {
    let mut vault = Vault::holding("resolve-comparison", Sensitive, &["a/Foo.md", "b/foo.md"]);
    for target in ["Foo", "foo", "FOO"] {
        assert_eq!(
            vault.resolves(target, &[]),
            strings(&["a/Foo.md", "b/foo.md"])
        );
    }
}

/// A target ending in `.md`, in any ASCII case, names the document and
/// its `.md.md` twin on either filesystem order.
#[test]
fn a_target_ending_in_md_names_both_the_document_and_its_md_md_twin() {
    let paths = ["archive/notes.md.md", "docs/NOTES.md", "docs/v1.md"];
    let mut folding = Vault::holding("resolve-folded-reductions", Folding, &paths);
    assert_eq!(
        folding.resolves("Notes.MD", &[]),
        strings(&["archive/notes.md.md", "docs/NOTES.md"])
    );
    assert_eq!(
        folding.resolves("NOTES.MD", &[]),
        strings(&["archive/notes.md.md", "docs/NOTES.md"])
    );
    let mut sensitive = Vault::holding("resolve-raw-reductions", Sensitive, &paths);
    assert_eq!(
        sensitive.resolves("Notes.MD", &[]),
        strings(&["archive/notes.md.md", "docs/NOTES.md"])
    );
}

/// Unicode lowercase variants share one resolution class on every root.
#[test]
fn unicode_lowercase_variants_share_one_class_on_every_root() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::holding(
            "resolve-non-ascii",
            order,
            &["a/Été.md", "b/été.md", "c/ÉTÉ.md"],
        );
        for target in ["Été", "été", "ÉtÉ"] {
            assert_eq!(
                vault.resolves(target, &[]),
                strings(&["a/Été.md", "b/été.md", "c/ÉTÉ.md"])
            );
        }
    }
}

/// **An ignored place stays out of a class unless the target names it.** With
/// `archive/**` ignored, a one-segment target passes over every archived
/// document, a longer target that does not reach `archive` passes over them
/// too, and a target that spells `archive` resolves to the one it names.
#[test]
fn an_ignored_place_resolves_only_where_the_target_names_it() {
    let archive = ["archive/**"];
    for order in [Sensitive, Folding] {
        let mut vault = Vault::holding(
            &format!("resolve-ignored-{order:?}"),
            order,
            &[
                "archive/glossary.md",
                "archive/norn/glossary.md",
                "docs/norn/glossary.md",
                "notes/glossary.md",
            ],
        );
        assert_eq!(
            vault.resolves("glossary", &archive),
            strings(&["docs/norn/glossary.md", "notes/glossary.md"]),
            "a one-segment target under {order:?}"
        );
        assert_eq!(
            vault.resolves("norn/glossary", &archive),
            strings(&["docs/norn/glossary.md"]),
            "a longer target that does not name `archive`, under {order:?}"
        );
        assert_eq!(
            vault.resolves("archive/norn/glossary", &archive),
            strings(&["archive/norn/glossary.md"]),
            "a target that names `archive`, under {order:?}"
        );
        assert_eq!(
            vault.resolves("archive/glossary", &archive),
            strings(&["archive/glossary.md"]),
            "a target that names `archive`, under {order:?}"
        );
        // With nothing ignored, the archived documents are in the class.
        assert_eq!(vault.resolves("glossary", &[]).len(), 4, "under {order:?}");
    }
}

/// **A `resolves` part another part drives is tested one document at a time,
/// and tests the class the target opens.** With `archive/**` ignored and a path
/// part driving the find, a one-segment target keeps the admitted document
/// beside it and none of the ignored ones, while a target that names `archive`
/// keeps the ignored document it names, on either root.
#[test]
fn a_resolves_part_beside_a_driving_part_judges_each_document_against_its_class() {
    let archive = ["archive/**"];
    let path_part = |glob: &str| [Predicate::path(glob)];
    for order in [Sensitive, Folding] {
        let mut vault = Vault::holding(
            &format!("resolve-beside-{order:?}"),
            order,
            &["notes/index.md", "archive/x/index.md", "other/index.md"],
        );
        let statements = vault.resolving_statements("index", &archive, &path_part("notes/**"));
        assert!(
            statements
                .iter()
                .any(|sql| sql.contains("dr.id = d.id") && sql.contains("dr.admitting_segments")),
            "the resolves part was not tested one document at a time under {order:?}: \
             {statements:?}"
        );
        assert_eq!(
            vault.resolves_beside("index", &archive, &path_part("notes/**")),
            strings(&["notes/index.md"]),
            "an admitted document the path part drives, under {order:?}"
        );
        assert_eq!(
            vault.resolves_beside("index", &archive, &path_part("other/**")),
            strings(&["other/index.md"]),
            "an admitted document the path part drives, under {order:?}"
        );
        assert_eq!(
            vault.resolves_beside("index", &archive, &path_part("archive/**")),
            Vec::<String>::new(),
            "an ignored document the target does not name, under {order:?}"
        );
        assert_eq!(
            vault.resolves_beside("archive/x/index", &archive, &path_part("archive/**")),
            strings(&["archive/x/index.md"]),
            "an ignored document the target names, under {order:?}"
        );
        assert_eq!(
            vault.resolves_beside("x/index", &archive, &path_part("archive/**")),
            Vec::<String>::new(),
            "an ignored document a target that does not spell `archive` passes over, \
             under {order:?}"
        );
        assert_eq!(
            vault.resolves_beside("archive/x/index", &archive, &path_part("notes/**")),
            Vec::<String>::new(),
            "a document outside the class the target opens, under {order:?}"
        );
    }
}

/// Ambiguity-ignore globs and paths use the comparison key. Case-variant
/// ignored places stay excluded unless the target names that place.
#[test]
fn ambiguity_ignore_globs_compare_case_variants_under_the_shared_key() {
    let mut vault = Vault::holding(
        "resolve-ignored-folded",
        Folding,
        &[
            "Archive/norn/glossary.md",
            "archive/deep/term.md",
            "docs/norn/glossary.md",
            "notes/term.md",
        ],
    );
    assert_eq!(
        vault.resolves("glossary", &["archive/**"]),
        strings(&["docs/norn/glossary.md"])
    );
    assert_eq!(
        vault.resolves("norn/glossary", &["archive/**"]),
        strings(&["docs/norn/glossary.md"])
    );
    assert_eq!(
        vault.resolves("term", &["Archive/**"]),
        strings(&["notes/term.md"])
    );
    for target in [
        "ARCHIVE/deep/term",
        "archive/deep/term",
        "Archive/Deep/Term",
    ] {
        assert_eq!(
            vault.resolves(target, &["Archive/**"]),
            strings(&["archive/deep/term.md"]),
            "`{target}`"
        );
    }
    assert_eq!(
        vault.resolves("archive/norn/glossary", &["archive/**"]),
        strings(&["Archive/norn/glossary.md"])
    );

    let class = vault.target_class("glossary", &["archive/**"]);
    assert!(!class.admits("Archive/norn/glossary.md"));
    assert_eq!(vault.class(&class), strings(&["docs/norn/glossary.md"]));
    let class = vault.target_class("term", &["Archive/**"]);
    assert!(!class.admits("archive/deep/term.md"));
    assert_eq!(vault.class(&class), strings(&["notes/term.md"]));
}

/// Case-sensitive filesystems use the same comparison key for ignore
/// globs and candidate paths, including differently cased places.
#[test]
fn ambiguity_ignore_globs_use_the_shared_key_on_case_sensitive_filesystems() {
    let mut vault = Vault::holding(
        "resolve-ignore-sensitive",
        Sensitive,
        &[
            "Archive/norn/glossary.md",
            "archive/deep/term.md",
            "docs/norn/glossary.md",
            "notes/term.md",
        ],
    );
    assert_eq!(
        vault.resolves("glossary", &["archive/**"]),
        strings(&["docs/norn/glossary.md"])
    );
    assert_eq!(
        vault.resolves("term", &["Archive/**"]),
        strings(&["notes/term.md"])
    );
    let class = vault.target_class("glossary", &["archive/**"]);
    assert!(!class.admits("Archive/norn/glossary.md"));
    assert_eq!(vault.class(&class), strings(&["docs/norn/glossary.md"]));
    let class = vault.target_class("term", &["Archive/**"]);
    assert!(!class.admits("archive/deep/term.md"));
    assert_eq!(vault.class(&class), strings(&["notes/term.md"]));
}

/// Ignore globs compare Unicode lowercase variants under the shared key.
#[test]
fn ambiguity_ignore_globs_compare_unicode_lowercase_variants() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::holding(
            "resolve-ignore-unicode",
            order,
            &["Été/x/term.md", "été/x/term.md"],
        );
        for glob in ["Été/**", "ÉTé/**", "été/**"] {
            assert!(vault.resolves("term", &[glob]).is_empty());
            assert_eq!(vault.resolves("été/x/term", &[glob]).len(), 2);
        }
    }
}

/// **A glob naming a leaf keeps a document out of a one-segment target's class
/// alone.** `attachments/*` ignores the document itself rather than a
/// directory, so a longer target naming it resolves, and a bare name does not.
#[test]
fn an_ignored_leaf_resolves_to_any_target_longer_than_its_name() {
    let mut vault = Vault::holding(
        "resolve-ignored-leaf",
        Sensitive,
        &["attachments/image.md", "notes/image.md"],
    );
    let ignored = ["attachments/*"];
    assert_eq!(
        vault.resolves("image", &ignored),
        strings(&["notes/image.md"])
    );
    assert_eq!(
        vault.resolves("attachments/image", &ignored),
        strings(&["attachments/image.md"])
    );
}

/// **A document at the root that the schema ignores is reached by a target
/// naming it.** With `glossary.md` ignored, the place is the document itself,
/// and `glossary` and `glossary.md` each name that place whole; it stands at
/// their exact vault path, so each resolves to it alone, ahead of the deeper
/// documents no glob ignores (ADR 0040).
#[test]
fn an_ignored_root_level_document_resolves_to_a_target_naming_it() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::holding(
            &format!("resolve-ignored-root-{order:?}"),
            order,
            &["docs/glossary.md", "glossary.md"],
        );
        for target in ["glossary", "glossary.md"] {
            assert_eq!(
                vault.resolves(target, &["glossary.md"]),
                strings(&["glossary.md"]),
                "`{target}` under {order:?}"
            );
        }
    }
}

/// **The exact vault path comes first, after the ambiguity-ignore globs
/// drop** (ADR 0040). A target names the document at its exact path alone,
/// however many deeper documents it is a suffix of, and every one of them
/// only where no document stands there. With `archive/**` ignored, `twin`
/// passes over the archived copy to `notes/twin.md`, `archive/twin` names the
/// archived copy, and `archive/x` names `archive/x.md` ahead of
/// `deep/archive/x.md`. Comparison twins at the exact path stay ambiguous.
#[test]
fn the_exact_vault_path_comes_first_after_the_ignore_globs() {
    let archive = ["archive/**"];
    for order in [Sensitive, Folding] {
        let mut vault = Vault::holding(
            &format!("resolve-exact-first-{order:?}"),
            order,
            &[
                "a/s/t/exact.md",
                "shadow/a/s/t/exact.md",
                "archive/twin.md",
                "notes/twin.md",
                "archive/x.md",
                "deep/archive/x.md",
            ],
        );
        for (target, named) in [
            ("a/s/t/exact", &["a/s/t/exact.md"][..]),
            ("a/s/t/exact.md", &["a/s/t/exact.md"]),
            ("s/t/exact", &["a/s/t/exact.md", "shadow/a/s/t/exact.md"]),
            ("twin", &["notes/twin.md"]),
            ("archive/twin", &["archive/twin.md"]),
            ("archive/x", &["archive/x.md"]),
            ("x", &["deep/archive/x.md"]),
        ] {
            assert_eq!(
                vault.resolves(target, &archive),
                strings(named),
                "`{target}` under {order:?}"
            );
        }
    }
    let mut twins = Vault::holding(
        "resolve-exact-twins",
        Sensitive,
        &["c/Y.md", "c/y.md", "deep/c/y.md"],
    );
    assert_eq!(
        twins.resolves("c/y", &[]),
        strings(&["c/Y.md", "c/y.md"]),
        "comparison twins at the exact path"
    );
}

/// Findings and resolution record the same comparison class on every root.
#[test]
fn findings_and_resolution_share_one_comparison_class_on_every_root() {
    for (order, joining) in [(Sensitive, "c/Foo.md"), (Folding, "c/FOO.md")] {
        let mut vault = Vault::holding(
            &format!("resolve-finding-{order:?}"),
            order,
            &["a/Foo.md", "b/foo.md", "archive/foo.md"],
        );
        let resolution = vault.target_class("Foo", &["archive/**"]);
        let class = vault.class(&resolution);
        assert_eq!(class, vault.resolves("Foo", &["archive/**"]));

        let mut request = vault.store.begin_request();
        request
            .record_finding(&finding_about_foo(&resolution, &class))
            .expect("recording the finding");
        let recorded = request
            .findings_in_class(resolution.probe())
            .expect("reading the class's findings");
        assert_eq!(recorded.len(), 1, "under {order:?}");
        let filed: Vec<String> = recorded[0]
            .candidates
            .iter()
            .map(|candidate| candidate.path.as_str().to_string())
            .collect();
        assert_eq!(filed, class, "under {order:?}");

        // A document joining the class under a spelling the root resolves to
        // it takes the finding for re-derivation.
        let joined = write_document(&mut request, &document(joining, "hash-j", "a body\n"));
        assert_eq!(
            joined.invalidated.findings_discarded, 1,
            "`{joining}` under {order:?}"
        );
        assert!(
            request
                .findings_in_class(resolution.probe())
                .expect("reading the class's findings")
                .is_empty()
        );
    }
}

/// The finding a producer files about the target `Foo`, whose class on the
/// root is `class`: under the resolution's class keys, with the class as its
/// candidates.
fn finding_about_foo(resolution: &TargetClass, class: &[String]) -> FindingFacts {
    FindingFacts {
        kind: FindingKind::PathNamesNoDocument,
        severity: Severity::Warning,
        path: path("note.md"),
        class_keys: resolution.class_keys(),
        path_keys: BTreeSet::new(),
        target: Some("Foo".to_string()),
        span: None,
        ordinal: None,
        candidates: class
            .iter()
            .map(|at| CandidateFact {
                path: path(at),
                suffix: at.clone(),
            })
            .collect(),
        candidates_total: class.len() as u64,
        message: "`Foo` names more than one document".to_string(),
        detail: None,
        rules: BTreeSet::new(),
        value: None,
    }
}

/// A changed document reaches findings filed under its comparison class.
#[test]
fn a_document_leaving_a_class_takes_the_findings_filed_under_it() {
    for (order, leaving) in [(Sensitive, "b/Foo.md"), (Folding, "b/FOO.md")] {
        let mut vault = Vault::holding(
            &format!("resolve-leaving-{order:?}"),
            order,
            &["a/Foo.md", leaving],
        );
        let resolution = vault.target_class("Foo", &[]);
        let class = vault.class(&resolution);
        assert_eq!(class, strings(&["a/Foo.md", leaving]), "under {order:?}");

        let mut request = vault.store.begin_request();
        request
            .record_finding(&finding_about_foo(&resolution, &class))
            .expect("recording the finding");
        let left = record_death(&mut request, &path(leaving), Provenance::WatcherRemoval);
        assert_eq!(
            left.invalidated.findings_discarded, 1,
            "`{leaving}` left the class of `Foo` under {order:?} and its finding stood"
        );
        assert!(
            request
                .findings_in_class(resolution.probe())
                .expect("reading the class's findings")
                .is_empty()
        );
        assert_eq!(
            left.affected_classes,
            BTreeSet::from([request.class_key_of(&path(leaving))]),
            "the classes a change names under {order:?}"
        );
    }
}

/// Candidate ladders order by the normalized suffix key, then original path spelling.
#[test]
fn a_class_read_orders_its_candidates_by_the_probed_key_then_path() {
    let paths = [
        "b/x/Foo.md",
        "a/y/foo.md",
        "c/x/FOO.md",
        "a/x/foo.md",
        "a/x/Foo.md",
    ];
    let mut vault = Vault::holding("resolve-class-order", Folding, &paths);
    let class = vault.target_class("foo", &[]);
    let read: Vec<String> = vault
        .store
        .begin_request()
        .suffix_candidates(&class)
        .expect("reading a class")
        .iter()
        .map(|at| at.as_str().to_string())
        .collect();

    let ordered_by = |key: fn(&norn_store::DocumentPath) -> String| {
        let mut ordered = strings(&paths);
        ordered.sort_by_key(|at| (key(&path(at)), at.clone()));
        ordered
    };
    let expected = ordered_by(|at| at.suffix_key().to_string());
    let by_raw_key = ordered_by(|at| {
        let mut segments: Vec<_> = at.as_str().split('/').collect();
        let leaf = segments.pop().expect("a leaf");
        format!(
            "{}/{}/",
            leaf.trim_end_matches(".md"),
            segments.into_iter().rev().collect::<Vec<_>>().join("/")
        )
    });
    let mut by_path = strings(&paths);
    by_path.sort();
    assert_ne!(
        expected, by_raw_key,
        "the fixture does not tell the keys apart"
    );
    assert_ne!(
        expected, by_path,
        "the fixture does not tell the key from the path"
    );
    assert_eq!(read, expected);
}

/// The finding a producer files about `Foo` under `class_keys`, with no
/// candidate head.
fn finding_filed_under(class_keys: &[&str]) -> FindingFacts {
    FindingFacts {
        kind: FindingKind::PathNamesNoDocument,
        severity: Severity::Warning,
        path: path("note.md"),
        class_keys: class_keys
            .iter()
            .map(|key| ClassKey::new(key).expect("a class key"))
            .collect(),
        path_keys: BTreeSet::new(),
        target: Some("Foo".to_string()),
        span: None,
        ordinal: None,
        candidates: Vec::new(),
        candidates_total: 0,
        message: "`Foo` names more than one document".to_string(),
        detail: None,
        rules: BTreeSet::new(),
        value: None,
    }
}

/// A compiled class reads identically across filesystem orders.
#[test]
fn a_class_compiled_under_another_order_reads_the_same_candidates() {
    let held = ["a/Foo.md", "b/foo.md"];
    let mut sensitive = Vault::holding("resolve-foreign-class", Sensitive, &held);
    let mut folding = Vault::holding("resolve-foreign-class-folded", Folding, &held);
    let foreign = folding.target_class("Foo", &[]);
    assert_eq!(folding.class(&foreign), strings(&held));
    assert_eq!(sensitive.class(&foreign), strings(&held));
    sensitive
        .store
        .begin_request()
        .emitted_plan(ExplainedStatement::SuffixCandidates(&foreign))
        .expect("a portable class plan");
    let own = sensitive.target_class("Foo", &[]);
    assert_eq!(sensitive.class(&own), strings(&held));
}

/// Class probes reach the same normalized findings across filesystem orders.
#[test]
fn class_probes_reach_normalized_findings_across_filesystem_orders() {
    let mut vault = Vault::holding("resolve-class-probe", Folding, &["a/Foo.md", "b/foo.md"]);
    let mut request = vault.store.begin_request();
    request
        .record_finding(&finding_filed_under(&["foo/"]))
        .expect("recording the finding");
    let probe = request.class_probe("Foo").expect("a class stem");
    assert_eq!(probe.key(), SuffixKey::Comparison);
    let found = request
        .findings_in_class(&probe)
        .expect("reading the class");
    assert_eq!(
        found.len(),
        1,
        "the class probe for `Foo` missed the finding"
    );

    let raw = norn_store::suffix_probe("Foo").expect("a suffix target");
    assert_eq!(raw.key(), SuffixKey::Comparison);
    for refused in [
        request.findings_in_class(&raw).map(|_| ()),
        request
            .emitted_plan(ExplainedStatement::FindingsInClass(&raw))
            .map(|_| ()),
    ] {
        assert!(
            refused.is_ok(),
            "the normalized probe reads across stores: {refused:?}"
        );
    }
    assert_eq!(
        request
            .findings_in_class(&probe)
            .expect("reading the class")
            .len(),
        1,
        "a refused read took the finding"
    );
}

/// Every root refuses unnormalized finding class keys, which changed
/// documents could never reach through comparison-key maintenance.
#[test]
fn findings_with_unnormalized_class_keys_are_refused_on_every_root() {
    let mut vault = Vault::holding("resolve-foreign-key", Folding, &["a/Foo.md"]);
    let mut request = vault.store.begin_request();
    let filed = finding_filed_under(&["Foo/"]);
    let refused = request.record_finding(&filed);
    assert!(
        matches!(refused, Err(StoreError::KeySpace { .. })),
        "a folding store filed a finding under `Foo/`: {refused:?}"
    );
    let refused = request.apply_increment(
        IncrementProvenance::Derived,
        [Change::Upsert(document("b/FOO.md", "hash-b", "a body\n"))],
        &[DerivedFinding {
            facts: filed,
            replaces: None,
            bytes_hash: None,
        }],
        &norn_store::ContentModel::none(),
    );
    assert!(
        matches!(refused, Err(StoreError::KeySpace { .. })),
        "a folding store's changeset filed a finding under `Foo/`: {refused:?}"
    );
    assert!(
        request
            .stored_findings(&path("note.md"))
            .expect("reading findings")
            .is_empty(),
        "a refused finding is at rest"
    );

    let mut sensitive = Vault::holding("resolve-foreign-key-raw", Sensitive, &["a/Foo.md"]);
    sensitive
        .store
        .begin_request()
        .record_finding(&finding_filed_under(&["Foo/", "foo/"]))
        .expect_err("a sensitive store also refuses an unnormalized class key");
}

/// Unicode case and canonical spellings name the same documents on every root.
#[test]
fn unicode_spellings_share_one_comparison_key_on_every_root() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::holding(
            "resolve-unicode-comparison",
            order,
            &["n422-Éclair.md", "résumé.md", "decomposed/cafe\u{301}.md"],
        );
        for (target, expected) in [
            ("n422-éclair", "n422-Éclair.md"),
            ("re\u{301}sume\u{301}", "résumé.md"),
            ("CAFÉ", "decomposed/cafe\u{301}.md"),
        ] {
            assert_eq!(
                vault.resolves(target, &[]),
                strings(&[expected]),
                "{order:?}: {target}"
            );
        }
    }
}

/// Only the Markdown extension is optional, including on dotted document stems.
#[test]
fn only_md_is_optional_in_a_target() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::holding(
            "resolve-md-only",
            order,
            &["photo.md", "v1.md", "v1.2.md", "note.md", "note.md.md"],
        );
        assert!(vault.resolves("photo.png", &[]).is_empty());
        assert_eq!(vault.resolves("v1.2", &[]), strings(&["v1.2.md"]));
        assert_eq!(
            vault.resolves("NOTE.MD", &[]),
            strings(&["note.md", "note.md.md"])
        );
        let mut with_photo =
            Vault::holding("resolve-photo-stem", order, &["photo.md", "photo.png.md"]);
        assert_eq!(
            with_photo.resolves("photo.png", &[]),
            strings(&["photo.png.md"])
        );
    }
}

/// Ignore globs and candidate paths use the same comparison key.
#[test]
fn ambiguity_ignore_matches_case_and_canonical_variants_on_every_root() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::holding(
            "resolve-unicode-ignore",
            order,
            &["ARCHIVE/re\u{301}sume\u{301}/note.md", "notes/note.md"],
        );
        assert_eq!(
            vault.resolves("note", &["archive/RÉSUMÉ/**"]),
            strings(&["notes/note.md"])
        );
        assert_eq!(
            vault.resolves("archive/résumé/note", &["archive/RÉSUMÉ/**"]),
            strings(&["ARCHIVE/re\u{301}sume\u{301}/note.md"])
        );
    }
}

/// Contextual lowercase cannot split documents sharing a complete path key.
#[test]
fn contextual_lowercase_twins_share_the_suffix_class() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::holding("resolve-contextual-lowercase", order, &["ΟΣ.md", "Οσ.md"]);
        for target in ["ΟΣ", "Οσ", "ΟΣ.md", "Οσ.md"] {
            assert_eq!(vault.resolves(target, &[]), strings(&["ΟΣ.md", "Οσ.md"]));
        }
    }
}
