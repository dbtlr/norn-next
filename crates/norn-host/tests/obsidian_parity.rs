//! **The Obsidian parity corpus**: norn's answer for each link case the probe in
//! `tests/fixtures/obsidian-links` recorded Obsidian 1.14.5 (iOS) resolving, as
//! a real host attached over the probe's vault reads it ([ADR 0040]).
//!
//! Two things are asserted per case. Norn's answer equals the expectation
//! authored here, and that expectation is compatible with what Obsidian was
//! recorded doing: a link norn judges healthy names the document Obsidian
//! opened, an ambiguity holds the document Obsidian opened among its
//! candidates, and a link Obsidian resolved is never judged broken by norn.
//! The compatibility check reads the recorded observations rather than a copy
//! of them, so it holds whatever the expectations drift to.
//!
//! The observations were taken on the baseline tree with the recorded input
//! delta applied, so the probe is installed as a person installs it — through
//! `install.json`'s renames to the decomposed names a checkout cannot keep —
//! and the delta applied before the vault is attached.
//!
//! [ADR 0040]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0040-a-link-resolves-as-obsidian-resolves-it.md
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own installed tree.

mod attach;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use norn_testkit::process::Sandbox;
use norn_wire::{
    Column, FindParams, FindingKind, LinkHealth, LinkRow, Predicate, ValidateParams,
    ValidateReport, VaultAddress,
};
use serde_json::Value;

/// The probe's vault-relative folder, which rooted and exact-path cases name.
const PROBE_ROOT: &str = "artifacts/scratch/norn-422";

/// Where the probe lives, from this crate's manifest.
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/obsidian-links")
}

fn read_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("reading the fixture file {}: {e}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("parsing the fixture file {}: {e}", path.display()))
}

/// A path under the store's comparison key, the one function that decides
/// whether two paths are the same, so a filesystem that changes a name's
/// normalization does not fail the corpus.
fn under_the_key(path: &str) -> String {
    norn_store::comparison_key(path)
}

// ---- the recorded cases ----

/// One case as the fixture's `cases.json` states it.
struct Case {
    id: String,
    source: String,
}

fn cases() -> Vec<Case> {
    let cases = read_json(&fixture().join("cases.json"));
    cases["cases"]
        .as_array()
        .expect("cases.json holds a `cases` array")
        .iter()
        .map(|case| Case {
            id: text_of(case, "id"),
            source: text_of(case, "source"),
        })
        .collect()
}

fn text_of(value: &Value, key: &str) -> String {
    value[key]
        .as_str()
        .unwrap_or_else(|| panic!("`{key}` is not text in {value}"))
        .to_string()
}

/// What Obsidian was recorded doing with one case.
struct Observed {
    /// The link text as observed, which a rename rewrote for a few cases.
    link: String,
    outcome: String,
    destination: Option<String>,
}

impl fmt::Display for Observed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.destination {
            Some(destination) => write!(formatter, "{} `{destination}`", self.outcome),
            None => write!(formatter, "{}", self.outcome),
        }
    }
}

fn observations() -> BTreeMap<String, Observed> {
    let results = read_json(&fixture().join("observations/ios-1.14.5/results.json"));
    results["read_results"]
        .as_array()
        .expect("results.json holds a `read_results` array")
        .iter()
        .map(|result| {
            (
                text_of(result, "case"),
                Observed {
                    link: text_of(result, "link"),
                    outcome: text_of(result, "outcome"),
                    destination: result["identified_destination"]
                        .as_str()
                        .map(str::to_string),
                },
            )
        })
        .collect()
}

// ---- an answer, and what is expected ----

/// What a link resolves to, as the corpus compares it. Paths are vault-relative
/// and compared under the store's comparison key.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Answer {
    Healthy(String),
    MissingAnchor(String),
    Ambiguous(BTreeSet<String>),
    Broken,
    NotJudged,
}

fn healthy(path: &str) -> Answer {
    Answer::Healthy(under_the_key(path))
}

fn missing_anchor(path: &str) -> Answer {
    Answer::MissingAnchor(under_the_key(path))
}

fn ambiguous(paths: &[&str]) -> Answer {
    Answer::Ambiguous(paths.iter().map(|path| under_the_key(path)).collect())
}

/// A path inside the probe's folder.
fn p(relative: &str) -> String {
    format!("{PROBE_ROOT}/{relative}")
}

/// The authored expectation of each case, by case id, over the vault the
/// recorded input delta leaves.
fn expectations() -> BTreeMap<&'static str, Answer> {
    let unique = p("targets/n422-unique.md");
    let sources_choice = p("sources/n422-choice.md");
    let path_choice = p("targets/n422-path-choice.md");
    let nested_path_choice = p("sources/targets/n422-path-choice.md");
    let space_renamed = p("targets/n422-space renamed.md");
    let case = p("targets/n422-Case.md");
    let eclair = p("targets/n422-Éclair.md");
    let cafe = p("unicode/nfc/n422-café.md");
    let the = p("unicode/nfd/n422-thé.md");
    let anchors = p("targets/n422-anchors.md");
    let exact = p("targets/n422-exact.md");
    let dotted = p("dots/n422-v1.2.md");
    let twins = [
        p("ambiguous/a/n422-twin.md"),
        p("ambiguous/b/n422-twin.md"),
        p("sources/n422-twin.md"),
    ];
    let twins: Vec<&str> = twins.iter().map(String::as_str).collect();

    let mut table: BTreeMap<&'static str, Answer> = BTreeMap::new();
    let mut expect = |ids: &[&'static str], answer: Answer| {
        for id in ids {
            assert!(table.insert(id, answer.clone()).is_none(), "{id} twice");
        }
    };
    expect(
        &[
            "M01", "M02", "M03", "M13", "W01", "W02", "W04", "W05", "W06",
        ],
        healthy(&unique),
    );
    expect(&["M04", "W03"], healthy(&sources_choice));
    expect(
        &["M05"],
        ambiguous(&[&sources_choice, &p("targets/n422-choice.md")]),
    );
    expect(&["M06", "M07"], healthy(&path_choice));
    expect(&["M08"], ambiguous(&[&path_choice, &nested_path_choice]));
    expect(&["M09"], healthy(&nested_path_choice));
    expect(&["M10", "M11", "W23"], healthy(&space_renamed));
    expect(&["M12"], healthy(&p("targets/n422-percent%name.md")));
    expect(&["M14", "W08"], healthy(&case));
    expect(&["M15", "W09"], healthy(&eclair));
    expect(&["M16", "W10"], healthy(&cafe));
    expect(&["M17", "W11"], healthy(&the));
    expect(&["M18", "M19", "W19", "W20"], healthy(&anchors));
    expect(&["M20", "M21", "W21", "W22"], missing_anchor(&anchors));
    expect(&["M22"], Answer::Broken);
    expect(&["M23", "W07"], healthy(&exact));
    expect(&["M24", "W15"], healthy(&dotted));
    expect(&["M25", "W13"], healthy(&p("dots/photo.png")));
    expect(&["M26", "W14"], healthy(&p("dots/v1.2.md")));
    expect(
        &["W12"],
        ambiguous(&[
            &p("unicode/pair-a/n422-résumé.md"),
            &p("unicode/pair-b/n422-résumé.md"),
        ]),
    );
    expect(&["W16"], healthy(&p("dots/n422-photo.png.md")));
    expect(&["W17", "A01", "A02"], ambiguous(&twins));
    expect(&["W18"], healthy("ambiguous/n422-none.md"));
    expect(&["W24"], healthy("Inbox/n422-space%20name.md"));
    expect(&["W25"], healthy("Inbox/n422-does-not-exist.md"));
    table
}

/// Whether `expected` is an answer Obsidian's recorded behavior permits,
/// or the reason it is not.
///
/// A healthy link names the document Obsidian opened with nothing missing; a
/// link with a missing anchor names it too, and Obsidian reported the anchor
/// missing; an ambiguity holds it among its candidates; a broken link is one
/// Obsidian opened nothing for; and a link not judged names a file that is not
/// a Markdown document.
fn compatible(expected: &Answer, observed: &Observed) -> Result<(), String> {
    let destination = observed.destination.as_deref().map(under_the_key);
    match expected {
        Answer::Healthy(target)
            if observed.outcome == "opened" && destination.as_ref() == Some(target) =>
        {
            Ok(())
        }
        Answer::MissingAnchor(target)
            if observed.outcome == "opened_document_with_missing_anchor"
                && destination.as_ref() == Some(target) =>
        {
            Ok(())
        }
        Answer::Ambiguous(candidates)
            if destination
                .as_ref()
                .is_some_and(|destination| candidates.contains(destination)) =>
        {
            Ok(())
        }
        Answer::Broken if observed.outcome == "open_error" => Ok(()),
        _ => Err(format!(
            "expected {expected:?}, which Obsidian's recorded `{observed}` does not permit"
        )),
    }
}

/// **A healthy expectation needs Obsidian to have opened the document with
/// nothing missing**: over a recorded missing anchor at the same destination
/// it is incompatible, and only the missing-anchor answer is.
#[test]
fn a_healthy_expectation_is_incompatible_with_a_recorded_missing_anchor() {
    let observed = |outcome: &str| Observed {
        link: "[[n422-anchors#Nope]]".to_string(),
        outcome: outcome.to_string(),
        destination: Some("artifacts/scratch/norn-422/n422-anchors.md".to_string()),
    };
    let target = under_the_key("artifacts/scratch/norn-422/n422-anchors.md");
    let missing = observed("opened_document_with_missing_anchor");
    assert!(compatible(&Answer::Healthy(target.clone()), &missing).is_err());
    assert!(compatible(&Answer::MissingAnchor(target.clone()), &missing).is_ok());
    let opened = observed("opened");
    assert!(compatible(&Answer::Healthy(target.clone()), &opened).is_ok());
    assert!(compatible(&Answer::MissingAnchor(target), &opened).is_err());
}

#[test]
fn every_recorded_case_has_one_expectation_compatible_with_obsidians_answer() {
    let expectations = expectations();
    let observations = observations();
    let cases = cases();
    assert_eq!(cases.len(), 53, "the probe's recorded cases");

    let mut failures = Vec::new();
    for case in &cases {
        let Some(observed) = observations.get(&case.id) else {
            failures.push(format!("{}: no recorded observation", case.id));
            continue;
        };
        let Some(expected) = expectations.get(case.id.as_str()) else {
            failures.push(format!(
                "{} `{}`: no authored expectation",
                case.id, observed.link
            ));
            continue;
        };
        if let Err(why) = compatible(expected, observed) {
            failures.push(format!("{} `{}`: {why}", case.id, observed.link));
        }
    }
    let known: BTreeSet<&str> = cases.iter().map(|case| case.id.as_str()).collect();
    failures.extend(
        expectations
            .keys()
            .filter(|id| !known.contains(**id))
            .map(|id| format!("{id}: an expectation for no recorded case")),
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// ---- the vault, as the observations were taken ----

/// The vault the observations were taken on: the probe installed as a person
/// installs it — its tree copied, then the files `install.json` lists renamed
/// to their decomposed names, which a checkout cannot keep — with the recorded
/// input delta applied.
fn observed_vault(vault: &Path) {
    norn_testkit::obsidian_probe::Probe::workspace()
        .and_then(|probe| probe.install(vault))
        .unwrap_or_else(|problem| panic!("installing the Obsidian probe: {problem}"));
    let delta = read_json(&fixture().join("observations/ios-1.14.5/input-state.json"));
    for removed in delta["removed_paths"]
        .as_array()
        .expect("the delta's removed paths")
    {
        let removed = removed.as_str().expect("a removed path");
        std::fs::remove_file(vault.join(removed))
            .unwrap_or_else(|e| panic!("removing `{removed}`: {e}"));
    }
    for (written, contents) in delta["written_files"]
        .as_object()
        .expect("the delta's written files")
    {
        let at = vault.join(written);
        std::fs::create_dir_all(at.parent().expect("a written file's folder"))
            .expect("create the written file's folder");
        let contents = contents.as_str().expect("a written file's contents");
        std::fs::write(&at, contents).unwrap_or_else(|e| panic!("writing `{written}`: {e}"));
    }
}

// ---- norn's answer ----

/// The links the document at `source` holds, in order.
fn links_of(host: &attach::ServingHost, address: &VaultAddress, source: &str) -> Vec<LinkRow> {
    let found = host
        .find(
            &FindParams::new(address.clone())
                .with_predicates([Predicate::path(source)])
                .with_columns([Column::links()]),
        )
        .unwrap_or_else(|refusal| panic!("a find of `{source}`: {refusal:?}"));
    let [row] = &found.answer.report.page.rows[..] else {
        panic!("`{source}` is not one row");
    };
    let links = row.links.as_ref().expect("the links column");
    assert_eq!(links.total, links.items.len() as u64, "`{source}`'s links");
    links.items.clone()
}

/// The spans of the links in each document that a missing-anchor finding
/// stands about.
fn missing_anchor_spans(
    host: &attach::ServingHost,
    address: &VaultAddress,
) -> BTreeSet<(String, Option<(u64, u64)>)> {
    let answered = host
        .validate(
            &ValidateParams::new(address.clone())
                .with_kinds([FindingKind::MissingAnchor])
                .with_limit(1000),
        )
        .expect("an attached vault answers a validate");
    let ValidateReport::Findings { page, .. } = &answered.answer.report else {
        panic!("a validate answered {:?}", answered.answer.report);
    };
    assert!(page.next.is_none(), "a page of anchor findings is cut");
    page.rows
        .iter()
        .map(|row| {
            (
                row.path.as_str().to_string(),
                row.span.map(|span| (span.line, span.column)),
            )
        })
        .collect()
}

/// What norn answers for `link`, held by the document at `source`.
fn answer_of(
    link: &LinkRow,
    source: &str,
    anchor_missing: &BTreeSet<(String, Option<(u64, u64)>)>,
) -> Answer {
    let heads: Vec<String> = link
        .targets
        .candidates()
        .iter()
        .map(|candidate| under_the_key(candidate.path.as_str()))
        .collect();
    assert_eq!(
        heads.len() as u64,
        link.targets.total(),
        "a head cut below its total"
    );
    match (link.health(), &heads[..]) {
        (LinkHealth::Healthy, [only]) => {
            let at = link.span.map(|span| (span.line, span.column));
            if anchor_missing.contains(&(source.to_string(), at)) {
                Answer::MissingAnchor(only.clone())
            } else {
                Answer::Healthy(only.clone())
            }
        }
        (LinkHealth::Ambiguous, _) => Answer::Ambiguous(heads.into_iter().collect()),
        (LinkHealth::Broken, _) => Answer::Broken,
        (LinkHealth::NotJudged, _) => Answer::NotJudged,
        (health, heads) => panic!("a link of health {health:?} naming {heads:?}"),
    }
}

#[test]
fn norn_answers_each_recorded_obsidian_case_as_authored() {
    let sandbox = Sandbox::new(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        "host-obsidian-parity",
    )
    .expect("a sandbox");
    let root = sandbox.work_dir().join("attached");
    let vault_root = root.join("vault");
    observed_vault(&vault_root);
    std::fs::create_dir_all(vault_root.join(".norn")).expect("create the vault's schema folder");
    std::fs::write(vault_root.join(".norn/schema.yaml"), attach::SCHEMA)
        .expect("write the vault schema");
    let vault = attach::Vault::adopt(&root);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let address = VaultAddress::name(vault.name().clone());

    let expectations = expectations();
    let observations = observations();
    let anchor_missing = missing_anchor_spans(&host, &address);
    let mut held: BTreeMap<String, Vec<LinkRow>> = BTreeMap::new();

    let mut failures = Vec::new();
    for case in cases() {
        let observed = observations
            .get(&case.id)
            .unwrap_or_else(|| panic!("{}: no recorded observation", case.id));
        let Some(expected) = expectations.get(case.id.as_str()) else {
            failures.push(format!("{}: no authored expectation", case.id));
            continue;
        };
        let links = held
            .entry(case.source.clone())
            .or_insert_with(|| links_of(&host, &address, &case.source));
        let mut named = links
            .iter()
            .filter(|link| link.title.as_deref() == Some(case.id.as_str()));
        let (Some(link), None) = (named.next(), named.next()) else {
            failures.push(format!(
                "{} `{}`: not one link titled `{}` in `{}`",
                case.id, observed.link, case.id, case.source
            ));
            continue;
        };
        let answer = answer_of(link, &case.source, &anchor_missing);
        if answer != *expected {
            failures.push(format!(
                "{} `{}`: norn answered {answer:?}, expected {expected:?}; Obsidian: {observed}",
                case.id, observed.link
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn norns_rename_keeps_authored_forms_and_resolves_to_obsidians_target() {
    use norn_wire::{
        ApplyMode, ApplyParams, ApplyReport, AuthoredPlan, DocumentPath, Operation, OperationKind,
        PlanDocument,
    };
    let sandbox = Sandbox::new(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        "host-obsidian-write-parity",
    )
    .expect("a sandbox");
    let root = sandbox.work_dir().join("attached");
    let vault_root = root.join("vault");
    norn_testkit::obsidian_probe::Probe::workspace()
        .and_then(|probe| probe.install(&vault_root))
        .expect("install baseline without the rename delta");
    std::fs::create_dir_all(vault_root.join(".norn")).expect("schema folder");
    std::fs::write(vault_root.join(".norn/schema.yaml"), attach::SCHEMA).expect("schema");
    let vault = attach::Vault::adopt(&root);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let address = VaultAddress::name(vault.name().clone());
    let results = read_json(&fixture().join("observations/ios-1.14.5/results.json"));
    let rename = &results["rename_operation"];
    let old = DocumentPath::new(text_of(rename, "old_path")).expect("old path");
    let new = DocumentPath::new(text_of(rename, "new_path")).expect("new path");
    let plan = AuthoredPlan::new(
        address.clone(),
        vec![Operation::new(OperationKind::move_document(
            old,
            new.clone(),
        ))],
    );
    let report = host
        .apply(ApplyParams::new(
            ApplyMode::Apply,
            PlanDocument::operations(plan),
        ))
        .expect("move admitted")
        .wait()
        .expect("move answered")
        .report;
    assert!(matches!(report, ApplyReport::Applied { .. }), "{report:?}");
    let observations = observations();
    for (id, written) in [
        ("M10", "[M10](../targets/n422-space%20renamed.md)"),
        ("M11", "[M11](<../targets/n422-space renamed.md>)"),
        ("W23", "[[n422-space renamed|W23]]"),
    ] {
        assert!(
            rename["results"]
                .as_array()
                .expect("rename results")
                .iter()
                .any(|result| result["case"] == id)
        );
        let source = cases()
            .into_iter()
            .find(|case| case.id == id)
            .expect("recorded case")
            .source;
        let text = std::fs::read_to_string(vault_root.join(&source)).expect("source text");
        assert!(text.contains(written), "{id}: expected {written} in {text}");
        let links = links_of(&host, &address, &source);
        let link = links
            .iter()
            .find(|link| link.title.as_deref() == Some(id))
            .expect("written link");
        let answer = answer_of(link, &source, &BTreeSet::new());
        assert_eq!(answer, healthy(new.as_str()));
        compatible(&answer, &observations[id]).unwrap_or_else(|why| panic!("{id}: {why}"));
    }
}
