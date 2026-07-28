//! The memory bars: what generating a tree costs, measured rather than
//! asserted.
//!
//! Peak memory is a function of the working set, not of the tree's size. The
//! generator is the first subject in this workspace that statement can be
//! measured against, and it holds it for a reason that is in its own code: it
//! draws each file and writes it before drawing the next, so what stays
//! resident is one document's bytes, one path and one digest per emitted file,
//! and nothing else the tree contains.
//!
//! Every case here spawns the generator as a child under the testkit's process
//! harness and reads the peak resident set the kernel accounted to it. That is
//! why the subject is the binary rather than a call into the library: a peak
//! read off this test process would include the harness, cargo's test runner
//! and every other case running beside it.
//!
//! The lanes divide by kind. The gate profile's bar runs on every pull request
//! — a count-shaped assertion against a fixed ceiling. The `soak` profile's
//! cases are `#[ignore]`d and belong to the scheduled lane, which is where the
//! >=5k profile's work lives and where the wall clock is legal to read.
#![cfg(unix)]

mod baselines;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use norn_fixtures::Profile;
use norn_testkit::process::{Run, Sandbox};

/// The seed every measurement generates at. One value, so two readings differ
/// by the profile alone.
const SEED: &str = "7";

/// What one generation cost.
struct Reading {
    peak_rss_bytes: u64,
    elapsed: Duration,
}

fn profile(name: &str) -> Profile {
    Profile::by_name(name).unwrap_or_else(|| panic!("no profile named `{name}`"))
}

/// Generate `name`'s tree in a child process and report what it cost.
///
/// The generator is installed into the sandbox before it runs, because the
/// artifact cargo built is a file a concurrent build may rewrite. The tree
/// goes inside the sandbox too, so it is removed with it.
fn generate(label: &str, name: &str) -> Reading {
    let profile = profile(name);
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let generator = sandbox
        .install_binary(Path::new(env!("CARGO_BIN_EXE_norn-fixtures")))
        .expect("installing the generator");
    let out_dir: PathBuf = sandbox.work_dir().join("vault");

    let started = Instant::now();
    let outcome = Run::new(&sandbox, &generator)
        .arg("generate")
        .arg(&out_dir)
        .args(["--profile", name, "--seed", SEED])
        .wait()
        .expect("running the generator");
    let elapsed = started.elapsed();
    outcome.assert_success();

    // A bar is only a statement about a generation that happened. The
    // generator reports what it emitted, so requiring the document count in
    // that report is what stops a run that did nothing from reading as cheap.
    let reported = outcome.stdout_text();
    assert!(
        reported.contains(&format!("{} documents", profile.docs)),
        "`{name}` was asked for {} documents and reported: {reported}",
        profile.docs
    );

    Reading {
        peak_rss_bytes: outcome.peak_rss_bytes,
        elapsed,
    }
}

#[test]
fn the_gate_profile_generates_inside_its_memory_bar() {
    let reading = generate("memory-gate", "realistic");
    assert!(
        reading.peak_rss_bytes <= baselines::GATE_PEAK_RSS_CEILING_BYTES,
        "generating `realistic` peaked at {} MiB against a {} MiB bar",
        baselines::mebibytes(reading.peak_rss_bytes),
        baselines::mebibytes(baselines::GATE_PEAK_RSS_CEILING_BYTES)
    );
}

#[test]
#[ignore = "soak-lane case: the >=5k profile is nightly work, not per-PR"]
fn the_soak_profile_generates_inside_its_baselines() {
    let reading = generate("memory-soak", "soak");
    baselines::record(
        "soak profile generation",
        &[
            (
                "peak resident set (MiB)",
                baselines::mebibytes(reading.peak_rss_bytes),
            ),
            (
                "peak resident set ceiling (MiB)",
                baselines::mebibytes(baselines::SOAK_PEAK_RSS_CEILING_BYTES),
            ),
            (
                "wall clock (s)",
                format!(
                    "{}.{:03}",
                    reading.elapsed.as_secs(),
                    reading.elapsed.subsec_millis()
                ),
            ),
            (
                "wall clock ceiling (s)",
                baselines::SOAK_WALL_CLOCK_CEILING.as_secs().to_string(),
            ),
        ],
    );
    assert!(
        reading.peak_rss_bytes <= baselines::SOAK_PEAK_RSS_CEILING_BYTES,
        "generating `soak` peaked at {} MiB against a {} MiB bar",
        baselines::mebibytes(reading.peak_rss_bytes),
        baselines::mebibytes(baselines::SOAK_PEAK_RSS_CEILING_BYTES)
    );
    assert!(
        reading.elapsed <= baselines::SOAK_WALL_CLOCK_CEILING,
        "generating `soak` took {:?}, past the {:?} sanity ceiling",
        reading.elapsed,
        baselines::SOAK_WALL_CLOCK_CEILING
    );
}

/// The memory invariant as a slope rather than a point.
///
/// The soak profile holds three times the gate profile's documents and about
/// five times its bytes. A generator whose cost were the tree would show that
/// multiple in its peak; this one shows a fraction of it, because the only
/// things that grow with the tree are one path and one digest per emitted file.
///
/// The pair is the sharper of the two instruments: a ceiling passes anything
/// that fits under it, and a ratio fails the moment the two scales stop moving
/// together.
#[test]
#[ignore = "soak-lane case: the >=5k profile is nightly work, not per-PR"]
fn peak_memory_holds_flat_from_the_gate_profile_to_the_soak_profile() {
    let gate = generate("memory-pair-gate", "realistic");
    let soak = generate("memory-pair-soak", "soak");
    let observed = baselines::per_mille(soak.peak_rss_bytes, gate.peak_rss_bytes);

    baselines::record(
        "peak memory across the two scales",
        &[
            (
                "realistic, 2k documents (MiB)",
                baselines::mebibytes(gate.peak_rss_bytes),
            ),
            (
                "soak, 6k documents (MiB)",
                baselines::mebibytes(soak.peak_rss_bytes),
            ),
            ("observed ratio", baselines::multiple(observed)),
            (
                "ratio bar",
                baselines::multiple(baselines::SOAK_TO_GATE_PEAK_RSS_PER_MILLE),
            ),
        ],
    );

    assert!(
        gate.peak_rss_bytes > 0 && soak.peak_rss_bytes > 0,
        "a generation reported no peak at all, so the pair compares nothing"
    );
    assert!(
        observed <= baselines::SOAK_TO_GATE_PEAK_RSS_PER_MILLE,
        "trebling the document count moved peak memory by {}x, past the {}x bar: \
         `realistic` peaked at {} MiB and `soak` at {} MiB",
        baselines::multiple(observed),
        baselines::multiple(baselines::SOAK_TO_GATE_PEAK_RSS_PER_MILLE),
        baselines::mebibytes(gate.peak_rss_bytes),
        baselines::mebibytes(soak.peak_rss_bytes)
    );
}
