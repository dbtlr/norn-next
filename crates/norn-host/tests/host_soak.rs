//! The scheduled soak lane's host workload: a long mixed load over an attached
//! vault, and the two shapes a leak takes.
//!
//! **Lockdown is counted over a nightly mixed load showing four things: zero
//! counter violations, a flat memory slope, no file-descriptor growth, and a
//! peak resident set under its authored ceiling.**
//! This case is that load and those four assertions. A load that runs for an
//! hour is how a cost paid once per reconciliation becomes visible: a
//! descriptor a subscription never closes, a buffer a heal never releases, and
//! a warm read that starts deriving all show as a trend rather than as a
//! failure at any one instant.
//!
//! # The composition
//!
//! The parent generates the ≥5k-document `soak` profile's tree and spawns a
//! child — this test binary re-executed in a harness mode an environment
//! variable selects, paired with a token the parent issued beside the tree so
//! that a variable leaked into the lane's own environment fails loudly instead
//! of turning the bar into a load nobody judges. The child attaches the tree
//! through a production host and then works it: it churns Markdown files in the
//! vault so the watcher has something to reconcile, polls the attachment's
//! trust state, and takes warm read-only requests against the derived store,
//! asserting that every one of them finishes with its derivation counters at
//! zero. Alongside the load it samples its own resident set and
//! open-descriptor count on a fixed cadence and prints each sample, which is
//! what the parent judges.
//!
//! **The child holds a demand lease for the whole load.** Demand is what keeps
//! the idle reaper away from the entry, and it is also the only thing that
//! re-attaches one that went untrusted, so the load holds one and asks again
//! — a bounded number of times — whenever the entry stops serving. What the
//! trust-state assertion says is that the host kept serving under load, not
//! that nothing ever hiccuped.
//!
//! # The deliberate recovery
//!
//! **A run that never lost its attachment measured nothing about getting one
//! back.** So the load does not wait for a hiccup: the child is started armed
//! at `norn-fs`'s watcher fault seam, at the stream stage, and the first change
//! it churns under live coverage is answered with the error a backend that
//! stopped for good produces. The entry publishes the loss and stops serving,
//! and the demand the load already holds is what re-installs coverage — the
//! production way back, reached through the production seam rather than through
//! anything this suite plumbed into the host.
//!
//! **The arm is budgeted to one watch.** The condition is per establishment, so
//! an unbudgeted arm would meet the coverage each recovery installs and the
//! load would spend its hour re-attaching — a different subject from the one
//! the memory and descriptor bands were authored over. One establishment armed
//! is one condition met and the rest of the run served.
//!
//! Three things are then required of the run. The recovery **happened**: the
//! seam's own record says the stream arm fired, exactly once, and the count of
//! recoveries the load reports is held to [`baselines::SOAK_RECOVERY_DOSE`]. It
//! **completed**: an attempt has [`RECOVERY_LIMIT`] to bring the entry back and
//! a run spends at most [`RECOVERY_ATTEMPTS`] of them. And **nothing starved
//! while it was in flight**: the load keeps churning and taking warm read-only
//! requests through the recovery rather than blocking on it.
//!
//! **The no-starvation term is wall clock, and it is the one term here that
//! is.** Starvation is a load that stopped, and a load that stopped advances no
//! tick — so a count of ticks compared against the loop's own iterations agrees
//! with itself whatever the run did. What is asked instead is the rate: the
//! stretch between losing service and getting it back is measured in real time,
//! and over it the load must have landed at least the churn turns and warm
//! reads its cadence owes for that much time. Every look at the entry's state
//! across the window carries the crate's own probe bound with it, so a host
//! that stopped answering is a typed failure rather than a slow window. That
//! term is what the other bars cannot say — a process that stopped serving to
//! recover holds a flat resident set and opens no descriptors.
//!
//! Generation happens in the parent so that the child's samples are of the
//! attachment and the load, and the counter assertions are made in the child
//! because a violation is the child's own request finishing non-zero — the exit
//! status carries it out.
//!
//! # What is a count and what is a clock
//!
//! The load's *duration* is a clock, which is why this case is the scheduled
//! lane's and not the per-PR lane's. Nothing here asserts on it: the bars are a
//! descriptor count, a ratio between two means of a sampled series, and — where
//! one is authored — a ceiling on the highest sample the series holds, and each
//! says the same thing on a slow runner as on a fast one.
//! `NORN_SOAK_DURATION_SECS` is how long the load runs, defaulting low enough
//! that a local `--ignored` run is usable; the workflow passes an hour.
//!
//! **The peak resident set is recorded every run and barred only where a
//! ceiling is authored.** [`baselines::SOAK_PEAK_RSS_CEILING_BYTES`] carries
//! the ceiling; a build that un-authors it back to `None` for recalibration
//! records the reading and bars nothing — and the ledger stamps every such
//! run's record non-qualifying, so calibration runs are never counted toward
//! the five (`norn_testkit::certification::ledger::NAMED_EXIT_BARS`, held to
//! the constant by a test in `settle.rs`, which holds every entry of that
//! registry to the baseline it names). It is the peak of the *load and the one
//! re-attach inside it*: the child samples itself from inside the churn loop,
//! which it reaches once the attachment is ready, so the attach the load starts
//! with is ahead of the first sample — and the deliberate recovery's re-attach,
//! which walks the same ≥5k tree, is not.
//!
//! **The slope is read over the samples outside the recovery windows.** A
//! re-attach's walk is attach cost, it lands in the first quartile every run,
//! and the slope is a bound on a rise between quartile means — so left in the
//! series it would sit in the denominator and depress a ratio that is only ever
//! failed by a large one. The peak keeps every sample and the slope drops the
//! window: the two bars read the run they each describe.
//!
//! Running this needs `/proc` or its BSD equivalent, so the case is present on
//! Linux and macOS and absent elsewhere. The scheduled lane runs it on Linux.
#![cfg(any(target_os = "linux", target_os = "macos"))]
// The load arms `norn-fs`'s watcher fault seam, and that seam has a reader only
// under this feature. Without it the case below would run a load that never
// meets its deliberate recovery and then fail its own dose, so the suite
// compiles to nothing instead and the lanes that run it name the feature.
#![cfg(feature = "induced-failure")]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree and its own accounting.

mod attach;
mod baselines;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use norn_fs::ContentHash;
use norn_host::{AttachMode, DemandLease, Host, ProductionEntryOps};
use norn_store::{DocumentPath, ExplainedStatement, Store, StoredPathOrder, class_probe};
use norn_testkit::attestation::{Attestation, SEAM};
use norn_testkit::process::{Run, Sandbox, open_fd_count};
use norn_testkit::wait::{Budget, Observed, wait_until};
use norn_wire::{ErrorEnvelope, ReasonCode, TrustState, VaultName};

/// The variable that puts this binary in harness mode, carrying the root the
/// generated tree sits under.
const HARNESS_ENV: &str = "NORN_HOST_SOAK_HARNESS";

/// The variable carrying the token the tree at that root was issued.
///
/// [`HARNESS_ENV`] alone does not select harness mode: a variable already in
/// the environment this lane runs under would make the case below a load run
/// that reports samples nobody judges, and the lane would pass having evaluated
/// no bar. The token is what says a parent in this run issued the harness.
const HARNESS_TOKEN_ENV: &str = "NORN_HOST_SOAK_HARNESS_TOKEN";

/// The case the child re-executes, which is the one that reads this constant.
const HARNESS_CASE: &str = "a_long_mixed_load_grows_neither_memory_nor_descriptors";

/// The variable the parent arms `norn-fs`'s watcher seam through.
///
/// Spelled here rather than imported because the seam it reaches is fenced
/// inside `norn-fs`: a harness arms it the way anything outside that crate
/// does, by putting the pair in the environment the child is started with.
const WATCH_ARMED_STAGES: &str = "NORN_FS_WATCH_ARMED_STAGES";

/// The variable that budgets that arm to a number of the child's watches.
const WATCH_ARMED_WATCHES: &str = "NORN_FS_WATCH_ARMED_WATCHES";

/// The variable naming the file a fired arm records itself in.
const ARM_HITS: &str = "NORN_FS_ARM_HITS";

/// The seam a fired watcher arm records itself under.
const WATCH_SEAM: &str = "norn-fs/watch";

/// **What the load is armed at: a backend stream that ends under live
/// coverage.** The entry publishes the loss, stops serving, and comes back only
/// through a demand — which is the recovery this load is required to trip and
/// go on working through.
const ARMED_STAGE: &str = "stream";
const ARMED_ANSWER: &str = "fails";

/// How many of the child's watch establishments the arm reaches.
///
/// One: the attach the load begins with. The condition is per establishment, so
/// an unbudgeted arm would meet the coverage every recovery installs and the
/// load would spend the run re-attaching instead of being served through one
/// recovery — a different subject from the one the memory and descriptor bands
/// were authored over.
const ARMED_WATCHES: &str = "1";

/// Where the child's arm records itself, under the sandbox's working directory.
///
/// **Outside every watched edge.** Coverage over a vault is the tree and the
/// tree's own parent, and the seam writes this file while coverage is live — so
/// a record inside either would be a filesystem change the backend reports, and
/// it could be the very delivery the stream arm stands in place of. The
/// generated tree sits under `attached/`, so a directory beside it is outside
/// both edges.
const ARM_RECORDS_DIR: &str = "arm-records";

/// How long the load runs, in seconds.
///
/// The default is short enough that running this suite locally is a normal
/// thing to do; the scheduled lane names an hour in the workflow step.
const DURATION_ENV: &str = "NORN_SOAK_DURATION_SECS";
const DEFAULT_DURATION: Duration = Duration::from_secs(90);

/// How often the child takes a sample of itself.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// How long one read of the derived store may take.
///
/// A look that opens a read request costs more than a label read, and this is
/// what separates a database that has stopped answering from a reconcile that
/// has not landed.
const STORE_READ_PROBE: Duration = Duration::from_secs(5);

/// How often a sample is preceded by a warm read-only request, in samples.
///
/// The cadence is the load's request dose, and it is what the no-starvation
/// term charges a recovery window the wall clock of. A recovery demanded on a
/// tick the cadence does not fall on takes a request anyway, so the window
/// always opens with one rather than with whatever the rotation happened to
/// owe.
const COUNTER_CHECK_EVERY: u64 = 5;

/// How many Markdown files the churn cycles through.
///
/// A bounded set that is created, modified and removed in rotation, so the
/// vault the watcher reconciles keeps changing without growing without bound.
const CHURN_FILES: u64 = 64;

/// Where the churned files sit, under the vault root.
const CHURN_DIR: &str = "soak-churn";

/// How much wall clock the run is given beyond the load itself.
///
/// It covers everything the child spends outside the declared duration, each
/// part carrying its own bound: attaching the ≥5k profile
/// ([`attach::READY_LIMIT`], 240s), the recovery that may still be in flight
/// when the duration runs out ([`RECOVERY_ATTEMPTS`] × [`RECOVERY_LIMIT`],
/// 180s), and the wait for the last write to reconcile ([`RECONCILE_LIMIT`],
/// 120s) — 540s in sum, so a child that reaches this is stuck rather than slow.
///
/// **A recovery inside the duration costs nothing here**: the load waits that
/// one out inside its own loop, churning and reading through it, so its ticks
/// are ticks of the declared duration. The term above is for the other
/// placement — a hiccup observed in the last stretch of the run, which the loop
/// keeps turning past the deadline to settle rather than ending the run on.
///
/// It is deliberately small enough that the child's deadline lands well inside
/// the job's own timeout. What the parent judges is the child's stdout, and it
/// reads that only once the child has ended, so a child the runner kills takes
/// every sample of the run with it.
const ATTACH_HEADROOM: Duration = Duration::from_secs(600);

/// How many times the load asks for an attachment that stopped serving again
/// before it calls the host unrecoverable.
///
/// Small on purpose: what the bar says is that the host kept serving under
/// load, and a run that needed many demands to stay attached is not that, even
/// if it eventually came back.
const RECOVERY_ATTEMPTS: u32 = 3;

/// The fewest ticks a recovery window may span.
///
/// **A window narrower than this proves nothing about starvation.** The
/// no-starvation term is charged over the window's wall clock, so a recovery
/// that landed inside a single tick would owe nothing and pass vacuously. This
/// is the floor that makes the term a measurement rather than an arithmetic
/// identity: two ticks is a window the cadence is charged over and the load had
/// to keep working across.
///
/// It is a count and not a clock — the window is read in ticks, and a tick is
/// the same unit of load on a slow runner as on a fast one. Two is grounded on
/// what the recovery has to do: the demand re-installs coverage over the ≥5k
/// profile, so the re-attach re-walks and content-hash heals that tree before
/// the entry reads ready, and the load samples itself once a second. Observed
/// at three ticks on a local macOS run at the default duration; the scheduled
/// Linux lane walks the same tree. Safe because the load's own bound on the
/// same window is [`RECOVERY_ATTEMPTS`] × [`RECOVERY_LIMIT`] above, two orders
/// of magnitude out: a window can only fail this by being *fast*, which is the
/// direction that makes the term vacuous. Review it if the recovery path stops
/// re-walking the tree, if the profile shrinks, or if [`SAMPLE_INTERVAL`] grows.
const RECOVERY_WINDOW_MINIMUM_TICKS: u64 = 2;

/// How much of a recovery window's wall clock the load is not charged the
/// cadence over.
///
/// One tick. The window's ends are read on tick boundaries and the doses are
/// owed per tick, so the charged span is the window minus the partial tick its
/// edges cost. Slack in the direction that makes a false failure impossible:
/// what the term is there to catch is a load that stopped for whole ticks.
const RECOVERY_WINDOW_SLACK: Duration = SAMPLE_INTERVAL;

/// How long one recovery attempt waits for the attachment to be ready again.
///
/// A bound on whether it came back at all: a re-attach that lands late still
/// says the host recovered, and how quickly it did is a clock this suite does
/// not read.
const RECOVERY_LIMIT: Duration = Duration::from_secs(60);

/// How long the load waits, once it ends, for the last thing it wrote to reach
/// the derived store.
///
/// A bound on whether the churn was reconciled at all, and nothing on how
/// quickly: a reconcile that lands late still says the watcher saw the write.
const RECONCILE_LIMIT: Duration = Duration::from_secs(120);

/// The fewest samples a judgment is made over.
///
/// The slope is a comparison of quartile means, so a series too short to have
/// four distinguishable quarters is a run that measured nothing.
const MINIMUM_SAMPLES: usize = 8;

/// **The soak lane's host workload**, and the harness it spawns.
///
/// The two roles are one case because the child selects it by name: with
/// [`HARNESS_ENV`] set this process is the child, and it runs the load instead
/// of judging one.
#[test]
#[ignore = "soak-lane case: runs in the nightly soak lane, not the workspace suite"]
fn a_long_mixed_load_grows_neither_memory_nor_descriptors() {
    if let Some(root) = std::env::var_os(HARNESS_ENV) {
        run_load(&attach::accepted_harness_root(&root, HARNESS_TOKEN_ENV));
        return;
    }

    baselines::assert_the_profile_the_bars_were_authored_on();
    let duration = declared_duration();
    let sandbox =
        Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), "host-soak").expect("a sandbox");
    let harness = sandbox
        .install_binary(&std::env::current_exe().expect("this suite's own executable"))
        .expect("installing the harness");
    let root: PathBuf = sandbox.work_dir().join("attached");
    attach::Vault::generate(&root, "soak");
    let token = attach::issue_harness_token(&root);
    let hits = arm_record_file(&sandbox);

    let outcome = Run::new(&sandbox, &harness)
        // `--ignored` is what makes the filter reach the case at all: the case
        // the child re-executes is ignored, and a plain run would skip it.
        .args(["--exact", HARNESS_CASE, "--ignored", "--nocapture"])
        .env(HARNESS_ENV, &root)
        .env(HARNESS_TOKEN_ENV, &token)
        .env(DURATION_ENV, duration.as_secs().to_string())
        // The dose: the child's first watch is armed at a stream that ends, so
        // the entry loses coverage once under the load and the demand the load
        // holds is what brings it back.
        .env(WATCH_ARMED_STAGES, format!("{ARMED_STAGE}={ARMED_ANSWER}"))
        .env(WATCH_ARMED_WATCHES, ARMED_WATCHES)
        .env(ARM_HITS, &hits)
        .deadline(duration + ATTACH_HEADROOM)
        .wait()
        .expect("running the load harness");
    // The counter-violation term is carried by the child: a warm request that
    // derived is a failed assertion inside the load, and the status is how it
    // reaches here.
    outcome.assert_success();
    // The slope is read off the tail of the series and the descriptor bar off
    // its last sample, so a stdout that was cut short is a judgment of a run's
    // beginning wearing the shape of a judgment of the run.
    assert!(
        !outcome.stdout_truncated,
        "the load wrote more than the capture limit, so the samples that decide this run are a \
         prefix of the ones it took"
    );

    let samples = Sample::parse_all(&outcome.stdout_text());
    let recoveries = reported_recoveries(&outcome.stdout_text());
    let attested = Attestation::read(&hits);
    // **The slope's series is the load at rest.** A recovery re-installs
    // coverage, and the walk that heals the ≥5k tree behind it is attach cost
    // landing inside the run — in the first quartile every time, because the
    // arm is owed to the first change the load churns. Left in, it inflates the
    // denominator of a ratio whose whole job is to bound a rise, so a bar that
    // never reddens would be the reading. The peak below keeps every sample:
    // what it is the height of is the run, recovery included.
    let settled = at_rest(&samples);
    assert!(
        settled.len() >= MINIMUM_SAMPLES,
        "the load reported {} samples outside its recovery windows, of {} in all, which is too \
         few to judge a slope over",
        settled.len(),
        samples.len()
    );

    let (first, last) = (
        samples.first().expect("a first sample"),
        samples.last().expect("a last sample"),
    );
    let head = quartile_mean(&settled, 0);
    let tail = quartile_mean(&settled, 3);
    let slope = baselines::per_mille(tail, head);
    let peak = peak_resident_set(&samples);
    let descriptor_growth = last.open_fds.saturating_sub(first.open_fds);

    baselines::record(
        "host mixed load",
        &[
            ("load duration (s)", duration.as_secs().to_string()),
            ("samples", samples.len().to_string()),
            ("samples outside a recovery", settled.len().to_string()),
            ("attachment recoveries", recoveries.to_string()),
            (
                "recovery dose",
                baselines::SOAK_RECOVERY_DOSE.to_string(),
            ),
            (
                "first quartile mean resident set (MiB)",
                baselines::mebibytes(head),
            ),
            (
                "last quartile mean resident set (MiB)",
                baselines::mebibytes(tail),
            ),
            ("peak resident set (MiB)", baselines::mebibytes(peak)),
            (
                "peak ceiling (MiB)",
                match baselines::SOAK_PEAK_RSS_CEILING_BYTES {
                    Some(ceiling) => baselines::mebibytes(ceiling),
                    None => "unauthored".to_string(),
                },
            ),
            ("observed slope", baselines::multiple(slope)),
            (
                "slope bar",
                baselines::multiple(baselines::SOAK_RSS_SLOPE_PER_MILLE),
            ),
            ("open descriptors, first sample", first.open_fds.to_string()),
            ("open descriptors, last sample", last.open_fds.to_string()),
            (
                "descriptor growth allowance",
                baselines::SOAK_FD_GROWTH_ALLOWANCE.to_string(),
            ),
        ],
    );

    // **Read after the summary is written, with the dose.** The summary is the
    // night's measurement product — the slope, the descriptor delta and the
    // peak — and the load ran an hour for it whether or not the arm fired. An
    // arm that missed is a bar that failed, and it fails below rather than
    // where it would take those readings with it.
    //
    // The arm's own record is read before the count it explains: a run that
    // recovered without it recovered from something this case did not arrange,
    // and the dose would then be met by a hiccup.
    attested.assert_reached(
        "the load's deliberate recovery",
        &[
            (SEAM, WATCH_SEAM),
            ("stage", ARMED_STAGE),
            ("answer", ARMED_ANSWER),
        ],
    );
    // Counted under this seam rather than over the whole file: a second arm the
    // child is given later is a different condition, and it would otherwise
    // read here as this one having fired twice.
    let firings = attested
        .hits()
        .iter()
        .filter(|hit| hit.get(SEAM) == Some(WATCH_SEAM))
        .count();
    assert_eq!(
        firings, 1,
        "the load's deliberate recovery fired {firings} times at {WATCH_SEAM}, where the arm is \
         budgeted to the one establishment its attach makes"
    );
    assert!(
        // The dose is a floor, so it is the reading and the run's count is the
        // ceiling: a run fits when its count reaches at least the dose.
        baselines::fits(baselines::SOAK_RECOVERY_DOSE, recoveries),
        "the load recovered {recoveries} times against a dose of {}, so the run's other readings \
         are of a load nothing ever disturbed",
        baselines::SOAK_RECOVERY_DOSE
    );
    assert!(
        baselines::fits(descriptor_growth, baselines::SOAK_FD_GROWTH_ALLOWANCE),
        "the load opened {descriptor_growth} descriptors it did not close, past an allowance of \
         {}: {} at the first sample and {} at the last",
        baselines::SOAK_FD_GROWTH_ALLOWANCE,
        first.open_fds,
        last.open_fds
    );
    assert!(
        head > 0,
        "the load reported no resident set at all, so the slope compares nothing"
    );
    if let Some(ceiling) = baselines::SOAK_PEAK_RSS_CEILING_BYTES {
        assert!(
            baselines::fits(peak, ceiling),
            "the load's resident set peaked at {} MiB, past the {} MiB ceiling, over {} samples \
             at the ≥5k profile",
            baselines::mebibytes(peak),
            baselines::mebibytes(ceiling),
            samples.len()
        );
    }
    assert!(
        baselines::fits(slope, baselines::SOAK_RSS_SLOPE_PER_MILLE),
        "the resident set rose by {}x across the load, past the {}x bar: the first quartile \
         averaged {} MiB and the last {} MiB over {} samples",
        baselines::multiple(slope),
        baselines::multiple(baselines::SOAK_RSS_SLOPE_PER_MILLE),
        baselines::mebibytes(head),
        baselines::mebibytes(tail),
        samples.len()
    );
}

/// One reading the child took of itself.
#[derive(Clone, Copy, Debug)]
struct Sample {
    rss_bytes: u64,
    open_fds: usize,
    /// Whether a recovery was in flight across the tick this was taken on.
    recovering: bool,
}

impl Sample {
    /// The sample lines in `report`, in the order the child printed them.
    ///
    /// A line is read by its keys rather than by position, and a line missing
    /// any of them is not a sample — the child's output also carries the test
    /// harness's own chatter, and reading that as a reading of zero would
    /// flatten a slope by adding samples nobody took.
    fn parse_all(report: &str) -> Vec<Sample> {
        report
            .lines()
            .filter(|line| line.starts_with("sample "))
            .filter_map(|line| {
                Some(Sample {
                    rss_bytes: field(line, "rss_bytes=")?.parse().ok()?,
                    open_fds: field(line, "open_fds=")?.parse().ok()?,
                    recovering: field(line, "recovering=")? == "1",
                })
            })
            .collect()
    }

    fn line(&self, elapsed: Duration) -> String {
        format!(
            "sample elapsed_ms={} rss_bytes={} open_fds={} recovering={}",
            elapsed.as_millis(),
            self.rss_bytes,
            self.open_fds,
            u8::from(self.recovering)
        )
    }
}

/// The value of `key` in a sample line, up to the next space.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let (_, rest) = line.split_once(key)?;
    Some(rest.split_whitespace().next().unwrap_or(rest))
}

/// How many times the load had to ask for its attachment again.
///
/// The child prints this once, after the loop it counts, so an absent line is a
/// load that never reached its end — which is a run to fail rather than a
/// reading of zero.
fn reported_recoveries(report: &str) -> u32 {
    let line = report
        .lines()
        .find(|line| line.starts_with(RECOVERY_LINE_PREFIX))
        .unwrap_or_else(|| {
            panic!(
                "the load printed no `{RECOVERY_LINE_PREFIX}` line, so it did not run its loop to \
                 the end and the samples above are of a load that stopped somewhere"
            )
        });
    field(line, "recoveries=")
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("`{line}` does not carry a recovery count"))
}

/// The samples the load took outside every recovery window.
///
/// **What the slope is read over.** A recovery re-installs coverage and the
/// walk that heals the tree behind it is attach cost, which is a different
/// subject from the steady load whose trend the slope bounds — and it lands in
/// the first quartile by construction, where it would depress the ratio the
/// slope compares. The peak is read over the whole series instead, because the
/// height the process reached is the height it reached.
fn at_rest(samples: &[Sample]) -> Vec<Sample> {
    samples
        .iter()
        .filter(|sample| !sample.recovering)
        .copied()
        .collect()
}

/// The highest resident set the series holds.
///
/// **The peak term of the memory invariant at this profile.** The slope reads
/// the series' trend and the quartile means it compares hide a spike between
/// them, so the height the load ever reached is a separate reading — and it is
/// the maximum of samples already taken rather than a second instrument.
///
/// One thing it is not: the samples are of the current resident set on a
/// one-second cadence, so this is the highest sampled value and not the
/// kernel's high-water mark — an allocation that lands and is released between
/// two ticks is not in it.
///
/// **The first attach is outside the series and the re-attach is inside it.**
/// The series begins once the attachment reads ready, so the cost of the attach
/// the load starts with is before the first sample. The deliberate recovery's
/// re-attach is not: it walks and content-hash heals the same ≥5k tree from
/// inside the run, and every sample it spans is in this maximum. So what the
/// peak is the height of is the load *and* one re-attach over the profile —
/// which is the whole series, by the same rule as before, now that the series
/// holds one.
fn peak_resident_set(samples: &[Sample]) -> u64 {
    samples
        .iter()
        .map(|sample| sample.rss_bytes)
        .max()
        .expect("a series judged here holds samples")
}

/// The mean resident set of one quarter of the series, indexed from zero.
///
/// Quartile means rather than endpoints: one sample taken while a changeset
/// commits is a spike, and a comparison of two single readings would let it
/// decide the run.
///
/// The last quartile ends at the last sample rather than at a multiple of the
/// quarter's width. A series whose length is not a multiple of four otherwise
/// leaves its final one to three samples out of the slope while the descriptor
/// bar reads the last of them, and the two bars judge different windows of the
/// same run.
fn quartile_mean(samples: &[Sample], quartile: usize) -> u64 {
    let size = samples.len() / 4;
    let start = if quartile == 3 {
        samples.len() - size
    } else {
        quartile * size
    };
    let slice = &samples[start..start + size];
    let total: u64 = slice.iter().map(|sample| sample.rss_bytes).sum();
    total / slice.len() as u64
}

/// How long the load runs, as the environment declares it.
fn declared_duration() -> Duration {
    let Some(declared) = std::env::var_os(DURATION_ENV) else {
        return DEFAULT_DURATION;
    };
    let declared = declared.to_string_lossy().trim().to_string();
    let seconds: u64 = declared.parse().unwrap_or_else(|_| {
        panic!("{DURATION_ENV} is a number of seconds, and reads `{declared}`")
    });
    assert!(seconds > 0, "{DURATION_ENV} is zero, so there is no load");
    Duration::from_secs(seconds)
}

/// The file the child's arms record themselves in, empty before the run.
///
/// **It is made here rather than left to the arm.** An arm appends, and
/// [`Attestation::read`] answers the same for a file nothing wrote and a file
/// that is not there — so a record file the child could never have opened would
/// read exactly like a boundary the watcher never reached. A file the child
/// names and cannot write ends that process saying so, which is the failure
/// this parent wants instead.
fn arm_record_file(sandbox: &Sandbox) -> PathBuf {
    let directory = sandbox.work_dir().join(ARM_RECORDS_DIR);
    std::fs::create_dir_all(&directory).expect("a directory for the arm records");
    let hits = directory.join("arm-hits");
    std::fs::write(&hits, b"").expect("an empty arm-hit record");
    hits
}

/// What the child prints once its loop ends, and what the parent reads the
/// recovery count off.
const RECOVERY_LINE_PREFIX: &str = "load ";

/// The harness: attach the tree at `root`, work it until the deadline, and
/// report a sample of this process on every tick.
#[allow(clippy::disallowed_macros)] // The child's samples are a machine-consumed stream its parent reads.
fn run_load(root: &Path) {
    let duration = declared_duration();
    // The lease below is what keeps the attachment: an entry nothing demands is
    // reaped once the idle interval passes. The interval is the second guard
    // behind it, and an interval inside the load's own duration would make a
    // future edit that stopped holding the lease read as a host that stopped
    // serving rather than as the policy it is.
    assert!(
        attach::IDLE_AFTER > duration,
        "the load runs for {duration:?} against an idle interval of {:?}, so an attachment nothing \
         demanded would be reaped part-way through it",
        attach::IDLE_AFTER
    );

    let vault = attach::Vault::adopt(root);
    let host = vault.host();
    // Held for the whole load, not dropped at ready: demand is what the reaper
    // counts, and it is also the only thing that re-attaches an entry that went
    // untrusted.
    let mut lease = attach::attach_and_wait(&host, vault.name());

    let mut store = vault.store();
    let subject = a_derived_path(&mut store);
    std::fs::create_dir_all(vault.path().join(CHURN_DIR)).expect("create the churn directory");

    let started = Instant::now();
    let deadline = started + duration;
    let mut tick = 0u64;
    let mut recoveries = 0u32;
    let mut recovering: Option<Recovering> = None;
    loop {
        // The state is read first, so a recovery that begins on this tick has
        // the churn turn and the warm read below inside its own window: what
        // the load kept doing while it recovered is counted over whole ticks.
        let observed = probed_state(&host, vault.name());
        let demanded = recovering.is_none() && !serving(&observed);
        if demanded {
            recoveries += 1;
            recovering = Some(Recovering::demanded(&host, vault.name(), tick, &observed));
        }

        let written = churn(vault.path(), tick);
        if let Some(flight) = recovering.as_mut() {
            flight.churn_turns += 1;
        }

        // The cadence, and one request on the tick a recovery is demanded on:
        // a window the cadence does not fall inside would otherwise open with
        // no request against it at all.
        if demanded || tick.is_multiple_of(COUNTER_CHECK_EVERY) {
            assert_warm_reads_derive_nothing(&mut store, &subject);
            if let Some(flight) = recovering.as_mut() {
                flight.warm_reads += 1;
            }
        }

        let sample = Sample {
            rss_bytes: current_rss_bytes(),
            open_fds: open_fd_count().expect("this process's descriptor count"),
            // The tick is inside a recovery window when one is outstanding
            // across it, settled or not: the re-attach's walk is work this
            // sample holds, and the slope's series is read without it.
            recovering: recovering.is_some(),
        };
        println!("{}", sample.line(started.elapsed()));

        if let Some(flight) = recovering.take() {
            match flight.settled(&host, vault.name(), tick) {
                Settled::Serving(fresh) => lease = fresh,
                Settled::Waiting(flight) => recovering = Some(flight),
            }
        }

        // **A recovery still in flight keeps the loop turning.** The bar is
        // that the host kept serving under load, not that nothing ever
        // hiccuped, so a demand raised in the last stretch of the run is waited
        // out the way one raised anywhere else is — churning and reading
        // through it — rather than ending the run. What bounds the overrun is
        // the recovery's own [`RECOVERY_ATTEMPTS`] × [`RECOVERY_LIMIT`], which
        // [`ATTACH_HEADROOM`] covers; a host that will not come back fails
        // there, saying so.
        if Instant::now() >= deadline && recovering.is_none() {
            assert_the_churn_reached_the_store(&mut store, &written);
            drop(lease);
            println!("{RECOVERY_LINE_PREFIX}ticks={tick} recoveries={recoveries}");
            return;
        }
        // Semantic: the interval is the sampling cadence. What the run
        // measures is a slope over samples taken at a fixed spacing, so this
        // sleep is the spacing itself and no condition ends it early.
        std::thread::sleep(SAMPLE_INTERVAL);
        tick += 1;
    }
}

/// Whether what the host answered is one it serves requests in: ready, or
/// healing toward it.
///
/// A refusal is never serving. The states this load has to survive — a watcher
/// overflow, coverage lost — cross as envelopes rather than as labels, so they
/// are read here as the `Err` they are and met with a fresh demand below.
fn serving(state: &Result<TrustState, ErrorEnvelope>) -> bool {
    matches!(state, Ok(TrustState::Ready | TrustState::Warming { .. }))
}

/// **A recovery the load is waiting out, and what the load went on serving
/// while it waited.**
///
/// The demand is asked for once and the loop keeps turning: the load churns,
/// takes its warm read-only requests and samples itself on every tick of the
/// wait, and the counts below are what those ticks did. A recovery a load
/// blocked on would satisfy every other bar in this suite while serving
/// nothing, which is the outcome the two counts rule out.
///
/// The caller's lease outlives this, so the entry's demand count never reaches
/// zero while a recovery is in flight and the reaper never sees an entry with
/// nothing demanding it.
struct Recovering {
    /// The demand the current attempt asked under, handed back to the load when
    /// the entry serves again.
    lease: DemandLease<ProductionEntryOps>,
    /// When the load stopped being served, which is where the window the
    /// no-starvation term is charged over begins.
    began_at: Instant,
    /// When the current attempt asked.
    asked: Instant,
    /// How many attempts have been spent, the current one included.
    attempts: u32,
    /// The tick the recovery began on, which is the first tick of the window
    /// the counts below cover.
    began: u64,
    /// What the entry read when the load stopped being served, for the failure
    /// message.
    withdrawn: Result<TrustState, ErrorEnvelope>,
    /// Churn turns the load took over the window, one per tick.
    churn_turns: u64,
    /// Warm read-only requests the load took over the window.
    warm_reads: u64,
}

/// What a look at an entry a recovery was demanded for left.
enum Settled {
    /// It serves the vault again, under this demand.
    Serving(DemandLease<ProductionEntryOps>),
    /// It has not come back yet, and the wait stands.
    Waiting(Recovering),
}

impl Recovering {
    /// Ask for an attachment that stopped serving again, and begin the window.
    ///
    /// **The bar is that the host kept serving under load, not that nothing
    /// ever hiccuped.** A watcher failure leaves the entry untrusted, and a
    /// demand is the only thing that re-attaches one — a load that never
    /// demands again would sit beside an untrusted entry for the rest of the
    /// run.
    fn demanded(
        host: &Host<ProductionEntryOps>,
        name: &VaultName,
        tick: u64,
        observed: &Result<TrustState, ErrorEnvelope>,
    ) -> Recovering {
        Recovering {
            lease: host
                .retry(name, AttachMode::Durable)
                .expect("re-requesting the attachment"),
            began_at: Instant::now(),
            asked: Instant::now(),
            attempts: 1,
            began: tick,
            withdrawn: observed.clone(),
            churn_turns: 0,
            warm_reads: 0,
        }
    }

    /// Where this recovery stands at the end of `tick`.
    ///
    /// An attempt has [`RECOVERY_LIMIT`] to bring the entry back. One that
    /// elapses is an attempt spent rather than the run failing, and a run that
    /// spends [`RECOVERY_ATTEMPTS`] of them is a host that will not come back.
    ///
    /// A recovery that came back prints what the load did across it, which is
    /// the window's own line in the stream its parent reads.
    #[allow(clippy::disallowed_macros)] // The child's report is a machine-consumed stream its parent reads.
    fn settled(mut self, host: &Host<ProductionEntryOps>, name: &VaultName, tick: u64) -> Settled {
        let observed = probed_state(host, name);
        assert!(
            !names_no_vault(&observed),
            "the host serves no vault under `{name}`: {observed:?}"
        );
        if observed == Ok(TrustState::Ready) {
            let window = self.began_at.elapsed();
            self.assert_the_load_was_served_across_it(tick, window);
            println!(
                "recovery began_tick={} ticks={} attempts={} churn_turns={} warm_reads={} \
                 window_ms={}",
                self.began,
                tick - self.began + 1,
                self.attempts,
                self.churn_turns,
                self.warm_reads,
                window.as_millis()
            );
            return Settled::Serving(self.lease);
        }
        if self.asked.elapsed() <= RECOVERY_LIMIT {
            return Settled::Waiting(self);
        }
        assert!(
            self.attempts < RECOVERY_ATTEMPTS,
            "the attachment stopped serving under load and {RECOVERY_ATTEMPTS} fresh demands did \
             not bring it back inside {RECOVERY_LIMIT:?} each: it read {:?} and now reads \
             {observed:?}",
            self.withdrawn
        );
        // The fresh demand is taken before the spent one is let go, so the
        // entry's demand count never passes through zero here.
        self.lease = host
            .retry(name, AttachMode::Durable)
            .expect("re-requesting the attachment");
        self.asked = Instant::now();
        self.attempts += 1;
        Settled::Waiting(self)
    }

    /// **The no-starvation term, charged over wall clock.** The stretch between
    /// the load losing its service and getting it back is real time, and over
    /// that time the load kept churning and kept taking warm read-only requests
    /// at the doses its cadence owes for it.
    ///
    /// **Wall clock and not ticks.** A tick count compared against the loop's
    /// own iterations is an identity: a load that made no progress advances no
    /// tick, so a window of three ticks that took half a minute reads the same
    /// as one that took three seconds, and starvation is exactly the thing that
    /// is invisible. What is asked here instead is the rate — for a window of
    /// `w` seconds the cadence owes `w` churn turns and `w /
    /// COUNTER_CHECK_EVERY` warm reads, and the load must have landed at least
    /// that many.
    ///
    /// One tick of the window is not charged ([`RECOVERY_WINDOW_SLACK`]): the
    /// window's ends are read on tick boundaries, so its edges hold a partial
    /// tick the cadence never owed a dose over. The comparison is a floor and
    /// not an equality for the same reason — a load doing more than its cadence
    /// owes is not starving.
    ///
    /// The window is also required to span [`RECOVERY_WINDOW_MINIMUM_TICKS`],
    /// so that a recovery landing inside a single tick is reported as the
    /// window that owed nothing rather than passing this vacuously.
    ///
    /// What this forbids is a load that recovers by stopping — every other bar
    /// in this suite is satisfied by a process that sat still.
    fn assert_the_load_was_served_across_it(&self, tick: u64, window: Duration) {
        let ticks = tick - self.began + 1;
        assert!(
            ticks >= RECOVERY_WINDOW_MINIMUM_TICKS,
            "a recovery came back inside {ticks} tick(s), under the \
             {RECOVERY_WINDOW_MINIMUM_TICKS} a window has to span for the load's doses over it to \
             owe anything, so nothing here was measured about starving"
        );

        let charged = window.saturating_sub(RECOVERY_WINDOW_SLACK);
        let owed_churn = doses_over(charged, SAMPLE_INTERVAL);
        assert!(
            self.churn_turns >= owed_churn,
            "the load took {} churn turns across the {window:?} a recovery was in flight for, \
             where its cadence owes {owed_churn} over that stretch, so the vault stopped changing \
             while the host came back",
            self.churn_turns
        );

        let owed_reads = doses_over(charged, SAMPLE_INTERVAL * COUNTER_CHECK_EVERY as u32);
        assert!(
            self.warm_reads >= owed_reads,
            "the load took {} warm read-only requests across the {window:?} a recovery was in \
             flight for, where its cadence owes {owed_reads} over that stretch, so it stopped \
             taking requests while the host came back",
            self.warm_reads
        );
    }
}

/// How many doses a cadence of one per `every` owes over `span`.
///
/// Whole doses only: a cadence owes its next one when the interval has passed,
/// not part-way through it.
fn doses_over(span: Duration, every: Duration) -> u64 {
    (span.as_millis() / every.as_millis()) as u64
}

/// One look at what the entry under `name` publishes, bounded.
///
/// A look is a lock and a label, and [`attach::STATE_PROBE`] is what the whole
/// crate separates a host that has stopped answering from a condition that has
/// not arrived with. The load reads the state on every tick and again on every
/// settling look, outside any wait, so the bound is applied here: a probe that
/// overran is not a recovery taking its time, and no number of fresh demands
/// answers it. It ends the run where it is found.
fn probed_state(
    host: &Host<ProductionEntryOps>,
    name: &VaultName,
) -> Result<TrustState, ErrorEnvelope> {
    let began = Instant::now();
    let observed = host.state(name);
    let took = began.elapsed();
    assert!(
        took <= attach::STATE_PROBE,
        "one look at what `{name}` publishes cost {took:?}, past the {:?} a look at a published \
         label may take: the host has stopped answering rather than the entry not being back",
        attach::STATE_PROBE
    );
    observed
}

/// Whether what the host answered is the refusal a name it holds no entry under
/// is refused with.
///
/// A wait polls an entry on its way somewhere. A name no entry stands behind is
/// a mistake in the case rather than a state converging, and it converges on
/// nothing, so it ends the wait where it is found instead of at the deadline.
fn names_no_vault(observed: &Result<TrustState, ErrorEnvelope>) -> bool {
    matches!(observed, Err(envelope) if envelope.code() == &ReasonCode::HostUnknownVault)
}

/// **The load is only a load if the vault it churns is being reconciled.**
///
/// The last document the churn wrote is asked for by path *and* by content: a
/// run whose writes the watcher never saw would sample a host that sat idle,
/// and a flat memory slope over an idle process says nothing about a leak. The
/// churn writes a bounded rotating set of names, so a row left by an earlier
/// turn of the rotation answers the path on its own — comparing the stored
/// content hash against the bytes the last tick wrote is what binds the proof
/// to that write. The wait is bounded and generous, because how quickly a
/// reconcile lands is a clock this suite does not read.
fn assert_the_churn_reached_the_store(store: &mut Store, written: &Written) {
    let expected = ContentHash::of(written.content.as_bytes()).to_hex();
    wait_until(
        &format!(
            "a reconcile to derive the bytes the churn last wrote to {}",
            written.path.as_str()
        ),
        // One look opens a read request against the derived store, which is
        // more than a label read and is bounded as such.
        Budget::new(RECONCILE_LIMIT, STORE_READ_PROBE),
        || {
            let derived = store
                .begin_request()
                .stored_document(&written.path)
                .expect("reading a document")
                .map(|document| document.content_hash);
            if derived.as_deref() == Some(expected.as_str()) {
                Observed::Met(())
            } else {
                Observed::pending(format!(
                    "the store holds {derived:?} against the {expected} that write hashes to"
                ))
            }
        },
    )
    .unwrap_or_else(|failure| {
        panic!("the load ran against a host that saw nothing: {failure}");
    });
}

/// **The counter-violation term.** A request that only reads derives nothing,
/// however long the load beside it has been running.
fn assert_warm_reads_derive_nothing(store: &mut Store, subject: &DocumentPath) {
    let probe = class_probe(subject.stem()).expect("a class stem off a derived path");
    let mut warm = store.begin_request();
    let _ = warm.stored_document(subject).expect("reading a document");
    let _ = warm.stored_facts(subject).expect("reading facts");
    let _ = warm.stored_tombstone(subject).expect("reading a tombstone");
    let _ = warm.stored_findings(subject).expect("reading findings");
    let _ = warm.findings_in_class(&probe).expect("reading a class");
    let _ = warm.suffix_candidates(&probe).expect("reading candidates");
    let _ = warm
        .emitted_plan(ExplainedStatement::SuffixCandidates(&probe))
        .expect("a query plan");
    let _ = warm.vault_schema_pin().expect("reading the pin");

    let reading = warm.finish();
    let moved: Vec<(&str, u64)> = reading
        .readings()
        .filter(|(_, value)| *value != 0)
        .collect();
    assert!(
        moved.is_empty(),
        "a warm request under load moved derivation counters: {moved:?}"
    );
}

/// The vault-relative path the churn writes on tick `n`.
fn churn_name(n: u64) -> String {
    format!("{CHURN_DIR}/note-{:04}.md", n % CHURN_FILES)
}

/// One document the attachment derived, which the warm reads ask about.
fn a_derived_path(store: &mut Store) -> DocumentPath {
    let request = store.begin_request();
    let page = request
        .stored_documents_after_ordered(None, 1, StoredPathOrder::Sensitive)
        .expect("reading a page of derived documents");
    page.into_iter()
        .next()
        .expect("an attachment over a generated tree derives documents")
        .path
}

/// What one turn of the churn left at a path, which is what the reconcile proof
/// asks the store for.
struct Written {
    path: DocumentPath,
    content: String,
}

/// One turn of the file churn: a document created, the one before it modified,
/// and the one before that removed. What comes back is the creation, which is
/// the newest write of the turn.
///
/// All three event kinds every tick, over a bounded rotating set, so the
/// watcher reconciles creations, modifications and deaths for as long as the
/// load runs without the vault growing without bound.
fn churn(vault: &Path, tick: u64) -> Written {
    let at = |n: u64| vault.join(churn_name(n));
    let content = format!("# Churn {tick}\n\nA body.\n");
    std::fs::write(at(tick), &content).expect("write a churn file");
    if tick >= 1 {
        std::fs::write(
            at(tick - 1),
            format!("# Churn {tick} again\n\nA longer body, rewritten.\n"),
        )
        .expect("rewrite a churn file");
    }
    if tick >= 2 {
        // The rotation removes a file a later tick recreates, so a removal
        // finding nothing is the rotation working rather than a failure.
        let _ = std::fs::remove_file(at(tick - 2));
    }
    Written {
        path: DocumentPath::new(&churn_name(tick)).expect("a document path"),
        content,
    }
}

/// This process's resident set, in bytes, as the kernel reports it now.
///
/// Two platforms, two sources, and the reading is the same quantity either
/// way: Linux publishes it in `/proc/self/status`, and macOS has no `/proc`, so
/// the accounting comes from `ps`, which reads the same field the kernel keeps.
/// A peak would not do here — the peak of a run never falls, so a slope over it
/// is flat whatever the subject did.
fn current_rss_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").expect("this process's status");
        let line = status
            .lines()
            .find(|line| line.starts_with("VmRSS:"))
            .expect("a resident-set line in this process's status");
        let kibibytes: u64 = line
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| panic!("`{line}` does not carry a resident set"));
        kibibytes * 1024
    }
    #[cfg(target_os = "macos")]
    {
        let reported = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p"])
            .arg(std::process::id().to_string())
            .output()
            .expect("asking for this process's resident set");
        let text = String::from_utf8_lossy(&reported.stdout);
        let kibibytes: u64 = text
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("`{text}` is not a resident set in kibibytes"));
        kibibytes * 1024
    }
}
