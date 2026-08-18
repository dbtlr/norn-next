//! What a scheduled lane ran, read off the suites' own output.
//!
//! # The gap this closes
//!
//! A record's case lines are the difference between a run that certified the
//! layer and a run that certified nothing: [`ledger::Record`] reads an outcome
//! per required case, and a case with no line is [`Outcome::NotRun`], which
//! classifies the whole record as a suite change. A lane that runs the
//! certification suites and says nothing about them therefore produces a record
//! indistinguishable from one that never ran them.
//!
//! So a lane runs each suite through `.github/scripts/certification-suite.sh`,
//! which keeps the test harness's own output as a log, and this module turns
//! those logs into the `<case id> <outcome>` lines [`ledger::OUTCOMES`] names.
//!
//! # Why the harness's output rather than a wrapper's verdict
//!
//! The alternative is a script that decides per case whether it passed, which
//! is a second judge of the same run: a wrapper that mapped a suite's exit
//! status onto its cases would record every case of a failing suite as failed,
//! including the ones that passed before it. The harness already states the
//! outcome of each test it ran, one line each, and reading that is a
//! transcription rather than a judgment.
//!
//! # The naming contract between the script and this module
//!
//! One log per cargo target, named `<package>__<target>.log` in the directory
//! [`LOGS`] names, where the target is an integration target's file stem or
//! `lib` for the package's library. The pair is what makes a bare test-function name
//! resolvable: two targets may each hold a test of the same name, and the
//! inventory's carrier is a file and a function rather than a function alone.
//! A log whose name does not carry the pair is refused rather than guessed at.
//!
//! # What a missing log means
//!
//! Nothing here invents an outcome. A required case whose target left no log
//! gets no line, which the ledger reads as `not-run` — the honest reading of a
//! lane that did not reach it. A log holding a test the inventory does not
//! require yields a line under a synthetic id, so a suite that grew a case the
//! inventory never adopted reaches the record as the suite change it is rather
//! than being dropped on the way.
//!
//! [`Outcome::NotRun`]: super::ledger::Outcome::NotRun
//! [`ledger::Record`]: super::ledger::Record
//! [`ledger::OUTCOMES`]: super::ledger::OUTCOMES

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::regression::{self, Target, TestRef};

use super::inventory::{self, REQUIRED_CASES};
use super::ledger::{self, Outcome};

/// The directory a lane's suite logs are written to and read from.
pub const LOGS: &str = "NORN_CERTIFICATION_LOGS";

/// What the collected outcomes are written as, inside the log directory.
///
/// Beside the logs rather than wherever a second variable points, so the lane
/// names one directory and the record step points [`ledger::OUTCOMES`] at this
/// file in it. Its extension is not a log's, so a second collection over the
/// same directory reads the logs and not its own previous answer.
pub const OUTCOMES_FILE: &str = "outcomes.txt";

/// The extension the script writes and this module reads.
const LOG_EXTENSION: &str = "log";

/// What separates the package from the target's name in a log's name.
///
/// Two underscores, because a target stem holds single ones: `kill_recovery`
/// is a stem and `norn-host__kill_recovery` is a name that splits back into
/// the pair it was built from.
const NAME_SEPARATOR: &str = "__";

/// What a package's library target is called in a log's name and on the lane
/// script's command line.
///
/// An integration target of the same name would be `crates/<package>/tests/lib.rs`,
/// which no package has and which [`Invocation::names_are_unambiguous`] holds
/// the inventory to.
const LIBRARY: &str = "lib";

/// One cargo invocation a lane makes: a target, and the feature its cases
/// compile behind.
///
/// **A target is not enough on its own.** A suite behind an off-by-default
/// feature compiles to zero tests without it, so a lane that ran the target
/// without the feature ran nothing and would leave a log saying so.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Invocation {
    pub package: String,
    pub target: Target,
    pub feature: Option<String>,
}

impl Invocation {
    /// What the target is called in a log's name and on the command line.
    pub fn target_name(&self) -> String {
        match &self.target {
            Target::Lib => LIBRARY.to_string(),
            Target::Integration(stem) => stem.clone(),
        }
    }

    /// The arguments `.github/scripts/certification-suite.sh` takes to run this
    /// invocation, which is what a lane's step spells out.
    pub fn script_arguments(&self) -> String {
        let target = self.target_name();
        match &self.feature {
            Some(feature) => format!("{} {target} {feature}", self.package),
            None => format!("{} {target}", self.package),
        }
    }

    /// The log this invocation's output belongs in, under `dir`.
    pub fn log(&self, dir: &Path) -> PathBuf {
        dir.join(format!(
            "{}{NAME_SEPARATOR}{}.{LOG_EXTENSION}",
            self.package,
            self.target_name()
        ))
    }

    /// Whether the invocations named here name distinct logs.
    ///
    /// One name per target is what makes a log readable back into the target it
    /// came from, and the one collision available is an integration target
    /// called `lib`. It is a rule about the inventory rather than about this
    /// module, so it is answered here and asserted by this crate's own suite.
    pub fn names_are_unambiguous(invocations: &BTreeSet<Invocation>) -> bool {
        !invocations
            .iter()
            .any(|invocation| invocation.target == Target::Integration(LIBRARY.to_string()))
    }
}

/// **Every cargo invocation the required cases need**, derived from the
/// inventory rather than transcribed beside it.
///
/// A lane that runs these runs every required case. A lane that runs fewer
/// leaves the rest with no line, and the record says which — which is what
/// makes this list a thing a lane is checked against rather than a list a lane
/// has to be trusted to hold.
pub fn required_invocations() -> BTreeSet<Invocation> {
    let mut wanted = BTreeSet::new();
    for case in REQUIRED_CASES {
        let Ok(target) = TestRef::parse(case.carrier).and_then(|reference| reference.target())
        else {
            continue;
        };
        wanted.insert(Invocation {
            package: target.package,
            target: target.target,
            feature: case.feature.map(str::to_string),
        });
    }
    wanted
}

/// **The outcomes file** for the logs in `dir`, one `<case id> <outcome>` line
/// per test some log reported.
///
/// Sorted by id, so two runs of the same suite produce files that differ only
/// where the run did.
///
/// # Two kinds of target, and only one of them reports strays
///
/// A **claimed** target is one every test of which is a certification case, so
/// a test in its log that the inventory does not name is recorded under a
/// synthetic id: it reaches the record as the suite change it is instead of
/// being dropped on the way. A package's library target is never claimed — it
/// holds every unit test the crate has, and the inventory names the handful of
/// them that carry cases — so its log is read for those and its other tests are
/// not the lane's business.
pub fn outcomes_from_logs(dir: &Path) -> Result<String, String> {
    let mut reported: BTreeMap<String, Outcome> = BTreeMap::new();
    for (package, name, text) in read_logs(dir)? {
        let target = named_target(&name);
        let mut results = parse_harness_output(&text);
        for case in REQUIRED_CASES {
            let Ok(reference) = TestRef::parse(case.carrier) else {
                continue;
            };
            let Ok(carried_by) = reference.target() else {
                continue;
            };
            if carried_by.package != package || carried_by.target != target {
                continue;
            }
            let listed = results
                .keys()
                .find(|listed| {
                    regression::matches_function(
                        listed,
                        &carried_by.module_prefix,
                        &reference.function,
                    )
                })
                .cloned();
            if let Some(outcome) = listed.and_then(|listed| results.remove(&listed)) {
                record(&mut reported, case.id.to_string(), outcome)?;
            }
        }
        if !is_claimed(&package, &target) {
            continue;
        }
        for (function, outcome) in results {
            record(
                &mut reported,
                format!("{package}{NAME_SEPARATOR}{name}::{function}"),
                outcome,
            )?;
        }
    }
    let mut rendered = String::new();
    for (id, outcome) in reported {
        rendered.push_str(&format!("{id} {}\n", outcome_word(outcome)));
    }
    Ok(rendered)
}

/// The target a log's name says it holds.
fn named_target(name: &str) -> Target {
    match name {
        LIBRARY => Target::Lib,
        stem => Target::Integration(stem.to_string()),
    }
}

/// Whether the inventory claims this target whole.
fn is_claimed(package: &str, target: &Target) -> bool {
    inventory::CLAIMED_TARGETS.iter().any(|claimed| {
        claimed.package == package && Target::Integration(claimed.stem.to_string()) == *target
    })
}

/// **The collector.** Where a run named a directory of logs, write the outcomes
/// they hold to [`OUTCOMES_FILE`] in it.
///
/// Returns the path written, or `None` where the run named no logs — which is
/// every run outside a lane, so the suite that calls this is an ordinary test
/// rather than a lane of its own.
///
/// **Collecting and emitting are two steps of a lane, never one.** A run that
/// named the logs and the record sink together would write the file the record
/// is assembled from inside the same binary as the test that assembles it, and
/// nothing orders two tests in one binary. So naming both is refused here
/// rather than left to produce a record that depends on which test ran first.
///
/// **A variable set to nothing names nothing.** A lane sets the log directory
/// for the job and clears it on the step that assembles the record, and an
/// empty value there has to read as "this step names no logs" rather than as a
/// directory whose name is the empty string.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: reads the variables the workflow's lane sets.
pub fn collect() -> Result<Option<PathBuf>, String> {
    let Some(logs) = std::env::var_os(LOGS).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if std::env::var_os(ledger::SINK).is_some() {
        return Err(format!(
            "{LOGS} and {} are both named in one run. The outcomes are read off the logs and the \
             record is assembled from the outcomes, and two tests of one binary are not ordered, \
             so a run that did both would record whichever happened first.",
            ledger::SINK
        ));
    }
    let logs = PathBuf::from(&logs);
    let rendered = outcomes_from_logs(&logs)?;
    let path = logs.join(OUTCOMES_FILE);
    std::fs::write(&path, rendered.as_bytes())
        .map_err(|problem| format!("writing {}: {problem}", path.display()))?;
    Ok(Some(path))
}

/// Each log in `dir`, as the package and target stem its name carries and the
/// text it holds.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: reads the logs the workflow's lane wrote.
fn read_logs(dir: &Path) -> Result<Vec<(String, String, String)>, String> {
    let listing = std::fs::read_dir(dir)
        .map_err(|problem| format!("reading {}: {problem}", dir.display()))?;
    let mut paths = Vec::new();
    for entry in listing {
        let path = entry
            .map_err(|problem| format!("reading an entry of {}: {problem}", dir.display()))?
            .path();
        if path.extension().and_then(|extension| extension.to_str()) == Some(LOG_EXTENSION) {
            paths.push(path);
        }
    }
    paths.sort();

    let mut logs = Vec::new();
    for path in paths {
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string();
        let Some((package, stem)) = name.split_once(NAME_SEPARATOR) else {
            return Err(format!(
                "`{}` is not `<package>{NAME_SEPARATOR}<target stem>.{LOG_EXTENSION}`, so nothing \
                 says which target's tests it holds",
                path.display()
            ));
        };
        let text = std::fs::read_to_string(&path)
            .map_err(|problem| format!("reading {}: {problem}", path.display()))?;
        logs.push((package.to_string(), stem.to_string(), text));
    }
    Ok(logs)
}

/// One outcome per test the harness reported, keyed by the test's name.
///
/// The harness writes one `test <name> ... <outcome>` line per test it ran, and
/// its own summary line — `test result: ok. …` — is not one of them. Anything
/// else in the log is cargo's chatter or a suite's own output, and a line whose
/// outcome word is not one the harness writes is not read as a result.
fn parse_harness_output(text: &str) -> BTreeMap<String, Outcome> {
    let mut results = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("test ") else {
            continue;
        };
        let Some((name, verdict)) = rest.split_once(" ... ") else {
            continue;
        };
        let outcome = match verdict.split(',').next().unwrap_or(verdict).trim() {
            "ok" => Outcome::Passed,
            "FAILED" => Outcome::Failed,
            "ignored" => Outcome::Skipped,
            _ => continue,
        };
        results.insert(name.trim().to_string(), outcome);
    }
    results
}

/// Record one outcome, refusing a second line for the same id.
///
/// Two logs reporting one case is a lane that ran a target twice, and the
/// second reading would silently replace the first — including a pass replacing
/// a failure.
fn record(
    reported: &mut BTreeMap<String, Outcome>,
    id: String,
    outcome: Outcome,
) -> Result<(), String> {
    if let Some(first) = reported.insert(id.clone(), outcome) {
        return Err(format!(
            "the lane's logs report `{id}` twice, as {} and as {}",
            outcome_word(first),
            outcome_word(outcome)
        ));
    }
    Ok(())
}

/// The word the outcomes file spells an outcome with, which is the vocabulary
/// [`ledger`] reads back.
fn outcome_word(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Passed => "passed",
        Outcome::Failed => "failed",
        Outcome::Skipped => "skipped",
        Outcome::NotRun => "not-run",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_required_invocations_are_the_targets_the_carriers_compile_into() {
        let invocations = required_invocations();
        assert!(
            invocations.contains(&Invocation {
                package: "norn-host".to_string(),
                target: Target::Integration("churn".to_string()),
                feature: None,
            }),
            "the churn suite is a required target: {invocations:?}"
        );
        assert!(
            invocations.contains(&Invocation {
                package: "norn-host".to_string(),
                target: Target::Integration("lockdown".to_string()),
                feature: Some("induced-failure".to_string()),
            }),
            "the host lockdown suite is required behind its feature: {invocations:?}"
        );
        assert!(
            invocations.contains(&Invocation {
                package: "norn-host".to_string(),
                target: Target::Lib,
                feature: None,
            }),
            "the cases carried by unit tests need the library target run: {invocations:?}"
        );
        assert!(
            Invocation::names_are_unambiguous(&invocations),
            "an integration target called `lib` and a package library would share a log name: \
             {invocations:?}"
        );
    }

    #[test]
    fn a_logs_name_says_which_target_it_holds() {
        let suite = Invocation {
            package: "norn-host".to_string(),
            target: Target::Integration("kill_recovery".to_string()),
            feature: Some("induced-failure".to_string()),
        };
        assert_eq!(
            suite.log(Path::new("/logs")),
            Path::new("/logs/norn-host__kill_recovery.log")
        );
        assert_eq!(
            suite.script_arguments(),
            "norn-host kill_recovery induced-failure"
        );
        let name = "norn-host__kill_recovery";
        assert_eq!(
            name.split_once(NAME_SEPARATOR),
            Some(("norn-host", "kill_recovery")),
            "the stem's own underscore does not split the name"
        );
        assert_eq!(
            named_target("kill_recovery"),
            Target::Integration("kill_recovery".to_string())
        );

        let library = Invocation {
            package: "norn-host".to_string(),
            target: Target::Lib,
            feature: None,
        };
        assert_eq!(
            library.log(Path::new("/logs")),
            Path::new("/logs/norn-host__lib.log")
        );
        assert_eq!(library.script_arguments(), "norn-host lib");
        assert_eq!(named_target("lib"), Target::Lib);
    }

    #[test]
    fn a_unit_tests_module_path_resolves_to_the_case_it_carries() {
        let carried = REQUIRED_CASES
            .iter()
            .find(|case| case.carrier.contains("/src/"))
            .expect("a case carried by a unit test");
        let reference = TestRef::parse(carried.carrier).expect("the carrier reference");
        let target = reference.target().expect("the carrier's target");
        assert_eq!(target.target, Target::Lib);
        let listed = format!("{}tests::{}", target.module_prefix, reference.function);
        assert!(
            regression::matches_function(&listed, &target.module_prefix, &reference.function),
            "`{listed}` is how the harness names `{}`",
            carried.carrier
        );
    }

    #[test]
    fn the_harnesss_own_lines_are_read_and_its_summary_is_not() {
        let results = parse_harness_output(
            "
running 3 tests
test a_passing_case ... ok
test a_failing_case ... FAILED
test a_skipped_case ... ignored, soak-lane case: runs in the nightly soak lane
some suite output that says test something ... ok is happening
test result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out
",
        );
        assert_eq!(results.get("a_passing_case"), Some(&Outcome::Passed));
        assert_eq!(results.get("a_failing_case"), Some(&Outcome::Failed));
        assert_eq!(results.get("a_skipped_case"), Some(&Outcome::Skipped));
        assert_eq!(
            results.len(),
            3,
            "the summary line and the suite's own output are not results: {results:?}"
        );
    }

    #[test]
    fn a_case_the_logs_report_twice_is_refused() {
        let mut reported = BTreeMap::new();
        record(&mut reported, "a-case".to_string(), Outcome::Failed).expect("the first line");
        let problem = record(&mut reported, "a-case".to_string(), Outcome::Passed)
            .expect_err("a second line for one case");
        assert!(problem.contains("twice"), "{problem}");
    }
}
