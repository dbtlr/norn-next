//! The qualification ledger: what one certification run was, and whether it
//! counts.
//!
//! # Why a record rather than a green check
//!
//! The layer's exit is five consecutive qualifying scheduled runs. "Consecutive"
//! and "qualifying" are both claims about history, and a green check is a claim
//! about one moment: it does not say which suite ran, on which candidate, on
//! what machine, or which of the required cases it actually executed. So each
//! run leaves a [`Record`], and the count is read off the records rather than
//! off the checks.
//!
//! # What makes a run qualifying
//!
//! Three things, and each of them is a field here rather than a judgment a
//! reader makes:
//!
//! 1. **It ran the required suite.** The case outcomes reconcile exactly against
//!    the inventory — every required id present, nothing else, nothing twice —
//!    and the inventory digest the record carries is this build's.
//! 2. **It passed.** Every case outcome is a pass, and the run's own result is a
//!    pass.
//! 3. **Its preflight admitted it.** The environmental classification ran and
//!    admitted the host. That slot is structure here and nothing else: what a
//!    preflight checks and what it refuses is its own task's, and this records
//!    the verdict rather than reaching one.
//!
//! A run that fails any of them is non-qualifying **with a typed reason**, from
//! the closed vocabulary in [`NonQualifying`]. The reason is what a campaign
//! reads to know whether the count restarts or the run simply did not count:
//! a product failure is a defect and resets nothing to be proud of, while a
//! cancellation is a run that never happened.
//!
//! # What is not implemented here
//!
//! The counting. Five consecutive qualifying *scheduled* runs is a rule over a
//! sequence of records — and manual runs never advance it, which is a fact about
//! how a record was produced rather than about its contents. The rule is
//! documented on [`Record`] and applied by the campaign that reads the records.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::inventory::{self, REQUIRED_CASES};
use super::manifest;

/// Where a run's record is written. A run that names no sink writes none.
pub const SINK: &str = "NORN_QUALIFICATION_LEDGER";

/// A file of `<case id> <outcome>` lines, one per case the run executed.
///
/// A case with no line is [`Outcome::NotRun`], which is what a lane that never
/// reached it produced — and what makes the record of a lane that ran no
/// certification suite at all read as the suite change it is.
pub const OUTCOMES: &str = "NORN_QUALIFICATION_OUTCOMES";

/// How the run ended, as the workflow saw its own job.
pub const RESULT: &str = "NORN_QUALIFICATION_RESULT";

/// The runner image or label, which the workflow knows and the process does not.
pub const RUNNER: &str = "NORN_QUALIFICATION_RUNNER";

/// The preflight's verdict, as `admitted` or `refused`. Absent where none ran.
pub const PREFLIGHT: &str = "NORN_QUALIFICATION_PREFLIGHT";

/// What one required case did in one run.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Passed,
    Failed,
    /// The case was compiled and the run skipped it — an ignored case no lane
    /// adopted, or a lane that stopped before reaching it.
    Skipped,
    /// The run never reached the case at all. Distinct from `Skipped` because a
    /// case a run never attempted says nothing about the case, where a skipped
    /// one says the lane declined it.
    NotRun,
}

impl Outcome {
    pub fn passed(&self) -> bool {
        matches!(self, Outcome::Passed)
    }
}

/// One case's line in the record.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CaseOutcome {
    /// The inventory id, which is what makes this line reconcilable.
    pub id: String,
    pub outcome: Outcome,
}

/// How the run itself ended, as the runner saw it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunResult {
    Passed,
    Failed,
    TimedOut,
    Cancelled,
}

/// Why a run does not count toward the five.
///
/// Closed, and closed on purpose: a reason nobody may invent is what keeps the
/// count from being advanced by a run whose failure was explained away in prose.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NonQualifying {
    /// The cases the run executed are not the cases the inventory requires —
    /// a line missing, a line for a case nothing requires, or a required case
    /// the run never attempted. A run of a different suite certifies a
    /// different thing, however green it was.
    SuiteChange,
    /// A required case failed against the product. This is the reason the layer
    /// exists to surface, and the only one that means the candidate is wrong.
    ProductFailure,
    /// The harness failed rather than the product — a fixture, a lease, a
    /// listing, an assertion helper.
    HarnessFailure,
    /// The run exceeded its bound. What was under way when time ran out is
    /// unknown, so nothing about the candidate is concluded.
    Timeout,
    /// The run was cancelled. It never happened.
    Cancellation,
    /// The host refused the run — the preflight declined it, or the environment
    /// broke underneath it. A broken environment is not a broken candidate.
    Environment,
}

/// Whether a run counts.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case", tag = "verdict")]
pub enum Classification {
    /// The run ran the required suite, passed it, and its preflight admitted the
    /// host.
    Qualifying,
    /// The run does not count, for exactly one typed reason.
    NonQualifying { reason: NonQualifying },
}

/// The machine a run happened on.
///
/// Every field is something a required outcome can depend on. The two that
/// decide a case's answer rather than only its speed are named separately, since
/// a campaign covering only one of each value has certified only one of the
/// answers the inventory's platform-deciding lanes require.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Platform {
    pub os: String,
    pub arch: String,
    /// The runner image or label, as the workflow named it.
    pub runner: String,
    /// The watcher backend the host installed coverage through, where the run
    /// knows it.
    pub watcher_backend: Option<String>,
    /// Whether the volume the vault trees were written on folds case, where the
    /// run knows it.
    pub volume_folds_case: Option<bool>,
}

/// The environmental preflight's verdict, as this record carries it.
///
/// **Structure only.** What a preflight measures, what it refuses, and how it
/// classifies a degraded host belong to the preflight's own task; a record that
/// re-decided any of that would be a second classifier. What is required here is
/// that a qualifying run says a preflight ran and admitted the host — so a
/// campaign cannot count a run nobody checked the machine for.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Preflight {
    /// Whether a preflight ran at all.
    pub ran: bool,
    /// Whether it admitted the host. `None` where none ran.
    pub admitted: Option<bool>,
    /// What it said, in whatever vocabulary it owns.
    #[serde(default)]
    pub detail: Option<String>,
}

impl Preflight {
    /// The record a run with no preflight carries.
    pub fn absent() -> Self {
        Preflight {
            ran: false,
            admitted: None,
            detail: None,
        }
    }
}

/// **One certification run.**
///
/// # The five-consecutive rule, documented and not implemented
///
/// The layer exits on five consecutive qualifying runs of the *scheduled* lane
/// over one frozen candidate. Four things that rule turns on, each answerable
/// from the fields below:
///
/// - **Consecutive** is over the scheduled sequence. A non-qualifying run breaks
///   the run of five whatever its reason; the reason decides what happens next,
///   not whether the count broke.
/// - **Manual runs never advance it.** A `workflow_dispatch` run produces a
///   record like any other and is outside the sequence, which is why
///   [`Record::scheduled`] is a field rather than something inferred.
/// - **One candidate.** Every record in the five carries the same
///   [`Record::candidate_sha`]; a new commit starts a new count.
/// - **One suite.** Every record in the five carries the same
///   [`Record::suite_manifest_digest`]; a lane, a bound or the inventory moving
///   under the campaign starts a new count.
///
/// Counting is the campaign's, over a sequence of these. Nothing here holds a
/// sequence, because a record is written by the run it describes and knows
/// nothing of the runs before it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Record {
    /// The commit the suites were built from and run against.
    pub candidate_sha: String,
    /// What certified it: [`super::manifest::digest`].
    pub suite_manifest_digest: String,
    /// The inventory's own contract digest, carried separately so a reader can
    /// tell an inventory edit from a lane edit without recomputing either.
    pub case_inventory_digest: String,
    /// Whether this run came off the schedule. A manual run never advances the
    /// count.
    pub scheduled: bool,
    pub platform: Platform,
    pub preflight: Preflight,
    /// One line per required case.
    pub cases: Vec<CaseOutcome>,
    pub result: RunResult,
    pub classification: Classification,
}

impl Record {
    /// **The classification a record's own contents imply.**
    ///
    /// Written rather than inferred at read time, because a record is evidence
    /// and evidence is not recomputed by whoever reads it — but derived here so
    /// that a writer states it once and [`Record::problems`] can hold the stated
    /// value to the contents beside it.
    ///
    /// The order the reasons are taken in is the order of what a reader needs to
    /// know first: a run that never finished says nothing about the suite, a
    /// suite that did not reconcile says nothing about the product, and only
    /// then does a failing case mean the candidate is wrong.
    pub fn implied_classification(&self) -> Classification {
        match self.result {
            RunResult::Cancelled => {
                return Classification::NonQualifying {
                    reason: NonQualifying::Cancellation,
                };
            }
            RunResult::TimedOut => {
                return Classification::NonQualifying {
                    reason: NonQualifying::Timeout,
                };
            }
            RunResult::Failed | RunResult::Passed => {}
        }
        // A run whose case lines do not reconcile, or that never attempted a
        // required case, did not run the required suite. Both are read before
        // the preflight and before the outcomes, because a run of a different
        // suite says nothing about the machine or the candidate.
        if !self.reconciliation_problems().is_empty()
            || self
                .cases
                .iter()
                .any(|case| case.outcome == Outcome::NotRun)
        {
            return Classification::NonQualifying {
                reason: NonQualifying::SuiteChange,
            };
        }
        if self.preflight.admitted != Some(true) {
            return Classification::NonQualifying {
                reason: NonQualifying::Environment,
            };
        }
        if self.cases.iter().any(|case| !case.outcome.passed()) || self.result != RunResult::Passed
        {
            return Classification::NonQualifying {
                reason: NonQualifying::ProductFailure,
            };
        }
        Classification::Qualifying
    }

    /// Whether the case outcomes are exactly the inventory, one line each.
    ///
    /// The reconciliation is the record's own, so a ledger read years from now
    /// is still checkable against the inventory the build it names carried —
    /// which is why the digests are recorded beside the lines.
    pub fn reconciliation_problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let mut counted: BTreeMap<&str, usize> = BTreeMap::new();
        for case in &self.cases {
            *counted.entry(case.id.as_str()).or_default() += 1;
        }
        for (id, count) in &counted {
            if *count > 1 {
                problems.push(format!("`{id}` is recorded {count} times"));
            }
        }
        let recorded: BTreeSet<&str> = counted.keys().copied().collect();
        let required: BTreeSet<&str> = REQUIRED_CASES.iter().map(|case| case.id).collect();
        for missing in required.difference(&recorded) {
            problems.push(format!(
                "the inventory requires `{missing}` and the run recorded no outcome for it"
            ));
        }
        for extra in recorded.difference(&required) {
            problems.push(format!(
                "the run recorded an outcome for `{extra}`, which the inventory does not require"
            ));
        }
        problems
    }

    /// **Everything wrong with this record**, one line each. A sound record
    /// produces none.
    ///
    /// This is what a reader runs before counting a record: the digests are
    /// present and this build's where the record claims to be about this build,
    /// the case lines reconcile, and the classification the record states is the
    /// one its contents imply.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.candidate_sha.trim().is_empty() {
            problems.push("the record names no candidate".to_string());
        }
        for (what, digest) in [
            ("the suite-manifest digest", &self.suite_manifest_digest),
            ("the case-inventory digest", &self.case_inventory_digest),
        ] {
            if !is_sha256_hex(digest) {
                problems.push(format!("{what} `{digest}` is not a sha256 digest"));
            }
        }
        if self.case_inventory_digest != inventory::contract_digest() {
            problems.push(format!(
                "the record carries the case-inventory digest `{}` and this build's inventory \
                 digests to `{}`. A record read against a different inventory is a record about a \
                 different set of obligations.",
                self.case_inventory_digest,
                inventory::contract_digest()
            ));
        }
        problems.extend(self.reconciliation_problems());

        let implied = self.implied_classification();
        if self.classification != implied {
            problems.push(format!(
                "the record states {:?} and its contents imply {implied:?}",
                self.classification
            ));
        }
        if self.classification == Classification::Qualifying && !self.scheduled {
            problems.push(
                "the record is qualifying and did not come off the schedule. A manual run never \
                 advances the count, so it is never recorded as qualifying."
                    .to_string(),
            );
        }
        problems
    }

    /// Whether this run counts toward the five.
    pub fn qualifies(&self) -> bool {
        self.scheduled && self.classification == Classification::Qualifying
    }
}

/// **The writer.** Assemble this run's record and write it where [`SINK`]
/// names, or do nothing where nothing named a sink.
///
/// Returns the path written, so a workflow step can say where the artifact is.
/// A run outside a workflow names no sink and this is a no-op, which is why the
/// suite that calls it is an ordinary test rather than a lane of its own.
///
/// The classification is derived from the assembled contents rather than taken
/// from the environment: a workflow cannot declare its own run qualifying.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Harness scaffolding: writes this run's own record where the workflow named it.
pub fn emit(workspace_root: &Path) -> Result<Option<PathBuf>, String> {
    let Some(sink) = std::env::var_os(SINK) else {
        return Ok(None);
    };
    let record = from_environment(workspace_root)?;
    let problems = record.problems();
    if !problems.is_empty() {
        return Err(format!(
            "the assembled record is not internally consistent, which is a defect in the writer \
             rather than a fact about the run:\n  {}",
            problems.join("\n  ")
        ));
    }
    let rendered = serde_json::to_string_pretty(&record)
        .map_err(|problem| format!("rendering the record: {problem}"))?;
    let path = PathBuf::from(&sink);
    std::fs::write(&path, rendered.as_bytes())
        .map_err(|problem| format!("writing {}: {problem}", path.display()))?;
    Ok(Some(path))
}

/// Assemble a record from what the workflow set and what this build knows.
///
/// The split is deliberate. The environment supplies what only the runner knows
/// — the candidate, the machine, whether the run came off the schedule, how the
/// job ended, what the preflight said, and which cases ran. Everything else is
/// computed here: both digests, and the classification the contents imply.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: reads the outcomes file the workflow named.
pub fn from_environment(workspace_root: &Path) -> Result<Record, String> {
    let suite_manifest_digest = manifest::digest(workspace_root)
        .map_err(|problem| format!("digesting the suite manifest: {problem}"))?;

    let recorded = match std::env::var_os(OUTCOMES) {
        Some(path) => {
            let text = std::fs::read_to_string(&path)
                .map_err(|problem| format!("reading the outcomes file: {problem}"))?;
            read_outcomes(&text)?
        }
        None => BTreeMap::new(),
    };
    for id in recorded.keys() {
        if inventory::case(id).is_none() {
            return Err(format!(
                "the outcomes file records `{id}`, which the inventory does not require"
            ));
        }
    }

    let mut record = Record {
        candidate_sha: environment("GITHUB_SHA").unwrap_or_else(|| "unknown".to_string()),
        suite_manifest_digest,
        case_inventory_digest: inventory::contract_digest(),
        scheduled: environment("GITHUB_EVENT_NAME").as_deref() == Some("schedule"),
        platform: Platform {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            runner: environment(RUNNER)
                .or_else(|| environment("RUNNER_NAME"))
                .unwrap_or_else(|| "unknown".to_string()),
            watcher_backend: environment("NORN_QUALIFICATION_WATCHER_BACKEND"),
            volume_folds_case: None,
        },
        preflight: match environment(PREFLIGHT).as_deref() {
            Some("admitted") => Preflight {
                ran: true,
                admitted: Some(true),
                detail: None,
            },
            Some("refused") => Preflight {
                ran: true,
                admitted: Some(false),
                detail: None,
            },
            _ => Preflight::absent(),
        },
        cases: REQUIRED_CASES
            .iter()
            .map(|case| CaseOutcome {
                id: case.id.to_string(),
                outcome: recorded.get(case.id).copied().unwrap_or(Outcome::NotRun),
            })
            .collect(),
        result: match environment(RESULT).as_deref() {
            Some("passed") => RunResult::Passed,
            Some("timed-out") => RunResult::TimedOut,
            Some("cancelled") => RunResult::Cancelled,
            _ => RunResult::Failed,
        },
        classification: Classification::Qualifying,
    };
    record.classification = record.implied_classification();
    Ok(record)
}

/// One `<case id> <outcome>` line per case the run executed.
fn read_outcomes(text: &str) -> Result<BTreeMap<String, Outcome>, String> {
    let mut recorded = BTreeMap::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((id, outcome)) = line.split_once(char::is_whitespace) else {
            return Err(format!(
                "line {} of the outcomes file is `{line}`, which is not `<case id> <outcome>`",
                number + 1
            ));
        };
        let outcome = match outcome.trim() {
            "passed" => Outcome::Passed,
            "failed" => Outcome::Failed,
            "skipped" => Outcome::Skipped,
            "not-run" => Outcome::NotRun,
            other => {
                return Err(format!(
                    "line {} of the outcomes file says `{other}`, which is not one of passed, \
                     failed, skipped, not-run",
                    number + 1
                ));
            }
        };
        if recorded.insert(id.trim().to_string(), outcome).is_some() {
            return Err(format!("the outcomes file records `{}` twice", id.trim()));
        }
    }
    Ok(recorded)
}

fn environment(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
}

#[cfg(test)]
mod tests {
    use super::{
        CaseOutcome, Classification, NonQualifying, Outcome, Platform, Preflight, Record, RunResult,
    };
    use crate::certification::inventory::{self, REQUIRED_CASES};

    fn every_case(outcome: Outcome) -> Vec<CaseOutcome> {
        REQUIRED_CASES
            .iter()
            .map(|case| CaseOutcome {
                id: case.id.to_string(),
                outcome,
            })
            .collect()
    }

    fn qualifying() -> Record {
        let mut record = Record {
            candidate_sha: "0".repeat(40),
            suite_manifest_digest: "a".repeat(64),
            case_inventory_digest: inventory::contract_digest(),
            scheduled: true,
            platform: Platform {
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
                runner: "ubuntu-latest".to_string(),
                watcher_backend: Some("inotify".to_string()),
                volume_folds_case: Some(false),
            },
            preflight: Preflight {
                ran: true,
                admitted: Some(true),
                detail: None,
            },
            cases: every_case(Outcome::Passed),
            result: RunResult::Passed,
            classification: Classification::Qualifying,
        };
        record.classification = record.implied_classification();
        record
    }

    #[test]
    fn a_sound_qualifying_record_produces_no_problem() {
        let record = qualifying();
        assert_eq!(record.problems(), Vec::<String>::new());
        assert!(record.qualifies());
    }

    #[test]
    fn a_record_round_trips_through_its_rendering() {
        let record = qualifying();
        let rendered = serde_json::to_string(&record).expect("rendering a record");
        let read: Record = serde_json::from_str(&rendered).expect("reading it back");
        assert_eq!(read, record);
    }

    /// A case the run recorded no outcome for is a suite change, whatever the
    /// rest of the run did: the suite that ran is not the suite required.
    #[test]
    fn a_missing_case_is_a_suite_change() {
        let mut record = qualifying();
        record.cases.pop();
        record.classification = record.implied_classification();
        assert_eq!(
            record.classification,
            Classification::NonQualifying {
                reason: NonQualifying::SuiteChange
            }
        );
        assert!(!record.qualifies());
        // The record is *typed* correctly and still holds a problem, because a
        // record that does not reconcile is evidence about a run rather than a
        // sound record: the reader is told which case is missing as well as that
        // the run does not count.
        let problems = record.problems();
        assert!(
            problems
                .iter()
                .all(|problem| problem.contains("recorded no outcome for it")),
            "{problems:?}"
        );
        assert_eq!(problems.len(), 1, "{problems:?}");
    }

    #[test]
    fn a_case_recorded_twice_is_caught() {
        let mut record = qualifying();
        let first = record.cases[0].clone();
        record.cases.push(first);
        record.classification = record.implied_classification();
        let problems = record.reconciliation_problems();
        assert!(
            problems.iter().any(|problem| problem.contains("2 times")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_case_the_inventory_does_not_require_is_caught() {
        let mut record = qualifying();
        record.cases.push(CaseOutcome {
            id: "not-a-case".to_string(),
            outcome: Outcome::Passed,
        });
        let problems = record.reconciliation_problems();
        assert!(
            problems
                .iter()
                .any(|problem| problem.contains("does not require")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_failing_case_is_a_product_failure() {
        let mut record = qualifying();
        record.cases[0].outcome = Outcome::Failed;
        record.result = RunResult::Failed;
        record.classification = record.implied_classification();
        assert_eq!(
            record.classification,
            Classification::NonQualifying {
                reason: NonQualifying::ProductFailure
            }
        );
    }

    /// A run nobody checked the machine for cannot qualify, and the reason is
    /// the environment rather than the product: nothing was concluded about the
    /// candidate.
    /// A run that never attempted the required cases did not run the required
    /// suite, whatever else it did. This is the classification today's
    /// scheduled soak lane earns: it runs the measurement suites and none of
    /// the certification cases.
    #[test]
    fn a_run_that_attempted_no_required_case_is_a_suite_change() {
        let mut record = qualifying();
        record.cases = every_case(Outcome::NotRun);
        record.classification = record.implied_classification();
        assert_eq!(
            record.classification,
            Classification::NonQualifying {
                reason: NonQualifying::SuiteChange
            }
        );
        assert_eq!(record.problems(), Vec::<String>::new());
    }

    #[test]
    fn a_run_with_no_preflight_is_environmental() {
        let mut record = qualifying();
        record.preflight = Preflight::absent();
        record.classification = record.implied_classification();
        assert_eq!(
            record.classification,
            Classification::NonQualifying {
                reason: NonQualifying::Environment
            }
        );
    }

    /// A run that never finished says nothing about the suite, so its reason is
    /// read off the result before the case lines are looked at.
    #[test]
    fn a_run_that_never_finished_is_typed_by_how_it_ended() {
        for (result, reason) in [
            (RunResult::TimedOut, NonQualifying::Timeout),
            (RunResult::Cancelled, NonQualifying::Cancellation),
        ] {
            let mut record = qualifying();
            record.cases = every_case(Outcome::NotRun);
            record.result = result;
            record.classification = record.implied_classification();
            assert_eq!(
                record.classification,
                Classification::NonQualifying { reason }
            );
        }
    }

    /// A record whose stated classification is not the one its contents imply
    /// is refused. This is the check that keeps a hand-edited record from
    /// counting.
    #[test]
    fn a_stated_classification_the_contents_do_not_imply_is_caught() {
        let mut record = qualifying();
        record.cases[0].outcome = Outcome::Failed;
        let problems = record.problems();
        assert!(
            problems.iter().any(|problem| problem.contains("imply")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_manual_run_is_never_recorded_as_qualifying() {
        let mut record = qualifying();
        record.scheduled = false;
        assert!(!record.qualifies());
        let problems = record.problems();
        assert!(
            problems
                .iter()
                .any(|problem| problem.contains("never advances the count")),
            "{problems:?}"
        );
    }

    #[test]
    fn an_outcomes_file_is_read_line_by_line() {
        let read = super::read_outcomes(
            "# a comment\n\nchurn-case-flip passed\nchurn-burst-and-coalescing  failed\n",
        )
        .expect("a well-formed outcomes file");
        assert_eq!(read.get("churn-case-flip"), Some(&Outcome::Passed));
        assert_eq!(
            read.get("churn-burst-and-coalescing"),
            Some(&Outcome::Failed)
        );
        assert_eq!(read.len(), 2);
    }

    /// An outcome nobody minted is refused rather than read as absent: a typo
    /// silently becoming `not-run` would turn a passing case into a suite
    /// change, which is the wrong story about a green run.
    #[test]
    fn an_unknown_outcome_is_refused() {
        assert!(super::read_outcomes("churn-case-flip green\n").is_err());
        assert!(super::read_outcomes("churn-case-flip\n").is_err());
        assert!(super::read_outcomes("churn-case-flip passed\nchurn-case-flip failed\n").is_err());
    }

    #[test]
    fn a_digest_from_another_inventory_is_caught() {
        let mut record = qualifying();
        record.case_inventory_digest = "b".repeat(64);
        let problems = record.problems();
        assert!(
            problems
                .iter()
                .any(|problem| problem.contains("different set of obligations")),
            "{problems:?}"
        );
    }
}
