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
//! Five things, and each of them is a field here rather than a judgment a
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
//! 4. **Every named exit bar was armed.** The exit checklist names bars, and a
//!    bar authored `None` is one the suite records a reading for and holds
//!    nothing against — so a run under it passed less than the exit asks. The
//!    build's claim about which bars are armed is [`NAMED_EXIT_BARS`], and the
//!    record carries the unarmed ones so it classifies the same however old it
//!    is when read.
//! 5. **It came off the schedule.** A run somebody started is outside the
//!    sequence the five are counted over, so it is typed
//!    [`NonQualifying::ManualDispatch`] rather than left to a reader to notice.
//!
//! A run that fails any of them is non-qualifying **with a typed reason**, from
//! the closed vocabulary in [`NonQualifying`]. The reason is what a campaign
//! reads to know whether the count restarts or the run simply did not count:
//! a product failure is a defect and resets nothing to be proud of, while a
//! cancellation is a run that never happened.
//!
//! # Two validators, and the difference matters
//!
//! [`Record::problems`] is everything wrong with a record, and a campaign runs
//! it before counting one — including, where the record claims to qualify, a
//! recomputation of the suite manifest against this build and the platform facts
//! the inventory's platform-deciding lanes turn on. [`Record::qualifies`] goes
//! through it, so there is no path that counts a record on its stated verdict
//! alone.
//!
//! [`Record::writer_defects`] is the subset that is wrong with the *writer*.
//! Case lines that do not reconcile are a fact about the run and belong in the
//! record; everything else is a record assembled wrong. That is what
//! [`emit`] refuses on, because a run whose suite changed still has to leave the
//! record saying so.
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
///
/// A lane writes it through [`super::lane::collect`], off the harness output
/// its suite steps kept.
pub const OUTCOMES: &str = "NORN_QUALIFICATION_OUTCOMES";

/// How the run ended, as the workflow saw its own job.
pub const RESULT: &str = "NORN_QUALIFICATION_RESULT";

/// The runner image or label, which the workflow knows and the process does not.
pub const RUNNER: &str = "NORN_QUALIFICATION_RUNNER";

/// The preflight's verdict, as `admitted` or `refused`. Absent where none ran.
pub const PREFLIGHT: &str = "NORN_QUALIFICATION_PREFLIGHT";

/// What the preflight said, in whatever vocabulary it owns — one line, because
/// a job environment is line-oriented. Read as prose here and never parsed: the
/// verdict is [`PREFLIGHT`], and this is the reason beside it.
pub const PREFLIGHT_DETAIL: &str = "NORN_QUALIFICATION_PREFLIGHT_DETAIL";

/// The watcher backend the lane's host installs coverage through.
///
/// The workflow's, like [`RUNNER`], and for the same reason: the backend is a
/// property of the runner image the lane picked — inotify on the Linux runner,
/// FSEvents on the macOS one — and a process that installed no coverage cannot
/// report it. A run that names none records `None`, and a qualifying record with
/// `None` here is refused: the inventory's backend-deciding lane is covered by a
/// run on each backend, and a record that does not say which one it was cannot
/// be counted toward either.
pub const WATCHER_BACKEND: &str = "NORN_QUALIFICATION_WATCHER_BACKEND";

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
#[serde(deny_unknown_fields)]
pub struct CaseOutcome {
    /// The inventory id, which is what makes this line reconcilable.
    pub id: String,
    pub outcome: Outcome,
}

/// How the run itself ended, as the runner saw it.
///
/// # `TimedOut` is not reachable from a `timeout-minutes` kill
///
/// A workflow's `timeout-minutes` cancels the job's steps outright. The step
/// that writes a record is an `if: always()` step, and `always()` does not run
/// after a timeout kill — so a run that hit its bound writes **no record at
/// all**, and this variant is set only where something inside the run detected
/// its own deadline and reported it.
///
/// That is deliberate rather than an oversight, and the campaign reads it as
/// such: **the absence of a record for a scheduled run is the timeout signal**,
/// together with the run metadata GitHub keeps, which says the job was cancelled
/// for time. A run of five is broken by a record that does not qualify and
/// equally by a scheduled run that left no record — a campaign that counted only
/// the records it found would count five qualifying runs across a gap.
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
    ///
    /// **A `timeout-minutes` kill produces no record to carry this**, because
    /// the step that writes one does not run after it — see [`RunResult`]. This
    /// reason is for a deadline the run detected in itself, and the campaign's
    /// signal for the other kind is a scheduled run with no record.
    Timeout,
    /// The run was not the scheduled one. A `workflow_dispatch` run executes the
    /// same suite and produces a record like any other, and it is outside the
    /// sequence the five are counted over: the count is of the schedule, so a
    /// run somebody started never advances it however green it was.
    ManualDispatch,
    /// The run was cancelled. It never happened.
    Cancellation,
    /// A bar the layer's exit contract names was unauthored in the build this
    /// run came from: somewhere in that build's suite a reading is recorded
    /// with nothing held against it, so a green run evaluated less than the
    /// exit asks. It is stamped from the build rather than from what this
    /// lane's own steps ran — a calibration window makes every lane's runs
    /// calibration runs, and five of them certify nothing.
    UnauthoredExitBar,
    /// The host refused the run, the environment broke underneath it, or the
    /// run cannot say which machine it was: no preflight verdict, or no answer
    /// for one of the two platform facts the inventory's platform-deciding lanes
    /// turn on. A broken or unknown environment is not a broken candidate.
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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

/// A bar the layer's exit checklist names, and whether this build has it armed.
///
/// The value itself stays in the measuring crate's own baselines file — that
/// file is the trend's whole memory, and it is a manifest entry, so authoring
/// or un-authoring the bar moves the suite digest. What lives here is the
/// *claim* that the bar is armed, which is what the classification below turns
/// on; a test beside the constant holds the claim to the constant, so the two
/// cannot drift apart quietly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExitBar {
    /// The name the record carries where the bar is unarmed.
    pub name: &'static str,
    /// Where the authored value lives, for a reader chasing the bar down.
    pub authored_at: &'static str,
    /// Whether the bar is authored in this build. `false` is the calibration
    /// state: the suite records the reading and holds nothing against it.
    pub armed: bool,
}

/// Every bar the layer's exit contract names.
///
/// The refusal below only reaches the bars this list names, so a bar nobody
/// registered would fail open — green runs under it would qualify. What holds
/// the list exhaustive is a test that scans every crate's `tests/baselines/`
/// file for `Option`-typed constants: an `Option` band *is* the
/// authored-or-calibrating spelling, so each one must have an entry here whose
/// pointer names it, and each entry's pointer must resolve to one.
pub const NAMED_EXIT_BARS: &[ExitBar] = &[ExitBar {
    name: "soak-host-peak-rss-ceiling",
    authored_at: "crates/norn-host/tests/baselines/mod.rs::SOAK_PEAK_RSS_CEILING_BYTES",
    armed: true,
}];

/// The names of the bars this build has unarmed — what the writer stamps into
/// [`Record::unauthored_exit_bars`], and empty in a build whose suite evaluates
/// the whole exit.
pub fn unauthored_exit_bars() -> Vec<String> {
    NAMED_EXIT_BARS
        .iter()
        .filter(|bar| !bar.armed)
        .map(|bar| bar.name.to_string())
        .collect()
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
///   [`Record::scheduled`] is a field rather than something inferred — and why
///   a record that came off no schedule classifies as
///   [`NonQualifying::ManualDispatch`] rather than as qualifying.
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
#[serde(deny_unknown_fields)]
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
    /// The named exit bars that were unauthored in the build this run executed
    /// — [`unauthored_exit_bars`] as the writer's build computed it. Carried in
    /// the record rather than recomputed at read time, so a record classifies
    /// the same from its own contents however far the baselines have moved
    /// since; empty is the only value a qualifying run carries. Required at
    /// parse: a record without it was written by an earlier schema and is
    /// refused whole, the same reading an unknown field gets.
    pub unauthored_exit_bars: Vec<String>,
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
        // A run nobody checked the machine for is a gap in the environment,
        // not in the candidate: nothing was concluded, so nothing qualifies.
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
        // Read after the outcomes, because a failed case is a fact about the
        // candidate whichever bars were armed around it — but before the
        // platform answers, because a green run under an unarmed bar concluded
        // less than the exit asks whatever machine it was on.
        if !self.unauthored_exit_bars.is_empty() {
            return Classification::NonQualifying {
                reason: NonQualifying::UnauthoredExitBar,
            };
        }
        // A run that cannot say which of the two platform answers the
        // inventory's platform-deciding lanes require it produced has
        // concluded nothing the campaign can count — but it is read after the
        // outcomes, because a failed case is a fact about the candidate
        // whatever the record knows about its machine, and `product-failure`
        // is the one reason that means the candidate is wrong. Read here
        // rather than left to the validator, because a run that classified
        // itself Qualifying and then failed validation would leave its record
        // saying one thing and the reader concluding another.
        if self.platform.watcher_backend.is_none() || self.platform.volume_folds_case.is_none() {
            return Classification::NonQualifying {
                reason: NonQualifying::Environment,
            };
        }
        // Last, because it is the one reason that is about how the run was
        // started rather than about what it found. A manual run that failed a
        // case is a product failure and should read as one; a manual run that
        // passed everything is simply outside the sequence.
        if !self.scheduled {
            return Classification::NonQualifying {
                reason: NonQualifying::ManualDispatch,
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
    ///
    /// `workspace_root` is where the suite manifest is recomputed from. The
    /// recomputation happens only where the record claims to be qualifying,
    /// because that is the only claim it is load-bearing for: a non-qualifying
    /// record is evidence about a run that did not count, and holding its
    /// manifest digest against a build years later would refuse the evidence
    /// rather than read it.
    pub fn problems(&self, workspace_root: &Path) -> Vec<String> {
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
        if self.classification == Classification::Qualifying {
            problems.extend(self.qualifying_claims(workspace_root));
        }
        problems.extend(self.reconciliation_problems());

        let implied = self.implied_classification();
        if self.classification != implied {
            problems.push(format!(
                "the record states {:?} and its contents imply {implied:?}",
                self.classification
            ));
        }
        problems
    }

    /// What a record claiming to qualify has to answer for, over and above what
    /// every record answers for.
    ///
    /// Each is a fact a campaign counting five of these relies on and cannot
    /// recover from anywhere else: which suite certified the candidate, which
    /// commit the candidate was, whether the run came off the schedule, and
    /// which of the two platform answers the inventory's platform-deciding lanes
    /// require this run supplied.
    fn qualifying_claims(&self, workspace_root: &Path) -> Vec<String> {
        self.qualifying_claims_against(workspace_root, NAMED_EXIT_BARS)
    }

    /// The claims check over an explicit bar registry, which is what lets a
    /// test hold a record against a build whose bars are unarmed without
    /// un-authoring a real one.
    ///
    /// The bar check is the counting side's own guard, independent of the
    /// record's carried list: a record claiming every bar was armed is still
    /// refused by a build that knows one is not. In real states the manifest
    /// recomputation below refuses the same records — un-authoring a bar edits
    /// a manifest entry — so this is the belt over that braces, reachable on
    /// its own only through a record whose carried list was tampered with.
    fn qualifying_claims_against(&self, workspace_root: &Path, bars: &[ExitBar]) -> Vec<String> {
        let mut problems = Vec::new();
        for bar in bars.iter().filter(|bar| !bar.armed) {
            problems.push(format!(
                "the record is qualifying and this build names the exit bar `{}` unauthored (its \
                 value lives at {}). Five qualifying runs cannot accumulate while a named exit \
                 bar is unarmed, so a count under this build refuses every one of them.",
                bar.name, bar.authored_at
            ));
        }
        match manifest::digest(workspace_root) {
            Ok(built) if built != self.suite_manifest_digest => problems.push(format!(
                "the record is qualifying and carries the suite-manifest digest `{}`, and this \
                 build's manifest digests to `{built}`. A run counted as qualifying is a run of \
                 the suite this build defines, so a record that names another one is a record \
                 about a different suite.",
                self.suite_manifest_digest
            )),
            Ok(_) => {}
            Err(problem) => problems.push(format!(
                "the record is qualifying and this build's suite manifest could not be digested \
                 to check it against: {problem}"
            )),
        }
        if !is_commit_sha(&self.candidate_sha) {
            problems.push(format!(
                "the record is qualifying and names the candidate `{}`, which is not a 40-digit \
                 lowercase commit sha. Five consecutive runs are over one frozen candidate, and a \
                 candidate nothing resolves is a candidate two records cannot be compared by.",
                self.candidate_sha
            ));
        }
        if !self.scheduled {
            problems.push(
                "the record is qualifying and did not come off the schedule. A manual run never \
                 advances the count, so it is never recorded as qualifying."
                    .to_string(),
            );
        }
        if self.platform.watcher_backend.is_none() {
            problems.push(
                "the record is qualifying and names no watcher backend. The inventory's \
                 backend-deciding lane is covered by a run on each backend, so a record that does \
                 not say which one it ran on covers neither."
                    .to_string(),
            );
        }
        if self.platform.volume_folds_case.is_none() {
            problems.push(
                "the record is qualifying and does not say whether the volume folds case. The \
                 inventory's volume-folding lane is covered by a run on a folding volume and a run \
                 on a case-sensitive one, so a record that does not say which it was covers \
                 neither."
                    .to_string(),
            );
        }
        problems
    }

    /// Everything wrong with this record that is wrong with the **writer**
    /// rather than with the run.
    ///
    /// Case lines that do not reconcile are a fact about the run — a run of a
    /// different suite — and the record is the place that fact is carried. Every
    /// other problem is a record the writer assembled wrong, which is a defect
    /// rather than evidence.
    pub fn writer_defects(&self, workspace_root: &Path) -> Vec<String> {
        let about_the_run = self.reconciliation_problems();
        self.problems(workspace_root)
            .into_iter()
            .filter(|problem| !about_the_run.contains(problem))
            .collect()
    }

    /// **Whether this run counts toward the five.**
    ///
    /// Every term, and no path around the validator: the run came off the
    /// schedule, it states that it qualifies, and nothing is wrong with the
    /// record that says so. A reader that took the stated classification alone
    /// would count a record whose digests, candidate or case lines had come
    /// apart from what it claims.
    pub fn qualifies(&self, workspace_root: &Path) -> bool {
        self.scheduled
            && self.classification == Classification::Qualifying
            && self.problems(workspace_root).is_empty()
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
    let defects = record.writer_defects(workspace_root);
    if !defects.is_empty() {
        return Err(format!(
            "the assembled record is not internally consistent, which is a defect in the writer \
             rather than a fact about the run:\n  {}",
            defects.join("\n  ")
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
/// computed here: both digests, the volume's own answer about case, and the
/// classification the contents imply.
///
/// **An outcome for a case the inventory does not require is recorded rather
/// than refused.** A run whose outcomes name an id nothing requires ran a
/// different suite from the one the layer asks for, which is exactly what
/// [`NonQualifying::SuiteChange`] is, and it is the symmetric case to a required
/// id with no line. Refusing to assemble would leave no record at all — and a
/// scheduled run with no record is how the campaign reads a timeout, so a stale
/// id would be reported as the wrong thing entirely.
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
            watcher_backend: environment(WATCHER_BACKEND),
            volume_folds_case: probe_volume_folding(),
        },
        preflight: match environment(PREFLIGHT).as_deref() {
            Some("admitted") => Preflight {
                ran: true,
                admitted: Some(true),
                detail: environment(PREFLIGHT_DETAIL),
            },
            Some("refused") => Preflight {
                ran: true,
                admitted: Some(false),
                detail: environment(PREFLIGHT_DETAIL),
            },
            // A verdict this does not recognise is no verdict: a preflight
            // whose spelling changed is a host nobody checked, which is what
            // the absent record says.
            _ => Preflight::absent(),
        },
        cases: REQUIRED_CASES
            .iter()
            .map(|case| CaseOutcome {
                id: case.id.to_string(),
                outcome: recorded.get(case.id).copied().unwrap_or(Outcome::NotRun),
            })
            .chain(
                recorded
                    .iter()
                    .filter(|(id, _)| inventory::case(id).is_none())
                    .map(|(id, outcome)| CaseOutcome {
                        id: id.clone(),
                        outcome: *outcome,
                    }),
            )
            .collect(),
        result: match environment(RESULT).as_deref() {
            Some("passed") => RunResult::Passed,
            Some("timed-out") => RunResult::TimedOut,
            Some("cancelled") => RunResult::Cancelled,
            _ => RunResult::Failed,
        },
        unauthored_exit_bars: unauthored_exit_bars(),
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

/// **What the volume the vault trees are written on does with case**, asked of
/// that volume rather than of the platform.
///
/// A case-sensitive volume mounted on a machine whose boot volume folds is
/// exactly what a target-family answer gets wrong, and the inventory's
/// volume-folding lane turns on this answer — so it is probed at the place the
/// fixtures put their trees: a directory of this run's own under the system
/// temporary directory, written to and removed. A probe that cannot run leaves
/// the field `None`, which a qualifying record is refused for.
///
/// It lives on the emitter's side rather than in the product: nothing the store
/// or the host does branches on this, and a probe compiled into a shipped build
/// would be a filesystem write no caller asked for.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Harness scaffolding: this run's own case probe, under the directory the fixtures write trees into.
fn probe_volume_folding() -> Option<bool> {
    let at = std::env::temp_dir().join(format!(
        "norn-qualification-case-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&at).ok()?;
    let folding = crate::churn::folding(&at).ok();
    let _ = std::fs::remove_dir_all(&at);
    Some(folding? == crate::churn::Folding::Folded)
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && is_lowercase_hex(value)
}

/// A git commit as a record names one: the full 40-digit hexadecimal, lowercase.
///
/// The abbreviation is refused as well as the empty string, because "one frozen
/// candidate" is a comparison between five records and two abbreviations of
/// different lengths do not compare.
fn is_commit_sha(value: &str) -> bool {
    value.len() == 40 && is_lowercase_hex(value)
}

fn is_lowercase_hex(value: &str) -> bool {
    value
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        CaseOutcome, Classification, ExitBar, NonQualifying, Outcome, Platform, Preflight, Record,
        RunResult,
    };
    use crate::certification::inventory::{self, REQUIRED_CASES};
    use crate::certification::manifest;

    /// The checkout these tests run inside, which is what a record's
    /// suite-manifest digest is recomputed against.
    fn workspace_root() -> PathBuf {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest
            .ancestors()
            .nth(2)
            .unwrap_or_else(|| panic!("no workspace root above {}", manifest.display()))
            .to_path_buf()
    }

    fn every_case(outcome: Outcome) -> Vec<CaseOutcome> {
        REQUIRED_CASES
            .iter()
            .map(|case| CaseOutcome {
                id: case.id.to_string(),
                outcome,
            })
            .collect()
    }

    fn qualifying_at(root: &Path) -> Record {
        let mut record = Record {
            candidate_sha: "0".repeat(40),
            suite_manifest_digest: manifest::digest(root).expect("digesting the suite manifest"),
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
            unauthored_exit_bars: Vec::new(),
            classification: Classification::Qualifying,
        };
        record.classification = record.implied_classification();
        record
    }

    #[test]
    fn a_sound_qualifying_record_produces_no_problem() {
        let root = workspace_root();
        let record = qualifying_at(&root);
        assert_eq!(record.problems(&root), Vec::<String>::new());
        assert!(record.qualifies(&root));
    }

    #[test]
    fn a_record_round_trips_through_its_rendering() {
        let root = workspace_root();
        let record = qualifying_at(&root);
        let rendered = serde_json::to_string(&record).expect("rendering a record");
        let read: Record = serde_json::from_str(&rendered).expect("reading it back");
        assert_eq!(read, record);
    }

    /// A case the run recorded no outcome for is a suite change, whatever the
    /// rest of the run did: the suite that ran is not the suite required.
    #[test]
    fn a_missing_case_is_a_suite_change() {
        let root = workspace_root();
        let mut record = qualifying_at(&root);
        record.cases.pop();
        record.classification = record.implied_classification();
        assert_eq!(
            record.classification,
            Classification::NonQualifying {
                reason: NonQualifying::SuiteChange
            }
        );
        assert!(!record.qualifies(&root));
        // The record is *typed* correctly and still holds a problem, because a
        // record that does not reconcile is evidence about a run rather than a
        // sound record: the reader is told which case is missing as well as that
        // the run does not count.
        let problems = record.problems(&root);
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
        let root = workspace_root();
        let mut record = qualifying_at(&root);
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
        let root = workspace_root();
        let mut record = qualifying_at(&root);
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
        let root = workspace_root();
        let mut record = qualifying_at(&root);
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

    /// A run that never attempted the required cases did not run the required
    /// suite, whatever else it did.
    ///
    /// **It is a lane regression rather than a steady state.** Both scheduled
    /// lanes run every certification target and read an outcome per case off
    /// the harness's own output, so a record whose cases are all `not-run` is
    /// a lane whose collection came apart — logs written where the collector
    /// does not read, or an outcomes file the record step does not find — and
    /// this classification is how it says so instead of passing quietly.
    #[test]
    fn a_run_that_attempted_no_required_case_is_a_suite_change() {
        let root = workspace_root();
        let mut record = qualifying_at(&root);
        record.cases = every_case(Outcome::NotRun);
        record.classification = record.implied_classification();
        assert_eq!(
            record.classification,
            Classification::NonQualifying {
                reason: NonQualifying::SuiteChange
            }
        );
        assert_eq!(record.problems(&root), Vec::<String>::new());
    }

    /// A run nobody checked the machine for cannot qualify, and the reason is
    /// the environment rather than the product: nothing was concluded about the
    /// candidate.
    ///
    /// **A refused host reads the same way, and a green suite does not rescue
    /// it.** Every case here passes: what the refusal says is that the machine
    /// those passes were taken on is not one the layer counts, so the record
    /// carries the outcomes and counts toward nothing.
    #[test]
    fn a_run_with_no_admitted_preflight_is_environmental() {
        let root = workspace_root();
        for preflight in [
            Preflight::absent(),
            Preflight {
                ran: true,
                admitted: Some(false),
                detail: Some("refused: 3.992 runnable per core".to_string()),
            },
        ] {
            let mut record = qualifying_at(&root);
            record.preflight = preflight.clone();
            record.classification = record.implied_classification();
            assert_eq!(
                record.classification,
                Classification::NonQualifying {
                    reason: NonQualifying::Environment
                },
                "{preflight:?}"
            );
            assert!(!record.qualifies(&root), "{preflight:?}");
        }
    }

    /// A run that never finished says nothing about the suite, so its reason is
    /// read off the result before the case lines are looked at.
    #[test]
    fn a_run_that_never_finished_is_typed_by_how_it_ended() {
        let root = workspace_root();
        for (result, reason) in [
            (RunResult::TimedOut, NonQualifying::Timeout),
            (RunResult::Cancelled, NonQualifying::Cancellation),
        ] {
            let mut record = qualifying_at(&root);
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
        let root = workspace_root();
        let mut record = qualifying_at(&root);
        record.cases[0].outcome = Outcome::Failed;
        let problems = record.problems(&root);
        assert!(
            problems.iter().any(|problem| problem.contains("imply")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_manual_run_is_never_recorded_as_qualifying() {
        let root = workspace_root();
        let mut record = qualifying_at(&root);
        record.scheduled = false;
        assert!(!record.qualifies(&root));
        let problems = record.problems(&root);
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

    /// **A qualifying record is held to this build's suite manifest.** A record
    /// naming another one is a record about a different suite, and counting it
    /// toward five consecutive runs of one suite is the error the digest exists
    /// to prevent.
    #[test]
    fn a_manifest_digest_from_another_suite_is_caught_where_the_record_qualifies() {
        let root = workspace_root();
        let mut record = qualifying_at(&root);
        record.suite_manifest_digest = "c".repeat(64);
        let problems = record.problems(&root);
        assert!(
            problems
                .iter()
                .any(|problem| problem.contains("a different suite")),
            "{problems:?}"
        );
        assert!(!record.qualifies(&root));
    }

    /// A non-qualifying record is *not* held to it. Its manifest digest is
    /// evidence about which suite ran, and a record read years later against a
    /// build that has moved on is evidence to read rather than to refuse.
    #[test]
    fn a_manifest_digest_from_another_suite_stands_where_the_record_does_not_qualify() {
        let root = workspace_root();
        let mut record = qualifying_at(&root);
        record.suite_manifest_digest = "c".repeat(64);
        record.cases = every_case(Outcome::NotRun);
        record.classification = record.implied_classification();
        assert_eq!(record.problems(&root), Vec::<String>::new());
    }

    /// **`qualifies` goes through the validator.** A record whose cases were
    /// never run and whose verdict says otherwise is refused, where a reader
    /// taking the stated classification alone would have counted it.
    #[test]
    fn a_record_that_states_qualifying_over_cases_nothing_ran_does_not_qualify() {
        let root = workspace_root();
        let mut record = qualifying_at(&root);
        record.cases = every_case(Outcome::NotRun);
        record.classification = Classification::Qualifying;
        assert!(!record.qualifies(&root));
        assert_eq!(
            record.implied_classification(),
            Classification::NonQualifying {
                reason: NonQualifying::SuiteChange
            }
        );
    }

    /// **A manual run that passed everything is typed rather than refused.** It
    /// executed the suite and its record is worth writing; what it is not is a
    /// run in the sequence, which is what the reason says. Nothing is wrong with
    /// the record, so the writer emits it.
    #[test]
    fn a_manual_run_that_passed_everything_is_a_manual_dispatch() {
        let root = workspace_root();
        let mut record = qualifying_at(&root);
        record.scheduled = false;
        record.classification = record.implied_classification();
        assert_eq!(
            record.classification,
            Classification::NonQualifying {
                reason: NonQualifying::ManualDispatch
            }
        );
        assert!(!record.qualifies(&root));
        assert_eq!(record.problems(&root), Vec::<String>::new());
        assert_eq!(record.writer_defects(&root), Vec::<String>::new());
    }

    /// A manual run that failed a case reads as the product failure it is: the
    /// reason a reader needs is what the run found, not how it was started.
    #[test]
    fn a_manual_run_that_failed_a_case_is_still_a_product_failure() {
        let root = workspace_root();
        let mut record = qualifying_at(&root);
        record.scheduled = false;
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

    /// An outcome for a case nothing requires is the suite change it is, and the
    /// record carrying it is one the writer emits: the reconciliation line is a
    /// fact about the run, so it is not a defect that stops the record being
    /// written.
    #[test]
    fn an_outcome_for_a_case_nothing_requires_is_a_written_suite_change() {
        let root = workspace_root();
        let mut record = qualifying_at(&root);
        record.cases.push(CaseOutcome {
            id: "a-case-no-inventory-requires".to_string(),
            outcome: Outcome::Passed,
        });
        record.classification = record.implied_classification();
        assert_eq!(
            record.classification,
            Classification::NonQualifying {
                reason: NonQualifying::SuiteChange
            }
        );
        assert!(
            record
                .problems(&root)
                .iter()
                .any(|problem| problem.contains("does not require")),
        );
        assert_eq!(record.writer_defects(&root), Vec::<String>::new());
    }

    /// The candidate is what "one frozen candidate" is compared by, so a
    /// qualifying record names a whole commit rather than an abbreviation or a
    /// placeholder.
    #[test]
    fn a_qualifying_record_names_a_whole_commit() {
        let root = workspace_root();
        for named in ["unknown", &"0".repeat(7), &"A".repeat(40)] {
            let mut record = qualifying_at(&root);
            record.candidate_sha = named.to_string();
            let problems = record.problems(&root);
            assert!(
                problems
                    .iter()
                    .any(|problem| problem.contains("lowercase commit sha")),
                "`{named}` was accepted: {problems:?}"
            );
        }
    }

    /// The two platform facts the inventory's platform-deciding lanes turn on
    /// are required of a qualifying record: a run that does not say which answer
    /// it produced covers neither of the two a lane needs.
    ///
    /// A record missing one classifies as environmental — something about the
    /// host is unknown — and is written rather than refused. A record that
    /// *states* Qualifying without them is caught by the validator, which is the
    /// hand-edited case.
    #[test]
    fn a_qualifying_record_says_which_platform_answers_it_produced() {
        let root = workspace_root();
        for (drop_backend, wanted) in [(true, "watcher backend"), (false, "volume folds case")] {
            let mut record = qualifying_at(&root);
            if drop_backend {
                record.platform.watcher_backend = None;
            } else {
                record.platform.volume_folds_case = None;
            }
            assert_eq!(
                record.implied_classification(),
                Classification::NonQualifying {
                    reason: NonQualifying::Environment
                }
            );

            record.classification = Classification::Qualifying;
            let problems = record.problems(&root);
            assert!(
                problems.iter().any(|problem| problem.contains(wanted)),
                "{problems:?}"
            );
            assert!(!record.qualifies(&root));

            // And the record the writer would actually assemble is one it
            // emits: an unknown machine is a fact about the run, so it is
            // recorded with its reason rather than refused.
            record.classification = record.implied_classification();
            assert_eq!(record.writer_defects(&root), Vec::<String>::new());
        }
    }

    /// A field nobody minted is refused rather than dropped. A record read back
    /// with an unknown key is a record written by another version of this type,
    /// and reading it as though the key were not there would count a run under
    /// rules this build cannot see.
    #[test]
    fn a_record_carrying_a_field_this_build_does_not_know_is_refused() {
        let root = workspace_root();
        let record = qualifying_at(&root);
        let rendered = serde_json::to_string(&record).expect("rendering a record");
        let doctored = rendered.replacen('{', r#"{"waived_by":"somebody","#, 1);
        assert!(serde_json::from_str::<Record>(&doctored).is_err());
    }

    /// The volume probe answers about the volume the fixtures write trees on
    /// rather than about the platform, so it answers at all: a record assembled
    /// on any machine this suite runs on carries a value here.
    #[test]
    fn the_volume_probe_answers() {
        assert!(super::probe_volume_folding().is_some());
    }

    /// A run green under an unarmed exit bar is a calibration run, and the
    /// record it leaves says so with a typed reason: the suite recorded the
    /// reading and held nothing against it, so the run evaluated less than the
    /// exit asks. The record is evidence rather than a defect — the writer
    /// emits it, and nothing else about it is wrong.
    #[test]
    fn a_run_under_an_unauthored_exit_bar_is_typed_by_it() {
        let root = workspace_root();
        let mut record = qualifying_at(&root);
        record.unauthored_exit_bars = vec!["soak-host-peak-rss-ceiling".to_string()];
        record.classification = record.implied_classification();
        assert_eq!(
            record.classification,
            Classification::NonQualifying {
                reason: NonQualifying::UnauthoredExitBar
            }
        );
        assert!(!record.qualifies(&root));
        assert_eq!(record.problems(&root), Vec::<String>::new());
        assert_eq!(record.writer_defects(&root), Vec::<String>::new());
    }

    /// A failed case still reads as the product failure it is: which bars were
    /// armed around it changes nothing about what the run found.
    #[test]
    fn a_product_failure_outranks_an_unauthored_exit_bar() {
        let root = workspace_root();
        let mut record = qualifying_at(&root);
        record.unauthored_exit_bars = vec!["soak-host-peak-rss-ceiling".to_string()];
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

    /// **A stated-qualifying record is refused by a build whose registry holds
    /// an unarmed bar.** This is the hand-edited half: a record whose own list
    /// is empty claims every bar was armed, and the build counting it knows
    /// otherwise — the same shape as the manifest recomputation beside it.
    #[test]
    fn a_qualifying_record_is_refused_where_this_build_has_an_unarmed_exit_bar() {
        let root = workspace_root();
        let record = qualifying_at(&root);
        let unarmed = [ExitBar {
            name: "soak-host-peak-rss-ceiling",
            authored_at: "crates/norn-host/tests/baselines/mod.rs::SOAK_PEAK_RSS_CEILING_BYTES",
            armed: false,
        }];
        let problems = record.qualifying_claims_against(&root, &unarmed);
        assert!(
            problems
                .iter()
                .any(|problem| problem.contains("`soak-host-peak-rss-ceiling` unauthored")),
            "{problems:?}"
        );
    }

    /// **An `Option`-typed baseline is an exit bar, and every one is
    /// registered.** A band authored `None` that nobody added to
    /// [`super::NAMED_EXIT_BARS`] would stamp nothing into any record and its
    /// green runs would qualify — the fail-open direction — so the registry is
    /// held exhaustive against the baselines files themselves, and each
    /// entry's pointer is held to a constant that exists. The files are found
    /// rather than listed, so a new crate's baselines join the sweep by
    /// existing.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: reads the checkout's own baselines files.
    fn every_option_typed_baseline_is_a_registered_exit_bar() {
        let root = workspace_root();
        let mut option_bars = Vec::new();
        let crates = std::fs::read_dir(root.join("crates")).expect("listing the crates directory");
        for crate_dir in crates {
            let crate_dir = crate_dir.expect("reading a crates entry").path();
            let baselines = crate_dir.join("tests/baselines/mod.rs");
            let Ok(text) = std::fs::read_to_string(&baselines) else {
                continue;
            };
            let file = baselines
                .strip_prefix(&root)
                .expect("a baselines path under the workspace root")
                .to_string_lossy()
                .replace('\\', "/");
            for line in text.lines() {
                let Some(rest) = line.trim().strip_prefix("pub const ") else {
                    continue;
                };
                let Some((name, ty)) = rest.split_once(':') else {
                    continue;
                };
                if ty.trim_start().starts_with("Option<") {
                    option_bars.push(format!("{file}::{}", name.trim()));
                }
            }
        }
        assert!(
            !option_bars.is_empty(),
            "the sweep found no Option-typed baseline at all, so it is reading the wrong tree"
        );
        for pointer in &option_bars {
            assert!(
                super::NAMED_EXIT_BARS
                    .iter()
                    .any(|bar| bar.authored_at == pointer.as_str()),
                "`{pointer}` is an Option-typed baseline with no exit-bar entry; a bar nobody \
                 registered fails open — its calibration runs would qualify"
            );
        }
        for bar in super::NAMED_EXIT_BARS {
            assert!(
                option_bars.iter().any(|pointer| pointer == bar.authored_at),
                "`{}` names no Option-typed constant in any baselines file, so the registry \
                 points a reader at nothing",
                bar.authored_at
            );
        }
    }

    #[test]
    fn a_digest_from_another_inventory_is_caught() {
        let root = workspace_root();
        let mut record = qualifying_at(&root);
        record.case_inventory_digest = "b".repeat(64);
        let problems = record.problems(&root);
        assert!(
            problems
                .iter()
                .any(|problem| problem.contains("different set of obligations")),
            "{problems:?}"
        );
    }
}
