//! Per-PR evidence that walking a larger vault does not retain the vault.
//!
//! Generation happens in the harness process. Each measurement then installs
//! this test binary as an isolated child, which drains the walk without
//! collecting its facts. The child reports its exact work count alongside the
//! peak resident set accounted by the kernel, so a cheap run that skipped the
//! tree cannot satisfy either bar.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use norn_fixtures::Profile;
use norn_fs::{WalkOptions, walk};
use norn_testkit::process::{Run, Sandbox};

const CHILD_ROOT: &str = "NORN_FS_WALK_MEMORY_ROOT";
const SEED: u64 = 7;

// Six native macos-arm64 debug readings on 2026-08-01 put `ambiguous` at
// 2.50-2.53 MiB and `realistic` at 2.62-2.69 MiB, ratios of 1.04-1.07. The
// ceiling is about three times the observed top; the ratio below is the sharp
// instrument against retaining the realistic tree's roughly 4.3 MiB of
// clutter in addition to the test executable's resident floor.
#[cfg(target_os = "macos")]
const REALISTIC_PEAK_RSS_CEILING_BYTES: u64 = 8 * 1024 * 1024;

// The existing fixture generator's ubuntu-latest x86_64-glibc baseline is
// 3.50-3.62 MiB versus 3.7-4.2 MiB on macos-arm64. Until this walker lane has
// its first hosted reading, Linux therefore uses the same conservative bar;
// subsequent changes move it only with reviewed measurement evidence. This is
// the same 8 MiB conservative ceiling already used for that generator lane.
#[cfg(target_os = "linux")]
const REALISTIC_PEAK_RSS_CEILING_BYTES: u64 = 8 * 1024 * 1024;

// A retained-tree cost grows by several MiB between these profiles. A 2x bar
// tolerates process-accounting noise around the executable's fixed floor but
// still rejects growth proportional to the generated tree.
const REALISTIC_TO_AMBIGUOUS_PEAK_RSS_PER_MILLE: u64 = 2_000;

struct Reading {
    peak_rss_bytes: u64,
    facts: usize,
}

#[test]
#[ignore = "memory-lane case: runs in the ci memory job, not the workspace suite"]
fn walk_peak_memory_is_bounded_by_the_frontier_not_the_tree() {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        drain_in_child(Path::new(&root));
        return;
    }

    let small = measure("walk-memory-ambiguous", "ambiguous");
    let realistic = measure("walk-memory-realistic", "realistic");
    let ratio = per_mille(realistic.peak_rss_bytes, small.peak_rss_bytes);

    eprintln!(
        "walk memory: ambiguous={} facts, {:.2} MiB; realistic={} facts, {:.2} MiB; ratio={:.2}x (bar {:.2}x)",
        small.facts,
        mebibytes(small.peak_rss_bytes),
        realistic.facts,
        mebibytes(realistic.peak_rss_bytes),
        ratio as f64 / 1_000.0,
        REALISTIC_TO_AMBIGUOUS_PEAK_RSS_PER_MILLE as f64 / 1_000.0,
    );

    assert!(
        realistic.peak_rss_bytes <= REALISTIC_PEAK_RSS_CEILING_BYTES,
        "walking `realistic` peaked at {:.2} MiB against a {:.2} MiB bar",
        mebibytes(realistic.peak_rss_bytes),
        mebibytes(REALISTIC_PEAK_RSS_CEILING_BYTES),
    );
    assert!(
        ratio <= REALISTIC_TO_AMBIGUOUS_PEAK_RSS_PER_MILLE,
        "walking `realistic` moved peak memory by {:.2}x from `ambiguous`, past the {:.2}x bar",
        ratio as f64 / 1_000.0,
        REALISTIC_TO_AMBIGUOUS_PEAK_RSS_PER_MILLE as f64 / 1_000.0,
    );
}

fn measure(label: &str, profile_name: &str) -> Reading {
    let profile = Profile::by_name(profile_name)
        .unwrap_or_else(|| panic!("fixture profile `{profile_name}` exists"));
    let sandbox =
        Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a measurement sandbox");
    let root = sandbox.work_dir().join("vault");

    // Deliberately outside the measured child: the reading belongs to the
    // walker alone, not to fixture construction.
    let manifest = norn_fixtures::generate(&profile, SEED, &root)
        .unwrap_or_else(|error| panic!("generating `{profile_name}`: {error}"));
    let expected_facts = manifest.files + manifest.symlinks.total();

    let child = installed_child(&sandbox);
    let outcome = Run::new(&sandbox, child)
        .args([
            "--exact",
            "walk_peak_memory_is_bounded_by_the_frontier_not_the_tree",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ROOT, &root)
        .deadline(Duration::from_secs(60))
        .wait()
        .expect("running the isolated walker");
    outcome.assert_success();

    let facts = reported_facts(&outcome.stdout_text()).unwrap_or_else(|| {
        panic!(
            "the `{profile_name}` child reported no work count: {}",
            outcome.stdout_text()
        )
    });
    assert_eq!(
        facts, expected_facts,
        "`{profile_name}` drained a different number of facts than the generated tree contains"
    );
    assert!(
        outcome.peak_rss_bytes > 0,
        "the `{profile_name}` child reported no peak resident set"
    );

    Reading {
        peak_rss_bytes: outcome.peak_rss_bytes,
        facts,
    }
}

#[allow(clippy::disallowed_macros)] // Harness protocol, not product rendering.
fn drain_in_child(root: &Path) {
    let mut facts = 0usize;
    for fact in walk(root, WalkOptions::default()).expect("starting the measured walk") {
        fact.expect("draining the measured walk");
        facts += 1;
    }
    // The parent needs the exact count from the process whose RSS it measured.
    println!("norn-fs walk facts: {facts}");
}

fn installed_child(sandbox: &Sandbox) -> PathBuf {
    let source = std::env::current_exe().expect("this suite's own executable");
    sandbox
        .install_binary(&source)
        .expect("installing this suite's executable")
}

fn reported_facts(output: &str) -> Option<usize> {
    output
        .lines()
        .find_map(|line| {
            line.split_once("norn-fs walk facts: ")
                .map(|(_, count)| count)
        })?
        .parse()
        .ok()
}

fn per_mille(numerator: u64, denominator: u64) -> u64 {
    numerator.saturating_mul(1_000).div_ceil(denominator)
}

fn mebibytes(bytes: u64) -> f64 {
    bytes as f64 / (1024 * 1024) as f64
}

#[test]
fn the_child_report_parser_requires_an_exact_count() {
    assert_eq!(reported_facts("norn-fs walk facts: 2361\n"), Some(2_361));
    assert_eq!(reported_facts("norn-fs walk facts: none\n"), None);
}
