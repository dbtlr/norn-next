//! Link health re-decided inside the changeset: after every entry of a
//! changeset is written, the store judges the links the changeset reaches —
//! the links its written documents hold, the suffix-addressed links whose keys
//! fall in a changed path's class, and the path-addressed links spelling a
//! changed path — and files their findings in the same transaction.
//!
//! The documents here are derived from Markdown through the text layer, as the
//! host derives them, so a link, a heading and a block reach the store with the
//! readings a vault gives them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use norn_store::{
    Change, ContentModel, DerivationCounters, DerivedFinding, FindingFacts, IncrementProvenance,
    OpenOutcome, Provenance, Store, StoreError, StoredPathOrder,
};
use norn_testkit::equivalence::{DerivedRows, StoreProjection};
use norn_wire::{
    FindParams, FindingKind, Pattern, Predicate, ResolutionTarget, VaultAddress, VaultName,
};

use crate::common::{Scratch, path, snapshot, unread_block};
use crate::health::derived;

use StoredPathOrder::{AsciiCaseInsensitive as Folding, Sensitive};

// ---- fixtures ----

/// The fingerprint the suite's schema is pinned under.
const SCHEMA: &str = "redecision-schema";

/// The declaration every changeset here is judged under: `archive/**` kept out
/// of every class a target that does not name it opens.
fn declared() -> ContentModel {
    ContentModel::under(SCHEMA)
        .declare_ambiguity_ignore(Pattern::parse("archive/**").expect("a glob"))
}

/// One link-health finding as a case reads it: its kind, the ordinal of the
/// link it is about, the paths its head names, and its total.
type Filed = (String, u64, Vec<String>, u64);

/// A store over a root proven to have one case behaviour, its schema pinned,
/// written one changeset at a time.
struct Vault {
    // Held for the vault's life: dropping a scratch removes its directory.
    _scratch: Scratch,
    store: Store,
    order: StoredPathOrder,
}

impl Vault {
    fn new(label: &str, order: StoredPathOrder) -> Self {
        let scratch = Scratch::new(label);
        let store = pinned_store(&scratch, order);
        Vault {
            _scratch: scratch,
            store,
            order,
        }
    }

    /// Apply `changes` as one changeset, and hand back what it derived.
    fn apply(&mut self, changes: impl IntoIterator<Item = Change>) -> DerivationCounters {
        let mut request = self.store.begin_request();
        request
            .apply_increment(IncrementProvenance::Derived, changes, &[], &declared())
            .unwrap_or_else(|refusal| panic!("{:?}: a changeset: {refusal}", self.order));
        request.finish()
    }

    /// Write each `(path, body)` as it now stands, in one changeset.
    fn write(&mut self, documents: &[(&str, &str)]) -> DerivationCounters {
        self.apply(upserts(documents))
    }

    /// Kill the document at `at`, in a changeset of its own.
    fn kill(&mut self, at: &str) -> DerivationCounters {
        self.apply([death(at)])
    }

    /// The link-health findings standing at `at`, in the order a reader reads
    /// them.
    fn findings(&mut self, at: &str) -> Vec<Filed> {
        filed(&mut self.store, at)
    }

    /// The paths of the documents holding a link that names `target` alone.
    fn backlinks(&self, target: &str) -> Vec<String> {
        let reader = std::sync::Arc::new(self.store.open_reader().reader.expect("a reader"));
        let found = reader
            .try_take()
            .expect("a handle nothing is reading holds its connection")
            .establish()
            .expect("a snapshot")
            .find(
                &FindParams::new(VaultAddress::name(
                    VaultName::new("notes").expect("a vault name"),
                ))
                .with_predicates([Predicate::links_to(
                    ResolutionTarget::new(target).expect("a target"),
                )]),
                &declared(),
            )
            .unwrap_or_else(|refusal| panic!("the backlinks of `{target}`: {refusal}"));
        assert_eq!(found.unsatisfied, Vec::new(), "`{target}`");
        found
            .rows
            .iter()
            .map(|row| row.path.as_str().to_string())
            .collect()
    }
}

/// A store at `scratch` under `order`, the suite's schema pinned.
fn pinned_store(scratch: &Scratch, order: StoredPathOrder) -> Store {
    let mut store = scratch.open_under(order);
    store
        .begin_request()
        .pin_vault_schema(SCHEMA.as_bytes(), SCHEMA)
        .expect("pinning the suite's schema");
    store
}

/// The link-health findings standing at `at` in `store`, in the order a
/// reader reads them.
fn filed(store: &mut Store, at: &str) -> Vec<Filed> {
    let mut filed: Vec<Filed> = store
        .begin_request()
        .stored_findings(&path(at))
        .expect("reading findings")
        .into_iter()
        .filter(|finding| finding.kind.starts_with("link/"))
        .map(|finding| {
            (
                finding.kind,
                finding
                    .ordinal
                    .expect("a link-health finding names its link"),
                finding
                    .candidates
                    .iter()
                    .map(|candidate| candidate.path.as_str().to_string())
                    .collect(),
                finding.candidates_total,
            )
        })
        .collect();
    filed.sort_by_key(|finding| finding.1);
    filed
}

/// Each `(path, body)` as an upsert of what the text layer derives from it.
fn upserts(documents: &[(&str, &str)]) -> Vec<Change> {
    documents
        .iter()
        .map(|(at, body)| Change::Upsert(derived(at, body)))
        .collect()
}

/// The death of the document at `at`.
fn death(at: &str) -> Change {
    Change::Death {
        path: path(at),
        provenance: Provenance::WatcherRemoval,
    }
}

fn broken(ordinal: u64) -> Filed {
    (
        FindingKind::Broken.as_str().to_string(),
        ordinal,
        Vec::new(),
        0,
    )
}

fn ambiguous(ordinal: u64, head: &[&str], total: u64) -> Filed {
    (
        FindingKind::Ambiguous.as_str().to_string(),
        ordinal,
        head.iter().map(|at| (*at).to_string()).collect(),
        total,
    )
}

fn missing_anchor(ordinal: u64, at: &str) -> Filed {
    (
        FindingKind::MissingAnchor.as_str().to_string(),
        ordinal,
        vec![at.to_string()],
        1,
    )
}

/// A counter's reading in `counters`.
fn counted(counters: &DerivationCounters, name: &str) -> u64 {
    counters
        .get(name)
        .unwrap_or_else(|| panic!("the counter `{name}`"))
}

// ---- the declaration ----

/// A changeset handed a declaration read under another schema than the one
/// the store pins is refused whole, before any entry is written: its link
/// health would be judged under ambiguity-ignore globs no rebuild under the
/// pinned schema reads.
#[test]
fn a_changeset_under_a_declaration_the_store_does_not_pin_is_refused() {
    let scratch = Scratch::new("redecide-unpinned");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    request
        .pin_vault_schema(b"version: 1\n", "pinned")
        .expect("pinning a schema");
    let before = request.write_generation().expect("the write generation");

    for (declared, under) in [
        (ContentModel::none(), None),
        (ContentModel::under("another"), Some("another")),
    ] {
        let refused = request
            .apply_increment(
                IncrementProvenance::Derived,
                [Change::Upsert(derived("a.md", "[[nowhere]]\n"))],
                &[],
                &declared,
            )
            .expect_err("a declaration the store does not pin");
        assert_eq!(
            refused,
            StoreError::UnpinnedDeclaration {
                what: "the declaration a changeset's link health is judged under was read",
                derived_under: under.map(str::to_string),
                pinned: Some("pinned".to_string()),
            }
        );
        assert_eq!(
            request.stored_document(&path("a.md")).expect("a read"),
            None,
            "a refused changeset's document stands"
        );
        assert_eq!(
            request.write_generation().expect("the write generation"),
            before,
            "a refused changeset took a generation"
        );
    }

    request
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Upsert(derived("a.md", "[[nowhere]]\n"))],
            &[],
            &ContentModel::under("pinned"),
        )
        .expect("the pinned schema's declaration");
}

// ---- the doors a caller files through ----

/// The findings only the store files: one of each link-health kind about the
/// document, and one of a caller's kind about a link, each beside what its
/// refusal names.
fn store_judged(at: &str) -> Vec<(FindingFacts, &'static str)> {
    let mut judged: Vec<(FindingFacts, &'static str)> = [
        FindingKind::Broken,
        FindingKind::Ambiguous,
        FindingKind::MissingAnchor,
    ]
    .into_iter()
    .map(|kind| {
        let mut finding = unread_block(at);
        finding.kind = kind;
        (finding, "a link-health finding")
    })
    .collect();
    let mut about_a_link = unread_block(at);
    about_a_link.ordinal = Some(0);
    judged.push((about_a_link, "a finding about a link"));
    judged
}

/// **A caller's door refuses a finding only the store files.** A finding of
/// each link-health kind, and a finding of a caller's kind about a link, is
/// refused by [`norn_store::Request::record_finding`] and by a changeset
/// carrying it, before anything is written: no finding stands, and the
/// changeset's document and generation do not either. The caller's own kind
/// about the document is recorded through both.
#[test]
fn a_callers_door_refuses_a_finding_only_the_store_files() {
    let mut vault = Vault::new("redecide-doors", Sensitive);
    let before = vault
        .store
        .begin_request()
        .write_generation()
        .expect("the write generation");
    for (finding, what) in store_judged("a.md") {
        let refused = StoreError::StoreJudged { what };
        assert_eq!(
            vault.store.begin_request().record_finding(&finding),
            Err(refused.clone()),
            "{finding:?}"
        );
        assert_eq!(
            vault.store.begin_request().apply_increment(
                IncrementProvenance::Derived,
                upserts(&[("a.md", "[[nowhere]]\n")]),
                &[DerivedFinding {
                    facts: finding.clone(),
                    replaces: None,
                }],
                &declared(),
            ),
            Err(refused),
            "{finding:?}"
        );
    }
    let request = vault.store.begin_request();
    assert_eq!(
        request.stored_findings(&path("a.md")).expect("a read"),
        Vec::new()
    );
    assert_eq!(
        request.stored_document(&path("a.md")).expect("a read"),
        None
    );
    assert_eq!(
        request.write_generation().expect("the write generation"),
        before
    );
    request.finish();

    vault
        .store
        .begin_request()
        .record_finding(&unread_block("a.md"))
        .expect("a caller's finding about the document");
    vault
        .store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            upserts(&[("b.md", "b\n")]),
            &[DerivedFinding {
                facts: unread_block("b.md"),
                replaces: None,
            }],
            &declared(),
        )
        .expect("a changeset carrying a caller's finding about the document");
}

// ---- the re-decided set ----

/// **A new document resolves a broken link in a document nobody touched.**
/// `[[t]]` names no document and is broken; a changeset that writes `t.md` and
/// nothing else changes the class `t/`, and the link that class holds is
/// re-decided in it: healthy, its finding gone. The one link re-decided is the
/// untouched document's.
#[test]
fn a_new_document_resolves_a_broken_link_in_an_untouched_document() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::new(&format!("redecide-resolves-{order:?}"), order);
        vault.write(&[("h.md", "[[t]]\n")]);
        assert_eq!(vault.findings("h.md"), [broken(0)], "{order:?}");

        let counters = vault.write(&[("t.md", "# T\n")]);
        assert_eq!(vault.findings("h.md"), [], "{order:?}");
        assert_eq!(counted(&counters, "links_redecided"), 1, "{order:?}");
        assert_eq!(counted(&counters, "findings_discarded"), 1, "{order:?}");
        assert_eq!(counted(&counters, "findings_written"), 0, "{order:?}");
    }
}

/// **A second candidate makes an untouched link ambiguous, and its death
/// heals it.** `[[t]]` names `a/t.md` alone; writing `b/t.md` makes it name
/// two, and killing `b/t.md` makes it name one again — each re-decided in the
/// changeset that moved the class. A candidate under an ambiguity-ignore glob
/// is no candidate: `archive/t.md` joining the class leaves the link healthy.
#[test]
fn a_second_candidate_makes_an_untouched_link_ambiguous_and_its_death_heals_it() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::new(&format!("redecide-ambiguous-{order:?}"), order);
        vault.write(&[("a/t.md", "a\n"), ("h.md", "[[t]]\n")]);
        assert_eq!(vault.findings("h.md"), [], "{order:?}");

        vault.write(&[("archive/t.md", "ignored\n")]);
        assert_eq!(
            vault.findings("h.md"),
            [],
            "{order:?}: a candidate the declaration ignores"
        );

        vault.write(&[("b/t.md", "b\n")]);
        assert_eq!(
            vault.findings("h.md"),
            [ambiguous(0, &["a/t.md", "b/t.md"], 2)],
            "{order:?}"
        );

        vault.kill("b/t.md");
        assert_eq!(vault.findings("h.md"), [], "{order:?}");
    }
}

/// **Editing a target's heading raises and clears a missing anchor.** A
/// heading anchor and a block anchor name places `t.md` holds; rewriting
/// `t.md` without them files a missing anchor for each link in the untouched
/// document, and restoring them clears both.
#[test]
fn editing_a_targets_heading_raises_and_clears_missing_anchor() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::new(&format!("redecide-anchors-{order:?}"), order);
        let holding = "# Intro\n\nA paragraph. ^blk\n";
        vault.write(&[("t.md", holding), ("h.md", "[[t#Intro]]\n\n[[t#^blk]]\n")]);
        assert_eq!(vault.findings("h.md"), [], "{order:?}");

        vault.write(&[("t.md", "# Other\n\nA paragraph.\n")]);
        assert_eq!(
            vault.findings("h.md"),
            [missing_anchor(0, "t.md"), missing_anchor(1, "t.md")],
            "{order:?}"
        );

        vault.write(&[("t.md", holding)]);
        assert_eq!(vault.findings("h.md"), [], "{order:?}");
    }
}

/// **A rename breaks the path links to the old path.** `[x](dir/t.md)` is
/// keyed by the path it spells, which no class range reaches: the rename is a
/// death of `dir/t.md` beside a write of `dir/u.md`, and the changeset
/// re-decides the link through the old path's key. Renaming it back heals it
/// through the same key.
#[test]
fn a_rename_breaks_path_links_to_the_old_path() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::new(&format!("redecide-rename-{order:?}"), order);
        vault.write(&[("dir/t.md", "t\n"), ("h.md", "[x](dir/t.md)\n")]);
        assert_eq!(vault.findings("h.md"), [], "{order:?}");

        let mut renamed = vec![death("dir/t.md")];
        renamed.extend(upserts(&[("dir/u.md", "t\n")]));
        vault.apply(renamed);
        assert_eq!(vault.findings("h.md"), [broken(0)], "{order:?}");

        let mut back = vec![death("dir/u.md")];
        back.extend(upserts(&[("dir/t.md", "t\n")]));
        vault.apply(back);
        assert_eq!(vault.findings("h.md"), [], "{order:?}");
    }
}

/// **A link reached two ways in one changeset files one finding.** `[[v1.2]]`
/// is held under the classes `v1.2/` and `v1/`, and `[[vault://v1.2]]` under
/// the paths `v1.2.md` and `v1.md`; each names an attachment, and naming no
/// document raises nothing. One changeset writing both documents reaches each
/// link twice, and each is judged once — ambiguous between the two, one
/// finding apiece, two links re-decided.
#[test]
fn a_link_reached_by_two_affected_classes_files_one_finding() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::new(&format!("redecide-two-classes-{order:?}"), order);
        vault.write(&[("h.md", "[[v1.2]]\n\n[[vault://v1.2]]\n")]);
        assert_eq!(vault.findings("h.md"), [], "{order:?}");

        let counters = vault.write(&[("v1.md", "one\n"), ("v1.2.md", "one point two\n")]);
        assert_eq!(
            vault.findings("h.md"),
            [
                ambiguous(0, &["v1.2.md", "v1.md"], 2),
                ambiguous(1, &["v1.2.md", "v1.md"], 2),
            ],
            "{order:?}"
        );
        assert_eq!(counted(&counters, "links_redecided"), 2, "{order:?}");
        assert_eq!(counted(&counters, "findings_written"), 2, "{order:?}");
    }
}

/// **A dotted leaf's finding is re-filed whichever of its classes changes.**
/// `[[v1.2#Intro]]` naming `v1.md` alone misses its anchor, and the finding is
/// filed under both `v1.2/` and `v1/`. A changeset changing only `v1.2/`
/// discards it through that key and re-decides the link ambiguous; a death in
/// `v1.2/` makes it miss its anchor again; a changeset changing only `v1/`
/// reaches it through the other key and heals it. Each time the old finding
/// is gone before the new one is filed, or the changeset would refuse a
/// second finding about one link.
#[test]
fn a_dotted_leaf_link_is_refiled_when_either_of_its_classes_changes() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::new(&format!("redecide-dotted-{order:?}"), order);
        vault.write(&[("h.md", "[[v1.2#Intro]]\n"), ("v1.md", "one\n")]);
        assert_eq!(
            vault.findings("h.md"),
            [missing_anchor(0, "v1.md")],
            "{order:?}"
        );

        vault.write(&[("x/v1.2.md", "one point two\n")]);
        assert_eq!(
            vault.findings("h.md"),
            [ambiguous(0, &["x/v1.2.md", "v1.md"], 2)],
            "{order:?}"
        );
        vault.kill("x/v1.2.md");
        assert_eq!(
            vault.findings("h.md"),
            [missing_anchor(0, "v1.md")],
            "{order:?}"
        );
        vault.write(&[("v1.md", "# Intro\n")]);
        assert_eq!(vault.findings("h.md"), [], "{order:?}");
        vault.write(&[("y/v1.md", "one\n")]);
        assert_eq!(
            vault.findings("h.md"),
            [ambiguous(0, &["v1.md", "y/v1.md"], 2)],
            "{order:?}"
        );
    }
}

/// **A rooted name is re-decided through a path key its other key sorts
/// below.** `[[vault://v1.2]]` is held under the paths `v1.2.md` and `v1.md`,
/// and names `v1.2.md` alone. Writing `v1.md` alone changes one of its paths:
/// its other key sorts below the one changed but is no changed path, so the
/// link belongs to the changed one's pass, and is filed ambiguous; killing
/// `v1.md` heals it the same way.
#[test]
fn a_rooted_name_is_redecided_through_its_one_changed_path() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::new(&format!("redecide-rooted-{order:?}"), order);
        vault.write(&[("h.md", "[[vault://v1.2]]\n"), ("v1.2.md", "x\n")]);
        assert_eq!(vault.findings("h.md"), [], "{order:?}");

        vault.write(&[("v1.md", "one\n")]);
        assert_eq!(
            vault.findings("h.md"),
            [ambiguous(0, &["v1.2.md", "v1.md"], 2)],
            "{order:?}"
        );
        vault.kill("v1.md");
        assert_eq!(vault.findings("h.md"), [], "{order:?}");
    }
}

/// How many links one page of a re-decision reads.
const PAGE: usize = 256;

/// **A written document's links are read past a page's end.** One new
/// document holds one link more than a page, each naming nothing, and every
/// one of them is filed broken: the next page resumes inside the document the
/// last one ended in.
#[test]
fn a_written_document_longer_than_a_page_files_every_link() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::new(&format!("redecide-long-written-{order:?}"), order);
        let body = "[[missing]]\n\n".repeat(PAGE + 1);
        let counters = vault.write(&[("h.md", &body)]);
        let expected: Vec<Filed> = (0..=PAGE as u64).map(broken).collect();
        assert_eq!(vault.findings("h.md"), expected, "{order:?}");
        assert_eq!(
            counted(&counters, "links_redecided"),
            PAGE as u64 + 1,
            "{order:?}"
        );
    }
}

/// **A path key's links are read past a page's end, each once.** One
/// document holds one link more than a page to `t.md#Missing`, which `t.md`
/// holds; rewriting `t.md` without the heading re-decides every one of them
/// through the path key, and files a missing anchor for each exactly once —
/// a link read twice would be a second finding about one link, which the
/// changeset refuses.
#[test]
fn a_path_keys_links_past_a_page_are_each_redecided_once() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::new(&format!("redecide-long-path-{order:?}"), order);
        let body = "[p](t.md#Missing)\n\n".repeat(PAGE + 1);
        vault.write(&[("t.md", "# Missing\n"), ("h.md", &body)]);
        assert_eq!(vault.findings("h.md"), [], "{order:?}");

        let counters = vault.write(&[("t.md", "# Other\n")]);
        let expected: Vec<Filed> = (0..=PAGE as u64)
            .map(|ordinal| missing_anchor(ordinal, "t.md"))
            .collect();
        assert_eq!(vault.findings("h.md"), expected, "{order:?}");
        assert_eq!(
            counted(&counters, "links_redecided"),
            PAGE as u64 + 1,
            "{order:?}"
        );
    }
}

/// **An ambiguous link is no one's backlink**, and the findings agree with the
/// read that says so: `[[t]]` names `a/t.md` and `b/t.md`, so it is filed
/// ambiguous and a links-to read of either document leaves its holder out,
/// while `[[a/t]]` names `a/t.md` alone, files nothing, and is its backlink.
/// Every link a finding stands about is a backlink of no candidate the
/// finding names.
#[test]
fn an_ambiguous_link_is_no_ones_backlink() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::new(&format!("redecide-backlinks-{order:?}"), order);
        vault.write(&[
            ("a/t.md", "a\n"),
            ("b/t.md", "b\n"),
            ("h.md", "[[t]]\n"),
            ("g.md", "[[a/t]]\n"),
        ]);
        let findings = vault.findings("h.md");
        assert_eq!(
            findings,
            [ambiguous(0, &["a/t.md", "b/t.md"], 2)],
            "{order:?}"
        );
        assert_eq!(vault.findings("g.md"), [], "{order:?}");
        assert_eq!(vault.backlinks("a/t"), ["g.md"], "{order:?}");
        assert_eq!(vault.backlinks("b/t"), Vec::<String>::new(), "{order:?}");
        for (_, _, head, _) in findings {
            for candidate in head {
                let target = candidate.trim_end_matches(".md").to_string();
                assert!(
                    !vault.backlinks(&target).contains(&"h.md".to_string()),
                    "{order:?}: `h.md` holds an ambiguous link and is a backlink of `{target}`"
                );
            }
        }
    }
}

// ---- a candidate's name ----

/// The suffix each candidate of each link-health finding standing at `at` is
/// named by, in the order a reader reads the findings.
fn named(store: &mut Store, at: &str) -> Vec<Vec<String>> {
    let mut findings: Vec<(u64, Vec<String>)> = store
        .begin_request()
        .stored_findings(&path(at))
        .expect("reading findings")
        .into_iter()
        .filter(|finding| finding.kind.starts_with("link/"))
        .map(|finding| {
            (
                finding
                    .ordinal
                    .expect("a link-health finding names its link"),
                finding
                    .candidates
                    .into_iter()
                    .map(|candidate| candidate.suffix)
                    .collect(),
            )
        })
        .collect();
    findings.sort_by_key(|finding| finding.0);
    findings.into_iter().map(|(_, suffixes)| suffixes).collect()
}

/// A store under `order` and one built from zero out of the same documents
/// are equal under the testkit's comparator, field by field.
fn assert_rebuilds(
    store: &mut Store,
    order: StoredPathOrder,
    held: &BTreeMap<String, String>,
    subject: &str,
) {
    let scratch = Scratch::new(&format!("{subject}-rebuilt"));
    let mut rebuilt = pinned_store(&scratch, order);
    rebuilt
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            held.iter()
                .map(|(at, body)| Change::Upsert(derived(at, body)))
                .collect::<Vec<_>>(),
            &[],
            &declared(),
        )
        .expect("building from zero");
    StoreProjection::read(store)
        .expect("a projection")
        .assert_equivalent(
            &StoreProjection::read(&mut rebuilt).expect("a projection"),
            subject,
        );
    assert_eq!(
        DerivedRows::read(store).expect("the derived rows").fields(),
        DerivedRows::read(&mut rebuilt)
            .expect("the derived rows")
            .fields(),
        "{subject}"
    );
}

/// Run `script` on both roots, a changeset per step — `Some(body)` writes the
/// path, `None` kills it — and after each step hold the suffixes the findings
/// at `at` carry to the step's `expected`, and the store to a rebuild.
fn candidates_named_after(label: &str, at: &str, script: &[(&[(&str, Option<&str>)], &[&[&str]])]) {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::new(&format!("{label}-{order:?}"), order);
        let mut held: BTreeMap<String, String> = BTreeMap::new();
        for (step, (changes, expected)) in script.iter().enumerate() {
            let changes: Vec<Change> = changes
                .iter()
                .map(|(written, body)| match body {
                    Some(body) => {
                        held.insert((*written).to_string(), (*body).to_string());
                        Change::Upsert(derived(written, body))
                    }
                    None => {
                        held.remove(*written);
                        death(written)
                    }
                })
                .collect();
            vault.apply(changes);
            let subject = format!("{label} {order:?} after step {step}");
            assert_eq!(
                named(&mut vault.store, at),
                expected
                    .iter()
                    .map(|head| head.iter().map(|suffix| (*suffix).to_string()).collect())
                    .collect::<Vec<Vec<String>>>(),
                "{subject}"
            );
            assert_rebuilds(&mut vault.store, order, &held, &subject);
        }
    }
}

/// **A path link's missing anchor names its candidate by the suffix the
/// candidate's class leaves it.** `[p](x/t.md#Nope)` names `x/t.md` alone,
/// which lacks the heading, and the finding names it `t`. Writing `y/t.md`
/// touches no key the link is held under — the link is keyed by the path it
/// spells — but gives `t` a second document, so the candidate is `x/t` now;
/// killing it makes the candidate `t` again.
#[test]
fn a_path_links_candidate_is_renamed_when_its_class_moves() {
    candidates_named_after(
        "redecide-named-path",
        "a.md",
        &[
            (
                &[("a.md", Some("[p](x/t.md#Nope)\n")), ("x/t.md", Some(""))],
                &[&["t"]],
            ),
            (&[("y/t.md", Some(""))], &[&["x/t"]]),
            (&[("y/t.md", None)], &[&["t"]]),
        ],
    );
}

/// **A same-document anchor names its own document by the suffix its class
/// leaves it.** `[[#Nope]]` in `x/t.md` names `x/t.md`, which lacks the
/// heading; a write and a death of `y/t.md` rename the candidate `x/t` and
/// back to `t`.
#[test]
fn a_same_document_anchors_candidate_is_renamed_when_its_class_moves() {
    candidates_named_after(
        "redecide-named-self",
        "x/t.md",
        &[
            (&[("x/t.md", Some("[[#Nope]]\n"))], &[&["t"]]),
            (&[("y/t.md", Some(""))], &[&["x/t"]]),
            (&[("y/t.md", None)], &[&["t"]]),
        ],
    );
}

/// **An ambiguous link names a dotted candidate by the suffix its reduction
/// leaves it.** `[[v1.2.3]]` names `v1.2.3.md` and `v1.2.md`, and the second
/// is named `v1.2` while no `v1.md` stands — the first by its written leaf, since `v1.2.3` reduces to the second. Writing `v1.md` moves the class
/// `v1/` — which no key of the link falls in — and `v1.2` then names two
/// documents, so the candidate is `v1.2.md`; killing it gives `v1.2` back.
#[test]
fn an_ambiguous_links_dotted_candidate_is_renamed_when_its_reduction_moves() {
    candidates_named_after(
        "redecide-named-dotted",
        "h.md",
        &[
            (
                &[
                    ("h.md", Some("[[v1.2.3]]\n")),
                    ("v1.2.3.md", Some("")),
                    ("v1.2.md", Some("")),
                ],
                &[&["v1.2.3.md", "v1.2"]],
            ),
            (&[("v1.md", Some(""))], &[&["v1.2.3.md", "v1.2.md"]]),
            (&[("v1.md", None)], &[&["v1.2.3.md", "v1.2"]]),
        ],
    );
}

// ---- incremental equals rebuild ----

/// A deterministic stream of choices, so a failing script replays.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn pick<'a>(&mut self, from: &[&'a str]) -> &'a str {
        from[self.below(from.len())]
    }
}

/// The schemas the edit script pins between, each a fingerprint beside the
/// ambiguity-ignore globs it declares: a directory kept out, a directory and a
/// dotted leaf kept out, and nothing kept out.
const SCHEMAS: [(&str, &[&str]); 3] = [
    ("script-schema-a", &["archive/**"]),
    ("script-schema-b", &["x/**", "*.2.md"]),
    ("script-schema-c", &[]),
];

/// The declaration of the schema at `schema` in [`SCHEMAS`].
fn declared_by(schema: usize) -> ContentModel {
    let (fingerprint, globs) = SCHEMAS[schema];
    globs
        .iter()
        .fold(ContentModel::under(fingerprint), |model, glob| {
            model.declare_ambiguity_ignore(Pattern::parse(glob).expect("a glob"))
        })
}

/// Pin the schema at `schema` in [`SCHEMAS`] on `store`.
fn pin_schema(store: &mut Store, schema: usize) {
    let fingerprint = SCHEMAS[schema].0;
    store
        .begin_request()
        .pin_vault_schema(fingerprint.as_bytes(), fingerprint)
        .expect("pinning a schema");
}

/// The places the edit script writes, kills and renames between: stems shared
/// across directories and across case, a dotted stem and its reductions, a
/// leaf that is a directory's name, places under the ignore globs, and places
/// only a path link names.
const PLACES: [&str; 30] = [
    "t.md",
    "T.md",
    "x/t.md",
    "X/t.md",
    "x/T.md",
    "y/t.md",
    "x/y/t.md",
    "archive/t.md",
    "archive/x/t.md",
    "t/x.md",
    "t/t.md",
    "v1.md",
    "v1.2.md",
    "V1.2.md",
    "x/v1.2.md",
    "archive/v1.2.md",
    "v1.2.3.md",
    "h.md",
    "x/h.md",
    "dir/u.md",
    "Dir/u.md",
    "u.md",
    "notes.md",
    "x/Notes2.md",
    "a.md",
    "x/a.md",
    "t.md.md",
    "x.md",
    "y/x.md",
    "t/v1.md",
];

/// The lines a body is made of: headings and blocks a link's anchor may name,
/// and links of every address — suffix, dotted, anchored, same-document,
/// relative and rooted paths, embeds, attachments, ignored, elsewhere, and
/// naming nothing — in both cases.
const LINES: [&str; 56] = [
    "# Intro",
    "## Setup",
    "# intro",
    "A paragraph. ^blk",
    "An item. ^b2",
    "[[t]]",
    "[[T]]",
    "[[x/t]]",
    "[[X/T]]",
    "[[y/t]]",
    "[[x/y/t]]",
    "[[t#Intro]]",
    "[[t#intro]]",
    "[[t#Setup]]",
    "[[t#^blk]]",
    "[[v1]]",
    "[[v1.2]]",
    "[[v1.2.3]]",
    "[[v1.2#Setup]]",
    "[[vault://t]]",
    "[[vault://x/t]]",
    "[[vault://v1.2]]",
    "[[vault://t/x]]",
    "[p](t.md)",
    "[p](T.md)",
    "[p](x/t.md#Intro)",
    "[p](../t.md)",
    "[p](../x/t.md)",
    "[p](./t.md)",
    "[p](t/x.md)",
    "[p](../../t.md)",
    "![[t]]",
    "![[x/t#^b2]]",
    "![[v1.2#^blk]]",
    "[[notes]]",
    "[[notes#Intro]]",
    "[[h#^b2]]",
    "[[archive/t]]",
    "[[missing]]",
    "[p](dir/u.md)",
    "[p](Dir/u.md)",
    "[[u#^blk]]",
    "[[u]]",
    "[w](https://example.com/page)",
    "[[Notes2]]",
    "[[#Intro]]",
    "[[#^blk]]",
    "[[a]]",
    "[[a#Intro]]",
    "[[t.md]]",
    "[[t.md#Intro]]",
    "[[x]]",
    "[[t/x]]",
    "[[x.md]]",
    "[i](img.png)",
    "![[pic.png]]",
];

/// A body of up to six of [`LINES`], each its own paragraph.
fn body(rng: &mut Rng) -> String {
    (0..rng.below(7))
        .map(|_| format!("{}\n\n", rng.pick(&LINES)))
        .collect()
}

/// The spelling `held` already holds of the path `at` names on a root under
/// `order`: `at` itself, or on a folding root any spelling that folds with it.
fn held_spelling(
    order: StoredPathOrder,
    held: &BTreeMap<String, String>,
    at: &str,
) -> Option<String> {
    match order {
        Sensitive => held.contains_key(at).then(|| at.to_string()),
        Folding => held
            .keys()
            .find(|spelled| spelled.eq_ignore_ascii_case(at))
            .cloned(),
    }
}

/// One changeset of an edit script over the documents `held` tracks on a root
/// under `order` — one to four writes, deaths and renames, a rename's body
/// sometimes rewritten and its death sometimes ahead of its write — and what
/// it says it does.
fn step(
    rng: &mut Rng,
    order: StoredPathOrder,
    held: &mut BTreeMap<String, String>,
) -> (Vec<Change>, String) {
    let mut changes = Vec::new();
    let mut said = Vec::new();
    for _ in 0..=rng.below(4) {
        let live: Vec<String> = held.keys().cloned().collect();
        let at = rng.pick(&PLACES).to_string();
        match rng.below(if live.is_empty() { 1 } else { 4 }) {
            0 | 3 => {
                let written = body(rng);
                // A folding root holds one spelling of a path, so a write
                // there is a write of the spelling it holds.
                let at = held_spelling(order, held, &at).unwrap_or(at);
                said.push(format!("write {at} {written:?}"));
                changes.push(Change::Upsert(derived(&at, &written)));
                held.insert(at, written);
            }
            1 => {
                let dead = live[rng.below(live.len())].clone();
                said.push(format!("kill {dead}"));
                changes.push(death(&dead));
                held.remove(&dead);
            }
            _ => {
                let from = live[rng.below(live.len())].clone();
                if held_spelling(order, held, &at).is_some_and(|existing| existing != from)
                    || from == at
                {
                    continue;
                }
                let kept = held.remove(&from).expect("a live document");
                let moved = if rng.below(3) == 0 { body(rng) } else { kept };
                said.push(format!("rename {from} to {at} {moved:?}"));
                if rng.below(2) == 0 || from.eq_ignore_ascii_case(&at) {
                    changes.push(death(&from));
                    changes.push(Change::Upsert(derived(&at, &moved)));
                } else {
                    changes.push(Change::Upsert(derived(&at, &moved)));
                    changes.push(death(&from));
                }
                held.insert(at, moved);
            }
        }
    }
    (changes, said.join("; "))
}

/// Apply `changes` to `store` as one changeset under the schema at `schema`.
fn apply_under(store: &mut Store, changes: Vec<Change>, schema: usize, subject: &str) {
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            changes,
            &[],
            &declared_by(schema),
        )
        .unwrap_or_else(|refusal| panic!("{subject}: a changeset: {refusal}"));
}

/// Write every document `held` holds into `store` as a heal does: shuffled,
/// in changesets of one to four.
fn heal(
    rng: &mut Rng,
    store: &mut Store,
    held: &BTreeMap<String, String>,
    schema: usize,
    subject: &str,
) {
    let mut entries: Vec<(&String, &String)> = held.iter().collect();
    for at in (1..entries.len()).rev() {
        entries.swap(at, rng.below(at + 1));
    }
    let mut at = 0;
    while at < entries.len() {
        let size = 1 + rng.below(4);
        let chunk = entries[at..(at + size).min(entries.len())]
            .iter()
            .map(|(written, body)| Change::Upsert(derived(written, body)))
            .collect();
        apply_under(store, chunk, schema, subject);
        at += size;
    }
}

/// `store` equals `other` under the testkit's comparator, finding order
/// included, and every derived row agrees field by field.
fn assert_same(store: &mut Store, other: &mut Store, subject: &str) {
    StoreProjection::read(store)
        .expect("a projection")
        .assert_equivalent(
            &StoreProjection::read(other).expect("a projection"),
            subject,
        );
    assert_eq!(
        DerivedRows::read(store).expect("the derived rows").fields(),
        DerivedRows::read(other).expect("the derived rows").fields(),
        "{subject}"
    );
}

/// A count the script reads from the environment variable `name`, or
/// `default` where it is unset — so a longer run is one variable away.
fn script_bound(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// **Incremental maintenance equals a rebuild, finding order included.** A
/// seeded edit script is applied to one store: changesets of one to four
/// writes, deaths and renames over bodies whose headings, blocks and links each
/// step rewrites, and now and then a pin of another schema — which moves the
/// ambiguity-ignore globs — followed by a heal of every document in shuffled
/// changesets. After every step a second store is built from zero out of the
/// documents the first holds, in one changeset, and the two are equal under
/// the testkit's comparator, which reads every finding and every candidate's
/// name in the order a reader does, and every derived row agrees field by
/// field; now and then a third store healed from zero in shuffled changesets
/// is held to the same. Both roots, the script reaching every kind the family
/// has.
///
/// `NORN_REDECISION_SEEDS` and `NORN_REDECISION_STEPS` lengthen the run.
#[test]
fn incremental_equals_rebuild_including_finding_order() {
    let seeds = script_bound("NORN_REDECISION_SEEDS", 3);
    let steps = script_bound("NORN_REDECISION_STEPS", 40);
    let mut kinds: BTreeSet<String> = BTreeSet::new();
    for order in [Sensitive, Folding] {
        for seed in 0x253b_0000..0x253b_0000 + seeds {
            let mut rng = Rng(seed);
            let label = format!("redecide-script-{order:?}-{seed:x}");
            let scratch = Scratch::new(&label);
            let mut store = scratch.open_under(order);
            let mut schema = 0;
            pin_schema(&mut store, schema);
            let mut held: BTreeMap<String, String> = BTreeMap::new();
            let mut script: Vec<String> = Vec::new();
            for at in 0..steps {
                if rng.below(12) == 0 && !held.is_empty() {
                    schema = (schema + 1 + rng.below(2)) % SCHEMAS.len();
                    script.push(format!("pin {} and heal", SCHEMAS[schema].0));
                    pin_schema(&mut store, schema);
                    let subject = format!("{order:?} seed {seed:#x}: {script:#?}");
                    heal(&mut rng, &mut store, &held, schema, &subject);
                } else {
                    let (changes, said) = step(&mut rng, order, &mut held);
                    script.push(said);
                    let subject = format!("{order:?} seed {seed:#x}: {script:#?}");
                    apply_under(&mut store, changes, schema, &subject);
                }
                let subject = format!("{order:?} seed {seed:#x}: {script:#?}");

                let rebuilt_scratch = Scratch::new(&format!("{label}-rebuilt-{at}"));
                let mut rebuilt = rebuilt_scratch.open_under(order);
                pin_schema(&mut rebuilt, schema);
                apply_under(
                    &mut rebuilt,
                    held.iter()
                        .map(|(written, body)| Change::Upsert(derived(written, body)))
                        .collect(),
                    schema,
                    &subject,
                );
                assert_same(&mut store, &mut rebuilt, &subject);
                kinds.extend(
                    StoreProjection::read(&mut store)
                        .expect("a projection")
                        .findings()
                        .iter()
                        .map(|finding| finding.kind.clone()),
                );

                if rng.below(4) == 0 {
                    let healed_scratch = Scratch::new(&format!("{label}-healed-{at}"));
                    let mut healed = healed_scratch.open_under(order);
                    pin_schema(&mut healed, schema);
                    heal(&mut rng, &mut healed, &held, schema, &subject);
                    assert_same(&mut healed, &mut rebuilt, &format!("healed: {subject}"));
                }
            }
        }
    }
    for kind in [
        FindingKind::Broken,
        FindingKind::Ambiguous,
        FindingKind::MissingAnchor,
    ] {
        assert!(
            kinds.contains(kind.as_str()),
            "the script never reached {kind:?}: {kinds:?}"
        );
    }
}

/// **A neighborhood of many pages equals a rebuild.** Seven hundred documents
/// each hold one to three links into one stem, its dotted neighbor and one
/// path, so every arm reads more than one page; each changeset after them
/// moves the stem's class, a path, a dotted reduction, or a holder, and the
/// store equals one built from zero on both roots.
#[test]
fn a_neighborhood_of_many_pages_equals_a_rebuild() {
    const LINKS: [&str; 6] = [
        "[[t]]\n\n",
        "[[x/t]]\n\n",
        "[p](x/t.md)\n\n",
        "[[t#Intro]]\n\n",
        "[[v1.2]]\n\n",
        "[[vault://t]]\n\n",
    ];
    for order in [Sensitive, Folding] {
        let label = format!("redecide-pages-{order:?}");
        let scratch = Scratch::new(&label);
        let mut store = scratch.open_under(order);
        pin_schema(&mut store, 0);
        let mut rng = Rng(0x253b_7000);
        let mut held: BTreeMap<String, String> = (0..700)
            .map(|at| {
                let links = (0..=rng.below(3)).map(|_| rng.pick(&LINKS)).collect();
                (format!("h/{at:04}.md"), links)
            })
            .collect();
        heal(&mut rng, &mut store, &held, 0, &label);
        let script: [&[(&str, Option<&str>)]; 6] = [
            &[("t.md", Some("# Intro\n"))],
            &[("x/t.md", Some("# Intro\n"))],
            &[("y/t.md", Some("x\n"))],
            &[("v1.md", Some("x\n")), ("v1.2.md", Some("x\n"))],
            &[("x/t.md", None)],
            &[("t.md", None), ("h/0001.md", Some("[[t]]\n[[v1]]\n"))],
        ];
        for (at, changes) in script.into_iter().enumerate() {
            let changes = changes
                .iter()
                .map(|(written, body)| match body {
                    Some(body) => {
                        held.insert((*written).to_string(), (*body).to_string());
                        Change::Upsert(derived(written, body))
                    }
                    None => {
                        held.remove(*written);
                        death(written)
                    }
                })
                .collect();
            apply_under(&mut store, changes, 0, &label);
            let rebuilt_scratch = Scratch::new(&format!("{label}-rebuilt-{at}"));
            let mut rebuilt = rebuilt_scratch.open_under(order);
            pin_schema(&mut rebuilt, 0);
            apply_under(
                &mut rebuilt,
                held.iter()
                    .map(|(written, body)| Change::Upsert(derived(written, body)))
                    .collect(),
                0,
                &label,
            );
            assert_same(
                &mut store,
                &mut rebuilt,
                &format!("{label} after step {at}"),
            );
        }
    }
}

// ---- the write-work bar ----

/// What writing one more document the stem `hub` names cost a store holding
/// `links` documents `h/NNN.md` that each link `[[hub]]`, `multiplicity`
/// documents `m/NNN/hub.md` the link names, and `unrelated` documents
/// neither: the counters the write moved and the steps its re-decision took.
fn hub_write(links: usize, multiplicity: usize, unrelated: usize) -> (DerivationCounters, u64) {
    let label = format!("redecide-work-{links}-{multiplicity}-{unrelated}");
    let mut vault = Vault::new(&label, Sensitive);
    let mut documents: Vec<(String, &str)> = (0..multiplicity)
        .map(|at| (format!("m/{at:03}/hub.md"), "hub\n"))
        .collect();
    documents.extend((0..links).map(|at| (format!("h/{at:03}.md"), "[[hub]]\n")));
    documents.extend((0..unrelated).map(|at| (format!("u/{at:04}.md"), "[[elsewhere]]\n")));
    vault.apply(
        documents
            .iter()
            .map(|(at, body)| Change::Upsert(derived(at, body)))
            .collect::<Vec<_>>(),
    );

    let mut request = vault.store.begin_request();
    request
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Upsert(derived("new/hub.md", "hub\n"))],
            &[],
            &declared(),
        )
        .expect("writing one more hub");
    let steps = request.read_steps();
    (request.finish(), steps)
}

/// **A write's work follows its neighborhood, not the vault.** Writing one
/// more document a stem names re-decides every link to that stem: over a grid
/// of fifty and five hundred links — five hundred is two chunks — crossed with
/// two and two hundred documents the stem names, the write re-decides exactly
/// the links, resolves the one key once, reads the candidates it names once,
/// and files one finding per link. Its steps are a cost per link plus a cost
/// per candidate, so what the extra links cost is the same whatever the stem
/// names; and a hundred or a thousand documents outside the neighborhood, each
/// holding a link of its own, cost the write the same.
#[test]
fn a_writes_work_follows_the_neighborhood_not_the_vault() {
    let mut steps = BTreeMap::new();
    for links in [50, 500] {
        for multiplicity in [2, 200] {
            let (counters, taken) = hub_write(links, multiplicity, 0);
            let at = format!("({links}, {multiplicity})");
            assert_eq!(counted(&counters, "links_redecided"), links as u64, "{at}");
            assert_eq!(counted(&counters, "link_health_keys_resolved"), 1, "{at}");
            assert_eq!(
                counted(&counters, "link_health_candidates_read"),
                multiplicity as u64 + 1,
                "{at}"
            );
            assert_eq!(counted(&counters, "findings_written"), links as u64, "{at}");
            assert_eq!(
                counted(&counters, "findings_discarded"),
                links as u64,
                "{at}"
            );
            steps.insert((links, multiplicity), taken as i64);
        }
    }
    eprintln!("re-decision steps over the grid: {steps:?}");
    let more_links_among = |multiplicity| steps[&(500, multiplicity)] - steps[&(50, multiplicity)];
    let more_candidates_under = |links| steps[&(links, 200)] - steps[&(links, 2)];
    assert!(
        more_links_among(2) > 0 && more_candidates_under(50) > 0,
        "the grid does not move the work: {steps:?}"
    );
    assert_eq!(
        more_links_among(200),
        more_links_among(2),
        "four hundred and fifty more links cost more where the stem names more documents, so \
         the work multiplies links by candidates: {steps:?}"
    );
    assert_eq!(more_candidates_under(500), more_candidates_under(50));

    for (links, multiplicity) in [(50, 2), (500, 200)] {
        let (within, within_steps) = hub_write(links, multiplicity, 100);
        let (beside, beside_steps) = hub_write(links, multiplicity, 1000);
        snapshot(&beside).assert_equal_counts(
            &snapshot(&within),
            &format!("({links}, {multiplicity}) beside ten times the unrelated documents"),
        );
        assert_eq!(
            beside_steps, within_steps,
            "({links}, {multiplicity}): ten times the unrelated documents moved the write's work"
        );
    }
}

/// What writing `hub.md` cost a store holding twenty documents that link
/// `[[hub]]` beside `beside` documents that each link a path under the folder
/// `folder`: the counters it moved and the steps its re-decision took.
fn folder_write(beside: usize, folder: &str) -> (DerivationCounters, u64) {
    let label = format!("redecide-folder-{beside}-{}", folder.trim_end_matches('/'));
    let mut vault = Vault::new(&label, Sensitive);
    let mut documents: Vec<(String, String)> = (0..20)
        .map(|at| (format!("h/{at:03}.md"), "[[hub]]\n".to_string()))
        .collect();
    documents.extend(
        (0..beside).map(|at| (format!("u{at:04}.md"), format!("[p]({folder}{at:04}.md)\n"))),
    );
    vault.apply(
        documents
            .iter()
            .map(|(at, body)| Change::Upsert(derived(at, body)))
            .collect::<Vec<_>>(),
    );
    let mut request = vault.store.begin_request();
    request
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Upsert(derived("hub.md", "hub\n"))],
            &[],
            &declared(),
        )
        .expect("writing the hub");
    let steps = request.read_steps();
    (request.finish(), steps)
}

/// **A class's walk reads no path-addressed link.** A folder note —
/// `hub.md` beside a folder `hub/` — is common, and every path link into the
/// folder is held under a path key spelled inside the range the class `hub/`
/// opens. Writing `hub.md` re-decides the twenty `[[hub]]` links, and its
/// steps are the same beside a hundred and two thousand links to paths under
/// `hub/` as beside as many under another folder.
#[test]
fn a_class_walk_reads_no_path_link_spelled_inside_its_range() {
    let (counters, few) = folder_write(100, "hub/");
    assert_eq!(counted(&counters, "links_redecided"), 20);
    let (_, many) = folder_write(2000, "hub/");
    let (_, elsewhere) = folder_write(2000, "elsewhere/");
    assert_eq!(
        (few, many),
        (elsewhere, elsewhere),
        "path links under the folder `hub/` moved the work of a write to the class `hub/`"
    );
}

// ---- a tear inside the re-decision ----

/// The environment variable that puts this suite's own binary in the child
/// role, carrying the database whose re-decision the child is killed inside.
const TORN_REDECISION_DATABASE: &str = "NORN_STORE_TORN_REDECISION_DATABASE";

/// The case the child is asked to run, which is this one.
const TORN_REDECISION_CASE: &str =
    "redecision::a_tear_inside_the_redecision_leaves_the_prior_findings";

/// How many documents hold `[[t]]` in the torn case: more than one chunk, so
/// the tear lands with one chunk filed and the next unread.
const TORN_HOLDERS: usize = 300;

/// **A process killed inside the re-decision leaves the prior findings.** The
/// parent files a broken finding about `[[t]]` in each of three hundred
/// documents. The child applies a changeset writing two documents `t` names,
/// and is killed once the re-decision has filed its first chunk: the class
/// discard has taken every broken finding and the first chunk's ambiguous
/// findings are written, all inside the open transaction. Reopened, the store
/// holds exactly what it held before — every broken finding, no ambiguous one,
/// neither document — and is a store this build reuses.
///
/// **This case is its own child**, for the reason the torn-changeset case in
/// the increment suite states.
#[test]
fn a_tear_inside_the_redecision_leaves_the_prior_findings() {
    if let Some(database) = std::env::var_os(TORN_REDECISION_DATABASE) {
        tear_the_redecision(Path::new(&database));
    }

    let scratch = Scratch::new("redecide-torn");
    let database = scratch.database();
    let holders: Vec<String> = (0..TORN_HOLDERS)
        .map(|at| format!("h/{at:03}.md"))
        .collect();
    {
        let mut store =
            Store::open(&database, Sensitive, crate::common::DERIVATION).expect("creating a store");
        store
            .begin_request()
            .pin_vault_schema(SCHEMA.as_bytes(), SCHEMA)
            .expect("pinning the suite's schema");
        store
            .begin_request()
            .apply_increment(
                IncrementProvenance::Derived,
                holders
                    .iter()
                    .map(|at| Change::Upsert(derived(at, "[[t]]\n")))
                    .collect::<Vec<_>>(),
                &[],
                &declared(),
            )
            .expect("writing the holders");
    }

    let child = Command::new(std::env::current_exe().expect("this suite's own executable"))
        .args(["--exact", TORN_REDECISION_CASE])
        .env(TORN_REDECISION_DATABASE, &database)
        .output()
        .expect("running this suite in the child role");
    assert!(
        !child.status.success(),
        "the child was to be killed inside a re-decision and it finished: {}",
        String::from_utf8_lossy(&child.stderr)
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            child.status.signal(),
            Some(6),
            "the child ended some way other than the abort the arrangement arms: {}",
            String::from_utf8_lossy(&child.stderr)
        );
    }

    let mut reopened =
        Store::open(&database, Sensitive, crate::common::DERIVATION).expect("reopening the store");
    assert_eq!(*reopened.open_outcome(), OpenOutcome::Reused);
    for at in &holders {
        assert_eq!(
            filed(&mut reopened, at),
            [broken(0)],
            "`{at}` lost the finding the torn re-decision discarded"
        );
    }
    let request = reopened.begin_request();
    for at in ["a/t.md", "b/t.md"] {
        assert_eq!(
            request.stored_document(&path(at)).expect("a read"),
            None,
            "`{at}`, which only the torn changeset wrote, is at rest"
        );
    }
    request.finish();
    reopened
        .verify_integrity()
        .expect("a store a re-decision was torn in");
}

/// The child half of the case above: apply a changeset whose re-decision this
/// process does not survive, and never return.
fn tear_the_redecision(database: &Path) -> ! {
    let mut store = Store::open(database, Sensitive, crate::common::DERIVATION)
        .expect("opening the store the parent wrote");
    norn_store::induced_failure::abort_after_link_health_chunks(1);
    let _ = store.begin_request().apply_increment(
        IncrementProvenance::Derived,
        upserts(&[("a/t.md", "a\n"), ("b/t.md", "b\n")]),
        &[],
        &declared(),
    );
    panic!("the changeset committed, so the arrangement that arms the abort did not fire");
}
