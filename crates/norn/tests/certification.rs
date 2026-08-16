//! The Layer 2 certification machinery's own gate.
//!
//! Three claims, and each is about the machinery rather than about the product:
//! the inventory of required cases is the set the suites actually hold, the
//! suite-manifest digest is a value two reads agree on, and the record one run
//! leaves behind reconciles against the inventory it names.
//!
//! # Why this suite lives here
//!
//! Its subject spans crates. The inventory names carriers in `norn-host`,
//! `norn-fs` and `norn-store`, and the manifest closes over workflow files,
//! the lockfile and the toolchain — none of which belongs to any one crate. A
//! suite whose subject is workspace-wide data lives with that data in this bin
//! package, which is where the regression registry's audit already sits.
//!
//! # What it costs
//!
//! The reconciliation asks cargo what compiled into each certification target,
//! under the feature each target is claimed with, which builds those suites. It
//! is the same trade the regression registry's carrier audit makes: a binding
//! checked against the built suite is worth a build, because a binding checked
//! against the text of a file is a claim about text.

use std::path::PathBuf;

use norn_testkit::certification::{
    inventory::{self, Lane, REQUIRED_CASES, Suite, UNREACHED_ARMS},
    ledger::{self, CaseOutcome, Classification, Outcome, Platform, Preflight, Record, RunResult},
    manifest,
};

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .unwrap_or_else(|| panic!("no workspace root above {}", manifest.display()))
        .to_path_buf()
}

/// **The reconciliation.** Every required case is a test cargo compiled, and
/// every test in a claimed certification suite is a required case.
///
/// This is the claim that makes a run's result mean the layer's obligations
/// were exercised. Both directions fail here rather than passing quietly: a
/// case deleted, renamed or left behind a feature nothing turns on, and a case
/// added to a suite and not to the record.
#[test]
fn the_inventory_reconciles_with_the_suites() {
    let problems = inventory::reconciliation_problems(&workspace_root());
    assert!(
        problems.is_empty(),
        "the case inventory does not reconcile with the suites:\n  {}",
        problems.join("\n  ")
    );
}

/// **The unreached arms are reported, and reported without failing.**
///
/// This is the surface that reports them: the reconciliation never reads the
/// table. What is missing there is an ownership ruling rather than a broken
/// reference, and a gate that failed on it would push the ruling into whoever
/// next touched the file. So the table is printed under the run — the arm and
/// what it awaits — and the claim asserted is only that it is a table somebody
/// wrote rather than a silence.
///
/// **The table stands empty, and that is a claim rather than an absence.** Its
/// rows leave by a carrier arriving, so an empty table says every arm the
/// contract names is met at the production path. Rows deleted without their
/// carriers arriving would empty it the same way and say the same thing
/// falsely, so what is asserted here is the pair: the table is empty only while
/// the trust-transition suite carries cases in a lane that needs a real
/// watcher.
#[test]
fn the_unreached_trust_transition_arms_are_named() {
    let mut rendered = String::from(
        "trust-transition arms the Layer 2 contract names and no test reaches at the production \
         path:\n",
    );
    if UNREACHED_ARMS.is_empty() {
        rendered.push_str(
            "\n  none — every arm the contract names is carried at the production path, and the \
             cases that carry them are the trust-transition rows of the inventory\n",
        );
    }
    for arm in UNREACHED_ARMS {
        rendered.push_str(&format!("\n  {}\n    awaits: {}\n", arm.arm, arm.awaits));
    }
    eprintln!("{rendered}");

    let carried_at_the_production_path: Vec<&str> = REQUIRED_CASES
        .iter()
        .filter(|case| case.suite == Suite::TrustTransition && !matches!(case.lane, Lane::Any))
        .map(|case| case.id)
        .collect();
    assert!(
        !UNREACHED_ARMS.is_empty() || !carried_at_the_production_path.is_empty(),
        "the unreached table is empty and no trust-transition case runs in a lane that needs a \
         real watcher. That reads as `every arm is carried` while nothing carries one over a real \
         backend — so an empty table means the rows moved out and their carriers did not arrive."
    );
    for arm in UNREACHED_ARMS {
        assert!(
            !arm.awaits.trim().is_empty(),
            "`{}` names no obstacle, so the ruling that assigns it has nothing to read",
            arm.arm
        );
    }
    eprintln!(
        "trust-transition arms met at the production path: {}",
        carried_at_the_production_path.join(", ")
    );
}

/// **The suite-manifest digest is a value, not an observation.**
///
/// Two reads of one checkout agree, which is the whole of what a campaign needs
/// from it: a digest that moved between two reads of the same tree could never
/// say whether two runs ran the same suite.
#[test]
fn the_suite_manifest_digest_is_the_same_read_twice() {
    let root = workspace_root();
    let first = manifest::digest(&root).expect("digesting the suite manifest");
    let second = manifest::digest(&root).expect("digesting the suite manifest again");
    assert_eq!(first, second);
    assert_eq!(first.len(), 64, "a sha256 digest is 64 hex characters");

    let covered = manifest::covered_paths(&root).expect("listing the covered paths");
    assert!(
        covered.len() >= manifest::MANIFEST_FILES.len(),
        "the manifest covers {} paths and names {} files outright",
        covered.len(),
        manifest::MANIFEST_FILES.len()
    );
    let mut sorted = covered.clone();
    sorted.sort();
    assert_eq!(
        covered, sorted,
        "the covered paths are hashed in the order they are listed, so the listing is sorted"
    );
    eprintln!("suite manifest digest {first} over {} files", covered.len());
}

/// The manifest closes over the lanes, the bars, every certification suite and
/// the rules a run is judged by — which is the property that makes it a *suite*
/// manifest rather than a file list: an assertion loosened, a bar moved or a
/// lane rewritten changes what a run is, and each has to move the value.
///
/// The claims are spelled out here rather than read off [`manifest::MANIFEST_FILES`],
/// because a test that walked the same list the digest walks would pass for any
/// list at all.
#[test]
fn the_manifest_covers_the_lanes_the_bars_and_the_suites() {
    let covered = manifest::covered_paths(&workspace_root()).expect("listing the covered paths");
    for required in [
        // The lanes, the toolchain and the resolved graph.
        ".github/workflows/ci.yml",
        ".github/workflows/soak.yml",
        ".github/scripts/lane-suite.sh",
        "Cargo.lock",
        "rust-toolchain.toml",
        // The rules: what the layer requires, how that is reconciled, and what
        // makes a record count.
        "crates/norn-testkit/src/certification/inventory.rs",
        "crates/norn-testkit/src/certification/ledger.rs",
        "crates/norn-testkit/src/certification/manifest.rs",
        "crates/norn-testkit/src/regression.rs",
        // The instruments a verdict is read off.
        "crates/norn-testkit/src/equivalence.rs",
        "crates/norn-testkit/src/churn.rs",
        "crates/norn-testkit/src/work.rs",
        "crates/norn-store/src/request.rs",
        // The seams every rung-2 and rung-3 case is reached through.
        "crates/norn-fs/src/faults.rs",
        "crates/norn-store/src/faults.rs",
        // Every suite a case in the inventory is carried by, and the bars in
        // them.
        "crates/norn-host/tests/churn.rs",
        "crates/norn-host/tests/equivalence.rs",
        "crates/norn-host/tests/lockdown.rs",
        "crates/norn-host/tests/kill_recovery.rs",
        "crates/norn-fs/tests/lockdown.rs",
        "crates/norn-store/tests/environment.rs",
        "crates/norn-store/tests/store/pillars.rs",
        // The recorded baselines a measurement step is judged against.
        "crates/norn-fixtures/tests/baselines/mod.rs",
        "crates/norn-host/tests/baselines/mod.rs",
        "crates/norn-text/tests/baselines/mod.rs",
    ] {
        assert!(
            covered.iter().any(|path| path == required),
            "the suite manifest does not cover `{required}`, so an edit to it would leave the \
             digest — and therefore the five-run count — standing"
        );
    }
}

/// **Every claimed certification target's file is in the manifest.**
///
/// The inventory claims those targets whole: every test in them is a
/// certification case. So the file each compiles from is a file whose edit
/// changes what a qualifying run asserted, and the coverage is derived from the
/// inventory's own list rather than transcribed — a suite added to
/// [`inventory::CLAIMED_TARGETS`] and not to the manifest is the drift this
/// catches.
#[test]
fn the_manifest_covers_every_claimed_certification_suite() {
    let covered = manifest::covered_paths(&workspace_root()).expect("listing the covered paths");
    for claimed in inventory::CLAIMED_TARGETS {
        let file = format!("crates/{}/tests/{}.rs", claimed.package, claimed.stem);
        assert!(
            covered.contains(&file),
            "`{} {}` is claimed whole as a certification suite and the manifest does not cover \
             `{file}`, so an assertion loosened in it would leave the digest standing",
            claimed.package,
            claimed.stem
        );
    }
}

/// **A record's own consistency.** A ledger assembled from this build's
/// inventory reconciles, and one assembled against a different set of
/// obligations is refused.
///
/// The validator is what a campaign runs before counting a record; this is the
/// proof that it counts a sound one and refuses a record whose case lines and
/// whose stated verdict have come apart.
#[test]
fn a_qualifying_record_validates_and_a_doctored_one_does_not() {
    let root = workspace_root();
    let sound = Record {
        candidate_sha: "0".repeat(40),
        suite_manifest_digest: manifest::digest(&root).expect("digesting the suite manifest"),
        case_inventory_digest: inventory::contract_digest(),
        scheduled: true,
        // The two platform-deciding facts are populated because a qualifying
        // record is required to carry them: the inventory's volume-folding and
        // backend-deciding lanes are each covered by a run of each answer, so a
        // record silent on either covers neither.
        platform: Platform {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            runner: "local".to_string(),
            watcher_backend: Some("the platform watcher this build installs".to_string()),
            volume_folds_case: Some(false),
        },
        preflight: Preflight {
            ran: true,
            admitted: Some(true),
            detail: None,
        },
        cases: REQUIRED_CASES
            .iter()
            .map(|case| CaseOutcome {
                id: case.id.to_string(),
                outcome: Outcome::Passed,
            })
            .collect(),
        result: RunResult::Passed,
        classification: Classification::Qualifying,
    };
    assert_eq!(sound.problems(&root), Vec::<String>::new());
    assert!(sound.qualifies(&root));

    let rendered = serde_json::to_string_pretty(&sound).expect("rendering the record");
    let read: Record = serde_json::from_str(&rendered).expect("reading the record back");
    assert_eq!(read, sound);

    let mut doctored = sound.clone();
    doctored.cases[0].outcome = Outcome::Failed;
    assert!(
        !doctored.problems(&root).is_empty(),
        "a record whose case outcomes and stated verdict disagree was accepted"
    );
    assert!(!doctored.qualifies(&root));
}

/// **The emitter.** Where a run named a sink, this run's record is assembled
/// and written there.
///
/// Env-gated for the reason every other harness sink is: a suite that wrote a
/// file on every developer's machine would be noise, and a run outside a
/// workflow names no sink and this does nothing. What it asserts either way is
/// that assembling a record from the environment succeeds — a writer that only
/// runs in CI is a writer nobody finds out is broken until CI runs it.
#[test]
fn this_runs_record_is_assembled_and_written_where_a_sink_names() {
    let root = workspace_root();
    let assembled =
        ledger::from_environment(&root).expect("assembling a record from this environment");
    assert_eq!(
        assembled.writer_defects(&root),
        Vec::<String>::new(),
        "a record assembled from the environment is one the writer built right, whatever the run \
         did. What the run did is carried in the record's classification and its case lines, and \
         a run that did not reconcile is evidence rather than a defect."
    );
    eprintln!(
        "qualification record: {:?}, suite manifest {}",
        assembled.classification, assembled.suite_manifest_digest
    );

    match ledger::emit(&root).expect("emitting the record") {
        Some(path) => eprintln!("wrote the qualification record to {}", path.display()),
        None => eprintln!("no {} was named, so this run wrote no record", ledger::SINK),
    }
}

/// The inventory says how many obligations each suite carries and which lanes a
/// campaign has to schedule. Printed under the run, because the number a person
/// quotes for "what Layer 2 requires" should come off the record rather than off
/// a memory of it.
#[test]
fn the_inventory_reports_what_the_layer_requires() {
    let mut rendered = String::from("Layer 2 certification inventory:\n");
    for suite in [
        Suite::Churn,
        Suite::InducedFailure,
        Suite::Operational,
        Suite::TrustTransition,
    ] {
        let count = REQUIRED_CASES
            .iter()
            .filter(|case| case.suite == suite)
            .count();
        rendered.push_str(&format!("  {:<18} {count}\n", suite.name()));
    }
    for lane in [
        Lane::Any,
        Lane::RealWatcher,
        Lane::RealWatcherVolumeFoldingDecides,
        Lane::RealWatcherBackendDecides,
    ] {
        let count = REQUIRED_CASES
            .iter()
            .filter(|case| case.lane == lane)
            .count();
        rendered.push_str(&format!("  lane {:<34} {count}\n", lane.name()));
    }
    rendered.push_str(&format!(
        "  inventory digest {}\n",
        inventory::contract_digest()
    ));
    eprintln!("{rendered}");

    assert_eq!(
        REQUIRED_CASES.len(),
        REQUIRED_CASES
            .iter()
            .map(|case| case.id)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    );
}
