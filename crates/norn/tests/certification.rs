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
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own log fixtures, and the lane files it reads.

use std::path::{Path, PathBuf};

use norn_testkit::certification::{
    inventory::{self, Lane, REQUIRED_CASES, Suite, UNREACHED_ARMS},
    lane,
    ledger::{self, CaseOutcome, Classification, Outcome, Platform, Preflight, Record, RunResult},
    manifest, preflight,
};
use norn_testkit::process::Sandbox;
use norn_testkit::regression::TestRef;

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
        ".github/scripts/certification-suite.sh",
        "Cargo.lock",
        "rust-toolchain.toml",
        // The rules: what the layer requires, how that is reconciled, and what
        // makes a record count.
        "crates/norn-testkit/src/certification/inventory.rs",
        "crates/norn-testkit/src/certification/lane.rs",
        "crates/norn-testkit/src/certification/ledger.rs",
        "crates/norn-testkit/src/certification/manifest.rs",
        "crates/norn-testkit/src/certification/preflight.rs",
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

/// **The collector.** A lane's suite logs become one outcome per required case,
/// and a full set of logs leaves no case `not-run`.
///
/// The claim under test is the one a scheduled lane's record rests on: every
/// required case is reachable from the logs the lane keeps, under the log names
/// the lane's script writes. A carrier the mapping cannot resolve, a target
/// whose log is named something the reader does not split, or an id the
/// translation drops all show here as a case with no line — which is exactly
/// what would classify a real run as a suite change.
///
/// The logs are synthesized rather than produced by running the suites: what is
/// being checked is the translation, and running every certification target to
/// check a mapping would make this case the lane it describes.
#[test]
fn a_lanes_suite_logs_become_an_outcome_for_every_required_case() {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), "lane-logs")
        .expect("a sandbox for the lane's logs");
    let logs = sandbox.work_dir();
    let invocations = lane::required_invocations();
    assert!(
        !invocations.is_empty(),
        "the inventory names no target to run, so a lane running it would certify nothing"
    );

    for invocation in &invocations {
        let mut rendered = String::from("running the suite\n");
        for case in REQUIRED_CASES {
            let reference = TestRef::parse(case.carrier).expect("a carrier reference");
            let target = reference.target().expect("a carrier's target");
            if target.package != invocation.package || target.target != invocation.target {
                continue;
            }
            // A unit test is listed under the module path its file compiles
            // into, and an integration test under its bare name — the two
            // spellings the harness writes, and both are what the collector
            // resolves against the inventory's carriers.
            rendered.push_str(&format!(
                "test {}{} ... ok\n",
                target.module_prefix, reference.function
            ));
        }
        rendered.push_str("test result: ok. some passed\n");
        std::fs::write(invocation.log(&logs), rendered).expect("writing a suite log");
    }

    let outcomes = lane::outcomes_from_logs(&logs).expect("reading the lane's logs");
    let lines: Vec<&str> = outcomes.lines().collect();
    for case in REQUIRED_CASES {
        assert!(
            lines.contains(&format!("{} passed", case.id).as_str()),
            "the logs report no outcome for `{}`, so a lane that ran it would record it not-run",
            case.id
        );
    }
    assert_eq!(
        lines.len(),
        REQUIRED_CASES.len(),
        "the outcomes carry a line the inventory does not require: {lines:?}"
    );
}

/// **The lane's collection step.** Where a run named a directory of logs, the
/// outcomes they hold are written beside them.
///
/// Its own entry point rather than a tail on the case above: that one asserts
/// over logs it synthesizes, and a fixture regression there would panic before
/// reaching this — leaving a real lane's outcomes file empty, every case
/// `not-run` and the record classified a suite change while every certification
/// suite in the same job ran green. This does one thing, so the only way it
/// collects nothing is that nothing named logs.
#[test]
fn the_lanes_case_outcomes_are_collected_where_a_lane_names_logs() {
    match lane::collect().expect("collecting the lane's outcomes") {
        Some(path) => eprintln!("wrote the lane's case outcomes to {}", path.display()),
        None => eprintln!("no {} was named, so this run collected none", lane::LOGS),
    }
}

/// **Every lane that writes a record ran the cases, labels what ran, and leaves
/// the record where the run can be read off it.**
///
/// Three claims about the lane files, and each is a way a record can be sound
/// and still wrong about the run behind it. A lane that skipped a required
/// target leaves those cases with no line, which reaches the record as a suite
/// change rather than as the lane gap it is. A lane's watcher-backend label is
/// a field the workflow sets rather than something the process observes, so a
/// label paired with a machine that installs a different backend would certify
/// the backend-deciding case on a runner that never ran it. And the record's
/// own plumbing is four independent strings — the log directory, the outcomes
/// file, the sink and the upload path — that name each other by convention and
/// nothing else, where every mismatch is silent: an outcomes file the record
/// does not find is 42 cases `not-run`, and a sink the upload does not find is
/// a run the campaign reads as killed for time.
///
/// The subject is every job that names a record sink, in any workflow: a job
/// that writes a qualification record is a lane whatever it is called. Read off
/// the text rather than off parsed YAML, and the runner and backend labels are
/// held together by adjacency because they are set one line apart in the block
/// that sets them.
#[test]
fn every_lane_that_writes_a_record_runs_the_cases_and_labels_its_backend() {
    let workflows = workspace_root().join(".github/workflows");
    let mut listing: Vec<PathBuf> = std::fs::read_dir(&workflows)
        .expect("reading the workflow directory")
        .map(|entry| entry.expect("a workflow directory entry").path())
        .collect();
    listing.sort();

    let mut lanes = Vec::new();
    for path in &listing {
        let workflow = std::fs::read_to_string(path).expect("reading a workflow");
        let file = path.file_name().unwrap_or_default().to_string_lossy();
        lanes.extend(
            jobs(&workflow)
                .into_iter()
                .filter(|(_, body)| body.contains(ledger::SINK))
                .map(|(job, body)| (format!("{file}:{job}"), body)),
        );
    }
    assert!(
        !lanes.is_empty(),
        "no job writes a qualification record, so nothing a campaign counts is produced"
    );

    for (lane, body) in &lanes {
        for invocation in lane::required_invocations() {
            let command = format!("certification-suite.sh {}", invocation.script_arguments());
            assert!(
                body.contains(&command),
                "`{lane}` writes a qualification record and does not run `{command}`, so every \
                 case that target carries reaches its record as not-run"
            );
        }
        assert_backend_label_matches_the_runner(lane, body);
        assert_the_outcomes_and_the_record_are_where_the_lane_looks(lane, body);
        assert_the_preflight_reading_precedes_the_build_and_reaches_the_record(lane, body);
    }
}

/// **A lane reads its host before it builds, classifies the reading, and the
/// verdict is in the environment the record is assembled from.**
///
/// Three links, and each is a way the slot silently empties. A reading taken
/// after a cold cargo build is a reading of that build — every lane would
/// refuse itself, and every record would be non-qualifying for a reason about
/// this run rather than about the machine. A reading nobody classifies leaves
/// the record with no verdict, which
/// [`ledger::NonQualifying::Environment`](norn_testkit::certification::ledger::NonQualifying)
/// reads as a host nobody checked. And a verdict written after the record is
/// assembled reaches nothing: `$GITHUB_ENV` applies to later steps only.
///
/// The order is read off line positions in the job body because that is what
/// decides it — steps run top to bottom — and the strings are the preflight
/// module's own constants rather than transcriptions, so a renamed script or
/// invocation fails here instead of leaving a lane calling nothing.
fn assert_the_preflight_reading_precedes_the_build_and_reaches_the_record(lane: &str, body: &str) {
    let at = |needle: &str| {
        body.lines()
            .map(str::trim)
            .filter(|line| !line.starts_with('#'))
            .position(|line| line.contains(needle))
    };

    let reading = at(preflight::READINGS_SCRIPT).unwrap_or_else(|| {
        panic!(
            "`{lane}` writes a qualification record and never runs `{}`, so its record carries no \
             host-health verdict and classifies as a host nobody checked",
            preflight::READINGS_SCRIPT
        )
    });
    let first_build = at("cargo ").unwrap_or_else(|| {
        panic!("`{lane}` writes a qualification record and runs no cargo command")
    });
    assert!(
        reading < first_build,
        "`{lane}` takes its host-health reading after it starts building. A cold build is minutes \
         of every core, so the load average it would read is this run's own compile and the lane \
         would refuse itself"
    );

    let classified = at(preflight::CLASSIFIER).unwrap_or_else(|| {
        panic!(
            "`{lane}` takes a host-health reading and never runs `{}`, so nothing turns it into \
             the verdict the record's preflight slot carries",
            preflight::CLASSIFIER
        )
    });
    let record = at(ledger::SINK).expect("a lane that writes a record names the sink");
    assert!(
        classified < record,
        "`{lane}` classifies its host after it assembles the record. A verdict appended to \
         `$GITHUB_ENV` reaches later steps only, so the record would be assembled without one"
    );

    let sink = one_setting(lane, body, preflight::SINK);
    let appended = format!("cat \"${}\" >> \"$GITHUB_ENV\"", preflight::SINK);
    assert!(
        body.contains(&appended),
        "`{lane}` writes its preflight verdict to `{sink}` and never appends it to the job \
         environment with `{appended}`, so the record is assembled without a verdict"
    );
}

/// Every non-comment line of a job body that sets `key`, as the value it sets.
///
/// A comment is not a setting: a job's own preamble is attributed to the job
/// above it — nothing at a job key's indentation is a job but the key — so a
/// paragraph describing one lane is read while another lane's body is.
fn settings<'a>(body: &'a str, key: &str) -> Vec<&'a str> {
    let prefix = format!("{key}: ");
    body.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.strip_prefix(&prefix))
        .map(str::trim)
        .collect()
}

/// The one value a job sets `key` to, refusing a job that sets it twice over or
/// not at all.
fn one_setting<'a>(lane: &str, body: &'a str, key: &str) -> &'a str {
    let set = settings(body, key);
    assert_eq!(
        set.len(),
        1,
        "`{lane}` sets `{key}` {} times, and this reads the one value it names: {set:?}",
        set.len()
    );
    set[0]
}

/// The backend a lane labels its records with is the one its runner installs,
/// and the runner it names is the machine the job runs on.
///
/// **The chain is three links and all three are checked.** `runs-on:` is the
/// machine; [`ledger::RUNNER`] is what the record calls it; and
/// [`ledger::WATCHER_BACKEND`] is what the record says that machine watches
/// through. A label alone says nothing about where it was produced, and two
/// labels agreeing with each other say nothing either — what makes them
/// truthful is that they are derived from the job's own image. Which backend a
/// platform installs is decided in `crates/norn-fs/src/watch.rs` — FSEvents on
/// macOS, the recommended native watcher on Linux, which is inotify, and a
/// polling substitute refused outright.
fn assert_backend_label_matches_the_runner(lane: &str, body: &str) {
    let machine = one_setting(lane, body, "runs-on");
    let installed = match machine {
        image if image.starts_with("ubuntu-") => "inotify",
        image if image.starts_with("macos-") => "fsevents",
        other => panic!(
            "`{lane}` runs on `{other}`, which is a runner this workspace states no watcher \
             backend for; what a platform installs is decided in crates/norn-fs/src/watch.rs"
        ),
    };

    let runner = format!("{}: ", ledger::RUNNER);
    let backend = format!("{}: ", ledger::WATCHER_BACKEND);
    let lines: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .collect();
    let labelled: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with(&backend))
        .map(|(at, _)| at)
        .collect();
    assert!(
        !labelled.is_empty(),
        "`{lane}` writes a record and names no watcher backend, and a record that names none \
         covers neither answer the backend-deciding case requires"
    );
    for at in labelled {
        let declared = lines[at]
            .strip_prefix(&backend)
            .expect("a line that starts with the backend key")
            .trim();
        let named_runner = lines[at.saturating_sub(1)]
            .strip_prefix(&runner)
            .unwrap_or_else(|| {
                panic!(
                    "`{}` in `{lane}` is not preceded by the runner it describes: `{}`",
                    lines[at],
                    lines[at.saturating_sub(1)]
                )
            })
            .trim();
        assert_eq!(
            named_runner, machine,
            "`{lane}` runs on `{machine}` and labels its records `{named_runner}`"
        );
        assert_eq!(
            declared, installed,
            "`{lane}` labels its records `{declared}` and hosts on `{machine}` install \
             `{installed}`"
        );
    }
}

/// A lane collects its outcomes where the collector writes them, and uploads
/// the record where the emitter writes it.
///
/// **Both sinks are written by a process whose directory is not the
/// workspace's.** Cargo runs an integration test of `norn` from `crates/norn`,
/// so a relative path in a lane names one file to the writer and another to
/// `actions/upload-artifact`, which resolves against the workspace. The record
/// is required to be workspace-absolute for that reason, and the upload is
/// required to name the same path.
///
/// The outcomes file has the same shape of hazard without the path confusion:
/// [`lane::collect`] writes [`lane::OUTCOMES_FILE`] inside the directory
/// [`lane::LOGS`] names, and the record step points [`ledger::OUTCOMES`] at it
/// by spelling the pairing out a second time. A record step that reads
/// elsewhere finds no outcomes and records every case `not-run`.
fn assert_the_outcomes_and_the_record_are_where_the_lane_looks(lane: &str, body: &str) {
    assert_eq!(
        body.matches(lane::CERTIFICATION_BINARY).count(),
        2,
        "`{lane}` runs `{}` other than twice. A lane runs it once to collect the outcomes off \
         its logs and once to assemble the record from them, and a lane missing the collection \
         records every case not-run.",
        lane::CERTIFICATION_BINARY
    );

    let logs: Vec<&str> = settings(body, lane::LOGS)
        .into_iter()
        .filter(|value| !value.is_empty() && *value != "''")
        .collect();
    assert_eq!(
        logs.len(),
        1,
        "`{lane}` names {} log directories, and the outcomes are collected into one: {logs:?}",
        logs.len()
    );
    let expected = format!("{}/{}", logs[0], lane::OUTCOMES_FILE);
    for named in settings(body, ledger::OUTCOMES) {
        assert_eq!(
            named, expected,
            "`{lane}` reads its outcomes from `{named}` and the collector writes them to \
             `{expected}`"
        );
    }

    let workspace = "${{ github.workspace }}/";
    let sink = one_setting(lane, body, ledger::SINK);
    assert!(
        sink.starts_with(workspace),
        "`{lane}` writes its record to `{sink}`, which the emitter resolves against \
         `crates/norn` and the upload resolves against the workspace, so the two name different \
         files and the artifact is never found"
    );
    assert!(
        settings(body, "path").contains(&sink),
        "`{lane}` writes its record to `{sink}` and uploads none of the paths it names: {:?}",
        settings(body, "path")
    );
}

/// Each job of one workflow, as its name and the text under it.
///
/// A job's own key is the only thing at two spaces of indentation under `jobs:`
/// that names a job: everything a job holds is deeper, and a comment at that
/// depth is prose about the job below it. Prose is not a key, so it falls into
/// the preceding job's text — which is why a reader of these bodies drops
/// comment lines before reading a setting off one.
fn jobs(workflow: &str) -> Vec<(String, String)> {
    let mut jobs: Vec<(String, String)> = Vec::new();
    let mut reached_the_jobs = false;
    for line in workflow.lines() {
        if line.trim_end() == "jobs:" {
            reached_the_jobs = true;
            continue;
        }
        if !reached_the_jobs {
            continue;
        }
        let names_a_job = line.starts_with("  ")
            && !line.starts_with("   ")
            && !line.trim_start().starts_with('#')
            && line.trim_end().ends_with(':');
        if names_a_job {
            jobs.push((line.trim().trim_end_matches(':').to_string(), String::new()));
        } else if let Some((_, body)) = jobs.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    jobs
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
