//! The one resolver: which documents a target names on a root, under the case
//! behaviour the root proved and the places its schema ignores, read the same
//! way by a find's `resolves` part, by a class read, and by the finding a
//! producer files about the class.

use std::sync::Arc;

use norn_store::{
    AmbiguityIgnore, CandidateFact, DeclaredFields, FindingFacts, Resolution, SnapshotReader,
    Store, StoredPathOrder,
};
use norn_wire::{FindParams, FindingKind, Pattern, Predicate, ResolutionTarget, Severity};

use crate::common::{Scratch, document, path, write_document, write_documents};

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

    /// The class the same resolution reads through the store's class read, in
    /// path order.
    fn class(&mut self, resolution: &Resolution) -> Vec<String> {
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
        let ignore = ignoring(&["archive/**"]);
        let resolution = Resolution::new("Foo", order, &ignore).expect("a suffix target");
        let class = vault.class(&resolution);
        assert_eq!(class, vault.resolves("Foo", &["archive/**"]));

        let mut request = vault.store.begin_request();
        request
            .record_finding(&FindingFacts {
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
            })
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
