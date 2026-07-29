//! The measurement harness the memory suites share: generate a tree in a
//! child process, and say what it cost.
//!
//! Peak memory is a function of the working set, not of the tree's size. The
//! generator is the first subject in this workspace that statement can be
//! measured against, and it holds it for a reason that is in its own code: it
//! draws each file and writes it before drawing the next, so what stays
//! resident is one document's bytes, one path and one digest per emitted file,
//! and nothing else the tree contains.
//!
//! Every case spawns the generator as a child under the testkit's process
//! harness and reads the peak resident set the kernel accounted to it. That is
//! why the subject is the binary rather than a call into the library: a peak
//! read off this test process would include the harness, cargo's test runner
//! and every other case running beside it.
//!
//! Both memory binaries compile this module and each uses part of it — only
//! the soak lane reads a wall clock — so the unused remainder in either is the
//! layout rather than a defect.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use norn_fixtures::Profile;
use norn_testkit::process::{Run, Sandbox};

use crate::baselines;

/// The seed every measurement generates at. One value, so two readings differ
/// by the profile alone.
const SEED: &str = "7";

/// What one generation cost.
pub struct Reading {
    pub peak_rss_bytes: u64,
    pub elapsed: Duration,
}

fn profile(name: &str) -> Profile {
    Profile::by_name(name).unwrap_or_else(|| panic!("no profile named `{name}`"))
}

/// A profile's document count, for a summary label and a failure message.
pub fn documents(name: &str) -> usize {
    profile(name).docs
}

/// Every baseline these suites assert against is a reading of the unoptimized
/// build. An optimized one allocates differently enough that the bars would be
/// measuring a subject they were not authored against, and a bar authored high
/// enough to hold either would pass quietly rather than fail.
///
/// It fails here rather than at compile time on purpose: a release build of
/// the workspace suite is a normal thing to want, and it is only the
/// measurement cases that are wrong under it.
#[allow(clippy::assertions_on_constants)] // The constant is the build profile, and the point is to fail the run under the wrong one.
fn assert_the_profile_the_bars_were_authored_on() {
    assert!(
        cfg!(debug_assertions),
        "the memory baselines are debug-profile values; this suite was built with \
         optimizations, so its bars describe a different subject"
    );
}

/// The document count the generator reported it emitted, if its report carries
/// one.
///
/// Parsed as a number rather than matched as a substring: `6000 documents`
/// occurs inside `16000 documents`, so a substring test would accept a
/// generation of a different size than the one it asked for.
fn reported_documents(report: &str) -> Option<usize> {
    let (head, _) = report.split_once(" documents")?;
    let digits = head
        .rsplit(|c: char| !c.is_ascii_digit())
        .next()
        .filter(|d| !d.is_empty())?;
    digits.parse().ok()
}

/// Generate `name`'s tree in a child process and report what it cost.
///
/// The generator is installed into the sandbox before it runs, because the
/// artifact cargo built is a file a concurrent build may rewrite. The tree
/// goes inside the sandbox too, so it is removed with it.
pub fn generate(label: &str, name: &str) -> Reading {
    assert_the_profile_the_bars_were_authored_on();
    let profile = profile(name);
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let generator = sandbox
        .install_binary(Path::new(env!("CARGO_BIN_EXE_norn-fixtures")))
        .expect("installing the generator");
    let out_dir: PathBuf = sandbox.work_dir().join("vault");

    let started = Instant::now();
    // The wait's bound is the widest wall clock the lanes sharing this helper
    // declare, so a generation the harness ends is one past every lane's
    // budget. Whether a reading fits the lane's own ceiling is that lane's
    // assertion, taken on the duration this returns.
    let outcome = Run::new(&sandbox, &generator)
        .arg("generate")
        .arg(&out_dir)
        .args(["--profile", name, "--seed", SEED])
        .deadline(baselines::SOAK_WALL_CLOCK_CEILING)
        .wait()
        .expect("running the generator");
    let elapsed = started.elapsed();
    outcome.assert_success();

    // A bar is only a statement about a generation that happened. The
    // generator reports what it emitted, so requiring the document count in
    // that report is what stops a run that did nothing from reading as cheap.
    let reported = outcome.stdout_text();
    assert_eq!(
        reported_documents(&reported),
        Some(profile.docs),
        "`{name}` was asked for {} documents and reported: {reported}",
        profile.docs
    );

    Reading {
        peak_rss_bytes: outcome.peak_rss_bytes,
        elapsed,
    }
}

/// Record and assert one flatness pair: `larger`'s peak against `smaller`'s,
/// held to `bar_per_mille`.
///
/// The pair is the sharper of the two instruments a memory lane has: a ceiling
/// passes anything that fits under it, and a ratio fails the moment two scales
/// stop moving together.
pub fn assert_flat(
    heading: &str,
    (smaller_name, smaller): (&str, Reading),
    (larger_name, larger): (&str, Reading),
    bar_per_mille: u64,
) {
    let observed = baselines::per_mille(larger.peak_rss_bytes, smaller.peak_rss_bytes);

    baselines::record(
        heading,
        &[
            (
                &format!(
                    "{smaller_name}, {} documents (MiB)",
                    documents(smaller_name)
                ),
                baselines::mebibytes(smaller.peak_rss_bytes),
            ),
            (
                &format!("{larger_name}, {} documents (MiB)", documents(larger_name)),
                baselines::mebibytes(larger.peak_rss_bytes),
            ),
            ("observed ratio", baselines::multiple(observed)),
            ("ratio bar", baselines::multiple(bar_per_mille)),
        ],
    );

    assert!(
        smaller.peak_rss_bytes > 0 && larger.peak_rss_bytes > 0,
        "a generation reported no peak at all, so the pair compares nothing"
    );
    assert!(
        observed <= bar_per_mille,
        "going from `{smaller_name}` ({} documents) to `{larger_name}` ({} documents) moved peak \
         memory by {}x, past the {}x bar: `{smaller_name}` peaked at {} MiB and `{larger_name}` \
         at {} MiB",
        documents(smaller_name),
        documents(larger_name),
        baselines::multiple(observed),
        baselines::multiple(bar_per_mille),
        baselines::mebibytes(smaller.peak_rss_bytes),
        baselines::mebibytes(larger.peak_rss_bytes)
    );
}

#[cfg(test)]
mod tests {
    use super::reported_documents;

    #[test]
    fn a_report_carries_the_count_it_states() {
        assert_eq!(
            reported_documents("realistic (seed 7): 2000 documents, 2361 files"),
            Some(2_000)
        );
    }

    /// The guard the parse exists for: a substring test accepts a run several
    /// times the size it asked for.
    #[test]
    fn a_larger_count_is_not_the_count_it_contains() {
        assert_eq!(
            reported_documents("soak (seed 7): 16000 documents, 1 file"),
            Some(16_000)
        );
    }

    #[test]
    fn a_report_with_no_count_parses_to_nothing() {
        assert_eq!(reported_documents("generation failed"), None);
        assert_eq!(reported_documents("no documents at all"), None);
    }
}
