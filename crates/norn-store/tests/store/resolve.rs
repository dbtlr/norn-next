//! The one resolver: which documents a target names on a root, under the case
//! behaviour the root proved and the places its schema ignores, read the same
//! way by a find's `resolves` part, by a class read, and by the finding a
//! producer files about the class.

use std::sync::Arc;

use std::collections::BTreeSet;

use norn_store::{
    AmbiguityIgnore, CandidateFact, Change, ClassKey, DeclaredFields, DerivedFinding,
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
}

impl Vault {
    fn holding(label: &str, order: StoredPathOrder, paths: &[&str]) -> Self {
        let scratch = Scratch::new(label);
        let mut store = Store::open(scratch.database(), order).expect("opening a store");
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
        }
    }

    /// The paths a find's `resolves` part keeps on this vault's root, the
    /// schema ignoring `ignored`, in path order.
    fn resolves(&self, target: &str, ignored: &[&str]) -> Vec<String> {
        let declared = DeclaredFields::under(SCHEMA).ignoring_ambiguity(ignoring(ignored));
        let params = FindParams::new(address()).with_predicates([Predicate::resolves(
            ResolutionTarget::new(target).expect("a target"),
        )]);
        let snapshot = self
            .reader
            .try_take()
            .expect("a free handle")
            .establish()
            .snapshot
            .expect("a snapshot");
        assert_eq!(snapshot.path_order(), self.store.path_order());
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

    /// `target` compiled on this vault's store, the schema ignoring `ignored`.
    fn target_class(&mut self, target: &str, ignored: &[&str]) -> TargetClass {
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

/// **On a root that folds ASCII case, every document whose folded key the
/// target opens is one class, and the spelling that matches exactly takes no
/// precedence.** The root itself does not tell `Foo` from `foo`.
#[test]
fn a_folding_root_resolves_every_case_spelling_as_one_class() {
    let vault = Vault::holding(
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

/// **On a root that tells spellings apart, the folded key is never consulted.**
/// `Foo` names the document spelled `Foo` and nothing else.
#[test]
fn a_sensitive_root_resolves_the_raw_spelling_alone() {
    let vault = Vault::holding("resolve-raw", Sensitive, &["a/Foo.md", "b/foo.md"]);
    assert_eq!(vault.resolves("Foo", &[]), strings(&["a/Foo.md"]));
    assert_eq!(vault.resolves("foo", &[]), strings(&["b/foo.md"]));
    assert!(vault.resolves("FOO", &[]).is_empty());
}

/// **A dotted leaf reduces both ways under the fold too.** `Notes.TAR` opens
/// the folded `notes.tar/` and `notes/`, so a document stored as
/// `notes.tar.gz` and one stored as `NOTES.md` are both in its class.
#[test]
fn a_folding_root_reduces_a_dotted_target_both_ways() {
    let paths = ["archive/notes.tar.gz", "docs/NOTES.md", "docs/v1.md"];
    let folding = Vault::holding("resolve-folded-reductions", Folding, &paths);
    assert_eq!(
        folding.resolves("Notes.TAR", &[]),
        strings(&["archive/notes.tar.gz", "docs/NOTES.md"])
    );
    assert_eq!(
        folding.resolves("NOTES.MD", &[]),
        strings(&["docs/NOTES.md"])
    );
    let sensitive = Vault::holding("resolve-raw-reductions", Sensitive, &paths);
    assert!(sensitive.resolves("Notes.TAR", &[]).is_empty());
}

/// **The fold is ASCII alone.** A letter outside ASCII keeps its case on a
/// folding root, so `Été` and `été` stay two classes there.
#[test]
fn a_folding_root_does_not_fold_a_letter_outside_ascii() {
    let vault = Vault::holding(
        "resolve-non-ascii",
        Folding,
        &["a/Été.md", "b/été.md", "c/ÉTÉ.md"],
    );
    assert_eq!(vault.resolves("Été", &[]), strings(&["a/Été.md"]));
    assert_eq!(vault.resolves("été", &[]), strings(&["b/été.md"]));
    assert_eq!(vault.resolves("ÉtÉ", &[]), strings(&["c/ÉTÉ.md"]));
}

/// **An ignored place stays out of a class unless the target names it.** With
/// `archive/**` ignored, a one-segment target passes over every archived
/// document, a longer target that does not reach `archive` passes over them
/// too, and a target that spells `archive` resolves to the one it names.
#[test]
fn an_ignored_place_resolves_only_where_the_target_names_it() {
    let archive = ["archive/**"];
    for order in [Sensitive, Folding] {
        let vault = Vault::holding(
            &format!("resolve-ignored-{order:?}"),
            order,
            &[
                "archive/glossary.md",
                "archive/norn/glossary.md",
                "docs/norn/glossary.md",
                "glossary.md",
            ],
        );
        assert_eq!(
            vault.resolves("glossary", &archive),
            strings(&["docs/norn/glossary.md", "glossary.md"]),
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

/// **On a root that folds ASCII case, an ignore glob matches with ASCII case
/// folded.** The root does not tell `archive` from `Archive`, so `archive/**`
/// keeps `Archive/norn/glossary.md` out of `glossary`'s class and `Archive/**`
/// keeps `archive/deep/term.md` out of `term`'s, in a find's `resolves` part
/// and in the class read alike; a target reaching the ignored place in any
/// case still resolves to it.
#[test]
fn a_folding_root_matches_an_ignore_glob_with_ascii_case_folded() {
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

/// **On a root that tells spellings apart, an ignore glob matches bytes.**
/// `archive/**` does not name `Archive/norn/glossary.md` there, nor
/// `Archive/**` `archive/deep/term.md`, so each stays in its class.
#[test]
fn a_sensitive_root_matches_an_ignore_glob_bytewise() {
    let mut vault = Vault::holding(
        "resolve-ignored-raw",
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
        strings(&["Archive/norn/glossary.md", "docs/norn/glossary.md"])
    );
    assert_eq!(
        vault.resolves("term", &["Archive/**"]),
        strings(&["archive/deep/term.md", "notes/term.md"])
    );
    // The spelling the glob does name is still ignored.
    assert_eq!(
        vault.resolves("term", &["archive/**"]),
        strings(&["notes/term.md"])
    );

    let class = vault.target_class("glossary", &["archive/**"]);
    assert!(class.admits("Archive/norn/glossary.md"));
    assert_eq!(
        vault.class(&class),
        strings(&["Archive/norn/glossary.md", "docs/norn/glossary.md"])
    );
    let class = vault.target_class("term", &["Archive/**"]);
    assert!(class.admits("archive/deep/term.md"));
    assert_eq!(
        vault.class(&class),
        strings(&["archive/deep/term.md", "notes/term.md"])
    );
}

/// **An ignore glob folds no letter outside ASCII.** On a folding root
/// `Été/**` does not name `été/x/term.md`, while `ÉTé/**` names
/// `Été/x/term.md`, because only the ASCII `T` differs.
#[test]
fn an_ignore_glob_never_folds_a_letter_outside_ascii() {
    let vault = Vault::holding(
        "resolve-ignored-non-ascii",
        Folding,
        &["Été/x/term.md", "été/x/term.md"],
    );
    assert_eq!(
        vault.resolves("term", &["Été/**"]),
        strings(&["été/x/term.md"])
    );
    assert_eq!(
        vault.resolves("term", &["ÉTé/**"]),
        strings(&["été/x/term.md"])
    );
    assert_eq!(
        vault.resolves("term", &["ÉTÉ/**"]),
        strings(&["Été/x/term.md", "été/x/term.md"])
    );
}

/// **A glob naming a leaf keeps a document out of a one-segment target's class
/// alone.** `attachments/*` ignores the document itself rather than a
/// directory, so a longer target naming it resolves, and a bare name does not.
#[test]
fn an_ignored_leaf_resolves_to_any_target_longer_than_its_name() {
    let vault = Vault::holding(
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
/// and `glossary` and `glossary.md` each name that place whole, so each
/// resolves to it beside the documents no glob ignores.
#[test]
fn an_ignored_root_level_document_resolves_to_a_target_naming_it() {
    for order in [Sensitive, Folding] {
        let vault = Vault::holding(
            &format!("resolve-ignored-root-{order:?}"),
            order,
            &["docs/glossary.md", "glossary.md"],
        );
        for target in ["glossary", "glossary.md"] {
            assert_eq!(
                vault.resolves(target, &["glossary.md"]),
                strings(&["docs/glossary.md", "glossary.md"]),
                "`{target}` under {order:?}"
            );
        }
    }
}

/// **A finding's class is the class a find's `resolves` part reads on the same
/// root.** A producer compiles the target once, reads its class through the
/// class read, and files the finding under the resolution's class keys; the
/// candidates it files are the rows the find keeps, and a document joining the
/// class under any spelling the root folds together reaches the finding.
#[test]
fn a_findings_class_is_the_class_resolves_reads_on_that_root() {
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
        target: Some("Foo".to_string()),
        span: None,
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
    }
}

/// **A document leaving a class takes the findings filed under it, and a
/// change names its class in the key space the store's root probes alone.**
/// On a root that folds ASCII case, `b/FOO.md` is in the class of `Foo`, and
/// a finding about `Foo` is filed under the folded key: the death of
/// `b/FOO.md` reaches it there. The raw key `FOO/` is one no finding in this
/// store is filed under, so the change does not name it.
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

/// **A class read hands its candidates back in the probed key's order, then
/// by path.** On a root that folds ASCII case the probed key is the folded
/// one, and its order is neither the raw key's nor the paths' own, so a read
/// ordered by either hands the class back in another order. Two spellings of
/// one folded key fall back on their paths.
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
    let expected = ordered_by(|at| at.folded_suffix_key().to_string());
    let by_raw_key = ordered_by(|at| at.suffix_key().to_string());
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
        target: Some("Foo".to_string()),
        span: None,
        candidates: Vec::new(),
        candidates_total: 0,
        message: "`Foo` names more than one document".to_string(),
        detail: None,
    }
}

/// **A class is read only under the order it was compiled under, which is
/// its store's.** A class compiled on a store whose root folds ASCII case
/// reads the folded key; handed to a store whose root tells spellings apart,
/// it would serve `a/Foo.md` and `b/foo.md` as one class that root keeps
/// apart, so the read and its plan are refused. The class that store compiles
/// itself holds `a/Foo.md` alone.
#[test]
fn a_class_compiled_under_another_order_is_refused() {
    let held = ["a/Foo.md", "b/foo.md"];
    let mut sensitive = Vault::holding("resolve-foreign-class", Sensitive, &held);
    let mut folding = Vault::holding("resolve-foreign-class-folded", Folding, &held);
    let foreign = folding.target_class("Foo", &[]);
    assert_eq!(folding.class(&foreign), strings(&held));

    {
        let request = sensitive.store.begin_request();
        for refused in [
            request.suffix_candidates(&foreign).map(|_| ()),
            request
                .emitted_plan(ExplainedStatement::SuffixCandidates(&foreign))
                .map(|_| ()),
        ] {
            assert!(
                matches!(
                    refused,
                    Err(StoreError::KeySpace {
                        order: Sensitive,
                        ..
                    })
                ),
                "a sensitive store read a folded class: {refused:?}"
            );
        }
    }
    let own = sensitive.target_class("Foo", &[]);
    assert_eq!(sensitive.class(&own), strings(&["a/Foo.md"]));
}

/// **A class probe for findings ranges over the store's own key space.** On a
/// folding root a finding about `Foo` is filed under the folded key `foo/`, so
/// the class probe the store builds for the stem `Foo` reaches it, and a probe
/// over the raw key — which ranges over `Foo/` and would miss it — is refused
/// by the read, the discard and the plan alike.
#[test]
fn a_class_probe_reaches_the_findings_filed_in_the_store_s_key_space() {
    let mut vault = Vault::holding("resolve-class-probe", Folding, &["a/Foo.md", "b/foo.md"]);
    let mut request = vault.store.begin_request();
    request
        .record_finding(&finding_filed_under(&["foo/"]))
        .expect("recording the finding");
    let probe = request.class_probe("Foo").expect("a class stem");
    assert_eq!(probe.key(), SuffixKey::Folded);
    let found = request
        .findings_in_class(&probe)
        .expect("reading the class");
    assert_eq!(
        found.len(),
        1,
        "the class probe for `Foo` missed the finding"
    );

    let raw = norn_store::suffix_probe("Foo").expect("a suffix target");
    assert_eq!(raw.key(), SuffixKey::Raw);
    for refused in [
        request.findings_in_class(&raw).map(|_| ()),
        request
            .emitted_plan(ExplainedStatement::FindingsInClass(&raw))
            .map(|_| ()),
        request
            .emitted_plan(ExplainedStatement::ClassDiscard(&raw))
            .map(|_| ()),
        request.discard_findings_in_class(&raw).map(|_| ()),
    ] {
        assert!(
            matches!(refused, Err(StoreError::KeySpace { order: Folding, .. })),
            "a folding store ranged over the raw key: {refused:?}"
        );
    }
    assert_eq!(
        request
            .findings_in_class(&probe)
            .expect("reading the class")
            .len(),
        1,
        "a refused discard took the finding"
    );
}

/// **A finding filed under a class key outside the store's key space is
/// refused**, through the door for a lone finding and inside a changeset
/// alike. On a folding root `Foo/` is a raw key no change there names, so a
/// finding filed under it would stand through `b/FOO.md` joining the class of
/// `Foo`. A sensitive root's key space holds every class key.
#[test]
fn a_finding_filed_outside_the_store_s_key_space_is_refused() {
    let mut vault = Vault::holding("resolve-foreign-key", Folding, &["a/Foo.md"]);
    let mut request = vault.store.begin_request();
    let filed = finding_filed_under(&["Foo/"]);
    let refused = request.record_finding(&filed);
    assert!(
        matches!(refused, Err(StoreError::KeySpace { order: Folding, .. })),
        "a folding store filed a finding under `Foo/`: {refused:?}"
    );
    let refused = request.apply_increment(
        IncrementProvenance::Derived,
        [Change::Upsert(document("b/FOO.md", "hash-b", "a body\n"))],
        &[DerivedFinding {
            facts: filed,
            replaces: None,
        }],
    );
    assert!(
        matches!(refused, Err(StoreError::KeySpace { order: Folding, .. })),
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
        .expect("a sensitive store files under any class key");
}
