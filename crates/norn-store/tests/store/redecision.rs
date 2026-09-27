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
    Change, ContentModel, DerivationCounters, IncrementProvenance, OpenOutcome, Provenance, Store,
    StoreError, StoredPathOrder,
};
use norn_testkit::equivalence::{DerivedRows, StoreProjection};
use norn_wire::{
    FindParams, FindingKind, Pattern, Predicate, ResolutionTarget, VaultAddress, VaultName,
};

use crate::common::{Scratch, path, snapshot};
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

/// The places the edit script writes, kills and renames between: stems shared
/// across directories, a dotted stem and its reduction, a place under the
/// ignore glob, and a place only a path link names.
const PLACES: [&str; 12] = [
    "t.md",
    "x/t.md",
    "y/t.md",
    "archive/t.md",
    "v1.md",
    "v1.2.md",
    "x/v1.2.md",
    "h.md",
    "x/h.md",
    "dir/u.md",
    "notes.md",
    "x/Notes2.md",
];

/// The lines a body is made of: headings and blocks a link's anchor may name,
/// and links of every address — suffix, dotted, anchored, path, rooted, embed,
/// ignored, elsewhere, and naming nothing.
const LINES: [&str; 24] = [
    "# Intro",
    "## Setup",
    "A paragraph. ^blk",
    "An item. ^b2",
    "[[t]]",
    "[[x/t]]",
    "[[t#Intro]]",
    "[[t#^blk]]",
    "[[v1.2]]",
    "[[v1.2#Setup]]",
    "[[vault://t]]",
    "[[vault://v1.2]]",
    "[p](t.md)",
    "[p](x/t.md#Intro)",
    "[p](../t.md)",
    "![[t]]",
    "[[notes]]",
    "[[h#^b2]]",
    "[[archive/t]]",
    "[[missing]]",
    "[p](dir/u.md)",
    "[[u#^blk]]",
    "[w](https://example.com/page)",
    "[[Notes2]]",
];

/// A body of up to five of [`LINES`], each its own paragraph.
fn body(rng: &mut Rng) -> String {
    (0..rng.below(6))
        .map(|_| format!("{}\n\n", rng.pick(&LINES)))
        .collect()
}

/// One step of an edit script over `vault`, whose documents `held` tracks:
/// the changeset, and what it says it does.
fn step(rng: &mut Rng, held: &mut BTreeMap<String, String>) -> (Vec<Change>, String) {
    let live: Vec<String> = held.keys().cloned().collect();
    let mut changes = Vec::new();
    let mut said = Vec::new();
    for _ in 0..=rng.below(3) {
        let at = rng.pick(&PLACES).to_string();
        match rng.below(if live.is_empty() { 1 } else { 3 }) {
            0 => {
                let written = body(rng);
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
                let Some(moved) = held.remove(&from) else {
                    continue;
                };
                said.push(format!("rename {from} to {at}"));
                changes.push(death(&from));
                changes.push(Change::Upsert(derived(&at, &moved)));
                held.insert(at, moved);
            }
        }
    }
    (changes, said.join("; "))
}

/// **Incremental maintenance equals a rebuild, finding order included.** A
/// seeded edit script — writes, deaths and renames, a changeset of one to three
/// of them at a time, over bodies whose headings, blocks and links each step
/// rewrites — is applied to one store, and after every step a second store is
/// built from zero out of the documents the first holds, in one changeset. The
/// two are equal under the testkit's comparator, which reads every finding in
/// the order a reader does, and every derived row agrees field by field, on a
/// root that tells spellings apart and on one that folds them. The script
/// reaches every kind the family has.
#[test]
fn incremental_equals_rebuild_including_finding_order() {
    const STEPS: usize = 40;
    let mut kinds: BTreeSet<String> = BTreeSet::new();
    for order in [Sensitive, Folding] {
        for seed in [0x253b_0001_u64, 0x253b_0002, 0x253b_0003] {
            let mut rng = Rng(seed);
            let mut vault = Vault::new(&format!("redecide-script-{order:?}-{seed:x}"), order);
            let mut held: BTreeMap<String, String> = BTreeMap::new();
            let mut script = Vec::new();
            for at in 0..STEPS {
                let (changes, said) = step(&mut rng, &mut held);
                script.push(said);
                vault.apply(changes);

                let rebuilt_scratch =
                    Scratch::new(&format!("redecide-script-{order:?}-{seed:x}-rebuilt-{at}"));
                let mut rebuilt = pinned_store(&rebuilt_scratch, order);
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

                let subject = format!("{order:?} seed {seed:#x} after {script:#?}");
                let incremental = StoreProjection::read(&mut vault.store).expect("a projection");
                let from_zero = StoreProjection::read(&mut rebuilt).expect("a projection");
                incremental.assert_equivalent(&from_zero, &subject);
                assert_eq!(
                    DerivedRows::read(&mut vault.store)
                        .expect("the derived rows")
                        .fields(),
                    DerivedRows::read(&mut rebuilt)
                        .expect("the derived rows")
                        .fields(),
                    "{subject}"
                );
                kinds.extend(
                    incremental
                        .findings()
                        .iter()
                        .map(|finding| finding.kind.clone()),
                );
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
