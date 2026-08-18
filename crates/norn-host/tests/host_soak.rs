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
//! that nothing ever hiccuped; how many recoveries a run needed is a recorded
//! reading.
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
//! ceiling is authored.** [`baselines::SOAK_PEAK_RSS_CEILING_BYTES`] is `None`
//! until calibration runs of the scheduled lane produce readings to author one
//! from, and the reading lands in the run's table meanwhile — a recorded
//! measurement with no bar over it, which is the state a bar is authored out
//! of.
//!
//! Running this needs `/proc` or its BSD equivalent, so the case is present on
//! Linux and macOS and absent elsewhere. The scheduled lane runs it on Linux.
#![cfg(any(target_os = "linux", target_os = "macos"))]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree and its own accounting.

mod attach;
mod baselines;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use norn_fs::ContentHash;
use norn_host::{AttachMode, DemandLease, Host, ProductionEntryOps};
use norn_store::{DocumentPath, ExplainedStatement, Store, StoredPathOrder, class_probe};
use norn_testkit::process::{Run, Sandbox, open_fd_count};
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
/// It covers everything the child spends outside the declared duration, each
/// part carrying its own bound: attaching the ≥5k profile
/// ([`attach::READY_LIMIT`], 240s), every recovery the load may attempt
/// ([`RECOVERY_ATTEMPTS`] × [`RECOVERY_LIMIT`], 180s), and the wait for the
/// last write to reconcile ([`RECONCILE_LIMIT`], 120s) — 540s in sum, so a
/// child that reaches this is stuck rather than slow.
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

    let outcome = Run::new(&sandbox, &harness)
        // `--ignored` is what makes the filter reach the case at all: the case
        // the child re-executes is ignored, and a plain run would skip it.
        .args(["--exact", HARNESS_CASE, "--ignored", "--nocapture"])
        .env(HARNESS_ENV, &root)
        .env(HARNESS_TOKEN_ENV, &token)
        .env(DURATION_ENV, duration.as_secs().to_string())
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
    let peak = peak_resident_set(&samples);
    let descriptor_growth = last.open_fds.saturating_sub(first.open_fds);

    baselines::record(
        "host mixed load",
        &[
            ("load duration (s)", duration.as_secs().to_string()),
            ("samples", samples.len().to_string()),
            ("attachment recoveries", recoveries.to_string()),
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
    if let Some(ceiling) = baselines::SOAK_PEAK_RSS_CEILING_BYTES {
        assert!(
            peak <= ceiling,
            "the load's resident set peaked at {} MiB, past the {} MiB ceiling, over {} samples \
             at the ≥5k profile",
            baselines::mebibytes(peak),
            baselines::mebibytes(ceiling),
            samples.len()
        );
    }
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

/// The highest resident set the series holds.
///
/// **The peak term of the memory invariant at this profile.** The slope reads
/// the series' trend and the quartile means it compares hide a spike between
/// them, so the height the load ever reached is a separate reading — and it is
/// the maximum of samples already taken rather than a second instrument.
///
/// The samples are of the current resident set on a one-second cadence, so this
/// is the highest sampled value and not the kernel's high-water mark: an
/// allocation that lands and is released between two ticks is not in it. What
/// the reading answers for is what the load sustains, which is the quantity a
/// ceiling at this profile is about.
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
    loop {
        let written = churn(vault.path(), tick);
        // Attached and answering: ready, or healing what the churn just
        // changed. A poll taken between a write and the reconcile it triggers
        // legitimately sees warming, and anything else is an attachment that
        // needs demanding again before it serves.
        let observed = host.state(vault.name());
        if !serving(&observed) {
            recoveries += 1;
            lease = recovered(&host, vault.name(), &observed);
        }
        if tick.is_multiple_of(COUNTER_CHECK_EVERY) {
            assert_warm_reads_derive_nothing(&mut store, &subject);
        }

        let sample = Sample {
            rss_bytes: current_rss_bytes(),
            open_fds: open_fd_count().expect("this process's descriptor count"),
        };
        println!("{}", sample.line(started.elapsed()));

        if Instant::now() >= deadline {
            assert_the_churn_reached_the_store(&mut store, &written);
            drop(lease);
            println!("{RECOVERY_LINE_PREFIX}ticks={tick} recoveries={recoveries}");
            return;
        }
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

/// Ask for an attachment that stopped serving again, and wait for it to come
/// back.
///
/// **The bar is that the host kept serving under load, not that nothing ever
/// hiccuped.** A watcher overflow leaves the entry untrusted, and a demand is
/// the only thing that re-attaches one — a load that never demands again would
/// sit beside an untrusted entry for the rest of the run. So a state outside
/// ready-or-warming is met with a bounded number of fresh demands, and only a
/// host that will not come back fails the load.
///
/// The caller's lease outlives this call, so the entry's demand count never
/// reaches zero while a recovery is in flight and the reaper never sees an
/// entry with nothing demanding it.
fn recovered(
    host: &Host<ProductionEntryOps>,
    name: &VaultName,
    observed: &Result<TrustState, ErrorEnvelope>,
) -> DemandLease<ProductionEntryOps> {
    let mut last = observed.clone();
    for _ in 0..RECOVERY_ATTEMPTS {
        let lease = host
            .retry(name, AttachMode::Durable)
            .expect("re-requesting the attachment");
        let deadline = Instant::now() + RECOVERY_LIMIT;
        loop {
            last = host.state(name);
            if last == Ok(TrustState::Ready) {
                return lease;
            }
            assert!(
                !names_no_vault(&last),
                "the host serves no vault under `{name}`: {last:?}"
            );
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    panic!(
        "the attachment stopped serving under load and {RECOVERY_ATTEMPTS} fresh demands did not \
         bring it back inside {RECOVERY_LIMIT:?} each: it read {observed:?} and now reads {last:?}"
    );
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
    let deadline = Instant::now() + RECONCILE_LIMIT;
    loop {
        let derived = store
            .begin_request()
            .stored_document(&written.path)
            .expect("reading a document")
            .map(|document| document.content_hash);
        if derived.as_deref() == Some(expected.as_str()) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the churn last wrote {} and no reconcile derived those bytes inside \
             {RECONCILE_LIMIT:?}, so the load ran against a host that saw nothing: the store holds \
             {derived:?} against the {expected} that write hashes to",
            written.path.as_str()
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
