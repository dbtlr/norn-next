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
#[ignore = "audit-lane case: compiled suite inventory runs in full regression"]
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
        .filter(|case| case.suite == Suite::TrustTransition && case.lane.needs_real_watcher())
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
        ".github/workflows/regression.yml",
        ".github/workflows/certify.yml",
        ".github/workflows/soak.yml",
        ".github/scripts/lane-suite.sh",
        ".github/scripts/test-suite.sh",
        ".github/scripts/regression-identity.py",
        ".github/scripts/certification-suite.sh",
        ".github/scripts/soak-candidate.sh",
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
        "crates/norn-testkit/src/regression/compiled.rs",
        "crates/norn-testkit/src/process.rs",
        "crates/norn-testkit/src/process/group.rs",
        "crates/norn-testkit/src/scratch.rs",
        // Build provenance decides whether authored measurement bars apply.
        "crates/norn-testkit/src/build_subject.rs",
        "crates/norn-testkit/src/build_subject/provenance.rs",
        "crates/norn-testkit/build.rs",
        // The instruments a verdict is read off.
        "crates/norn-testkit/src/equivalence.rs",
        "crates/norn-testkit/src/churn.rs",
        "crates/norn-testkit/src/work.rs",
        "crates/norn-store/src/request/instrument.rs",
        "crates/norn-db/src/plan.rs",
        // The seams every rung-2 and rung-3 case is reached through.
        "crates/norn-fs/src/faults.rs",
        "crates/norn-fs/src/walk/faults.rs",
        "crates/norn-fs/src/watch/faults.rs",
        "crates/norn-store/src/faults.rs",
        "crates/norn-db/src/faults.rs",
        // Every suite a case in the inventory is carried by, and the bars in
        // them.
        "crates/norn-host/tests/churn.rs",
        "crates/norn-host/tests/equivalence.rs",
        "crates/norn-host/tests/lockdown.rs",
        "crates/norn-host/tests/kill_recovery.rs",
        "crates/norn-fs/tests/lockdown.rs",
        "crates/norn-fs/tests/observations.rs",
        "crates/norn-store/tests/environment.rs",
        "crates/norn-store/tests/store/pillars.rs",
        "crates/norn-store/tests/store/lifecycle.rs",
        "crates/norn-store/tests/store/equivalence.rs",
        // The soak workload whose comparisons apply the authored soak bars.
        "crates/norn-host/tests/host_soak.rs",
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
    let digest = manifest::digest(&root).expect("digesting the suite manifest");
    let sound = Record {
        candidate_sha: "0".repeat(40),
        suite_manifest_digest: digest.clone(),
        dispatcher_digest: digest,
        case_inventory_digest: inventory::contract_digest(),
        scheduled: true,
        dispatching_actor: ledger::DISPATCHER_IDENTITY.to_string(),
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
        unauthored_exit_bars: Vec::new(),
        classification: Classification::Qualifying,
    };
    // **Everything left is this build's own exit-bar registry.** With every bar
    // armed this record produces no problem at all and counts; a bar authored
    // `None` refuses every qualifying record while it stands, which is a fact
    // about that calibration window rather than about this record — so each
    // such refusal is named here and nothing else is admitted beside them. The
    // record's own consistency is what the rest of this case is about, and it
    // holds whichever way the registry stands.
    let unarmed = ledger::unauthored_exit_bars();
    let problems = sound.problems(&root);
    for name in &unarmed {
        assert!(
            problems
                .iter()
                .any(|problem| problem.contains(&format!("`{name}` unauthored"))),
            "{problems:?}"
        );
    }
    assert_eq!(problems.len(), unarmed.len(), "{problems:?}");
    assert_eq!(sound.qualifies(&root), unarmed.is_empty());

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

    let outcomes =
        lane::outcomes_from_logs(&workspace_root(), &logs).expect("reading the lane's logs");
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
    match lane::collect(&workspace_root()).expect("collecting the lane's outcomes") {
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
        assert_the_lane_certifies_the_ref_it_was_dispatched_at(lane, body);
    }
}

/// **A lane certifies the ref it was dispatched at, and carries down what only
/// the dispatcher knows.**
///
/// The dispatcher tags the pinned commit and starts the certification run at
/// that tag, so the lane's own checkout *is* the candidate and
/// [`ledger::Record::candidate_sha`] is `GITHUB_SHA`. Three ways that comes
/// apart, and each is silent:
///
/// - a `ref:` on a checkout, which is a second opinion about which commit to
///   certify taken inside the run it decides for;
/// - no [`ledger::SCHEDULED`] stamp, which leaves every run manual and the
///   count unable to advance — or, worse, leaves a record inheriting a default
///   nobody sent;
/// - no [`ledger::DISPATCHER_DIGEST`] stamp, which leaves the suite a value
///   this run alone attests.
///
/// Read structurally rather than by substring: every claim below is about a
/// key a step sets, so a lane that merely *mentions* one of these names in a
/// comment satisfies nothing here.
fn assert_the_lane_certifies_the_ref_it_was_dispatched_at(lane: &str, body: &str) {
    let pinned = settings(body, "ref");
    assert!(
        pinned.is_empty(),
        "`{lane}` names `ref:` {} time(s), and a lane dispatched at the tag holding the pinned \
         candidate is already standing in it — so a `ref:` here certifies some other commit than \
         the one the record will name: {pinned:?}",
        pinned.len()
    );
    for (key, what) in [
        (
            ledger::SCHEDULED,
            "what the dispatch asserted about the nightly cron, which every run of this file \
             looks like a manual dispatch from the inside",
        ),
        (
            ledger::DISPATCHING_ACTOR,
            "who the runner says made the dispatch, which is the unforgeable half of the \
             schedule term — without it the assertion above decides the count on its own",
        ),
        (
            ledger::DISPATCHER_DIGEST,
            "the suite-manifest digest the dispatcher read at the pin, which is the only reading \
             of the suite that is not this run's own",
        ),
    ] {
        let stamped: Vec<&str> = body
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with('#'))
            .filter(|line| line.contains(&format!("{key}=")))
            .collect();
        assert!(
            !stamped.is_empty(),
            "`{lane}` writes a qualification record and appends no `{key}` to the job \
             environment, so the record carries nothing for {what}"
        );
        for line in stamped {
            let value = stamped_value(line, key);
            // A `${{ }}` expression inside `run:` is shell text GitHub expands
            // before bash reads the line: the injection the `env:` binding
            // exists to prevent. Only a shell reference to the bound variable
            // is a stamp.
            assert!(
                value.starts_with('$') && !value.contains("${{"),
                "`{lane}` stamps `{key}` as `{value}`, a literal or an expression expanded \
                 into shell text rather than the bound variable the dispatch carried: {line}"
            );
        }
    }

    // **The shell variable is only half the chain.** The stamps above read
    // `$SCHEDULED`, `$DISPATCHING_ACTOR` and `$DISPATCHER_DIGEST`, and what
    // those hold is the `env:` mapping of the step that sets them. A mapping
    // naming a literal — `DISPATCHING_ACTOR: github-actions[bot]` — leaves
    // every assertion above satisfied and hands a typed dispatch the identity
    // the record treats as the dispatcher's, which is the whole of the
    // schedule guard. The `env:` mapping is also the only safe way to carry an
    // input into a `run:` block, because a `${{ }}` expansion inside one is
    // dispatcher text pasted into the script. So the three values a record
    // reads off the dispatch are held to the expressions that produce them,
    // exactly and nowhere else.
    for (name, expression, what) in [
        (
            "SCHEDULED",
            "${{ inputs.scheduled }}",
            "what the dispatch asserted",
        ),
        (
            "DISPATCHING_ACTOR",
            "${{ github.triggering_actor }}",
            "who the runner says made the dispatch, which no input can reach",
        ),
        (
            "DISPATCHER_DIGEST",
            "${{ inputs.dispatcher_digest }}",
            "the dispatcher's own reading of the suite at the pin",
        ),
    ] {
        let bound = settings(body, name);
        assert!(
            !bound.is_empty(),
            "`{lane}` stamps `{name}` into the record and binds it to nothing, so the stamp \
             carries an empty value for {what}"
        );
        for value in bound {
            assert_eq!(
                value, expression,
                "`{lane}` binds `{name}` to `{value}` rather than to `{expression}`. That value \
                 is {what}, and a literal there is a value somebody wrote into the lane instead \
                 of one the run was given."
            );
        }
    }

    assert_the_dispatch_inputs_are_shaped_before_they_are_stamped(lane, body);
}

/// The two dispatcher-supplied inputs are held to a shape before the step
/// appends anything to `GITHUB_ENV`.
///
/// **A stamp is a line, and a line break is a second stamp.** `GITHUB_ENV` is
/// read as `KEY=VALUE` lines, so an input carrying a newline writes whatever
/// follows it as an assignment of its own — including
/// [`ledger::DISPATCHING_ACTOR`], the unforgeable half of the schedule term.
/// The `env:` binding checked above keeps the value out of the script text; it
/// does not keep a newline out of the value. What does is the refusal: the
/// assertion is exactly `true` or `false`, the digest is 64 lowercase hex
/// characters or empty, and neither shape holds a line break.
///
/// Read positionally, because the order is the claim: a check that runs after
/// the append has already let the extra assignment through.
fn assert_the_dispatch_inputs_are_shaped_before_they_are_stamped(lane: &str, body: &str) {
    let lines: Vec<&str> = body.lines().map(str::trim).collect();
    let stamp = format!("{}=", ledger::SCHEDULED);
    let stamped_at = lines
        .iter()
        .position(|line| !line.starts_with('#') && line.contains(&stamp))
        .unwrap_or_else(|| {
            panic!(
                "`{lane}` appends no `{}` stamp to read an order against",
                ledger::SCHEDULED
            )
        });
    for (variable, shape) in [
        ("SCHEDULED", "exactly `true` or `false`"),
        ("DISPATCHER_DIGEST", "64 lowercase hex characters, or empty"),
    ] {
        let refused_at = lines[..stamped_at]
            .iter()
            .position(|line| line.contains("::error::") && line.contains(variable))
            .unwrap_or_else(|| {
                panic!(
                    "`{lane}` stamps the dispatch without refusing a `{variable}` that is not {shape} \
                     first, so a newline in that input writes a second assignment into the job \
                     environment"
                )
            });
        // The refusal has to be this diagnosis's own: an `exit 1` belonging to
        // the next check would read as this one's, and a lane that names a
        // shape and carries on would pass.
        let next_diagnosis = lines[refused_at + 1..stamped_at]
            .iter()
            .position(|line| line.contains("::error::"))
            .map_or(stamped_at, |offset| refused_at + 1 + offset);
        assert!(
            lines[refused_at..next_diagnosis]
                .iter()
                .any(|line| line.contains("exit 1")),
            "`{lane}` names a `{variable}` that is not {shape} and stamps it anyway: the \
             diagnosis is not followed by a refusal before the append"
        );
    }
}

/// What a stamp line assigns to `key`: the text between `key=` and the quote
/// closing the echoed assignment.
///
/// The line as a whole is no evidence about the value — every stamp ends in
/// `>> "$GITHUB_ENV"`, so a literal stamp still carries a `$`.
fn stamped_value<'a>(line: &'a str, key: &str) -> &'a str {
    line.split_once(&format!("{key}="))
        .map(|(_, assigned)| assigned)
        .unwrap_or_default()
        .split('"')
        .next()
        .unwrap_or_default()
        .trim()
}

/// **The pointer script holds the pin to the default branch.**
///
/// The dispatcher builds the pinned tree, and GitHub serves every sha reachable
/// in the repository network — a fork's pull-request head included. The
/// ancestry check is therefore the thing standing between a one-line pointer
/// edit and unreviewed code running with the dispatcher's token, and nothing
/// else in this workspace exercises it: a run of the script needs a remote and
/// a clone, which a pull request does not have.
///
/// So the invocation is read off the file. Weaker than running it, and it is
/// the claim that can be made here: an ancestry check deleted, or turned into a
/// comparison against something other than the branch the dispatcher passes in,
/// fails a pull request rather than passing quietly into a nightly.
#[test]
fn the_pointer_script_holds_the_pin_to_the_default_branch() {
    let script = std::fs::read_to_string(workspace_root().join(POINTER_SCRIPT))
        .expect("reading the pointer script");
    let invocations: Vec<&str> = script
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter(|line| line.contains("merge-base --is-ancestor"))
        .collect();
    assert_eq!(
        invocations.len(),
        1,
        "`{POINTER_SCRIPT}` names `merge-base --is-ancestor` {} times. The dispatcher checks the \
         pinned tree out and builds it, so a pin that was never on the default branch is \
         unreviewed code running with the dispatcher's token: {invocations:?}",
        invocations.len()
    );
    let invocation = invocations[0];
    assert!(
        invocation.contains("\"$sha\"") && invocation.contains("${default}"),
        "`{POINTER_SCRIPT}` checks ancestry of something other than the pin against the branch it \
         was given: {invocation}"
    );
    assert!(
        script
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with('#'))
            .any(|line| line.contains("git fetch --no-tags origin \"+refs/heads/${default}")),
        "`{POINTER_SCRIPT}` checks ancestry against a ref it never fetches, which answers nothing \
         on a clone that does not already hold the default branch"
    );
    assert!(
        !script
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with('#'))
            .any(|line| line.contains("--depth")),
        "`{POINTER_SCRIPT}` fetches shallowly, and an ancestry check over a shallow clone answers \
         about the history that was fetched rather than about the branch"
    );
}

/// The one parser of the pointer format, called by the dispatcher.
const POINTER_SCRIPT: &str = ".github/scripts/soak-candidate.sh";

/// **The dispatcher carries the clock and nothing a run does.**
///
/// A workflow's steps come from the ref it was triggered on, and a schedule
/// only ever triggers the default branch. The dispatcher therefore lives on the
/// default branch and the run lives at the pin — and the whole value of that
/// split is that nothing deciding what a run *does* sits in the dispatcher,
/// where the candidate's suite-manifest digest does not close over it. Lane
/// behaviour drifting back into the dispatcher's file would be a lane edit the
/// digest cannot see, and five runs agreeing on a digest would stop meaning
/// five runs of the same lanes.
///
/// So two claims: the dispatcher runs no lane script, and the one `cargo` it
/// runs is the digest computation it hands down.
#[test]
fn the_dispatcher_carries_no_lane_behaviour() {
    let dispatcher = std::fs::read_to_string(workspace_root().join(DISPATCHER))
        .expect("reading the dispatcher workflow");
    let steps: Vec<&str> = dispatcher
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .collect();

    for script in [
        ".github/scripts/lane-suite.sh",
        ".github/scripts/certification-suite.sh",
        TRIPWIRE,
    ] {
        let calls: Vec<&&str> = steps.iter().filter(|line| line.contains(script)).collect();
        assert!(
            calls.is_empty(),
            "`{DISPATCHER}` calls `{script}`. What a run does lives at the pin, inside the \
             suite-manifest digest the candidate is certified under, and a lane step here is one \
             that digest cannot see: {calls:?}"
        );
    }

    let cargo: Vec<&&str> = steps
        .iter()
        .filter(|line| line.contains("cargo "))
        .collect();
    assert_eq!(
        cargo.len(),
        1,
        "`{DISPATCHER}` runs cargo {} times, and the one invocation it is allowed is the \
         suite-manifest digest it reads at the pin and hands to the run: {cargo:?}",
        cargo.len()
    );
    assert!(
        cargo[0].contains("--test certification"),
        "`{DISPATCHER}`'s one cargo invocation is not the certification suite's own digest \
         reading, so the value it attests is computed by something other than the suite that \
         defines it: {}",
        cargo[0]
    );
    assert!(
        steps
            .iter()
            .any(|line| line.contains(ledger::CANDIDATE_POINTER)),
        "`{DISPATCHER}` runs no step naming `{}`, so nothing resolves the candidate and the \
         certification run is dispatched at whatever ref somebody last named. A comment naming \
         the pointer is not a step that reads it.",
        ledger::CANDIDATE_POINTER
    );
    assert!(
        steps.iter().any(|line| line.starts_with("- cron:")),
        "`{DISPATCHER}` holds no schedule, and the nightly is the one thing this file exists to \
         carry"
    );
}

/// The workflow on the default branch that reads the pointer and starts a
/// certification run at it.
const DISPATCHER: &str = ".github/workflows/soak.yml";

/// **The pinned candidate is one commit sha somebody can read.**
///
/// The pointer is the campaign's whole statement of what is being certified, so
/// a file the dispatcher refuses is a file that starts no certification run at
/// all. The shape is held here, where a pull request meets it, rather than at
/// 04:00 — and it is the same shape `.github/scripts/soak-candidate.sh`
/// enforces, one definition of the format read twice rather than two
/// definitions that can drift apart.
///
/// The sha is not resolved against the object store here: a reviewer's checkout
/// may be shallow, and it may not hold a commit that exists on the remote. What
/// resolves it is the dispatcher, which fetches the commit and checks the tree
/// at it carries the certification workflow before it dispatches anything — so
/// an unfetchable or pre-mechanism pin ends that job red rather than leaving a
/// certification run without a record.
#[test]
fn the_pinned_candidate_is_one_commit_sha() {
    let pointer = workspace_root().join(ledger::CANDIDATE_POINTER);
    let text = std::fs::read_to_string(&pointer).unwrap_or_else(|problem| {
        panic!(
            "reading {}: {problem} — the scheduled lanes read this pointer to know which commit \
             they certify",
            pointer.display()
        )
    });
    let named: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    assert_eq!(
        named.len(),
        1,
        "`{}` holds {} lines that are not comments, and the lanes read one sha out of it: {named:?}",
        ledger::CANDIDATE_POINTER,
        named.len()
    );
    let sha = named[0];
    assert!(
        sha.len() == 40
            && sha
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "`{}` names `{sha}`, which is not a 40-digit lowercase commit sha — so the lanes read no \
         candidate and every scheduled run records a mismatch",
        ledger::CANDIDATE_POINTER
    );
    eprintln!("the pinned soak candidate is {sha}");
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
    // The first step that does work, by whatever spelling it reaches cargo
    // with. Keying on the literal `cargo ` alone would read the lanes' own
    // suite steps as something other than a build — every one of them runs
    // through a script — and place the reading behind an hour of soak load
    // while still passing.
    let first_build = body
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .position(|line| line.contains("cargo ") || reaches_a_suite(line))
        .unwrap_or_else(|| {
            panic!("`{lane}` writes a qualification record and runs no suite and no cargo command")
        });
    assert!(
        reading < first_build,
        "`{lane}` takes its host-health reading after it starts working. A cold build is minutes \
         of every core and a soak step is an hour of them, so the share it would read is this \
         run's own work and the lane would refuse itself"
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

/// **The line the lane's reading script writes is the line the classifier
/// reads**, key for key.
///
/// The measurement exists twice on purpose — a lane's reading is taken before
/// the toolchain is built, which is before any Rust of ours can run — and two
/// spellings of one rule drift, invisibly. The drift is silent in the worst
/// direction: a key renamed on one side leaves the field unmeasured on the
/// other, every scheduled run records `environment`, both lanes stay green, and
/// the count a campaign is waiting on never advances.
///
/// The script is run, not read. What binds the two spellings is that the line
/// it emits survives [`preflight::Reading::parse`] and comes back out of
/// `render` unchanged: a key only one side knows is dropped by the parse and
/// missing from the re-render.
///
/// The verdict is not the subject and is not asserted. A machine running this
/// suite is a machine under load, and what is being checked is that every field
/// was *measured* — which is what the classifier refuses on when the spellings
/// have parted.
#[test]
#[cfg(unix)]
fn a_lanes_reading_script_writes_the_line_the_classifier_reads() {
    let root = workspace_root();
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), "host-readings")
        .expect("a sandbox for the readings script");
    let began = std::time::Instant::now();
    let outcome = norn_testkit::process::Run::new(&sandbox, root.join(preflight::READINGS_SCRIPT))
        .wait()
        .expect("running the host-readings script");
    let took = began.elapsed();
    outcome.assert_success();

    // The window is the other half of the agreement, and the bound is placed
    // against readings taken over it: a script that stopped settling would read
    // the tail of the checkout that ran before it — the same runner reads in the
    // fifties that way — and `BUSY_BOUND` would then be a bound on a different
    // quantity than the one it was calibrated against. Both spellings reach the
    // window by sleeping or by asking their platform's tool to, so the time the
    // script spends is what says it took one.
    let window = preflight::SETTLE + preflight::SAMPLE_WINDOW;
    assert!(
        took >= window,
        "`{}` answered in {took:?}, inside the {window:?} settle and sample window the classifier \
         reads over — so the two are sampling different quantities and the busy bound is placed \
         against neither",
        preflight::READINGS_SCRIPT
    );

    let written = outcome.stdout_text();
    let written = written.trim_end_matches('\n');
    assert!(
        !written.contains('\n'),
        "`{}` writes more than the one assignment a job environment carries: {written:?}",
        preflight::READINGS_SCRIPT
    );
    let line = written
        .strip_prefix(&format!("{}=", preflight::READING))
        .unwrap_or_else(|| {
            panic!(
                "`{}` writes {written:?}, which assigns nothing to `{}` — so the lane's reading \
                 never reaches the job environment and every record classifies as a host nobody \
                 checked",
                preflight::READINGS_SCRIPT,
                preflight::READING
            )
        });

    let reading = preflight::Reading::parse(line);
    assert_eq!(
        reading.render(),
        line,
        "`{}` and `Reading::parse`/`render` have parted: a pair one side writes and the other \
         cannot read leaves its field unmeasured, and every scheduled run then records the \
         environment rather than the candidate",
        preflight::READINGS_SCRIPT
    );
    assert!(
        reading.cores.is_some(),
        "`{}` measured no core count: {line:?}",
        preflight::READINGS_SCRIPT
    );
    assert!(
        reading.busy_deci_percent.is_some(),
        "`{}` measured no processor share: {line:?}",
        preflight::READINGS_SCRIPT
    );
    let daemon_is_read = reading.fseventsd != preflight::Fseventsd::NotApplicable;
    assert_eq!(
        daemon_is_read,
        cfg!(target_os = "macos"),
        "`{}` and the classifier disagree about whether this platform runs an event daemon: \
         {line:?}",
        preflight::READINGS_SCRIPT
    );
    eprintln!("host-health reading: {line}");
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
    // Named one by one rather than by prefix: a label this workspace has not
    // read the machine behind is a label it states no backend for.
    let installed = match machine {
        "ubuntu-latest" => "inotify",
        // The hosted macOS image the per-PR platform lane runs on.
        "macos-15" => "fsevents",
        // The self-hosted runner the scheduled macOS lane runs on: a macOS 15
        // virtual machine on Apple silicon, registered to this repository under
        // this label, whose job asserts the architecture and the major version
        // before it reads anything.
        "macos-norn-soak" => "fsevents",
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

/// **Every CI suite runs through the flake tripwire, and every ledger
/// entry can match something.**
///
/// The bar the tripwire carries is that a second occurrence of a ledgered
/// failure surfaces as a record rather than as a quiet rerun, and it has two
/// silent holes. A suite step that stops going through the wrapper is scanned
/// against nothing; an entry missing a field is an entry the matcher skips, so
/// the class it was opened for stops being watched while the file still shows
/// it. Both are read off the files themselves rather than off a memory of how
/// they were wired.
///
/// The PR gate and full regression are the subject. The dormant soak tier keeps every suite
/// log and leaves a typed record, so a recurrence there is written down
/// already; what a rerun erases is a per-PR failure nobody kept.
#[test]
fn every_ci_suite_runs_through_the_tripwire_and_every_ledger_entry_can_match() {
    let root = workspace_root();
    for file in ["ci.yml", "regression.yml"] {
        let workflow = std::fs::read_to_string(root.join(".github/workflows").join(file))
            .expect("reading a CI workflow");
        let unwrapped: Vec<&str> = workflow
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with('#'))
            .filter(|line| reaches_a_suite(line))
            .filter(|line| !line.contains(TRIPWIRE))
            .collect();
        assert!(
            unwrapped.is_empty(),
            "`{file}` has suites outside `{TRIPWIRE}`: {unwrapped:?}"
        );
        assert!(
            workflow.contains(TRIPWIRE),
            "`{file}` runs no wrapped suite"
        );
    }

    let ledger =
        std::fs::read_to_string(root.join(".github/flake-ledger")).expect("reading the ledger");
    let entries = ledger_entries(&ledger);
    assert!(
        !entries.is_empty(),
        "the flake ledger holds no entry, so the tripwire matches every failure against nothing"
    );
    for fields in &entries {
        for key in ["id", "signature", "class", "first-seen", "disposition"] {
            let value = fields
                .iter()
                .find(|(named, _)| *named == key)
                .map(|(_, value)| value.trim());
            assert!(
                value.is_some_and(|value| !value.is_empty()),
                "a flake-ledger entry names no `{key}`, and the matcher skips an entry it cannot \
                 read whole — so the class it was opened for stops being watched: {fields:?}"
            );
        }
    }
    eprintln!(
        "flake ledger: {} entries the per-PR lane is scanned against",
        entries.len()
    );
}

/// One CI job's uncommented body lines, selected by its key.
fn ci_job(workflow: &str, key: &str) -> Vec<String> {
    let header = format!("  {key}:");
    let mut lines = workflow
        .lines()
        .skip_while(|line| *line != header)
        .skip(1)
        .take_while(|line| line.is_empty() || line.starts_with("    ") || line.starts_with("  #"))
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .map(str::to_string)
        .peekable();
    assert!(lines.peek().is_some(), "the workflow has no `{key}` job");
    lines.collect()
}

/// The PR gate retains every ordinary host target while deep lanes run in full regression.
#[test]
fn the_fast_gate_keeps_ordinary_functional_coverage() {
    let gate = std::fs::read_to_string(workspace_root().join(".github/workflows/ci.yml"))
        .expect("reading the PR gate");
    assert_fast_gate(&gate);
    let mut mutants = vec![
        gate.replace("  architecture:", "  missing:"),
        gate.replace("--retries 0", "--retries 1"),
        gate.replace("cargo test --locked --workspace --doc", "true"),
        gate.replace(
            "    name: lint and document",
            "    continue-on-error: true\n    name: lint and document",
        ),
        gate.replace("cargo fmt --all --check", "true"),
        gate.replace(
            "--features induced-failure\n",
            "--features induced-failure --skip ordinary\n",
        ),
        gate.replace(
            "--features induced-failure\n",
            "--features induced-failure --test churn\n",
        ),
        gate.replace("--include-ignored", ""),
        gate.replace(
            "    name: build and test",
            "    if: always()\n    name: build and test",
        ),
        gate.replace(
            "      - name: Test\n",
            "      - name: Test\n        if: always()\n",
        ),
    ];
    for case in NATIVE_CASES {
        mutants.push(
            gate.lines()
                .filter(|line| !line.contains(case))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    for mutant in mutants {
        assert_ne!(mutant, gate, "a mutant made no change");
        assert!(
            std::panic::catch_unwind(|| assert_fast_gate(&mutant)).is_err(),
            "a PR coverage mutant survived"
        );
    }
}

fn assert_fast_gate(workflow: &str) {
    assert_triggers(
        workflow,
        &[
            "pull_request:",
            "push:",
            "branches: [main]",
            "workflow_dispatch:",
        ],
    );
    assert_complete_active_matrix(workflow, false);
}

const NATIVE_CASES: &[&str] = &[
    "a_case_flip_converges_on_a_build_from_zero",
    "a_case_renamed_parent_over_a_save_converges_on_a_build_from_zero",
    "an_unreadable_document_is_marked_broken_and_the_heal_reaches_ready",
    "an_unreadable_subtree_is_withheld_and_marked_and_the_heal_reaches_ready",
];

/// Full regression alone must vouch for every fault and compiled-audit target.
#[test]
fn full_regression_adopts_fault_and_audit_cases_with_their_features() {
    let root = workspace_root();
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), "regression-routing")
        .expect("a sandbox for regression alone");
    let workflows = sandbox.root().join(".github/workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    std::fs::copy(
        root.join(".github/workflows/regression.yml"),
        workflows.join("regression.yml"),
    )
    .unwrap();
    for (package, targets, prefix) in [
        (
            "norn-host",
            &["lockdown", "kill_recovery"][..],
            "fault-lane case:",
        ),
        (
            "norn",
            &["regression", "certification"][..],
            "audit-lane case:",
        ),
    ] {
        let manifest = sandbox.root().join(package);
        std::fs::create_dir_all(manifest.join("tests")).unwrap();
        let mut lanes: Vec<_> = targets.iter().map(|stem| (*stem, prefix)).collect();
        if package == "norn-host" {
            for (target, lane) in [
                ("counter_gate", "counter-lane case:"),
                ("folding_cost", "counter-lane case:"),
                ("memory", "memory-lane case:"),
            ] {
                lanes.push((target, lane));
                std::fs::copy(
                    root.join(format!("crates/{package}/tests/{target}.rs")),
                    manifest.join(format!("tests/{target}.rs")),
                )
                .unwrap();
            }
        }
        for target in targets {
            let source =
                std::fs::read_to_string(root.join(format!("crates/{package}/tests/{target}.rs")))
                    .unwrap();
            assert!(
                source.contains(&format!("#[ignore = \"{prefix}")),
                "{package} {target} has no lane case"
            );
            if package == "norn-host" {
                let tests = source
                    .lines()
                    .filter(|line| line.trim() == "#[test]")
                    .count();
                let ignored = source
                    .lines()
                    .filter(|line| line.trim().starts_with("#[ignore = \"fault-lane case:"))
                    .count();
                assert_eq!(tests, ignored, "every fault case is ignored");
                assert_eq!(tests, if *target == "lockdown" { 20 } else { 6 });
            }
            std::fs::write(manifest.join(format!("tests/{target}.rs")), source).unwrap();
        }
        norn_testkit::lanes::assert_lane_steps_agree(&manifest, package, &lanes);
        norn_testkit::lanes::assert_lane_steps_name_the_features_their_targets_need(
            &manifest, package, &lanes,
        );
    }
    let gate = std::fs::read_to_string(root.join(".github/workflows/ci.yml")).unwrap();
    assert_fast_gate(&gate);
    let manifest = std::fs::read_to_string(root.join("crates/norn-host/Cargo.toml")).unwrap();
    assert!(
        !manifest.contains("autotests = false"),
        "ordinary host targets remain auto-discovered"
    );
}

const FULL_JOB_KEYS: &[&str] = &[
    "build",
    "induced-failure",
    "platform",
    "folding-counters",
    "architecture",
    "gates",
    "memory",
];

/// Pin the block-mapping form this file-specific command guard understands.
/// Unknown execution settings must fail rather than leave a literal command
/// present while changing the shell, compiler environment or working directory.
fn assert_matrix_mapping_keys(workflow: &str, full: bool) {
    let mut root_key = "";
    for line in workflow
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
    {
        let indent = line.len() - line.trim_start().len();
        if indent == 0 {
            let (key, _) = line.split_once(':').expect("a top-level workflow key");
            assert!(
                [
                    "name",
                    "run-name",
                    "on",
                    "permissions",
                    "concurrency",
                    "jobs"
                ]
                .contains(&key)
                    || full && key == "env",
                "unsupported workflow key: {key}"
            );
            root_key = key;
        } else if root_key == "env" {
            let (key, _) = line
                .trim_start()
                .split_once(':')
                .expect("an identity env binding");
            assert_eq!(indent, 2);
            assert!(
                [
                    "NORN_REGRESSION_SHA",
                    "NORN_REGRESSION_EVENT_SHA",
                    "NORN_REGRESSION_WORKFLOW_SHA"
                ]
                .contains(&key),
                "unsupported workflow environment: {key}"
            );
        }
    }
    for (_, body) in jobs(workflow) {
        let mut nested = "";
        let mut step_name = "";
        let mut literal_run = false;
        for line in body
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        {
            let indent = line.len() - line.trim_start().len();
            if indent == 10 && nested == "run" {
                assert!(
                    literal_run && step_name == "Show toolchain",
                    "plain command continuations are unsupported"
                );
                assert!(
                    ["rustc --version", "cargo --version"].contains(&line.trim()),
                    "the toolchain report carries only version reads"
                );
                continue;
            }
            let field = line
                .trim_start()
                .strip_prefix("- ")
                .unwrap_or(line.trim_start());
            let (key, value) = field.split_once(':').expect("a block mapping field");
            match indent {
                4 => {
                    assert!(
                        ["name", "runs-on", "timeout-minutes", "steps"].contains(&key),
                        "unsupported job key: {key}"
                    );
                    nested = "";
                }
                6 | 8 => {
                    if indent == 6 {
                        assert!(line.starts_with("      - "));
                    }
                    assert!(
                        ["name", "if", "run", "env", "uses", "with"].contains(&key)
                            || (full
                                && key == "id"
                                && step_name == "Record candidate identity"
                                && value.trim() == "identity"),
                        "unsupported step key: {key}"
                    );
                    if matches!(key, "env" | "with") {
                        assert!(
                            value.trim().is_empty(),
                            "nested settings use block mappings"
                        );
                    }
                    if key != "run" {
                        assert!(
                            !value.trim_start().starts_with(['|', '>']),
                            "only a run may be a literal block"
                        );
                    }
                    if indent == 6 {
                        step_name = "";
                    }
                    if key == "name" {
                        step_name = value.trim();
                    }
                    literal_run = key == "run" && value.trim() == "|";
                    nested = key;
                }
                10 if nested == "env" => assert!(
                    (key == "RUSTDOCFLAGS"
                        && value.trim() == "-D warnings"
                        && step_name == "Document")
                        || (key == "LANE_FEATURES" && value.trim() == "induced-failure"),
                    "unsupported compiler/runtime environment: {key}"
                ),
                10 if nested == "with" => assert!(
                    [
                        "persist-credentials",
                        "tool",
                        "args",
                        "name",
                        "path",
                        "if-no-files-found"
                    ]
                    .contains(&key),
                    "unsupported action input: {key}"
                ),
                _ => panic!("unsupported workflow mapping layout: {line}"),
            }
        }
    }
}

/// Semantic step values in the pinned block layout; names and widening conditions
/// are checked separately. Keeping inputs with their step prevents a harmless
/// cache step from supplying a setting a required test/action no longer receives.
fn matrix_step_values(workflow: &str, job: &str) -> Vec<String> {
    let (_, body) = jobs(workflow)
        .into_iter()
        .find(|(key, _)| key == job)
        .expect("a matrix job");
    let mut reached_steps = false;
    let mut steps: Vec<Vec<String>> = Vec::new();
    for line in body
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
    {
        if line == "    steps:" {
            reached_steps = true;
            continue;
        }
        if !reached_steps {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if line.starts_with("      - ") {
            steps.push(Vec::new());
        }
        let field = line
            .trim_start()
            .strip_prefix("- ")
            .unwrap_or(line.trim_start());
        let key = field.split_once(':').map(|(key, _)| key);
        if indent <= 8 && matches!(key, Some("name" | "if" | "env" | "with")) {
            continue;
        }
        steps
            .last_mut()
            .expect("a step field")
            .push(field.to_string());
    }
    steps.into_iter().map(|fields| fields.join("\n")).collect()
}

fn assert_matrix_step_values(workflow: &str, job: &str, runs: &[&str], full: bool) {
    let mut expected = vec!["uses: actions/checkout@v7\npersist-credentials: false".to_string()];
    if full {
        expected.push("id: identity\nrun: python3 .github/scripts/regression-identity.py \"$RUNNER_TEMP/regression-identity.json\"".to_string());
    }
    if job == "build" {
        expected.push("run: |\nrustc --version\ncargo --version".to_string());
    }
    if matches!(job, "platform" | "folding-counters") {
        expected.push("run: test \"$(uname -m)\" = arm64".to_string());
    }
    if job == "architecture" {
        expected.push("uses: docker://rhysd/actionlint:1.7.12\nargs: -color".to_string());
    }
    expected.push("uses: Swatinem/rust-cache@v2".to_string());
    if !full && matches!(job, "build" | "host-functional") {
        expected.push("uses: taiki-e/install-action@v2\ntool: cargo-nextest@0.9.114".to_string());
    }
    for command in runs {
        let command = if full {
            command.replace("cargo test --locked", "cargo test --locked --no-fail-fast")
        } else {
            command.to_string()
        };
        let mut step = format!("run: {command}");
        if command.starts_with("cargo doc ") {
            step.push_str("\nRUSTDOCFLAGS: -D warnings");
        }
        if ["counter_gate", "folding_cost", "lockdown", "kill_recovery"]
            .iter()
            .any(|target| command.contains(&format!("lane-suite.sh norn-host {target}")))
        {
            step.push_str("\nLANE_FEATURES: induced-failure");
        }
        expected.push(step);
    }
    if full {
        expected.push("uses: actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9\nname: regression-${{ github.job }}-${{ github.run_attempt }}\npath: ${{ runner.temp }}/regression-identity.json\nif-no-files-found: error".to_string());
    }
    assert_eq!(
        matrix_step_values(workflow, job),
        expected,
        "{job} changed its ordered execution or inputs"
    );
}

/// Literal commands establish the plain-workspace authority the generic reader excludes.
fn assert_complete_active_matrix(workflow: &str, full: bool) {
    assert_matrix_mapping_keys(workflow, full);
    assert_eq!(
        jobs(workflow)
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        if full {
            FULL_JOB_KEYS
        } else {
            &[
                "build",
                "lint",
                "host-functional",
                "platform",
                "architecture",
            ]
        }
    );
    let commands: &[(&str, &str, &[&str])] = &[
        (
            "build",
            "build and test",
            &[
                "cargo fmt --all --check",
                "cargo build --locked --workspace",
                "cargo clippy --locked --workspace --all-targets -- -D warnings",
                "cargo clippy --locked -p norn-host --test performance -- -D warnings",
                "cargo clippy --locked -p norn-host --test performance --features induced-failure -- -D warnings",
                "python3 -m unittest discover -s scripts/performance -v",
                "cargo clippy --locked -p norn-db -p norn-fs -p norn-store --lib -- -D warnings",
                "cargo clippy --locked -p norn-host --all-targets --features induced-failure -- -D warnings",
                "cargo doc --locked --workspace --no-deps --document-private-items",
                ".github/scripts/flake-tripwire.sh cargo test --locked --workspace",
            ],
        ),
        (
            "induced-failure",
            "induced-failure recovery",
            &[
                ".github/scripts/flake-tripwire.sh cargo test --locked -p norn-host --features induced-failure",
            ],
        ),
        (
            "platform",
            "determinism and filesystem (macOS ARM64)",
            &[
                ".github/scripts/flake-tripwire.sh cargo test --locked -p norn-embed --test determinism",
                ".github/scripts/flake-tripwire.sh cargo test --locked -p norn-fs",
                ".github/scripts/flake-tripwire.sh cargo test --locked -p norn-host --features induced-failure",
            ],
        ),
        (
            "folding-counters",
            "counter bars on a folding root (macOS ARM64)",
            &[
                ".github/scripts/flake-tripwire.sh .github/scripts/lane-suite.sh norn-host folding_cost --nocapture --test-threads=1",
                ".github/scripts/flake-tripwire.sh .github/scripts/lane-suite.sh norn-host counter_gate --nocapture --test-threads=1",
            ],
        ),
        (
            "architecture",
            "architecture gate",
            &[
                ".github/scripts/flake-tripwire.sh cargo test --locked -p norn-testkit --test architecture",
            ],
        ),
        (
            "gates",
            "counter gates",
            &[
                ".github/scripts/flake-tripwire.sh .github/scripts/lane-suite.sh norn-host counter_gate --nocapture --test-threads=1",
                ".github/scripts/flake-tripwire.sh .github/scripts/lane-suite.sh norn-host folding_cost --nocapture --test-threads=1",
            ],
        ),
        (
            "memory",
            "memory invariant",
            &[
                ".github/scripts/flake-tripwire.sh .github/scripts/lane-suite.sh norn-fixtures memory --nocapture --test-threads=1",
                ".github/scripts/flake-tripwire.sh .github/scripts/lane-suite.sh norn-fs walk_memory --nocapture --test-threads=1",
                ".github/scripts/flake-tripwire.sh .github/scripts/lane-suite.sh norn-store count_memory --nocapture --test-threads=1",
                ".github/scripts/flake-tripwire.sh .github/scripts/lane-suite.sh norn-store heap --nocapture --test-threads=1",
                ".github/scripts/flake-tripwire.sh .github/scripts/lane-suite.sh norn-host memory --nocapture --test-threads=1",
            ],
        ),
    ];
    for &(mut key, mut name, runs) in commands {
        if !full && matches!(key, "folding-counters" | "gates" | "memory") {
            continue;
        }
        let mut runs = runs.to_vec();
        if full {
            match key {
                "build" => runs.extend([
                    ".github/scripts/flake-tripwire.sh .github/scripts/lane-suite.sh norn regression",
                    ".github/scripts/flake-tripwire.sh .github/scripts/lane-suite.sh norn certification",
                ]),
                "induced-failure" | "platform" => runs.extend([
                    ".github/scripts/flake-tripwire.sh .github/scripts/lane-suite.sh norn-host lockdown",
                    ".github/scripts/flake-tripwire.sh .github/scripts/lane-suite.sh norn-host kill_recovery",
                ]),
                _ => {},
            }
        } else if key == "build" {
            let lint_runs: Vec<&str> = runs
                .iter()
                .copied()
                .filter(|command| {
                    command.starts_with("cargo fmt")
                        || command.starts_with("cargo clippy")
                        || command.starts_with("cargo doc")
                        || command.starts_with("python3")
                })
                .collect();
            let lint_lines = ci_job(workflow, "lint");
            assert_ci_lines(
                &lint_lines,
                &[
                    "name: lint and document",
                    "runs-on: ubuntu-latest",
                    "timeout-minutes: 30",
                ],
            );
            assert_job_cannot_skip_or_excuse(&lint_lines);
            assert_matrix_step_values(workflow, "lint", &lint_runs, false);
            runs = vec![
                "cargo build --locked --workspace",
                ".github/scripts/flake-tripwire.sh cargo nextest run --locked --retries 0 --no-fail-fast --workspace",
                ".github/scripts/flake-tripwire.sh cargo test --locked --workspace --doc",
            ];
        } else if key == "induced-failure" {
            key = "host-functional";
            name = "host functional (induced-failure)";
            runs = vec![
                ".github/scripts/flake-tripwire.sh cargo nextest run --locked --retries 0 --no-fail-fast -p norn-host --features induced-failure",
                ".github/scripts/flake-tripwire.sh cargo test --locked -p norn-host --features induced-failure --doc",
            ];
        } else if key == "platform" {
            runs = vec![
                ".github/scripts/flake-tripwire.sh cargo test --locked -p norn-embed --test determinism",
                ".github/scripts/flake-tripwire.sh cargo test --locked -p norn-fs",
                ".github/scripts/flake-tripwire.sh .github/scripts/test-suite.sh cargo test --locked -p norn-host --features induced-failure --lib",
                ".github/scripts/flake-tripwire.sh .github/scripts/test-suite.sh cargo test --locked -p norn-host --features induced-failure --test early_reads",
                ".github/scripts/flake-tripwire.sh .github/scripts/test-suite.sh cargo test --locked -p norn-host --features induced-failure --test provisional_registration",
                ".github/scripts/flake-tripwire.sh cargo test --locked -p norn-host --features induced-failure --test obsidian_parity",
                ".github/scripts/flake-tripwire.sh .github/scripts/test-suite.sh cargo test --locked -p norn-host --features induced-failure --test churn a_case_flip_converges_on_a_build_from_zero -- --exact --include-ignored",
                ".github/scripts/flake-tripwire.sh .github/scripts/test-suite.sh cargo test --locked -p norn-host --features induced-failure --test churn a_case_renamed_parent_over_a_save_converges_on_a_build_from_zero -- --exact --include-ignored",
                ".github/scripts/flake-tripwire.sh .github/scripts/test-suite.sh cargo test --locked -p norn-host --features induced-failure --test lockdown an_unreadable_document_is_marked_broken_and_the_heal_reaches_ready -- --exact --include-ignored",
                ".github/scripts/flake-tripwire.sh .github/scripts/test-suite.sh cargo test --locked -p norn-host --features induced-failure --test lockdown an_unreadable_subtree_is_withheld_and_marked_and_the_heal_reaches_ready -- --exact --include-ignored",
                ".github/scripts/flake-tripwire.sh .github/scripts/lane-suite.sh norn-host folding_cost --nocapture --test-threads=1",
            ];
        }
        let lines = ci_job(workflow, key);
        let name = if full {
            format!("full {name}")
        } else {
            name.to_string()
        };
        assert_ci_lines(&lines, &[&format!("name: {name}")]);
        let timeout = match key {
            "memory" => 20,
            "architecture" => 10,
            _ => 30,
        };
        assert_ci_lines(&lines, &[&format!("timeout-minutes: {timeout}")]);
        let mac = matches!(key, "platform" | "folding-counters");
        assert_ci_lines(
            &lines,
            &[if mac {
                "runs-on: macos-15"
            } else {
                "runs-on: ubuntu-latest"
            }],
        );
        if mac {
            assert_ci_lines(&lines, &["run: test \"$(uname -m)\" = arm64"]);
        }
        if full {
            assert_full_job_collects_evidence(workflow, key);
        } else {
            assert_job_cannot_skip_or_excuse(&lines);
        }
        assert_matrix_step_values(workflow, key, &runs, full);
    }
    assert!(
        !ci_job(workflow, "build")
            .iter()
            .any(|line| line.starts_with("run:")
                && line.contains("cargo test")
                && line.contains("--features induced-failure")),
        "host recovery stays in its independent job"
    );
}

fn assert_full_job_collects_evidence(workflow: &str, key: &str) {
    let (_, body) = jobs(workflow)
        .into_iter()
        .find(|(found, _)| found == key)
        .expect("a full job");
    let lines = ci_job(workflow, key);
    assert_ci_lines(
        &lines,
        &[
            "- uses: actions/checkout@v7",
            "persist-credentials: false",
            "run: python3 .github/scripts/regression-identity.py \"$RUNNER_TEMP/regression-identity.json\"",
            "uses: actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9",
            "name: regression-${{ github.job }}-${{ github.run_attempt }}",
            "path: ${{ runner.temp }}/regression-identity.json",
            "if-no-files-found: error",
        ],
    );
    assert!(
        !lines.iter().any(|line| line.starts_with("ref:")),
        "checkout must use the captured workflow ref"
    );
    let executable = body
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let steps: Vec<_> = executable.split("      - ").skip(1).collect();
    assert!(steps[0].starts_with("uses: actions/checkout@v7\n"));
    assert!(steps[1].starts_with("name: Record candidate identity\n"));
    assert!(steps[1].lines().any(|line| line == "        run: python3 .github/scripts/regression-identity.py \"$RUNNER_TEMP/regression-identity.json\""));
    assert!(steps[1].lines().any(|line| line == "        id: identity"));
    for step in steps.iter().take(2) {
        assert!(
            !step
                .lines()
                .any(|line| line.trim_start().starts_with("if:"))
        );
    }
    for (index, step) in steps.iter().enumerate().skip(2) {
        let expected = if index == steps.len() - 1 {
            "if: ${{ !cancelled() }}"
        } else {
            "if: ${{ !cancelled() && steps.identity.outcome == 'success' }}"
        };
        assert!(
            step.lines().any(|line| line
                .trim_start()
                .strip_prefix("- ")
                .unwrap_or(line.trim_start())
                == expected),
            "{key} must reject a mismatched checkout, collect later test failures and retain its receipt: {step}"
        );
    }
}

fn assert_regression_workflow(workflow: &str) {
    assert_triggers(
        workflow,
        &[
            "schedule:",
            "- cron: '17 2 * * *'",
            "workflow_dispatch:",
            "inputs:",
            "candidate:",
            "description: Expected full commit SHA; select the workflow branch or tag separately.",
            "type: string",
            "required: false",
            "default: ''",
        ],
    );
    for binding in [
        "  NORN_REGRESSION_SHA: ${{ inputs.candidate || github.sha }}",
        "  NORN_REGRESSION_EVENT_SHA: ${{ github.sha }}",
        "  NORN_REGRESSION_WORKFLOW_SHA: ${{ github.workflow_sha }}",
    ] {
        assert!(workflow.lines().any(|line| line == binding));
        let key = binding.trim().split_once(':').expect("an env key").0;
        assert_eq!(
            workflow
                .lines()
                .filter(|line| line.trim_start().starts_with(&format!("{key}:")))
                .count(),
            1
        );
    }
    assert!(
        workflow
            .lines()
            .any(|line| line == "  group: ${{ github.workflow }}")
    );
    assert!(
        workflow
            .lines()
            .any(|line| line == "  cancel-in-progress: false")
    );
    assert_complete_active_matrix(workflow, true);
}

#[test]
fn full_regression_keeps_every_active_obligation_and_collects_later_failures() {
    let workflow =
        std::fs::read_to_string(workspace_root().join(".github/workflows/regression.yml"))
            .expect("reading full regression");
    assert_regression_workflow(&workflow);
    for job in ["build", "induced-failure", "platform"] {
        let targets: &[&str] = if job == "build" {
            &["norn regression", "norn certification"]
        } else {
            &["norn-host lockdown", "norn-host kill_recovery"]
        };
        for target in targets {
            let (key, body) = jobs(&workflow)
                .into_iter()
                .find(|(key, _)| key == job)
                .unwrap();
            let mutant_body = body
                .lines()
                .filter(|line| !line.contains(&format!("lane-suite.sh {target}")))
                .collect::<Vec<_>>()
                .join("\n");
            let mutant = workflow.replace(&body, &mutant_body);
            assert_ne!(mutant, workflow, "{key} {target} removal changed nothing");
            assert!(
                std::panic::catch_unwind(|| assert_regression_workflow(&mutant)).is_err(),
                "{key} lost {target}"
            );
        }
    }
    for mutant in [
        workflow.replace(" && steps.identity.outcome == 'success'", ""),
        workflow.replace("        id: identity\n", ""),
        workflow.replace(
            "      - name: Upload candidate identity\n        if: ${{ !cancelled() }}",
            "      - name: Upload candidate identity\n        if: ${{ !cancelled() && steps.identity.outcome == 'success' }}",
        ),
        workflow.replace(
            "run: .github/scripts/flake-tripwire.sh cargo test --locked --no-fail-fast --workspace",
            "run: true",
        ),
        workflow.replace(
            "--no-fail-fast --workspace",
            "--no-fail-fast --workspace --lib",
        ),
        workflow.replacen("        if: ${{ !cancelled() && steps.identity.outcome == 'success' }}\n", "", 1),
        workflow.replace("        if: ${{ !cancelled() && steps.identity.outcome == 'success' }}", "        if: success()"),
        workflow.replacen(
            "    name: full build and test",
            "    if: always()\n    name: full build and test",
            1,
        ),
        workflow.replace("LANE_FEATURES: induced-failure", "LANE_FEATURES: missing"),
        workflow.replace(
            "        run: .github/scripts/flake-tripwire.sh cargo test --locked --no-fail-fast --workspace\n",
            "        run: .github/scripts/flake-tripwire.sh cargo test --locked --no-fail-fast --workspace\n          || true\n",
        ),
        workflow.replace(
            "        run: .github/scripts/flake-tripwire.sh cargo test --locked --no-fail-fast --workspace\n",
            "        run: .github/scripts/flake-tripwire.sh cargo test --locked --no-fail-fast --workspace\n          --lib\n",
        ),
        workflow.replace(
            "      - name: Test\n",
            "      - name: Extra compiler setting\n        if: ${{ !cancelled() && steps.identity.outcome == 'success' }}\n        run: echo CARGO_PROFILE_TEST_DEBUG_ASSERTIONS=false >> \"$GITHUB_ENV\"\n      - name: Test\n",
        ),
        workflow.replace("          cargo --version\n", "          cargo --version\n          echo RUSTFLAGS=changed >> \"$GITHUB_ENV\"\n"),
        workflow.replace("args: -color", "args: -version"),
        workflow.replace(
            "        run: python3 .github/scripts/regression-identity.py \"$RUNNER_TEMP/regression-identity.json\"\n",
            "        run: python3 .github/scripts/regression-identity.py \"$RUNNER_TEMP/regression-identity.json\"\n          || true\n",
        ),
        workflow.replace(
            "  NORN_REGRESSION_SHA:",
            "  CARGO_PROFILE_TEST_DEBUG_ASSERTIONS: 'false'\n  NORN_REGRESSION_SHA:",
        ),
        workflow.replace(
            "        run: cargo doc --locked --workspace --no-deps --document-private-items\n        env:\n          RUSTDOCFLAGS: -D warnings\n",
            "        run: cargo doc --locked --workspace --no-deps --document-private-items\n",
        ).replace(
            "      - name: Test\n",
            "      - name: Test\n        env:\n          RUSTDOCFLAGS: -D warnings\n",
        ),
        workflow.replace(
            "      - name: Test\n",
            "      - continue-on-error: true\n        name: Test\n",
        ),
        workflow.replace(
            "      - name: Workflow lint\n        if: ${{ !cancelled() && steps.identity.outcome == 'success' }}\n",
            "      - if: false\n        name: Workflow lint\n",
        ),
        workflow.replace(
            "        run: .github/scripts/flake-tripwire.sh cargo test --locked --no-fail-fast --workspace",
            "        shell: true {0}\n        run: .github/scripts/flake-tripwire.sh cargo test --locked --no-fail-fast --workspace",
        ),
        workflow.replace("\njobs:\n", "\ndefaults:\n  run:\n    shell: true {0}\njobs:\n"),
        workflow.replace("    name: full build and test", "    defaults:\n      run:\n        shell: true {0}\n    name: full build and test"),
        workflow.replace(
            "      - name: Test\n        if: ${{ !cancelled() && steps.identity.outcome == 'success' }}\n",
            "      - name: Test\n        env:\n          CARGO_PROFILE_TEST_DEBUG_ASSERTIONS: 'false'\n        if: ${{ !cancelled() && steps.identity.outcome == 'success' }}\n",
        ),
        workflow.replace(
            "      - name: Test\n        if: ${{ !cancelled() && steps.identity.outcome == 'success' }}\n",
            "      - name: Test\n        # regression-identity.py\n",
        ),
    ] {
        assert!(
            std::panic::catch_unwind(|| assert_regression_workflow(&mutant)).is_err(),
            "a coverage/failure-routing mutant survived"
        );
    }
}

fn assert_ci_lines(lines: &[String], required: &[&str]) {
    for line in required {
        assert!(
            lines.iter().any(|actual| actual == line),
            "missing `{line}`: {lines:?}"
        );
    }
}

fn assert_job_cannot_skip_or_excuse(lines: &[String]) {
    for line in lines {
        let key = line.trim_start_matches("- ");
        assert!(
            !["needs:", "if:", "continue-on-error:"]
                .iter()
                .any(|forbidden| key.starts_with(forbidden)),
            "a gate orders, skips or excuses its evidence: {line}"
        );
    }
}

/// Pin the entire trigger block: path filters or extra events change coverage.
fn assert_triggers(workflow: &str, expected: &[&str]) {
    let trigger = workflow
        .split_once("\non:\n")
        .expect("an on block")
        .1
        .split_once("\npermissions:")
        .expect("permissions after triggers")
        .0;
    let lines: Vec<_> = trigger
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    assert_eq!(lines, expected, "workflow trigger scope changed");
}

/// The wrapper every correctness-suite step is run through.
const TRIPWIRE: &str = ".github/scripts/flake-tripwire.sh";

/// Exact filters and ignored lanes must execute cases, and keep failures red.
#[test]
#[cfg(unix)]
fn a_suite_runner_rejects_empty_runs_and_preserves_failures() {
    use norn_testkit::process::{Run, RunStatus};

    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), "suite-runner")
        .expect("a sandbox for the suite runner");
    let (_, search) = fixture_cargo(&sandbox);
    let args = sandbox.work_dir().join("cargo-args");
    for (output, exit, expected) in [
        ("test result: ok. 1 passed; 0 failed; 0 ignored", 0, 0),
        ("test result: ok. 12 passed; 0 failed; 0 ignored", 0, 0),
        (
            "     Summary [ 1.200s] 12 tests run: 12 passed, 3 skipped",
            0,
            0,
        ),
        (
            "     Summary [ 0.001s] 0 tests run: 0 passed, 12 skipped",
            0,
            1,
        ),
        ("     Summary [ 0.001s] 1 test run: 1 passed", 0, 0),
        (
            "     Summary [ 0.001s] 1 test run: 0 passed, 1 failed",
            100,
            100,
        ),
        ("test result: ok. 0 passed; 0 failed; 1 ignored", 0, 1),
        ("", 0, 1),
        (
            "test result: ok. 1 passed; 0 failed\ntest result: ok. 0 passed; 0 failed",
            0,
            1,
        ),
        ("test result: FAILED. 0 passed; 1 failed", 101, 101),
        ("test result: ok. 1 passed; 0 failed", 17, 17),
    ] {
        let outcome = Run::new(&sandbox, "/bin/bash")
            .arg(workspace_root().join(".github/scripts/test-suite.sh"))
            .arg("/bin/sh")
            .arg("-c")
            .arg("printf '%s\\n' \"$1\"; exit \"$2\"")
            .arg("fixture")
            .arg(output)
            .arg(exit.to_string())
            .wait()
            .expect("running the suite runner over fixture output");
        assert_eq!(
            outcome.status,
            RunStatus::Exited(expected),
            "output {output:?}, exit {exit}: {}{}",
            outcome.stdout_text(),
            outcome.stderr_text()
        );
        assert!(outcome.stdout_text().contains(output));

        for features in ["", "induced-failure"] {
            let outcome = Run::new(
                &sandbox,
                workspace_root().join(".github/scripts/lane-suite.sh"),
            )
            .arg("fixture-package")
            .arg("fixture-target")
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env("PATH", &search)
            .env("LANE_FEATURES", features)
            .env("NORN_FIXTURE_ARGS", &args)
            .env("NORN_FIXTURE_OUTPUT", output)
            .env("NORN_FIXTURE_STATUS", exit.to_string())
            .wait()
            .expect("running the ignored-lane entrypoint with fixture cargo");
            assert_eq!(
                outcome.status,
                RunStatus::Exited(expected),
                "lane output {output:?}: {}{}",
                outcome.stdout_text(),
                outcome.stderr_text()
            );
            let expected_args = if features.is_empty() {
                "test\n--locked\n-p\nfixture-package\n--test\nfixture-target\n--\n--ignored\n--nocapture\n--test-threads=1\n"
            } else {
                "test\n--locked\n-p\nfixture-package\n--test\nfixture-target\n--features\ninduced-failure\n--\n--ignored\n--nocapture\n--test-threads=1\n"
            };
            assert_eq!(
                std::fs::read_to_string(&args).expect("cargo's received arguments"),
                expected_args
            );
        }
    }
}

/// Certification still executes fault carriers after they move out of ordinary CI.
#[test]
#[cfg(unix)]
fn certification_executes_ignored_fault_carriers() {
    use norn_testkit::process::{Run, RunStatus};
    let sandbox = Sandbox::new(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        "certification-faults",
    )
    .unwrap();
    let (_, search) = fixture_cargo(&sandbox);
    let args = sandbox.work_dir().join("cargo-args");
    for (target, feature) in [
        ("lockdown", Some("induced-failure")),
        ("kill_recovery", Some("induced-failure")),
        ("lib", None),
        ("churn", Some("induced-failure")),
        ("lockdown", None),
    ] {
        let mut run = Run::new(
            &sandbox,
            workspace_root().join(".github/scripts/certification-suite.sh"),
        )
        .args(["norn-host", target]);
        if let Some(feature) = feature {
            run = run.arg(feature);
        }
        let outcome = run
            .env("PATH", &search)
            .env("NORN_CERTIFICATION_LOGS", sandbox.work_dir().join("logs"))
            .env("NORN_FIXTURE_ARGS", &args)
            .env("NORN_FIXTURE_OUTPUT", "test result: ok. 1 passed; 0 failed")
            .env("NORN_FIXTURE_STATUS", "0")
            .wait()
            .unwrap();
        assert_eq!(outcome.status, RunStatus::Exited(0));
        let mut expected = vec!["test", "--locked", "-p", "norn-host"];
        if target == "lib" {
            expected.push("--lib");
        } else {
            expected.extend(["--test", target]);
        }
        if let Some(feature) = feature {
            expected.extend(["--features", feature]);
        }
        if matches!(target, "lockdown" | "kill_recovery") && feature == Some("induced-failure") {
            expected.extend(["--", "--include-ignored"]);
        }
        assert_eq!(
            std::fs::read_to_string(&args).unwrap(),
            format!("{}\n", expected.join("\n"))
        );
    }
}

/// A complete passing log remains evidence when its downstream writer fails.
#[test]
#[cfg(unix)]
fn a_suite_reports_its_own_outcome_after_a_downstream_failure() {
    use norn_testkit::process::{Run, RunStatus};
    use std::os::unix::fs::PermissionsExt;

    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), "suite-pipeline")
        .expect("a sandbox for the pipeline");
    let (bin, search) = fixture_cargo(&sandbox);
    let tee = bin.join("tee");
    std::fs::write(&tee, "#!/bin/sh\ncat > \"$1\"\ncat \"$1\"\nexit 1\n")
        .expect("a writer that records everything before failing");
    std::fs::set_permissions(&tee, std::fs::Permissions::from_mode(0o755))
        .expect("an executable writer");
    for script in ["test-suite.sh", "lane-suite.sh"] {
        let command = Run::new(
            &sandbox,
            workspace_root().join(".github/scripts").join(script),
        );
        let command = if script == "lane-suite.sh" {
            command.arg("fixture-package").arg("fixture-target")
        } else {
            command
                .arg("/bin/sh")
                .arg("-c")
                .arg("printf 'test result: ok. 1 passed; 0 failed\\n'")
        };
        let outcome = command
            .env("PATH", &search)
            .env("NORN_FIXTURE_ARGS", sandbox.work_dir().join("cargo-args"))
            .env("NORN_FIXTURE_OUTPUT", "test result: ok. 1 passed; 0 failed")
            .env("NORN_FIXTURE_STATUS", "0")
            .wait()
            .expect("running a passing suite beside a failed writer");
        assert_eq!(
            outcome.status,
            RunStatus::Exited(0),
            "{script}: {}{}",
            outcome.stdout_text(),
            outcome.stderr_text()
        );
    }
}

/// The scripts reach a fixture Cargo that records arguments and emits a chosen verdict.
#[cfg(unix)]
fn fixture_cargo(sandbox: &Sandbox) -> (PathBuf, std::ffi::OsString) {
    use std::os::unix::fs::PermissionsExt;

    let bin = sandbox.work_dir().join("bin");
    std::fs::create_dir(&bin).expect("a command directory");
    let cargo = bin.join("cargo");
    std::fs::write(&cargo, "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$NORN_FIXTURE_ARGS\"\nprintf '%s\\n' \"$NORN_FIXTURE_OUTPUT\"\nexit \"$NORN_FIXTURE_STATUS\"\n")
        .expect("a Cargo command fixture");
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755))
        .expect("an executable Cargo fixture");
    let inherited = std::env::var_os("PATH").expect("a command search path");
    let search =
        std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&inherited)))
            .expect("a search path with fixture commands first");
    (bin, search)
}

/// Whether a workflow line runs a suite — directly, or through one of the
/// scripts that runs one.
///
/// **Reaching cargo is what makes a step a suite step, not the spelling it
/// reaches it with.** A step that runs `lane-suite.sh` runs
/// `cargo test -- --ignored`, and a rule written over the literal `cargo test`
/// passes over it while the claim it enforces says otherwise. The scripts are
/// named here so a step wrapped in one is still seen.
fn reaches_a_suite(line: &str) -> bool {
    [
        "cargo test",
        "cargo nextest run",
        "lane-suite.sh",
        "test-suite.sh",
        "certification-suite.sh",
    ]
    .iter()
    .any(|runner| line.contains(runner))
}

/// Each entry of the flake ledger, as the `key`/`value` pairs its block holds.
fn ledger_entries(ledger: &str) -> Vec<Vec<(&str, &str)>> {
    ledger
        .split("\n\n")
        .map(|block| {
            block
                .lines()
                .filter(|line| !line.starts_with('#'))
                .filter_map(|line| line.split_once(": "))
                .collect::<Vec<(&str, &str)>>()
        })
        .filter(|fields| !fields.is_empty())
        .collect()
}

/// One ledger field, by key.
#[cfg(unix)]
fn ledger_field<'a>(fields: &[(&'a str, &'a str)], key: &str) -> &'a str {
    fields
        .iter()
        .find(|(named, _)| *named == key)
        .map(|(_, value)| value.trim())
        .unwrap_or_else(|| panic!("a flake-ledger entry names no `{key}`: {fields:?}"))
}

/// **A ledgered signature is matched against a failure and never against a run
/// where the test it names passed.**
///
/// The discrimination is the whole of the mechanism. libtest prints
/// `test <name> ... ok` for every test that passes, so a signature naming a
/// test is in the output of every green run of the suite that holds it — and a
/// matcher that scanned the whole log would report the class as having recurred
/// the first time anything else in that suite went red. The record would then
/// be identical whether the ledgered failure happened or not, which is the bar
/// this exists for inverted: the correct reflex would become ignoring it.
///
/// Both directions are read off the real script and the real ledger, over
/// synthesized suite output, so an entry added to the file is covered by the
/// same two claims without being named here.
#[test]
#[cfg(unix)]
fn a_ledgered_signature_matches_a_failure_and_never_a_run_that_passed_it() {
    let root = workspace_root();
    let ledger =
        std::fs::read_to_string(root.join(".github/flake-ledger")).expect("reading the ledger");
    let entries = ledger_entries(&ledger);
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), "flake-tripwire")
        .expect("a sandbox for the tripwire");

    // Every ledgered signature on a status line reporting a test that passed,
    // beside one unrelated failure. This is the shape of an ordinary red run in
    // a suite that holds the ledgered cases, and nothing in the ledger is what
    // failed.
    for nextest in [false, true] {
        let mut passed = String::from("running tests\n");
        for fields in &entries {
            let signature = ledger_field(fields, "signature");
            if nextest {
                passed.push_str(&format!("        PASS [ 0.001s] fixture suite::{signature}\n        SKIP [ 0.000s] fixture suite::{signature}\n"));
            } else {
                passed.push_str(&format!("test suite::{signature} ... ok\n"));
            }
        }
        passed.push_str(
        "test suite::something_else ... FAILED\n\nfailures:\n\n---- suite::something_else stdout \
         ----\nthread 'suite::something_else' panicked at src/lib.rs:1:1:\nassertion \
         failed\n\nfailures:\n    suite::something_else\n\ntest result: FAILED. 1 failed\n",
    );
        let (status, summary, annotations) = tripwire_over(&sandbox, &root, "passed", &passed);
        assert_eq!(
            status,
            norn_testkit::process::RunStatus::Exited(101),
            "the tripwire changed the suite's own verdict"
        );
        assert!(
            summary.contains("no ledgered signature matched"),
            "a failure of no ledgered class was recorded as a recurrence: {}",
            unread(&summary)
        );
        assert!(
            !summary.contains("bound was breached"),
            "passing/skipped status lines cannot classify breached bounds: {}",
            unread(&summary)
        );
        for fields in &entries {
            let id = ledger_field(fields, "id");
            assert!(
                !annotations.contains(id),
                "`{id}` was reported as recurring by a run in which its own test passed, so the \
             entry cannot tell a recurrence from any other red run: {}",
                unread(&annotations)
            );
        }

        // The same entries, each in a run that really did fail on it.
        for fields in &entries {
            let (id, signature) = (
                ledger_field(fields, "id"),
                ledger_field(fields, "signature"),
            );
            let failed = format!(
                "running 1 test\ntest suite::a_case ... FAILED\n\nfailures:\n\n---- suite::a_case \
             stdout ----\nthread 'suite::a_case' panicked at src/lib.rs:1:1:\n{signature}\n\ntest \
             result: FAILED. 1 failed\n"
            );
            let failed = if nextest {
                format!(
                    "        FAIL [ 0.001s] fixture suite::a_case\n--- STDERR: fixture suite::a_case ---\n{signature}\n     Summary [ 0.001s] 1 test run: 0 passed, 1 failed\n"
                )
            } else {
                failed
            };
            let (status, summary, annotations) = tripwire_over(&sandbox, &root, id, &failed);
            assert_eq!(
                status,
                norn_testkit::process::RunStatus::Exited(101),
                "the tripwire changed the suite's own verdict"
            );
            assert!(
                annotations.contains(&format!("Ledgered flake recurred: {id}")),
                "`{id}` recurred and the run carries no annotation naming it: {}",
                unread(&annotations)
            );
            assert!(
                summary.contains(ledger_field(fields, "disposition")),
                "`{id}` recurred and the job summary carries no disposition to read: {}",
                unread(&summary)
            );
        }
    }
}

/// **A starved probe is named as a probe-bound breach and meets its class-A
/// entry.**
///
/// A probe-bound breach is a reading of the runner rather than of the product:
/// one evaluation that took longer than its bound says the process was not
/// scheduled for that long. The ruling makes such a run a non-qualifying
/// evidence source on its own timing, and the record of that is the tripwire's
/// to write, so a rerun of it is deliberate rather than quiet.
///
/// The failure is the one CI produced on 2026-09-28, rendered by the wait
/// module itself, so the ledger's signature and the tripwire's reading are held
/// to the text a starved wait really prints rather than to a copy of it.
#[test]
#[cfg(unix)]
fn a_starved_probe_is_named_as_a_probe_bound_breach_and_meets_its_class_a_entry() {
    let root = workspace_root();
    let sandbox = Sandbox::new(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        "flake-tripwire-probe",
    )
    .expect("a sandbox for the tripwire");
    let starved = wait_failure(
        norn_testkit::wait::FailureKind::ProbeOverran {
            took: std::time::Duration::from_micros(255_400),
        },
        std::time::Duration::from_millis(3_880),
        78,
    );
    assert!(
        starved.contains(
            "stopped at probe 78, which took 255.4ms and passed its 250ms probe bound 3.88s into \
             a 900s work bound"
        ),
        "the wait module no longer renders a probe-bound breach the way CI recorded it: {}",
        unread(&starved)
    );

    let (status, summary, annotations) = tripwire_over(
        &sandbox,
        &root,
        "probe-bound",
        &failed_run(
            "a_walk_that_refuses_leaves_the_findings_in_its_scope_standing",
            &starved,
        ),
    );
    assert_eq!(
        status,
        norn_testkit::process::RunStatus::Exited(101),
        "the tripwire changed the suite's own verdict"
    );
    assert!(
        annotations.contains(&format!("Ledgered flake recurred: {PROBE_BOUND_ENTRY}")),
        "a starved probe recurred and the run carries no annotation naming \
         `{PROBE_BOUND_ENTRY}`: {}",
        unread(&annotations)
    );
    assert!(
        summary.contains("a probe bound was breached"),
        "a starved probe recurred and the job summary does not name the bound it breached: \
         {}",
        unread(&summary)
    );
    assert!(
        !summary.contains("a work bound was breached"),
        "a probe-bound breach was named as a work-bound one: {}",
        unread(&summary)
    );
    assert!(
        summary.contains("This run is a non-qualifying evidence source on its own timing"),
        "a run that starved a probe and breached nothing else is not named non-qualifying: {}",
        unread(&summary)
    );
    assert!(
        !summary.contains(NOT_CLEARED) && !annotations.contains(NOT_CLEARED),
        "a run that breached no work bound is said to be left uncleared by one: {}",
        unread(&summary)
    );
}

/// **A work-bound breach is named as one, and is never read as a starved
/// probe.**
///
/// A wait that passed its work bound rendered no probe-bound breach, so the run
/// carries no evidence that the runner starved a probe. Under a class-A entry
/// such a recurrence reopens the ruling, so a tripwire that filed it with the
/// starved probes would excuse exactly the failure the ruling keeps open.
///
/// The rendering does not show every probe answered inside its bound: a
/// condition that ends a wait on its own patience returns from inside the probe
/// before the probe bound is read, and a starved writer thread can be what ran
/// the patience out, as class-a-lease-changing-hands records. The summary
/// states the ruled consequence and claims nothing about the runner.
#[test]
#[cfg(unix)]
fn a_work_bound_breach_is_named_as_one_and_never_read_as_a_starved_probe() {
    let root = workspace_root();
    let sandbox = Sandbox::new(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        "flake-tripwire-work",
    )
    .expect("a sandbox for the tripwire");
    let elapsed = wait_failure(
        norn_testkit::wait::FailureKind::Elapsed,
        std::time::Duration::from_secs(900),
        3_600,
    );

    let (status, summary, annotations) = tripwire_over(
        &sandbox,
        &root,
        "work-bound",
        &failed_run("a_case_whose_condition_never_held", &elapsed),
    );
    assert_eq!(
        status,
        norn_testkit::process::RunStatus::Exited(101),
        "the tripwire changed the suite's own verdict"
    );
    assert!(
        !annotations.contains(PROBE_BOUND_ENTRY),
        "a work-bound breach was reported as a starved probe: {}",
        unread(&annotations)
    );
    assert!(
        summary.contains("a work bound was breached"),
        "a work-bound breach recurred and the job summary does not name the bound it breached: \
         {}",
        unread(&summary)
    );
    assert!(
        !summary.contains("a probe bound was breached"),
        "a work-bound breach was named as a probe-bound one: {}",
        unread(&summary)
    );
    assert!(
        summary.contains("reopens that ruling"),
        "a work-bound breach recurred and the job summary does not state that under a class-A \
         entry it reopens the ruling: {}",
        unread(&summary)
    );
    for unrendered in ["inside its probe bound", "starved runner"] {
        assert!(
            !summary.contains(unrendered),
            "the job summary claims `{unrendered}` of a work-bound breach, which its rendering \
             does not show: {}",
            unread(&summary)
        );
    }
}

/// **A run that also breached a work bound is not cleared by a starved probe,
/// on any surface the tripwire writes.**
///
/// The ruling reads a run that starved a probe as a non-qualifying evidence
/// source, and a work-bound breach as its own reading, which under a class-A
/// entry reopens that ruling. A run that holds both is therefore not
/// non-qualifying: the starved probe is recorded, and it does not clear the
/// work bound passed beside it. A run whose annotation called it
/// non-qualifying while its summary said otherwise would be read by whichever
/// surface a person opened first, so each one is read here: every ledgered
/// recurrence's annotation, and every block of the summary.
///
/// The starved probe is in a test a class-A entry names, so the entry matched
/// by test name is held to the same reading as the one matched by the probe
/// bound's phrase.
#[test]
#[cfg(unix)]
fn a_run_that_also_breached_a_work_bound_is_not_cleared_by_its_starved_probe() {
    let root = workspace_root();
    let ledger =
        std::fs::read_to_string(root.join(".github/flake-ledger")).expect("reading the ledger");
    let named_case = ledger_entries(&ledger)
        .into_iter()
        .find(|fields| ledger_field(fields, "id") == NAMED_CLASS_A_ENTRY)
        .map(|fields| ledger_field(&fields, "signature").to_owned())
        .unwrap_or_else(|| panic!("the flake ledger holds no `{NAMED_CLASS_A_ENTRY}`"));
    let sandbox = Sandbox::new(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        "flake-tripwire-both",
    )
    .expect("a sandbox for the tripwire");
    let starved = wait_failure(
        norn_testkit::wait::FailureKind::ProbeOverran {
            took: std::time::Duration::from_micros(255_400),
        },
        std::time::Duration::from_millis(3_880),
        78,
    );
    let elapsed = wait_failure(
        norn_testkit::wait::FailureKind::Elapsed,
        std::time::Duration::from_secs(900),
        3_600,
    );
    let output = format!(
        "{}{}",
        failed_run(&named_case, &starved),
        failed_run("a_case_whose_condition_never_held", &elapsed)
    );

    let (status, summary, annotations) = tripwire_over(&sandbox, &root, "both", &output);
    assert_eq!(
        status,
        norn_testkit::process::RunStatus::Exited(101),
        "the tripwire changed the suite's own verdict"
    );

    let recurrences: Vec<&str> = annotations
        .lines()
        .filter(|line| line.contains("Ledgered flake recurred"))
        .collect();
    for entry in [PROBE_BOUND_ENTRY, NAMED_CLASS_A_ENTRY] {
        assert!(
            recurrences
                .iter()
                .any(|line| line.contains(&format!("Ledgered flake recurred: {entry}"))),
            "a run that starved a probe carries no annotation naming `{entry}`: {}",
            unread(&annotations)
        );
    }
    for line in &recurrences {
        assert!(
            line.contains(NOT_CLEARED) && !line.contains("non-qualifying on its own timing"),
            "a run that also breached a work bound is annotated as cleared by its starved probe: \
             {}",
            unread(line)
        );
    }

    let blocks: Vec<&str> = summary.split("\n### ").skip(1).collect();
    for heading in [
        format!("Flake tripwire: `{PROBE_BOUND_ENTRY}` recurred"),
        format!("Flake tripwire: `{NAMED_CLASS_A_ENTRY}` recurred"),
        "Flake tripwire: a probe bound was breached".to_owned(),
        "Flake tripwire: a work bound was breached".to_owned(),
    ] {
        assert!(
            blocks.iter().any(|block| block.starts_with(&heading)),
            "a run that breached both bounds has no summary block `{heading}`: {}",
            unread(&summary)
        );
    }
    for block in &blocks {
        assert!(
            !block.contains("non-qualifying") || block.contains(NOT_CLEARED),
            "a summary block reads a run that also breached a work bound as non-qualifying: {}",
            unread(block)
        );
        assert!(
            !block.contains("This run is a non-qualifying") && !block.contains("takes neither"),
            "a summary block leaves open or clears a run that also breached a work bound: {}",
            unread(block)
        );
        let heading = block.lines().next().unwrap_or_default();
        let names_the_run = heading == "Flake tripwire: a probe bound was breached"
            || heading.ends_with("` recurred");
        assert!(
            !names_the_run || block.contains(NOT_CLEARED),
            "a summary block about the starved probe does not say it leaves the run uncleared: \
             {}",
            unread(block)
        );
    }
    assert!(
        blocks
            .iter()
            .any(|block| block.starts_with("Flake tripwire: a work bound")
                && block.contains("reopens that ruling")),
        "the work-bound block of a run that breached both does not say it reopens the ruling: {}",
        unread(&summary)
    );
}

/// **A tripwire case that fails is read by the tripwire as a failure of no
/// ledgered class and no breached bound.**
///
/// CI runs this file through the tripwire it tests. A case here that echoed a
/// signature or a bound's phrase into its own failure message would have a
/// regression in the tripwire filed as the very recurrence the tripwire failed
/// to read — a probe-bound breach, whose disposition is a deliberate rerun, for
/// a defect no rerun fixes. So every summary, annotation and rendering these
/// cases echo passes through [`unread`] first.
#[test]
#[cfg(unix)]
fn a_failing_tripwire_case_is_read_as_no_ledgered_class_and_no_breached_bound() {
    let root = workspace_root();
    let ledger =
        std::fs::read_to_string(root.join(".github/flake-ledger")).expect("reading the ledger");
    let sandbox = Sandbox::new(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        "flake-tripwire-unread",
    )
    .expect("a sandbox for the tripwire");
    let starved = wait_failure(
        norn_testkit::wait::FailureKind::ProbeOverran {
            took: std::time::Duration::from_micros(255_400),
        },
        std::time::Duration::from_millis(3_880),
        78,
    );
    let elapsed = wait_failure(
        norn_testkit::wait::FailureKind::Elapsed,
        std::time::Duration::from_secs(900),
        3_600,
    );
    let echoed = format!("the tripwire read this wrong: {ledger}\n{starved}\n{elapsed}");

    let (status, summary, annotations) = tripwire_over(
        &sandbox,
        &root,
        "unread",
        &failed_run("a_tripwire_case", &unread(&echoed)),
    );
    assert_eq!(
        status,
        norn_testkit::process::RunStatus::Exited(101),
        "the tripwire changed the suite's own verdict"
    );
    assert!(
        summary.contains("no ledgered signature matched")
            && !annotations.contains("Ledgered flake recurred"),
        "a tripwire case's own failure message was read as a ledgered recurrence: {}",
        unread(&annotations)
    );
    assert!(
        !summary.contains("bound was breached"),
        "a tripwire case's own failure message was read as a breached bound: {}",
        unread(&summary)
    );
}

/// `text` as a tripwire case echoes it into its own failure message: every
/// phrase the tripwire reads — each ledger signature and each bound's phrase —
/// is spelled with a `·` after its first character, so the tripwire running
/// over this file in CI reads none of them.
#[cfg(unix)]
fn unread(text: &str) -> String {
    let ledger = std::fs::read_to_string(workspace_root().join(".github/flake-ledger"))
        .expect("reading the ledger");
    let signatures = ledger_entries(&ledger)
        .into_iter()
        .map(|fields| ledger_field(&fields, "signature").to_owned());
    let phrases = BOUND_PHRASES.iter().map(|phrase| (*phrase).to_owned());
    signatures
        .chain(phrases)
        .fold(text.to_owned(), |text, phrase| {
            let mut chars = phrase.chars();
            let spelled = match chars.next() {
                Some(first) => format!("{first}·{}", chars.as_str()),
                None => return text,
            };
            text.replace(&phrase, &spelled)
        })
}

/// The phrases `.github/scripts/flake-tripwire.sh` reads a breached bound by:
/// the probe bound's, then the work bound's.
#[cfg(unix)]
const BOUND_PHRASES: [&str; 2] = ["stopped at probe", "work bound after"];

/// The ledger entry a probe-bound breach is matched to.
#[cfg(unix)]
const PROBE_BOUND_ENTRY: &str = "class-a-probe-bound";

/// A class-A entry whose signature is a test name rather than a bound's phrase.
#[cfg(unix)]
const NAMED_CLASS_A_ENTRY: &str = "class-a-recovery-handoff";

/// What every surface of a run that also breached a work bound says of it.
#[cfg(unix)]
const NOT_CLEARED: &str = "this run is not a non-qualifying evidence source";

/// A failed wait on the real-watcher lease, as the wait module renders it, at
/// the bounds the lease acquisition carries on a hosted runner.
#[cfg(unix)]
fn wait_failure(
    kind: norn_testkit::wait::FailureKind,
    elapsed: std::time::Duration,
    probes: usize,
) -> String {
    norn_testkit::wait::WaitFailure {
        what: "the real-watcher lease".to_owned(),
        kind,
        budget: norn_testkit::wait::Budget::new(
            std::time::Duration::from_secs(900),
            std::time::Duration::from_millis(250),
        ),
        elapsed,
        probes,
        last_state: "held by pid 4242 holding a lease since unix 0 for 3.8s".to_owned(),
    }
    .to_string()
}

/// A suite run in which `case` failed by panicking with `message`, in the
/// shape libtest prints it.
#[cfg(unix)]
fn failed_run(case: &str, message: &str) -> String {
    format!(
        "running 1 test\ntest tests::{case} ... FAILED\n\nfailures:\n\n---- tests::{case} stdout \
         ----\nthread 'tests::{case}' panicked at crates/norn-testkit/src/isolation.rs:298:59:\n\
         {message}\n\nfailures:\n    tests::{case}\n\ntest result: FAILED. 0 passed; 1 failed\n"
    )
}

/// Run the tripwire over `output` as a failing suite's own output, and hand
/// back what the run left: its exit status, the job summary and the
/// annotations.
#[cfg(unix)]
fn tripwire_over(
    sandbox: &Sandbox,
    root: &Path,
    label: &str,
    output: &str,
) -> (norn_testkit::process::RunStatus, String, String) {
    let fixture = sandbox.work_dir().join(format!("{label}.log"));
    std::fs::write(&fixture, output).expect("writing the synthesized suite output");
    let summary = sandbox.work_dir().join(format!("{label}.summary"));
    std::fs::write(&summary, "").expect("an empty job summary");

    // 101 is what libtest exits with, and it is what the run's own status has
    // to still be on the far side of the wrapper.
    let outcome = norn_testkit::process::Run::new(sandbox, root.join(TRIPWIRE))
        .arg("/bin/sh")
        .arg("-c")
        .arg(format!("cat {}; exit 101", fixture.display()))
        .env("GITHUB_STEP_SUMMARY", &summary)
        .wait()
        .expect("running the flake tripwire");
    let written = std::fs::read_to_string(&summary).expect("reading the job summary");
    (
        outcome.status,
        written,
        format!("{}{}", outcome.stdout_text(), outcome.stderr_text()),
    )
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
        Lane::FoldingVolume,
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
