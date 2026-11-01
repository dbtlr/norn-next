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
    Change, ContentModel, DerivationCounters, DerivedFinding, DiscardScope, FindingFacts,
    IncrementProvenance, OpenOutcome, Provenance, Store, StoreError, StoredPathOrder,
};
use norn_testkit::equivalence::{DerivedRows, StoreProjection};
use norn_testkit::work::STEPS_PER_EMPTY_KEY;
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

fn vault_scheme(ordinal: u64, at: &str) -> Filed {
    (
        FindingKind::VaultScheme.as_str().to_string(),
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
                    bytes_hash: None,
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
                bytes_hash: None,
            }],
            &declared(),
        )
        .expect("a changeset carrying a caller's finding about the document");
}

/// The discard scopes a caller could name that reach a link-health finding:
/// every kind, each link-health kind alone, and one beside a caller's kind.
const REACHING_LINK_HEALTH: [DiscardScope<'static>; 5] = [
    DiscardScope::EveryKind,
    DiscardScope::Kinds(&[FindingKind::Broken]),
    DiscardScope::Kinds(&[FindingKind::Ambiguous]),
    DiscardScope::Kinds(&[FindingKind::MissingAnchor]),
    DiscardScope::Kinds(&[FindingKind::FrontmatterUnreadable, FindingKind::Broken]),
];

/// **A caller's subject discard never takes a finding only the store files.**
/// `a.md` holds a broken link, and a discard about `a.md` whose scope reaches
/// a link-health kind is refused before it runs: the broken link's finding
/// stands, since nothing a caller records would file it again. A scope of the
/// caller's own kinds takes the caller's finding and leaves the link's.
#[test]
fn a_callers_subject_discard_refuses_a_scope_reaching_link_health() {
    let mut vault = Vault::new("redecide-subject-discard", Sensitive);
    vault.write(&[("a.md", "[[nowhere]]\n")]);
    vault
        .store
        .begin_request()
        .record_finding(&unread_block("a.md"))
        .expect("a caller's finding about the document");
    for scope in REACHING_LINK_HEALTH {
        assert_eq!(
            vault
                .store
                .begin_request()
                .discard_findings_about(&path("a.md"), scope),
            Err(StoreError::StoreJudged {
                what: "a link-health finding"
            }),
            "{scope:?}"
        );
        assert_eq!(vault.findings("a.md"), [broken(0)], "{scope:?}");
    }

    let taken = vault
        .store
        .begin_request()
        .discard_findings_about(
            &path("a.md"),
            DiscardScope::Kinds(&[FindingKind::FrontmatterUnreadable]),
        )
        .expect("a discard of a caller's kind");
    assert_eq!(taken.findings_discarded, 1);
    assert_eq!(vault.findings("a.md"), [broken(0)]);
}

/// **A changeset's replacement never takes a finding only the store files.**
/// `a.md` holds a broken link. A changeset writing `b.md` and carrying a
/// caller's finding about `a.md` that replaces a scope reaching a link-health
/// kind there is refused before anything is written: the broken link's
/// finding stands, and `b.md`, the caller's finding and a generation do not.
#[test]
fn a_changesets_replacement_refuses_a_scope_reaching_link_health() {
    let mut vault = Vault::new("redecide-replacement", Sensitive);
    vault.write(&[("a.md", "[[nowhere]]\n")]);
    let before = vault
        .store
        .begin_request()
        .write_generation()
        .expect("the write generation");
    for scope in REACHING_LINK_HEALTH {
        assert_eq!(
            vault.store.begin_request().apply_increment(
                IncrementProvenance::Derived,
                upserts(&[("b.md", "b\n")]),
                &[DerivedFinding {
                    facts: unread_block("a.md"),
                    replaces: Some(scope),
                    bytes_hash: None,
                }],
                &declared(),
            ),
            Err(StoreError::StoreJudged {
                what: "a link-health finding"
            }),
            "{scope:?}"
        );
        assert_eq!(vault.findings("a.md"), [broken(0)], "{scope:?}");
    }
    let request = vault.store.begin_request();
    assert_eq!(
        request
            .stored_findings(&path("a.md"))
            .expect("a read")
            .len(),
        1,
        "a refused changeset filed its finding"
    );
    assert_eq!(
        request.stored_document(&path("b.md")).expect("a read"),
        None
    );
    assert_eq!(
        request.write_generation().expect("the write generation"),
        before
    );
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

/// **A target that names an ignored place three deep resolves to it.**
/// `archive/old/twin.md` stands under the ignored `archive/**`, so it is in
/// the class of a target of three segments that reaches its ignored place,
/// and in no class of a shorter one: `[[archive/old/twin]]` resolves to it,
/// while `[[old/twin]]` names nothing and is broken.
#[test]
fn a_target_reaching_an_ignored_place_three_deep_resolves_to_it() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::new(&format!("redecide-three-deep-{order:?}"), order);
        vault.write(&[
            ("archive/old/twin.md", "twin\n"),
            ("holder.md", "[[archive/old/twin]]\n"),
            ("other.md", "[[old/twin]]\n"),
        ]);
        assert_eq!(vault.findings("holder.md"), [], "{order:?}");
        assert_eq!(vault.findings("other.md"), [broken(0)], "{order:?}");
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

/// Suffix and rooted `.md` links each have two document-name keys.
/// Creating both candidates rejudges each link once and files one
/// ambiguity finding per link, replacing its earlier broken finding.
#[test]
fn a_link_reached_by_two_affected_classes_files_one_finding() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::new(&format!("redecide-two-classes-{order:?}"), order);
        vault.write(&[("h.md", "[[v1.md]]\n\n[[vault://v1.md]]\n")]);
        assert_eq!(vault.findings("h.md"), [broken(0), broken(1)], "{order:?}");

        let counters = vault.write(&[("v1.md", "one\n"), ("v1.md.md", "one point two\n")]);
        assert_eq!(
            vault.findings("h.md"),
            [
                ambiguous(0, &["v1.md.md", "v1.md"], 2),
                ambiguous(1, &["v1.md", "v1.md.md"], 2),
            ],
            "{order:?}"
        );
        assert_eq!(counted(&counters, "links_redecided"), 2, "{order:?}");
        assert_eq!(counted(&counters, "findings_written"), 2, "{order:?}");
    }
}

/// A link ending in `.md` is rejudged when either document-name class
/// changes. Adding its `.md.md` twin makes it ambiguous; removing the
/// twin restores the missing-anchor finding, and adding the heading heals it.
/// Neither document stands at the link's exact path, which would name it
/// alone.
#[test]
fn a_link_ending_in_md_is_rejudged_when_either_document_name_class_changes() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::new(&format!("redecide-dotted-{order:?}"), order);
        vault.write(&[("h.md", "[[v1.md#Intro]]\n"), ("a/v1.md", "one\n")]);
        assert_eq!(
            vault.findings("h.md"),
            [missing_anchor(0, "a/v1.md")],
            "{order:?}"
        );

        vault.write(&[("x/v1.md.md", "one point two\n")]);
        assert_eq!(
            vault.findings("h.md"),
            [ambiguous(0, &["x/v1.md.md", "a/v1.md"], 2)],
            "{order:?}"
        );
        vault.kill("x/v1.md.md");
        assert_eq!(
            vault.findings("h.md"),
            [missing_anchor(0, "a/v1.md")],
            "{order:?}"
        );
        vault.write(&[("a/v1.md", "# Intro\n")]);
        assert_eq!(vault.findings("h.md"), [], "{order:?}");
        vault.write(&[("y/v1.md", "one\n")]);
        assert_eq!(
            vault.findings("h.md"),
            [ambiguous(0, &["a/v1.md", "y/v1.md"], 2)],
            "{order:?}"
        );
    }
}

/// A rooted `.md` link is rejudged when either complete path changes.
/// Adding `v1.md` beside `v1.md.md` makes it ambiguous; deleting it heals it.
#[test]
fn a_rooted_name_is_redecided_through_its_one_changed_path() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::new(&format!("redecide-rooted-{order:?}"), order);
        vault.write(&[("h.md", "[[vault://v1.md]]\n"), ("v1.md.md", "x\n")]);
        assert_eq!(
            vault.findings("h.md"),
            [vault_scheme(0, "v1.md.md")],
            "{order:?}"
        );

        vault.write(&[("v1.md", "one\n")]);
        assert_eq!(
            vault.findings("h.md"),
            [ambiguous(0, &["v1.md", "v1.md.md"], 2)],
            "{order:?}"
        );
        vault.kill("v1.md");
        assert_eq!(
            vault.findings("h.md"),
            [vault_scheme(0, "v1.md.md")],
            "{order:?}"
        );
    }
}

/// **A vault-scheme finding rides the changeset like the other link-health
/// kinds.** A `vault://` link of either family is broken until its target is
/// created, files a vault-scheme finding while the target stands, is broken
/// again when the target is deleted, and follows the target's heading: the
/// anchor it names makes the finding a vault-scheme one, and losing it makes
/// the missing anchor stand alone. Rewriting the link as a `/` link heals it.
#[test]
fn a_vault_scheme_finding_rides_the_changeset() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::new(&format!("redecide-vault-scheme-{order:?}"), order);
        vault.write(&[(
            "h.md",
            "[[vault://dir/t.md]]\n\n[m](vault://dir/t.md)\n\n[[vault://dir/t.md#Intro]]\n",
        )]);
        assert_eq!(
            vault.findings("h.md"),
            [broken(0), broken(1), broken(2)],
            "{order:?}"
        );

        let counters = vault.write(&[("dir/t.md", "# Intro\n")]);
        assert_eq!(
            vault.findings("h.md"),
            [
                vault_scheme(0, "dir/t.md"),
                vault_scheme(1, "dir/t.md"),
                vault_scheme(2, "dir/t.md"),
            ],
            "{order:?}"
        );
        assert_eq!(counted(&counters, "findings_written"), 3, "{order:?}");

        vault.write(&[("dir/t.md", "# Other\n")]);
        assert_eq!(
            vault.findings("h.md"),
            [
                vault_scheme(0, "dir/t.md"),
                vault_scheme(1, "dir/t.md"),
                missing_anchor(2, "dir/t.md"),
            ],
            "{order:?}"
        );

        vault.write(&[("h.md", "[[/dir/t.md]]\n\n[m](vault://dir/t.md)\n")]);
        assert_eq!(
            vault.findings("h.md"),
            [vault_scheme(1, "dir/t.md")],
            "{order:?}"
        );

        vault.kill("dir/t.md");
        assert_eq!(vault.findings("h.md"), [broken(0), broken(1)], "{order:?}");
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

/// One step of a naming script: the changes it makes, each a path beside the
/// body it is written with or `None` for its death, and the suffixes the
/// findings at the watched path name their candidates by after it.
type NamingStep<'a> = (&'a [(&'a str, Option<&'a str>)], &'a [&'a [&'a str]]);

/// Run `script` on both roots, a changeset per step — `Some(body)` writes the
/// path, `None` kills it — and after each step hold the suffixes the findings
/// at `at` carry to the step's `expected`, and the store to a rebuild.
fn candidates_named_after(label: &str, at: &str, script: &[NamingStep<'_>]) {
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

/// An ambiguous candidate's minimal suffix is recomputed when one of
/// its optional `.md` spellings gains a competing document. Creating
/// `v1.md` changes the candidate's suffix; removing it restores the suffix.
#[test]
fn an_ambiguous_candidates_minimal_suffix_changes_when_an_md_name_becomes_ambiguous() {
    candidates_named_after(
        "redecide-named-dotted",
        "h.md",
        &[
            (
                &[
                    ("h.md", Some("[[v1.md.md]]\n")),
                    ("v1.md.md.md", Some("")),
                    ("v1.md.md", Some("")),
                ],
                &[&["v1.md.md.md", "v1.md"]],
            ),
            (&[("v1.md", Some(""))], &[&["v1.md.md.md", "v1.md.md"]]),
            (&[("v1.md", None)], &[&["v1.md.md.md", "v1.md"]]),
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
/// across directories and across case, a dotted stem and its optional `.md` spellings, a
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
    let left = DerivedRows::read(store).expect("the derived rows");
    let right = DerivedRows::read(other).expect("the derived rows");
    left.projection()
        .assert_equivalent(right.projection(), subject);
    assert_eq!(left.fields(), right.fields(), "{subject}");
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
        FindingKind::VaultScheme,
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
/// moves the stem's class, a path, an optional `.md` class, or a holder, and the
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

/// What writing `zz/hub.md` cost a store under `order` holding `ignored`
/// documents `{place}/aNNNN/hub.md`, which `archive/**` keeps out of the class
/// `hub` opens where the root's order matches `place` to it, beside
/// `holder.md` linking `[[hub]]`: the counters the write moved, the steps its
/// re-decision took, and the findings it left at `holder.md`.
fn hub_write_beside_ignored(
    order: StoredPathOrder,
    place: &str,
    ignored: usize,
) -> (DerivationCounters, u64, Vec<Filed>) {
    let mut vault = Vault::new(
        &format!("redecide-ignored-{order:?}-{place}-{ignored}"),
        order,
    );
    let mut documents: Vec<(String, &str)> = (0..ignored)
        .map(|at| (format!("{place}/a{at:04}/hub.md"), "hub\n"))
        .collect();
    documents.push(("holder.md".to_string(), "[[hub]]\n"));
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
            [Change::Upsert(derived("zz/hub.md", "hub\n"))],
            &[],
            &declared(),
        )
        .expect("writing the hub");
    let steps = request.read_steps();
    let counters = request.finish();
    (counters, steps, vault.findings("holder.md"))
}

/// **A write's work does not follow the members its class keeps out.** Two
/// thousand documents `archive/aNNNN/hub.md` under the ignored `archive/**`
/// sort ahead of `zz/hub.md`, and writing it re-decides the broken `[[hub]]`
/// in the same steps beside them as beside twenty: the head the re-decision
/// reads seeks the members the class admits, never the ones it keeps out, so
/// the link resolves to the one document written and its broken finding goes.
/// On a root that folds case `Archive/aNNNN/hub.md` is the same ignored place,
/// so its stored admitting count keeps it out of the class just the same.
#[test]
fn a_writes_work_does_not_follow_the_ignored_members_of_its_class() {
    for (order, place) in [
        (Sensitive, "archive"),
        (Folding, "archive"),
        (Folding, "Archive"),
    ] {
        let at = format!("{order:?} {place}");
        let (few, few_steps, few_filed) = hub_write_beside_ignored(order, place, 20);
        let (many, many_steps, many_filed) = hub_write_beside_ignored(order, place, 2000);
        assert_eq!(few_filed, Vec::new(), "{at}");
        assert_eq!(many_filed, Vec::new(), "{at}");
        assert_eq!(counted(&many, "links_redecided"), 1, "{at}");
        assert_eq!(counted(&many, "link_health_keys_resolved"), 1, "{at}");
        assert_eq!(counted(&many, "findings_discarded"), 1, "{at}");
        snapshot(&many).assert_equal_counts(
            &snapshot(&few),
            &format!("{at}: a hundred times the ignored members"),
        );
        assert_eq!(
            many_steps, few_steps,
            "{at}: a hundred times the ignored members moved the re-decision's work"
        );
    }
}

/// Six hundred links to `hub.md` share the keys `hub/` and `hub.md/`.
/// Changing `hub/` rejudges them in chunks while resolving each shared
/// key and reading its candidates once.
#[test]
fn a_key_every_chunk_holds_is_resolved_once() {
    let mut vault = Vault::new("redecide-cross-class-chunks", Sensitive);
    let mut documents: Vec<(String, &str)> = (0..2)
        .map(|at| (format!("m/{at:03}/hub.md"), "hub\n"))
        .collect();
    documents.extend((0..600).map(|at| (format!("h/{at:03}.md"), "[[hub.md]]\n")));
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
    let counters = request.finish();
    assert_eq!(counted(&counters, "links_redecided"), 600);
    assert_eq!(counted(&counters, "link_health_keys_resolved"), 2);
    // `hub/` names the three hubs, and `hub.md/` names nothing.
    assert_eq!(counted(&counters, "link_health_candidates_read"), 3);
    assert_eq!(counted(&counters, "findings_written"), 600);
}

/// **A key's total is counted once per changeset, however its chunks fall.**
/// Sixty documents the stem `hub` names fill the head of `hub/`, so what it
/// names is counted rather than read whole. One changeset writes six
/// documents of 256 links each, so its written documents are read in six
/// chunks, and only the first, third and fifth hold `[[hub]]`: the chunks
/// between hold `[[elsewhere]]`. The key is resolved in each chunk that holds
/// it, and only its first resolution counts the sixty; the other two read its
/// head alone, five rows each.
#[test]
fn a_filled_keys_total_is_counted_once_across_chunks_apart() {
    let mut vault = Vault::new("redecide-filled-key-apart", Sensitive);
    vault.apply(
        (0..60)
            .map(|at| Change::Upsert(derived(&format!("m/{at:03}/hub.md"), "hub\n")))
            .collect::<Vec<_>>(),
    );

    let hubs = "[[hub]]\n".repeat(256);
    let elsewhere = "[[elsewhere]]\n".repeat(256);
    let written: Vec<Change> = (0..6)
        .map(|at| {
            let body = if at % 2 == 0 { &hubs } else { &elsewhere };
            Change::Upsert(derived(&format!("w/{at}.md"), body))
        })
        .collect();
    let mut request = vault.store.begin_request();
    request
        .apply_increment(IncrementProvenance::Derived, written, &[], &declared())
        .expect("writing six documents");
    let counters = request.finish();
    assert_eq!(counted(&counters, "links_redecided"), 6 * 256);
    // `hub/` in the three chunks that hold it, and `elsewhere/` in the three
    // between: no chunk holds a key the chunk before it held.
    assert_eq!(counted(&counters, "link_health_keys_resolved"), 6);
    // The sixty counted once, and a head of five read twice more.
    assert_eq!(
        counted(&counters, "link_health_candidates_read"),
        60 + 2 * 5
    );
    assert_eq!(counted(&counters, "findings_written"), 6 * 256);
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
    eprintln!(
        "hub.md write steps: 100 under hub/ {few}, 2000 under hub/ {many}, 2000 elsewhere {elsewhere}"
    );
    assert_eq!(
        (few, many),
        (elsewhere, elsewhere),
        "path links under the folder `hub/` moved the work of a write to the class `hub/`"
    );
}

/// What killing `deaths` documents `d/NNNNN.md` in one changeset cost a store
/// holding them beside a hundred documents that each link `[[elsewhere]]`,
/// and — where `neighborhood` — a document `h/NNNNN.md` for each that links
/// it by its stem and by its path: the counters the changeset moved, the
/// steps its re-decision took, and how long the changeset took.
fn mass_delete(
    deaths: usize,
    neighborhood: bool,
) -> (DerivationCounters, u64, std::time::Duration) {
    let label = format!("redecide-mass-delete-{deaths}-{neighborhood}");
    let mut vault = Vault::new(&label, Sensitive);
    let mut documents: Vec<(String, String)> = (0..deaths)
        .map(|at| (format!("d/{at:05}.md"), format!("alpha w{at}\n")))
        .collect();
    documents.extend((0..100).map(|at| (format!("u/{at:03}.md"), "[[elsewhere]]\n".to_string())));
    if neighborhood {
        documents.extend((0..deaths).map(|at| {
            (
                format!("h/{at:05}.md"),
                format!("[[{at:05}]]\n\n[p](../d/{at:05}.md)\n"),
            )
        }));
    }
    vault.apply(
        documents
            .iter()
            .map(|(at, body)| Change::Upsert(derived(at, body)))
            .collect::<Vec<_>>(),
    );

    let mut request = vault.store.begin_request();
    let started = std::time::Instant::now();
    request
        .apply_increment(
            IncrementProvenance::Derived,
            (0..deaths)
                .map(|at| death(&format!("d/{at:05}.md")))
                .collect::<Vec<_>>(),
            &[],
            &declared(),
        )
        .expect("the mass delete");
    let took = started.elapsed();
    let steps = request.read_steps();
    (request.finish(), steps, took)
}

/// **A death no link reaches costs its re-decision a small constant.** A
/// mass delete names a class and a path per document it kills, and most name
/// ones no link points at: killing six hundred such documents re-decides no
/// link, and its re-decision's steps stay within [`STEPS_PER_EMPTY_KEY`] per
/// class and path it names. The same changeset beside a link to each by its
/// stem and by its path re-decides those twelve hundred links, and what they
/// cost is the links' own work on top.
#[test]
fn a_mass_delete_no_link_reaches_costs_a_constant_per_key() {
    let (quiet, quiet_steps, quiet_took) = mass_delete(600, false);
    let (busy, busy_steps, busy_took) = mass_delete(600, true);
    eprintln!(
        "600 deaths: no neighborhood {quiet_steps} steps in {quiet_took:?}, \
         a link each {busy_steps} steps in {busy_took:?}"
    );
    assert_eq!(counted(&quiet, "links_redecided"), 0);
    assert_eq!(counted(&busy, "links_redecided"), 1200);
    let keys = 600 * 2;
    assert!(
        quiet_steps <= STEPS_PER_EMPTY_KEY * keys,
        "six hundred deaths no link reaches took {quiet_steps} steps, more than \
         {STEPS_PER_EMPTY_KEY} for each of the {keys} classes and paths they name"
    );
    assert!(
        busy_steps > quiet_steps,
        "the links the deaths reach cost nothing: {busy_steps} against {quiet_steps}"
    );
}

// ---- an in-place rewrite ----

/// The hub the in-place cases rewrite: two headings, a block, and two links
/// of its own — one to a document that stands, one to nothing.
const HUB: &str = "# Intro\n\n## Intro Two\n\nA paragraph. ^blk\n\n[[own]]\n\n[[nowhere]]\n";

/// [`HUB`] with its first heading, its second and its block written as
/// given, and `added` between them.
fn hub_body(intro: &str, two: &str, added: &str, block: &str) -> String {
    format!("{intro}\n\n{added}{two}\n\nA paragraph. {block}\n\n[[own]]\n\n[[nowhere]]\n")
}

/// The links [`HUB`] holds.
const HUB_OWN_LINKS: u64 = 2;

/// What each holder `h/NNN.md` links the hub by: by its stem, with a heading
/// it lacks, with a heading and the block it holds, by its path, bare and
/// with a heading it lacks, and by its stem with the second heading's slug.
/// The path link with the heading it lacks files a missing anchor keyed by
/// the path `hub.md` and, through its candidate's name `hub`, under the class
/// `hub/`, which no key of the link reaches.
const IN_LINKS: &str = "[[hub]]\n\n[[hub#Nope]]\n\n[[hub#Intro]]\n\n[[hub#^blk]]\n\n\
                        [p](../hub.md)\n\n[q](../hub.md#Nope)\n\n[[hub#intro-two]]\n";

/// The links [`IN_LINKS`] holds.
const IN_LINKS_PER_HOLDER: u64 = 7;

/// A vault under `order` holding `hub.md` as [`HUB`], `own.md`, and
/// `holders` documents `h/NNN.md` each holding [`IN_LINKS`], beside the
/// documents it holds.
fn hub_vault(
    label: &str,
    order: StoredPathOrder,
    holders: usize,
) -> (Vault, BTreeMap<String, String>) {
    let mut vault = Vault::new(label, order);
    let mut held: BTreeMap<String, String> = BTreeMap::from([
        ("hub.md".to_string(), HUB.to_string()),
        ("own.md".to_string(), "own\n".to_string()),
    ]);
    held.extend((0..holders).map(|at| (format!("h/{at:03}.md"), IN_LINKS.to_string())));
    vault.apply(
        held.iter()
            .map(|(at, body)| Change::Upsert(derived(at, body)))
            .collect::<Vec<_>>(),
    );
    (vault, held)
}

/// Every finding `store` holds at a path other than `except`, as the
/// testkit's comparator reads them.
fn findings_beside(
    store: &mut Store,
    except: &str,
) -> Vec<norn_testkit::equivalence::ProjectedFinding> {
    StoreProjection::read(store)
        .expect("a projection")
        .findings()
        .iter()
        .filter(|finding| finding.path != except)
        .cloned()
        .collect()
}

/// Apply `changes` to `vault` as one changeset, and hand back what it
/// derived and the steps its re-decision took.
fn measured(vault: &mut Vault, changes: Vec<Change>) -> (DerivationCounters, u64) {
    let mut request = vault.store.begin_request();
    request
        .apply_increment(IncrementProvenance::Derived, changes, &[], &declared())
        .unwrap_or_else(|refusal| panic!("{:?}: a changeset: {refusal}", vault.order));
    let steps = request.read_steps();
    (request.finish(), steps)
}

/// **An in-place rewrite that moves no anchor value re-decides only its own
/// links** (ADR 0047). `hub.md` is linked by every holder six ways — by its
/// stem and by its path, with anchors it holds and lacks, one of them a path
/// link whose missing anchor is filed under the class `hub/` through its
/// candidate's name. Rewriting its body with its heading and block kept, and
/// its own links changed, re-decides exactly its own three links, discards
/// only the finding standing at its own path, and leaves every holder's
/// findings as they stood — which are the findings a rebuild files. The
/// write's work is the same beside twenty holders as beside two hundred.
#[test]
fn an_in_place_rewrite_that_moves_no_anchor_redecides_only_its_own_links() {
    for order in [Sensitive, Folding] {
        let mut steps = BTreeMap::new();
        for holders in [20, 200] {
            let label = format!("redecide-in-place-{order:?}-{holders}");
            let (mut vault, mut held) = hub_vault(&label, order, holders);
            let before = findings_beside(&mut vault.store, "hub.md");
            assert!(
                before.iter().any(|finding| {
                    finding.path == "h/000.md"
                        && finding.kind == FindingKind::MissingAnchor.as_str()
                        && finding.path_keys.contains("hub.md")
                        && finding.class_keys.contains("hub/")
                }),
                "{order:?}: no holder's path link carries a finding under its candidate's class: \
                 {before:#?}"
            );
            assert_eq!(vault.findings("hub.md"), [broken(1)], "{order:?}");

            let rewritten = "# Intro\n\nA line before the second heading.\n\n## Intro Two\n\n\
                             The paragraph, reworded. ^blk\n\n[[own]]\n\n[[nowhere]]\n\n\
                             [[elsewhere]]\n";
            held.insert("hub.md".to_string(), rewritten.to_string());
            let (counters, taken) = measured(
                &mut vault,
                vec![Change::Upsert(derived("hub.md", rewritten))],
            );
            let at = format!("{order:?} beside {holders} holders");
            assert_eq!(counted(&counters, "links_redecided"), 3, "{at}");
            assert_eq!(counted(&counters, "findings_discarded"), 1, "{at}");
            assert_eq!(counted(&counters, "findings_written"), 2, "{at}");
            assert_eq!(
                findings_beside(&mut vault.store, "hub.md"),
                before,
                "{at}: a holder's findings moved"
            );
            assert_eq!(vault.findings("hub.md"), [broken(1), broken(2)], "{at}");
            assert_rebuilds(&mut vault.store, order, &held, &at);
            steps.insert(holders, taken);
        }
        assert_eq!(
            steps[&20], steps[&200],
            "{order:?}: ten times the holders moved an in-place rewrite's work: {steps:?}"
        );
    }
}

/// **An in-place rewrite that moves an anchor value re-decides the links
/// naming what it moved** (ADR 0047): beside its own links, exactly the
/// anchored links that could name the document and name a moved heading
/// reading, heading slug or block id. Each rewrite here moves one value — a
/// heading renamed, a heading added, a heading's reading moved where its slug
/// stays, a slug moved where its reading stays, a block id renamed — and the
/// next restores it, so each missing anchor it moves is filed and ended once
/// each way, each time at the link's one place, and the store equals a
/// rebuild after every step. Each holder names the hub seven ways, so the
/// value a step moves reaches one or two of them and the rest stand. A
/// heading respelled only in case moves neither its reading nor its slug, so
/// it is an in-place rewrite that re-decides its own links alone.
#[test]
fn an_in_place_rewrite_that_moves_an_anchor_redecides_the_links_naming_it() {
    const HOLDERS: usize = 20;
    // Its own links, and in each holder the links naming the moved value:
    // `[[hub#Intro]]` by the reading `intro`, `[[hub#intro-two]]` by the slug
    // `intro-two`, `[[hub#^blk]]` by the block `blk`, and both links naming
    // `Nope` by the reading `nope`.
    let naming = |per_holder: u64| HUB_OWN_LINKS + per_holder * HOLDERS as u64;
    let reach = naming(1);
    // The ordinals of the holder's links missing their anchor: the two naming
    // `Nope` unless `nope`, and each other anchored link named.
    let holder_with = |nope: bool, missing: &[u64]| {
        let mut ordinals: Vec<u64> = missing.to_vec();
        if !nope {
            ordinals.extend([1, 5]);
        }
        ordinals.sort();
        ordinals
            .into_iter()
            .map(|ordinal| missing_anchor(ordinal, "hub.md"))
            .collect::<Vec<Filed>>()
    };
    let standing = holder_with(false, &[]);
    let hub = |intro: &str, two: &str, added: &str, block: &str| hub_body(intro, two, added, block);
    let script: [(&str, String, u64, Vec<Filed>); 11] = [
        (
            "the heading renamed",
            hub("# Other", "## Intro Two", "", "^blk"),
            reach,
            holder_with(false, &[2]),
        ),
        (
            "the heading restored",
            HUB.to_string(),
            reach,
            standing.clone(),
        ),
        (
            "a heading added",
            hub("# Intro", "## Intro Two", "## Nope\n\n", "^blk"),
            naming(2),
            holder_with(true, &[]),
        ),
        (
            "the heading removed",
            HUB.to_string(),
            naming(2),
            standing.clone(),
        ),
        (
            "a reading moved where the slug stays",
            hub("# Intro!", "## Intro Two", "", "^blk"),
            reach,
            holder_with(false, &[2]),
        ),
        (
            "the reading restored",
            HUB.to_string(),
            reach,
            standing.clone(),
        ),
        (
            "a slug moved where the reading stays",
            hub("# Intro", "## Intro  Two", "", "^blk"),
            reach,
            holder_with(false, &[6]),
        ),
        (
            "the slug restored",
            HUB.to_string(),
            reach,
            standing.clone(),
        ),
        (
            "the block renamed",
            hub("# Intro", "## Intro Two", "", "^b2"),
            reach,
            holder_with(false, &[3]),
        ),
        (
            "the block restored",
            HUB.to_string(),
            reach,
            standing.clone(),
        ),
        (
            "a heading respelled only in case",
            hub("# INTRO", "## intro two", "", "^blk"),
            HUB_OWN_LINKS,
            standing.clone(),
        ),
    ];
    for order in [Sensitive, Folding] {
        let label = format!("redecide-in-place-anchor-{order:?}");
        let (mut vault, mut held) = hub_vault(&label, order, HOLDERS);
        assert_eq!(vault.findings("h/000.md"), standing, "{order:?}");
        for (said, body, redecided, expected) in &script {
            let at = format!("{order:?}: {said}");
            held.insert("hub.md".to_string(), body.clone());
            let (counters, _) = measured(&mut vault, vec![Change::Upsert(derived("hub.md", body))]);
            assert_eq!(counted(&counters, "links_redecided"), *redecided, "{at}");
            for holder in ["h/000.md", "h/019.md"] {
                assert_eq!(&vault.findings(holder), expected, "{at}: {holder}");
            }
            assert_rebuilds(&mut vault.store, order, &held, &at);
        }
    }
}

/// **A moved anchor's work follows the links naming the moved value**
/// (ADR 0047's write-work bar). `hub.md` is linked three ways: by holders
/// `n/NNN.md` naming the heading the rewrite renames, by holders `o/NNN.md`
/// naming its other heading, its block and a heading it lacks, and, beside a
/// second document of the same class `z/hub.md` holding the same heading, by
/// holders `s/NNN.md` naming that heading there. Renaming the heading
/// re-decides the hub's own links and the `n` links alone, so its work grows
/// with the `n` holders and is the same beside ten `o` and `s` holders as
/// beside a hundred of each, and the store equals a rebuild.
#[test]
fn a_moved_anchors_work_follows_the_links_naming_it() {
    let vault_of = |label: &str, order, naming: usize, others: usize| {
        let mut vault = Vault::new(label, order);
        let mut held: BTreeMap<String, String> = BTreeMap::from([
            ("hub.md".to_string(), HUB.to_string()),
            ("own.md".to_string(), "own\n".to_string()),
            ("z/hub.md".to_string(), "# Intro\n".to_string()),
        ]);
        held.extend((0..naming).map(|at| (format!("n/{at:03}.md"), "[[hub#Intro]]\n".to_string())));
        held.extend((0..others).map(|at| {
            (
                format!("o/{at:03}.md"),
                "[[hub#Intro Two]]\n\n[[hub#^blk]]\n\n[[hub#Nope]]\n".to_string(),
            )
        }));
        held.extend(
            (0..others).map(|at| (format!("s/{at:03}.md"), "[[z/hub#Intro]]\n".to_string())),
        );
        vault.apply(
            held.iter()
                .map(|(at, body)| Change::Upsert(derived(at, body)))
                .collect::<Vec<_>>(),
        );
        (vault, held)
    };
    let renamed = hub_body("# Other", "## Intro Two", "", "^blk");
    for order in [Sensitive, Folding] {
        let mut steps = BTreeMap::new();
        for (naming, others) in [(10, 10), (10, 100), (100, 10)] {
            let at = format!("{order:?} beside {naming} naming and {others} other holders");
            let (mut vault, mut held) = vault_of(
                &format!("redecide-anchor-work-{order:?}-{naming}-{others}"),
                order,
                naming,
                others,
            );
            held.insert("hub.md".to_string(), renamed.clone());
            let (counters, taken) = measured(
                &mut vault,
                vec![Change::Upsert(derived("hub.md", &renamed))],
            );
            assert_eq!(
                counted(&counters, "links_redecided"),
                HUB_OWN_LINKS + naming as u64,
                "{at}"
            );
            assert_rebuilds(&mut vault.store, order, &held, &at);
            steps.insert((naming, others), taken);
        }
        assert_eq!(
            steps[&(10, 10)],
            steps[&(10, 100)],
            "{order:?}: links naming other values or another document moved a renamed \
             heading's work: {steps:?}"
        );
        assert!(
            steps[&(100, 10)] > steps[&(10, 10)],
            "{order:?}: the links naming the renamed heading cost nothing: {steps:?}"
        );
    }
}

/// **A link reached by a names move and by an anchor move in one changeset
/// holds one finding** (ADR 0047). A path link `[q](./a/note.md#Gone)` misses
/// its anchor, and its finding is filed under the path `a/note.md` and,
/// through its candidate's name, under the class `note/`. One changeset adds
/// the heading `Gone` to `a/note.md` in place and gives birth to `b/note.md`,
/// so the class's findings pass judges the link through its candidate's
/// class and the anchor arm reaches it by its path and the moved value. The
/// store holds one finding about it, which a rebuild holds too, and the
/// reverse changeset restores the missing anchor.
#[test]
fn a_link_a_names_move_and_an_anchor_move_both_reach_holds_one_finding() {
    for order in [Sensitive, Folding] {
        let at = format!("{order:?}");
        let mut vault = Vault::new(&format!("redecide-anchor-and-names-{order:?}"), order);
        let mut held: BTreeMap<String, String> = BTreeMap::from([
            ("a/note.md".to_string(), "# Here\n".to_string()),
            ("h.md".to_string(), "[q](./a/note.md#Gone)\n".to_string()),
        ]);
        vault.apply(
            held.iter()
                .map(|(at, body)| Change::Upsert(derived(at, body)))
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            vault.findings("h.md"),
            [missing_anchor(0, "a/note.md")],
            "{at}"
        );
        held.insert("a/note.md".to_string(), "# Gone\n".to_string());
        held.insert("b/note.md".to_string(), "b\n".to_string());
        vault.apply(vec![
            Change::Upsert(derived("a/note.md", "# Gone\n")),
            Change::Upsert(derived("b/note.md", "b\n")),
        ]);
        assert_eq!(vault.findings("h.md"), [], "{at}: the anchor now stands");
        assert_rebuilds(&mut vault.store, order, &held, &at);
        held.insert("a/note.md".to_string(), "# Here\n".to_string());
        held.remove("b/note.md");
        vault.apply(vec![
            Change::Upsert(derived("a/note.md", "# Here\n")),
            death("b/note.md"),
        ]);
        assert_eq!(
            vault.findings("h.md"),
            [missing_anchor(0, "a/note.md")],
            "{at}"
        );
        assert_rebuilds(&mut vault.store, order, &held, &at);
    }
}

/// What one changeset carrying `findings` re-decided.
fn redecided_by(vault: &mut Vault, changes: Vec<Change>, findings: &[DerivedFinding<'_>]) -> u64 {
    let mut request = vault.store.begin_request();
    request
        .apply_increment(IncrementProvenance::Derived, changes, findings, &declared())
        .unwrap_or_else(|refusal| panic!("{:?}: a changeset: {refusal}", vault.order));
    counted(&request.finish(), "links_redecided")
}

/// **A moved marked reading reaches the links naming it** (ADR 0047). A
/// heading anchor is matched by its marked reading too, the text past its
/// `#` markers, so `[[t### Intro]]` names the heading `Intro`. Adding the
/// heading ends the link's missing anchor and removing it files the finding
/// again, each time as a rebuild does.
#[test]
fn a_moved_marked_reading_reaches_the_links_naming_it() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::new(&format!("redecide-marked-reading-{order:?}"), order);
        let mut held = BTreeMap::from([
            ("t.md".to_string(), "# Other\n".to_string()),
            ("h.md".to_string(), "[[t### Intro]]\n".to_string()),
        ]);
        vault.write(&[("t.md", "# Other\n"), ("h.md", "[[t### Intro]]\n")]);
        assert_eq!(
            vault.findings("h.md"),
            [missing_anchor(0, "t.md")],
            "{order:?}"
        );
        for (body, expected) in [
            ("# Intro\n", vec![]),
            ("# Other\n", vec![missing_anchor(0, "t.md")]),
        ] {
            let at = format!("{order:?}: t.md as {body:?}");
            held.insert("t.md".to_string(), body.to_string());
            assert_eq!(
                redecided_by(&mut vault, upserts(&[("t.md", body)]), &[]),
                1,
                "{at}"
            );
            assert_eq!(vault.findings("h.md"), expected, "{at}");
            assert_rebuilds(&mut vault.store, order, &held, &at);
        }
    }
}

/// **A link two arms reach is judged once** (ADR 0047). Beside an in-place
/// rewrite that moves the anchor a link names, the link is also reached by a
/// second moved value it names, by a birth in its class, by its path, by a
/// directory mark over its path, and, for a link in the rewritten document
/// itself, by the write: each re-decides it exactly once, and its finding
/// ends.
#[test]
fn a_link_two_arms_reach_is_judged_once() {
    for order in [Sensitive, Folding] {
        let case = |label: &str| Vault::new(&format!("redecide-once-{label}-{order:?}"), order);

        // Its reading and its slug, both moved.
        let mut vault = case("two-values");
        vault.write(&[("t.md", "# Other\n"), ("h.md", "[[t#nope]]\n")]);
        assert_eq!(
            vault.findings("h.md"),
            [missing_anchor(0, "t.md")],
            "{order:?}"
        );
        let redecided = redecided_by(&mut vault, upserts(&[("t.md", "# nope\n")]), &[]);
        assert_eq!(
            (redecided, vault.findings("h.md")),
            (1, vec![]),
            "{order:?}: two values"
        );

        // A birth in its class.
        let mut vault = case("class");
        vault.write(&[("t.md", "# Other\n"), ("h.md", "[[t#Nope]]\n")]);
        let redecided = redecided_by(
            &mut vault,
            upserts(&[("t.md", "# Nope\n"), ("archive/t.md", "plain\n")]),
            &[],
        );
        assert_eq!(
            (redecided, vault.findings("h.md")),
            (1, vec![]),
            "{order:?}: class"
        );

        // A vault file born at the path it spells.
        let mut vault = case("path");
        vault.write(&[("v1.2.md", "# Other\n"), ("h.md", "[p](./v1.2#Nope)\n")]);
        let redecided = redecided_by(
            &mut vault,
            vec![
                Change::Upsert(derived("v1.2.md", "# Nope\n")),
                Change::FileBirth { path: path("v1.2") },
            ],
            &[],
        );
        assert_eq!(redecided, 1, "{order:?}: path");

        // A directory mark over its path.
        let mut vault = case("directory");
        vault.write(&[("x/t.md", "# Other\n"), ("h.md", "[p](/x/t.md#Nope)\n")]);
        assert_eq!(
            vault.findings("h.md"),
            [missing_anchor(0, "x/t.md")],
            "{order:?}"
        );
        let redecided = redecided_by(
            &mut vault,
            upserts(&[("x/t.md", "# Nope\n")]),
            &[DerivedFinding {
                facts: crate::health::directory_mark("x"),
                replaces: None,
                bytes_hash: None,
            }],
        );
        assert_eq!(redecided, 1, "{order:?}: directory");

        // A link in the rewritten document itself.
        let mut vault = case("self");
        vault.write(&[("t.md", "# Other\n\n[[#Nope]]\n")]);
        let redecided = redecided_by(
            &mut vault,
            upserts(&[("t.md", "# Nope\n\n[[#Nope]]\n")]),
            &[],
        );
        assert_eq!(
            (redecided, vault.findings("t.md")),
            (1, vec![]),
            "{order:?}: self"
        );
    }
}

/// **A hub rewritten twice in one changeset, across several pages of
/// in-links, equals a rebuild** (ADR 0047). Five hundred and nineteen
/// holders — more than two of the anchor arm's pages — name `t.md` seven
/// ways. One changeset rewrites it twice, moving its anchors each time, and
/// rewrites every fifth holder; the next moves them back beside a death of an
/// ignored twin and a birth in the class; the last moves one heading alone.
/// After each, the store equals a rebuild.
#[test]
fn a_hub_rewritten_twice_over_several_pages_equals_a_rebuild() {
    const HOLDERS: usize = 2 * 256 + 7;
    let links = [
        "[[t#Nope]]",
        "[[t.md#nope]]",
        "[[vault://t#Nope]]",
        "[p](./t.md#Nope)",
        "[[t### Nope]]",
        "[[t#A]]",
        "[[x/t#Nope]]",
    ];
    for order in [Sensitive, Folding] {
        let mut vault = Vault::new(&format!("redecide-hub-pages-{order:?}"), order);
        let mut held: BTreeMap<String, String> = BTreeMap::from([
            ("t.md".to_string(), "# A\n\n## Setup\n".to_string()),
            ("y/t.md".to_string(), "# Nope\n".to_string()),
            ("archive/t.md".to_string(), "# A\n".to_string()),
        ]);
        held.extend((0..HOLDERS).map(|at| {
            (
                format!("h/{at:03}.md"),
                format!("{}\n", links[at % links.len()]),
            )
        }));
        vault.apply(
            held.iter()
                .map(|(at, body)| Change::Upsert(derived(at, body)))
                .collect::<Vec<_>>(),
        );
        // `[[t#...]]` resolves to `t.md` alone once its twin is gone.
        held.remove("y/t.md");
        vault.kill("y/t.md");

        let mut changes = vec![Change::Upsert(derived("t.md", "# X\n\n## Setup\n"))];
        for at in (0..HOLDERS).step_by(5) {
            let at = format!("h/{at:03}.md");
            let body = format!("{}\n\nmore\n", held[&at].trim_end());
            held.insert(at.clone(), body.clone());
            changes.push(Change::Upsert(derived(&at, &body)));
        }
        held.insert("t.md".to_string(), "# Nope\n\n## Setup\n".to_string());
        changes.push(Change::Upsert(derived("t.md", "# Nope\n\n## Setup\n")));
        vault.apply(changes);
        assert_rebuilds(&mut vault.store, order, &held, &format!("{order:?}: twice"));

        held.insert("t.md".to_string(), "# A\n\n## Setup\n".to_string());
        held.remove("archive/t.md");
        held.insert("x/t.md".to_string(), "# Nope\n".to_string());
        vault.apply(vec![
            Change::Upsert(derived("t.md", "# A\n\n## Setup\n")),
            death("archive/t.md"),
            Change::Upsert(derived("x/t.md", "# Nope\n")),
        ]);
        assert_rebuilds(&mut vault.store, order, &held, &format!("{order:?}: back"));

        held.insert("t.md".to_string(), "# Nope\n".to_string());
        vault.write(&[("t.md", "# Nope\n")]);
        assert_rebuilds(
            &mut vault.store,
            order,
            &held,
            &format!("{order:?}: one heading"),
        );
    }
}

/// **A write that is not an in-place rewrite moves its names and reaches
/// every link to them** (ADR 0047): a birth, a death, a rename, a respelling
/// only a folding comparison calls the same path, a readable document
/// becoming names alone and a names-only row becoming readable — with the
/// same anchors on both sides, and with none, where only whether the body is
/// known moved. Each re-decides every link the hub's class and path reach,
/// and the store equals a rebuild after every step. The one in-place step
/// the script needs to get back to the hub's anchors re-decides only the
/// links naming them.
#[test]
fn a_write_that_is_not_in_place_reaches_every_link_to_its_names() {
    const HOLDERS: u64 = 20;
    let in_links = IN_LINKS_PER_HOLDER * HOLDERS;
    let in_suffix = (IN_LINKS_PER_HOLDER - 1) * HOLDERS;
    for order in [Sensitive, Folding] {
        let label = format!("redecide-not-in-place-{order:?}");
        let (mut vault, mut held) = hub_vault(&label, order, HOLDERS as usize);
        let names_alone = || {
            vec![Change::Unreadable {
                path: path("hub.md"),
            }]
        };
        let script: [(&str, Vec<Change>, u64, usize); 10] = [
            ("the hub's names alone", names_alone(), in_links, 0),
            (
                "the hub readable again",
                upserts(&[("hub.md", HUB)]),
                in_links + HUB_OWN_LINKS,
                2,
            ),
            ("the hub's names alone again", names_alone(), in_links, 0),
            (
                "the hub readable with no anchor",
                upserts(&[("hub.md", "")]),
                in_links,
                5,
            ),
            // Readable over readable at the same path, so an in-place rewrite:
            // its headings and block are born, and the links naming them —
            // `[[hub#Intro]]`, `[[hub#^blk]]` and `[[hub#intro-two]]` in each
            // holder — are re-decided beside its own.
            (
                "the hub as it was",
                upserts(&[("hub.md", HUB)]),
                HUB_OWN_LINKS + 3 * HOLDERS,
                2,
            ),
            (
                "the hub respelled by case",
                vec![death("hub.md"), Change::Upsert(derived("Hub.md", HUB))],
                in_links + HUB_OWN_LINKS,
                2,
            ),
            (
                "the hub respelled back",
                vec![Change::Upsert(derived("hub.md", HUB)), death("Hub.md")],
                in_links + HUB_OWN_LINKS,
                2,
            ),
            // A birth elsewhere in the class reaches the holders' suffix links
            // and the path link whose finding names its candidate in the class,
            // and no other path link.
            (
                "a second hub born",
                upserts(&[("x/hub.md", "")]),
                in_suffix,
                2,
            ),
            ("the second hub dead", vec![death("x/hub.md")], in_suffix, 2),
            // Renamed out of its path, the hub breaks both path links, and its
            // stem still names it.
            (
                "the hub renamed",
                vec![death("hub.md"), Change::Upsert(derived("y/hub.md", HUB))],
                in_links + HUB_OWN_LINKS,
                3,
            ),
        ];
        let mut readable = true;
        for (said, changes, reach, filed) in script {
            let at = format!("{order:?}: {said}");
            for change in &changes {
                match change {
                    Change::Upsert(facts) => {
                        readable = true;
                        held.insert(facts.path.as_str().to_string(), facts.body.clone());
                    }
                    Change::Death { path, .. } => {
                        held.remove(path.as_str());
                    }
                    Change::Unreadable { .. } => readable = false,
                    other => unreachable!("{other:?}"),
                }
            }
            let (counters, _) = measured(&mut vault, changes);
            assert_eq!(counted(&counters, "links_redecided"), reach, "{at}");
            assert_eq!(vault.findings("h/000.md").len(), filed, "{at}");
            // A names-only row is written by its own entry, which a rebuild
            // of readable documents does not carry; the script under
            // `narrowed_rewrites_equal_a_rebuild_at_every_changeset_size` does.
            if readable {
                assert_rebuilds(&mut vault.store, order, &held, &at);
            }
        }
    }
}

/// **A row whose admitting count a re-pin moves is not rewritten in place**
/// (ADR 0047). `[[hub]]` names `a/hub.md` alone while `archive/**` keeps
/// `archive/hub.md` out of the class, and both alone once a schema declaring
/// no glob is pinned. The heal after the pin writes the holder first, which
/// judges its link against the count the old glob left on `archive/hub.md`,
/// and `a/hub.md`, whose count stays, in place; writing `archive/hub.md` moves
/// its count, so it reaches the holder's link, which is ambiguous, as a
/// rebuild under the new schema files it. Pinning the glob back converges the
/// same way.
#[test]
fn a_row_whose_admitting_count_moved_reaches_every_link_to_it() {
    for order in [Sensitive, Folding] {
        let label = format!("redecide-admitting-{order:?}");
        let scratch = Scratch::new(&label);
        let mut store = scratch.open_under(order);
        pin_schema(&mut store, 0);
        let held: BTreeMap<String, String> = BTreeMap::from([
            ("holder.md".to_string(), "[[hub]]\n".to_string()),
            ("a/hub.md".to_string(), "a\n".to_string()),
            ("archive/hub.md".to_string(), "archived\n".to_string()),
        ]);
        apply_under(
            &mut store,
            held.iter()
                .map(|(at, body)| Change::Upsert(derived(at, body)))
                .collect(),
            0,
            &label,
        );
        assert_eq!(filed(&mut store, "holder.md"), [], "{order:?}");
        for (schema, after) in [
            (2, vec![ambiguous(0, &["a/hub.md", "archive/hub.md"], 2)]),
            (0, Vec::new()),
        ] {
            let at = format!("{order:?} under {}", SCHEMAS[schema].0);
            pin_schema(&mut store, schema);
            for (written, reach) in [("holder.md", 1), ("a/hub.md", 0), ("archive/hub.md", 1)] {
                let mut request = store.begin_request();
                request
                    .apply_increment(
                        IncrementProvenance::Derived,
                        [Change::Upsert(derived(written, &held[written]))],
                        &[],
                        &declared_by(schema),
                    )
                    .unwrap_or_else(|refusal| panic!("{at}: {refusal}"));
                assert_eq!(
                    counted(&request.finish(), "links_redecided"),
                    reach,
                    "{at}: writing {written}"
                );
            }
            assert_eq!(filed(&mut store, "holder.md"), after, "{at}");
            let rebuilt_scratch = Scratch::new(&format!("{label}-rebuilt-{schema}"));
            let mut rebuilt = rebuilt_scratch.open_under(order);
            pin_schema(&mut rebuilt, schema);
            apply_under(
                &mut rebuilt,
                held.iter()
                    .map(|(at, body)| Change::Upsert(derived(at, body)))
                    .collect(),
                schema,
                &at,
            );
            assert_same(&mut store, &mut rebuilt, &at);
        }
    }
}

// ---- incremental equals rebuild over in-place rewrites ----

/// The lines a document's anchors are made of in [`mixed_step`]: headings,
/// one a case twin of another, one whose reading is another's slug, one
/// whose reading is a block id, an empty one and a non-ASCII one, and
/// paragraphs carrying block ids.
const ANCHOR_LINES: [&str; 14] = [
    "# Intro",
    "## Setup",
    "# intro",
    "A paragraph. ^blk",
    "An item. ^b2",
    "## Nope",
    "# Intro Two",
    "# intro-two",
    "### Deep",
    "# blk",
    "# setup",
    "#",
    "# Café",
    "Other para. ^nope",
];

/// Links naming the values [`ANCHOR_LINES`] holds, in each way a link is
/// read: by suffix and by path, with and without `.md`, rooted and by
/// `vault://`, by text reading, marked reading and slug, by block id, and
/// within the holding document — so a moved anchor value reaches some link
/// in most scripts.
const ANCHORED_LINKS: [&str; 36] = [
    "[[t#Nope]]",
    "[[t#nope]]",
    "[[t#Setup]]",
    "[[x/t#setup]]",
    "[[t#^b2]]",
    "[p](./t.md#Nope)",
    "[p](../t.md#Intro)",
    "[[vault://t#Setup]]",
    "[[h#Intro]]",
    "[[#Nope]]",
    "[[#^b2]]",
    "[[a#^blk]]",
    "[[t### Intro]]",
    "[[t#### Deep]]",
    "[[t#intro-two]]",
    "[[t#Intro Two]]",
    "[[t.md#Setup]]",
    "[[vault://x/t#Intro]]",
    "[[vault://t.md#intro-two]]",
    "[[T#Intro]]",
    "![[t#Intro]]",
    "[[t#blk]]",
    "[[t#^nope]]",
    "[[t/x#Intro]]",
    "[[v1.2#Intro]]",
    "[[v1.2.md#Setup]]",
    "[[t.md.md#Deep]]",
    "[[t###]]",
    "[[t## Setup]]",
    "[p](/t.md#Setup)",
    "[p](t.md#intro-two)",
    "[[x/t#Café]]",
    "[[t#Cafe]]",
    "[[#Intro Two]]",
    "[[archive/t#Intro]]",
    "[[u#Intro]]",
];

/// The lines a body's rewritable rest is made of: [`LINES`] without its
/// anchors, [`ANCHORED_LINKS`], and plain prose.
fn rest_line(rng: &mut Rng) -> &'static str {
    loop {
        let line = match rng.below(5) {
            0 => rng.pick(&["Plain words.", "Other words.", "A third remark."]),
            1 => rng.pick(&ANCHORED_LINKS),
            _ => rng.pick(&LINES),
        };
        if !ANCHOR_LINES.contains(&line) && !LINES[..5].contains(&line) {
            return line;
        }
    }
}

/// One document as [`mixed_step`] holds it: its anchor lines in order, and
/// the rest of its lines, each placed after the anchor line it follows.
#[derive(Clone, Debug)]
struct Composed {
    anchors: Vec<&'static str>,
    rest: Vec<(usize, &'static str)>,
}

impl Composed {
    fn random(rng: &mut Rng) -> Self {
        let anchors: Vec<&'static str> =
            (0..rng.below(4)).map(|_| rng.pick(&ANCHOR_LINES)).collect();
        let mut composed = Composed {
            anchors,
            rest: Vec::new(),
        };
        composed.rewrite_rest(rng);
        composed
    }

    /// Its rest drawn again, its anchor lines kept in order: a rewrite in
    /// place that moves no anchor value.
    fn rewrite_rest(&mut self, rng: &mut Rng) {
        let mut rest: Vec<(usize, &'static str)> = (0..rng.below(6))
            .map(|_| (rng.below(self.anchors.len() + 1), rest_line(rng)))
            .collect();
        rest.sort_by_key(|(after, _)| *after);
        self.rest = rest;
    }

    /// One anchor line added, removed or replaced, its rest kept.
    fn move_an_anchor(&mut self, rng: &mut Rng) {
        match (rng.below(3), self.anchors.is_empty()) {
            (0, _) | (_, true) => {
                let at = rng.below(self.anchors.len() + 1);
                self.anchors.insert(at, rng.pick(&ANCHOR_LINES));
            }
            (1, false) => {
                self.anchors.remove(rng.below(self.anchors.len()));
            }
            _ => {
                let at = rng.below(self.anchors.len());
                self.anchors[at] = rng.pick(&ANCHOR_LINES);
            }
        }
        let last = self.anchors.len();
        for (after, _) in &mut self.rest {
            *after = (*after).min(last);
        }
    }

    fn body(&self) -> String {
        let mut body = String::new();
        let mut rest = self.rest.iter().peekable();
        for at in 0..=self.anchors.len() {
            while let Some((_, line)) = rest.next_if(|(after, _)| *after == at) {
                body.push_str(line);
                body.push_str("\n\n");
            }
            if let Some(anchor) = self.anchors.get(at) {
                body.push_str(anchor);
                body.push_str("\n\n");
            }
        }
        body
    }
}

/// What a path holds in [`mixed_step`]'s script: a readable document, or its
/// names alone.
type MixedHeld = BTreeMap<String, Option<Composed>>;

/// The change writing what `held` holds at `at`.
fn mixed_change(at: &str, held: &Option<Composed>) -> Change {
    match held {
        Some(composed) => Change::Upsert(derived(at, &composed.body())),
        None => Change::Unreadable { path: path(at) },
    }
}

/// The spelling `held` already holds of the path `at` names under `order`.
fn mixed_spelling(order: StoredPathOrder, held: &MixedHeld, at: &str) -> Option<String> {
    match order {
        Sensitive => held.contains_key(at).then(|| at.to_string()),
        Folding => held
            .keys()
            .find(|spelled| spelled.eq_ignore_ascii_case(at))
            .cloned(),
    }
}

/// One changeset of exactly `size` entries over the paths `held` holds on a
/// root under `order`, weighted toward rewrites in place: a live document's
/// body rewritten with its anchors kept, or as often with one anchor moved; a
/// fresh write; a death; a rename; a readable document left with its names
/// alone; and a names-only row read again. Beside the changes it hands back
/// the path key of each readable document whose anchor it moved in place.
fn mixed_step(
    rng: &mut Rng,
    order: StoredPathOrder,
    held: &mut MixedHeld,
    size: usize,
) -> (Vec<Change>, String, BTreeSet<String>) {
    let mut changes = Vec::new();
    let mut said = Vec::new();
    let mut anchors_moved = BTreeSet::new();
    while changes.len() < size {
        let live: Vec<String> = held.keys().cloned().collect();
        let pick_live = |rng: &mut Rng| live[rng.below(live.len())].clone();
        let roll = if live.is_empty() { 0 } else { rng.below(20) };
        match roll {
            0..=2 => {
                let at = rng.pick(&PLACES).to_string();
                let at = mixed_spelling(order, held, &at).unwrap_or(at);
                let composed = Composed::random(rng);
                said.push(format!("write {at} {:?}", composed.body()));
                changes.push(Change::Upsert(derived(&at, &composed.body())));
                held.insert(at, Some(composed));
            }
            3..=12 => {
                let at = pick_live(rng);
                let readable = held[&at].is_some();
                let mut composed = held[&at].clone().unwrap_or_else(|| Composed::random(rng));
                if roll <= 7 {
                    composed.rewrite_rest(rng);
                    said.push(format!("rewrite {at} in place {:?}", composed.body()));
                } else {
                    composed.move_an_anchor(rng);
                    said.push(format!("move an anchor of {at} {:?}", composed.body()));
                    if readable {
                        anchors_moved.insert(path(&at).path_key().to_string());
                    }
                }
                changes.push(Change::Upsert(derived(&at, &composed.body())));
                held.insert(at, Some(composed));
            }
            13 | 14 => {
                let at = pick_live(rng);
                said.push(format!("kill {at}"));
                changes.push(death(&at));
                held.remove(&at);
            }
            15 | 16 => {
                let at = pick_live(rng);
                said.push(format!("{at} unreadable"));
                changes.push(Change::Unreadable { path: path(&at) });
                held.insert(at, None);
            }
            _ => {
                let from = pick_live(rng);
                let to = rng.pick(&PLACES).to_string();
                if from == to
                    || mixed_spelling(order, held, &to).is_some_and(|existing| existing != from)
                    || changes.len() + 2 > size
                {
                    continue;
                }
                let kept = held.remove(&from).expect("a live path");
                said.push(format!("rename {from} to {to}"));
                if rng.below(2) == 0 || from.eq_ignore_ascii_case(&to) {
                    changes.push(death(&from));
                    changes.push(mixed_change(&to, &kept));
                } else {
                    changes.push(mixed_change(&to, &kept));
                    changes.push(death(&from));
                }
                held.insert(to, kept);
            }
        }
    }
    (changes, said.join("; "), anchors_moved)
}

/// **Narrowing in-place rewrites leaves the findings a rebuild files**
/// (ADR 0047). A seeded script of changesets of exactly one, two, five and
/// sixteen entries — mostly bodies rewritten with their anchors kept, beside
/// anchors moved, fresh writes, deaths, renames, documents left with their
/// names alone and read again, and now and then a pin that moves the
/// ambiguity-ignore globs followed by a heal in changesets of the same size —
/// is applied to one store, and after every changeset a store built from
/// zero out of the same paths, in one changeset, equals it under the
/// testkit's comparator, finding order included, on both roots. The script
/// is held to reaching both narrowings: some changesets name fewer paths than
/// they write, and some name no path at a document whose anchor they moved in
/// place, which the anchored-link index then reached.
///
/// `NORN_IN_PLACE_SEEDS` and `NORN_IN_PLACE_STEPS` lengthen the run.
#[test]
fn narrowed_rewrites_equal_a_rebuild_at_every_changeset_size() {
    let seeds = script_bound("NORN_IN_PLACE_SEEDS", 3);
    let steps = script_bound("NORN_IN_PLACE_STEPS", 12);
    let mut narrowed: BTreeMap<usize, u64> = BTreeMap::new();
    let mut anchors_narrowed: BTreeMap<usize, u64> = BTreeMap::new();
    let mut runs: BTreeMap<usize, u64> = BTreeMap::new();
    for size in [1, 2, 5, 16] {
        for order in [Sensitive, Folding] {
            for seed in 0x0460_0000..0x0460_0000 + seeds {
                let mut rng = Rng(seed ^ ((size as u64) << 32));
                let label = format!("redecide-in-place-script-{size}-{order:?}-{seed:x}");
                let scratch = Scratch::new(&label);
                let mut store = scratch.open_under(order);
                let mut schema = 0;
                pin_schema(&mut store, schema);
                let mut held = MixedHeld::new();
                let mut script: Vec<String> = Vec::new();
                for at in 0..steps {
                    let subject = |script: &[String]| {
                        format!("size {size} {order:?} seed {seed:#x}: {script:#?}")
                    };
                    if rng.below(10) == 0 && !held.is_empty() {
                        schema = (schema + 1 + rng.below(2)) % SCHEMAS.len();
                        script.push(format!("pin {} and heal", SCHEMAS[schema].0));
                        pin_schema(&mut store, schema);
                        let mut entries: Vec<Change> = held
                            .iter()
                            .map(|(written, kept)| mixed_change(written, kept))
                            .collect();
                        for at in (1..entries.len()).rev() {
                            entries.swap(at, rng.below(at + 1));
                        }
                        for chunk in entries.chunks(size) {
                            apply_under(&mut store, chunk.to_vec(), schema, &subject(&script));
                        }
                    } else {
                        let (changes, said, anchors_moved) =
                            mixed_step(&mut rng, order, &mut held, size);
                        script.push(said);
                        let written: BTreeSet<String> = changes
                            .iter()
                            .map(|change| match change {
                                Change::Upsert(facts) => facts.path.path_key().to_string(),
                                Change::Death { path, .. } | Change::Unreadable { path } => {
                                    path.path_key().to_string()
                                }
                                other => unreachable!("{other:?}"),
                            })
                            .collect();
                        let outcome = store
                            .begin_request()
                            .apply_increment(
                                IncrementProvenance::Derived,
                                changes,
                                &[],
                                &declared_by(schema),
                            )
                            .unwrap_or_else(|refusal| {
                                panic!("{}: a changeset: {refusal}", subject(&script))
                            });
                        let named: BTreeSet<String> = outcome
                            .affected_paths
                            .iter()
                            .map(|key| key.as_str().to_string())
                            .collect();
                        assert!(
                            named.is_subset(&written),
                            "{}: named {named:?} beyond {written:?}",
                            subject(&script)
                        );
                        if named.len() < written.len() {
                            *narrowed.entry(size).or_insert(0) += 1;
                        }
                        if anchors_moved.iter().any(|moved| !named.contains(moved)) {
                            *anchors_narrowed.entry(size).or_insert(0) += 1;
                        }
                    }
                    let subject = subject(&script);
                    let rebuilt_scratch = Scratch::new(&format!("{label}-rebuilt-{at}"));
                    let mut rebuilt = rebuilt_scratch.open_under(order);
                    pin_schema(&mut rebuilt, schema);
                    apply_under(
                        &mut rebuilt,
                        held.iter()
                            .map(|(written, kept)| mixed_change(written, kept))
                            .collect(),
                        schema,
                        &subject,
                    );
                    assert_same(&mut store, &mut rebuilt, &subject);
                }
                *runs.entry(size).or_insert(0_u64) += 1;
            }
        }
    }
    eprintln!(
        "in-place script: seeds run per size and root {runs:?}, narrowed changesets \
         {narrowed:?}, anchor moves narrowed {anchors_narrowed:?}"
    );
    for size in [1, 2, 5, 16] {
        assert!(
            narrowed.get(&size).is_some_and(|count| *count > 0),
            "changesets of {size} never reached an in-place rewrite: {narrowed:?}"
        );
        assert!(
            anchors_narrowed.get(&size).is_some_and(|count| *count > 0),
            "changesets of {size} never narrowed an anchor move: {anchors_narrowed:?}"
        );
    }
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
        store
            .finish_construction()
            .expect("finishing the seed's construction");
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

/// **A changeset naming more classes and paths than one occupancy chunk
/// reaches every key in the chunk after the first.** Three hundred documents
/// `f/k000.md` to `f/k299.md` are killed, and `y/zzz.md` written, in one
/// changeset: 301 classes and 301 paths, so the keys `a.md` is reached by —
/// the class `k299/`, the path `f/k298.md`, and the naming class `zzz/` its
/// path link's candidate holds, with no suffix link under it — all stand in
/// the second chunk. Each link is re-decided: two broken, and the missing
/// anchor's candidate renamed `x/zzz`.
#[test]
fn a_changeset_past_one_chunk_of_keys_reaches_every_key_in_the_next() {
    const FILLERS: usize = 300;
    for order in [Sensitive, Folding] {
        let label = format!("redecide-second-chunk-{order:?}");
        let mut vault = Vault::new(&label, order);
        let mut held: BTreeMap<String, String> = BTreeMap::new();
        let holder = "[p](x/zzz.md#Nope)\n\n[[k299]]\n\n[p](f/k298.md)\n";
        let mut first = vec![
            ("a.md".to_string(), holder.to_string()),
            ("x/zzz.md".to_string(), String::new()),
        ];
        first.extend((0..FILLERS).map(|at| (format!("f/k{at:03}.md"), String::new())));
        for (at, body) in &first {
            held.insert(at.clone(), body.clone());
        }
        vault.apply(
            first
                .iter()
                .map(|(at, body)| Change::Upsert(derived(at, body)))
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            vault.findings("a.md"),
            [missing_anchor(0, "x/zzz.md")],
            "{order:?}"
        );
        assert_eq!(named(&mut vault.store, "a.md"), [vec!["zzz".to_string()]]);

        let mut second: Vec<Change> = (0..FILLERS)
            .map(|at| {
                let at = format!("f/k{at:03}.md");
                held.remove(&at);
                death(&at)
            })
            .collect();
        held.insert("y/zzz.md".to_string(), String::new());
        second.push(Change::Upsert(derived("y/zzz.md", "")));
        vault.apply(second);

        assert_eq!(
            vault.findings("a.md"),
            [missing_anchor(0, "x/zzz.md"), broken(1), broken(2)],
            "{order:?}"
        );
        assert_eq!(
            named(&mut vault.store, "a.md"),
            [vec!["x/zzz".to_string()], Vec::new(), Vec::new()],
            "{order:?}"
        );
        assert_rebuilds(&mut vault.store, order, &held, &label);
    }
}

/// Comparison twins joining and leaving a class rejudge its untouched links.
#[test]
fn comparison_twins_rejudge_link_health_when_created_and_removed() {
    for order in [Sensitive, Folding] {
        for (first, twin, link) in [
            ("ΟΣ.md", "Οσ.md", "[[ΟΣ]]\n\n[x](ΟΣ.md)\n"),
            ("Note.md", "note.md", "[[note]]\n\n[x](NOTE.md)\n"),
            (
                "résumé.md",
                "re\u{301}sume\u{301}.md",
                "[[résumé]]\n\n[x](re\u{301}sume\u{301}.MD)\n",
            ),
        ] {
            let mut vault = Vault::new("redecide-comparison-twins", order);
            vault.write(&[(first, "target\n"), ("holder.md", link)]);
            assert_eq!(vault.findings("holder.md"), []);
            vault.write(&[(twin, "twin\n")]);
            let mut candidates = [first, twin];
            candidates.sort();
            assert_eq!(
                vault.findings("holder.md"),
                [ambiguous(0, &candidates, 2), ambiguous(1, &candidates, 2)],
                "{order:?}"
            );
            vault.kill(twin);
            assert_eq!(vault.findings("holder.md"), []);
        }
    }
}

/// **Link health re-decides a suffix link when a document appears or
/// disappears at its exact vault path** (ADR 0040, ADR 0047), in both
/// families. Creating `a/x.md` beside `deep/a/x.md` and `other/a/x.md` turns
/// `[[a/x]]` and `[x](a/x.md)` from ambiguous to naming it alone, and
/// deleting it restores the ambiguity. `[[b/y]]`, naming `deep/b/y.md` alone,
/// moves to `b/y.md` when that is created, and back when it is deleted.
#[test]
fn an_exact_path_document_appearing_or_leaving_rejudges_its_suffix_links() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::new(&format!("redecide-exact-{order:?}"), order);
        vault.write(&[
            ("deep/a/x.md", "x\n"),
            ("other/a/x.md", "x\n"),
            ("deep/b/y.md", "y\n"),
            ("h.md", "[[a/x]] [x](a/x.md) [[b/y]]\n"),
        ]);
        let deeper = [
            ambiguous(0, &["deep/a/x.md", "other/a/x.md"], 2),
            ambiguous(1, &["deep/a/x.md", "other/a/x.md"], 2),
        ];
        assert_eq!(vault.findings("h.md"), deeper, "{order:?}");
        assert_eq!(vault.backlinks("/deep/b/y"), ["h.md"], "{order:?}");

        vault.write(&[("a/x.md", "x\n"), ("b/y.md", "y\n")]);
        assert_eq!(vault.findings("h.md"), [], "{order:?}");
        assert_eq!(vault.backlinks("/a/x"), ["h.md"], "{order:?}");
        assert_eq!(vault.backlinks("/b/y"), ["h.md"], "{order:?}");
        assert!(vault.backlinks("/deep/b/y").is_empty(), "{order:?}");

        vault.kill("a/x.md");
        vault.kill("b/y.md");
        assert_eq!(vault.findings("h.md"), deeper, "{order:?}");
        assert_eq!(vault.backlinks("/deep/b/y"), ["h.md"], "{order:?}");
    }
}

/// **A Markdown destination is read as written in the body**: an
/// angle-bracketed destination keeps its literal space, a percent-encoded
/// one is decoded, and either names `t/space name.md` from `src/h.md`; a
/// decoded separator names nothing, so that link is broken.
#[test]
fn a_written_markdown_destination_is_decoded_as_its_brackets_say() {
    for order in [Sensitive, Folding] {
        let mut vault = Vault::new(&format!("redecide-decoded-{order:?}"), order);
        vault.write(&[
            ("t/space name.md", "s\n"),
            (
                "src/h.md",
                "[a](<../t/space name.md>) [b](../t/space%20name.md) [c](t%2Fspace%20name.md)\n",
            ),
        ]);
        assert_eq!(vault.findings("src/h.md"), [broken(2)], "{order:?}");
        assert_eq!(vault.backlinks("/t/space name"), ["src/h.md"], "{order:?}");
    }
}
