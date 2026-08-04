//! The scheduled soak lane's host workload: a long mixed load over an attached
//! vault, and the two shapes a leak takes.
//!
//! **Lockdown is counted over a nightly mixed load showing three things: zero
//! counter violations, a flat memory slope, and no file-descriptor growth.**
//! This case is that load and those three assertions. A load that runs for an
//! hour is how a cost paid once per reconciliation becomes visible: a
//! descriptor a subscription never closes, a buffer a heal never releases, and
//! a warm read that starts deriving all show as a trend rather than as a
//! failure at any one instant.
//!
//! # The composition
//!
//! The parent generates the ≥5k-document `soak` profile's tree and spawns a
//! child — this test binary re-executed in a harness mode an environment
//! variable selects. The child attaches the tree through a production host and
//! then works it: it churns Markdown files in the vault so the watcher has
//! something to reconcile, polls the attachment's trust state, and takes warm
//! read-only requests against the derived store, asserting that every one of
//! them finishes with its derivation counters at zero. Alongside the load it
//! samples its own resident set and open-descriptor count on a fixed cadence
//! and prints each sample, which is what the parent judges.
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
//! descriptor count and a ratio between two means of a sampled series, and both
//! say the same thing on a slow runner as on a fast one. `NORN_SOAK_DURATION_SECS`
//! is how long the load runs, defaulting low enough that a local `--ignored`
//! run is usable; the workflow passes an hour.
//!
//! Running this needs `/proc` or its BSD equivalent, so the case is present on
//! Linux and macOS and absent elsewhere. The scheduled lane runs it on Linux.
#![cfg(any(target_os = "linux", target_os = "macos"))]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree and its own accounting.

mod attach;
mod baselines;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use norn_store::{DocumentPath, ExplainedStatement, Store, class_probe};
use norn_testkit::process::{Run, Sandbox, open_fd_count};
use norn_wire::TrustState;

/// The variable that puts this binary in harness mode, carrying the root the
/// generated tree sits under.
const HARNESS_ENV: &str = "NORN_HOST_SOAK_HARNESS";

/// The case the child re-executes, which is the one that reads this constant.
const HARNESS_CASE: &str = "a_long_mixed_load_grows_neither_memory_nor_descriptors";

/// How long the load runs, in seconds.
///
/// The default is short enough that running this suite locally is a normal
/// thing to do; the scheduled lane names an hour in the workflow step.
const DURATION_ENV: &str = "NORN_SOAK_DURATION_SECS";
const DEFAULT_DURATION: Duration = Duration::from_secs(90);

/// How often the child takes a sample of itself.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// How often a sample is preceded by a warm read-only request, in samples.
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
/// It covers generating nothing — the parent did that — and attaching the ≥5k
/// profile, which is the only unbounded-looking part of the child's run. A
/// child that reaches this is stuck rather than slow.
const ATTACH_HEADROOM: Duration = Duration::from_secs(900);

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
        run_load(Path::new(&root));
        return;
    }

    let duration = declared_duration();
    let sandbox =
        Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), "host-soak").expect("a sandbox");
    let harness = sandbox
        .install_binary(&std::env::current_exe().expect("this suite's own executable"))
        .expect("installing the harness");
    let root: PathBuf = sandbox.work_dir().join("attached");
    attach::Vault::generate(&root, "soak");

    let outcome = Run::new(&sandbox, &harness)
        // `--ignored` is what makes the filter reach the case at all: the case
        // the child re-executes is ignored, and a plain run would skip it.
        .args(["--exact", HARNESS_CASE, "--ignored", "--nocapture"])
        .env(HARNESS_ENV, &root)
        .env(DURATION_ENV, duration.as_secs().to_string())
        .deadline(duration + ATTACH_HEADROOM)
        .wait()
        .expect("running the load harness");
    // The counter-violation term is carried by the child: a warm request that
    // derived is a failed assertion inside the load, and the status is how it
    // reaches here.
    outcome.assert_success();

    let samples = Sample::parse_all(&outcome.stdout_text());
    assert!(
        samples.len() >= MINIMUM_SAMPLES,
        "the load reported {} samples, which is too few to judge a slope over",
        samples.len()
    );

    let (first, last) = (
        samples.first().expect("a first sample"),
        samples.last().expect("a last sample"),
    );
    let head = quartile_mean(&samples, 0);
    let tail = quartile_mean(&samples, 3);
    let slope = baselines::per_mille(tail, head);
    let descriptor_growth = last.open_fds.saturating_sub(first.open_fds);

    baselines::record(
        "host mixed load",
        &[
            ("load duration (s)", duration.as_secs().to_string()),
            ("samples", samples.len().to_string()),
            (
                "first quartile mean resident set (MiB)",
                baselines::mebibytes(head),
            ),
            (
                "last quartile mean resident set (MiB)",
                baselines::mebibytes(tail),
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

    assert!(
        descriptor_growth <= baselines::SOAK_FD_GROWTH_ALLOWANCE,
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
    assert!(
        slope <= baselines::SOAK_RSS_SLOPE_PER_MILLE,
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
}

impl Sample {
    /// The sample lines in `report`, in the order the child printed them.
    ///
    /// A line is read by its keys rather than by position, and a line missing
    /// either key is not a sample — the child's output also carries the test
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
                })
            })
            .collect()
    }

    fn line(&self, elapsed: Duration) -> String {
        format!(
            "sample elapsed_ms={} rss_bytes={} open_fds={}",
            elapsed.as_millis(),
            self.rss_bytes,
            self.open_fds
        )
    }
}

/// The value of `key` in a sample line, up to the next space.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let (_, rest) = line.split_once(key)?;
    Some(rest.split_whitespace().next().unwrap_or(rest))
}

/// The mean resident set of one quarter of the series, indexed from zero.
///
/// Quartile means rather than endpoints: one sample taken while a changeset
/// commits is a spike, and a comparison of two single readings would let it
/// decide the run.
fn quartile_mean(samples: &[Sample], quartile: usize) -> u64 {
    let size = samples.len() / 4;
    let start = quartile * size;
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

/// The harness: attach the tree at `root`, work it until the deadline, and
/// report a sample of this process on every tick.
#[allow(clippy::disallowed_macros)] // The child's samples are a machine-consumed stream its parent reads.
fn run_load(root: &Path) {
    let vault = attach::Vault::adopt(root);
    let host = vault.host();
    attach::attach_and_wait(&host, vault.name());

    let mut store = vault.store();
    let subject = a_derived_path(&mut store);
    std::fs::create_dir_all(vault.path().join(CHURN_DIR)).expect("create the churn directory");

    let started = Instant::now();
    let deadline = started + declared_duration();
    let mut tick = 0u64;
    loop {
        churn(vault.path(), tick);
        // Attached and answering: ready, or healing what the churn just
        // changed. A poll taken between a write and the reconcile it triggers
        // legitimately sees warming, and only losing the attachment or the
        // trust in it is a load the host stopped serving.
        let observed = host.state(vault.name()).expect("registered vault state");
        assert!(
            matches!(observed, TrustState::Ready | TrustState::Warming { .. }),
            "the attachment stopped serving under load: {observed:?}"
        );
        if tick.is_multiple_of(COUNTER_CHECK_EVERY) {
            assert_warm_reads_derive_nothing(&mut store, &subject);
        }

        let sample = Sample {
            rss_bytes: current_rss_bytes(),
            open_fds: open_fd_count().expect("this process's descriptor count"),
        };
        println!("{}", sample.line(started.elapsed()));

        if Instant::now() >= deadline {
            assert_the_churn_reached_the_store(&mut store, tick);
            return;
        }
        std::thread::sleep(SAMPLE_INTERVAL);
        tick += 1;
    }
}

/// **The load is only a load if the vault it churns is being reconciled.**
///
/// The last document the churn wrote is asked for by path: a run whose writes
/// the watcher never saw would sample a host that sat idle, and a flat memory
/// slope over an idle process says nothing about a leak. The wait is bounded
/// and generous, because how quickly a reconcile lands is a clock this suite
/// does not read.
fn assert_the_churn_reached_the_store(store: &mut Store, tick: u64) {
    let path = DocumentPath::new(&churn_name(tick)).expect("a document path");
    let deadline = Instant::now() + RECONCILE_LIMIT;
    loop {
        let found = store
            .begin_request()
            .stored_document(&path)
            .expect("reading a document")
            .is_some();
        if found {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the churn wrote {} and no reconcile derived it inside {RECONCILE_LIMIT:?}, so the \
             load ran against a host that saw nothing",
            path.as_str()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
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
        .stored_documents_after(None, 1)
        .expect("reading a page of derived documents");
    page.into_iter()
        .next()
        .expect("an attachment over a generated tree derives documents")
        .path
}

/// One turn of the file churn: a document created, the one before it modified,
/// and the one before that removed.
///
/// All three event kinds every tick, over a bounded rotating set, so the
/// watcher reconciles creations, modifications and deaths for as long as the
/// load runs without the vault growing without bound.
fn churn(vault: &Path, tick: u64) {
    let at = |n: u64| vault.join(churn_name(n));
    std::fs::write(at(tick), format!("# Churn {tick}\n\nA body.\n")).expect("write a churn file");
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
