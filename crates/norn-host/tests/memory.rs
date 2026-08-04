//! The per-PR memory bars for attachment: what attaching a vault costs,
//! measured rather than asserted, at the profiles the per-PR lane owns.
//!
//! Peak memory is a function of the working set, not of vault size. Attachment
//! is the first subject in this workspace that statement can be measured
//! against over a whole vault, and it holds it for a reason that is in its own
//! code: the heal walks the tree in a stream and commits what it derives in
//! bounded changesets, so what stays resident is one changeset rather than the
//! vault's facts.
//!
//! Two instruments, both counts and neither a clock. A **ceiling** holds the
//! ~2k-document `realistic` profile's attach peak under a checked-in number. A
//! **flatness pair** holds the ratio between the 300-document `ambiguous`
//! profile and `realistic`, which is the sharper of the two: a ceiling passes
//! anything that fits under it, and a ratio fails the moment two scales stop
//! moving together. Both profiles are under 5k documents, so both are this
//! lane's work under ADR 0004's by-kind split.
//!
//! # What the measurement charges to whom
//!
//! Each reading is of a child process, spawned under the testkit's process
//! harness, whose peak resident set the kernel accounts and the harness reads.
//! The child is this test binary re-executed in a harness mode an environment
//! variable selects: it adopts a tree already on disk, attaches it, waits for
//! ready, detaches, and reports what it derived. **Generation happens in the
//! parent**, so the child's peak is the attachment's and not the generator's,
//! and a peak read off the test process itself would include cargo's runner and
//! every case running beside it.
//!
//! [`the_gate_profile_attaches_inside_its_memory_bar`] is both a bar and the
//! harness entry point — the child re-executes that case by name, and the
//! environment variable is what tells it to attach instead of to measure. One
//! case doing both is what keeps this suite from carrying a case that passes
//! trivially in the lane whenever nothing spawned it. **The variable alone does
//! not select the harness**: it is paired with a token the parent issued beside
//! the tree, so a variable leaked into the lane's own environment fails loudly
//! here instead of turning every bar into a harness run.
//!
//! **Every case here is `#[ignore]`d into the `memory-lane` lane**, and the CI
//! `memory invariant` job is the only thing that runs them. A measurement
//! running beside the workspace suite measures the workspace suite too, so
//! "build and test" stays free of measurement.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

mod attach;
mod baselines;

use std::path::{Path, PathBuf};
use std::time::Duration;

use norn_testkit::process::{Run, Sandbox};

/// The variable that puts this binary in harness mode, carrying the root the
/// generated tree sits under.
const HARNESS_ENV: &str = "NORN_HOST_ATTACH_HARNESS";

/// The variable carrying the token the tree at that root was issued.
///
/// [`HARNESS_ENV`] alone does not select harness mode: a variable already in
/// the environment this lane runs under would make each bar below a harness run
/// that reports an attachment and evaluates no bar at all. The token is what
/// says a parent in this run issued the harness.
const HARNESS_TOKEN_ENV: &str = "NORN_HOST_ATTACH_HARNESS_TOKEN";

/// The case the child re-executes, which is the one that reads this constant.
const HARNESS_CASE: &str = "the_gate_profile_attaches_inside_its_memory_bar";

/// How long a child may take to attach before the harness ends it.
///
/// A runaway bound rather than a bar on speed: an attachment approaching this
/// is stuck, not slow, and how long one takes is a clock this lane does not
/// read. It sits above the child's own bound on the same attach
/// ([`attach::READY_LIMIT`]) so the child fails naming the state it saw, and
/// far enough inside the lane's job timeout that the first stuck child is
/// ended and reported here; a run where every child hangs is the job
/// timeout's to end.
const ATTACH_DEADLINE: Duration = Duration::from_secs(300);

/// **The ceiling**, and the harness the pair spawns.
///
/// The two roles are one case because the child selects it by name: with
/// [`HARNESS_ENV`] and its token set this process is the child, and it attaches
/// the tree the variable names instead of measuring anything.
#[test]
#[ignore = "memory-lane case: runs in the ci memory job, not the workspace suite"]
fn the_gate_profile_attaches_inside_its_memory_bar() {
    if let Some(root) = std::env::var_os(HARNESS_ENV) {
        attach_and_report(&attach::accepted_harness_root(&root, HARNESS_TOKEN_ENV));
        return;
    }

    let peak = attach_peak("attach-gate", "realistic");
    baselines::record(
        "gate profile attachment",
        &[
            ("peak resident set (MiB)", baselines::mebibytes(peak)),
            (
                "peak resident set ceiling (MiB)",
                baselines::mebibytes(baselines::ATTACH_PEAK_RSS_CEILING_BYTES),
            ),
        ],
    );
    assert!(
        peak <= baselines::ATTACH_PEAK_RSS_CEILING_BYTES,
        "attaching `realistic` peaked at {} MiB against a {} MiB bar",
        baselines::mebibytes(peak),
        baselines::mebibytes(baselines::ATTACH_PEAK_RSS_CEILING_BYTES)
    );
}

/// The memory invariant as a slope rather than a point, at per-PR scale.
///
/// `ambiguous` and `realistic` are both under 5k documents and span 6.7x in
/// documents. An attachment whose cost were the vault would show that spread in
/// its peak; this one shows a fraction of it, because the only thing that grows
/// with the vault is how many bounded changesets the heal commits.
#[test]
#[ignore = "memory-lane case: runs in the ci memory job, not the workspace suite"]
fn peak_memory_holds_flat_from_the_ambiguity_profile_to_the_gate_profile() {
    let small = attach_peak("attach-pair-ambiguous", "ambiguous");
    let large = attach_peak("attach-pair-realistic", "realistic");
    let observed = baselines::per_mille(large, small);

    baselines::record(
        "peak memory across the two per-PR attach scales",
        &[
            (
                "ambiguous, 300 documents (MiB)",
                baselines::mebibytes(small),
            ),
            (
                "realistic, 2000 documents (MiB)",
                baselines::mebibytes(large),
            ),
            ("observed ratio", baselines::multiple(observed)),
            (
                "ratio bar",
                baselines::multiple(baselines::ATTACH_PAIR_PEAK_RSS_PER_MILLE),
            ),
        ],
    );

    assert!(
        small > 0 && large > 0,
        "an attachment reported no peak at all, so the pair compares nothing"
    );
    assert!(
        observed <= baselines::ATTACH_PAIR_PEAK_RSS_PER_MILLE,
        "going from `ambiguous` (300 documents) to `realistic` (2000 documents) moved the attach \
         peak by {}x, past the {}x bar: `ambiguous` peaked at {} MiB and `realistic` at {} MiB",
        baselines::multiple(observed),
        baselines::multiple(baselines::ATTACH_PAIR_PEAK_RSS_PER_MILLE),
        baselines::mebibytes(small),
        baselines::mebibytes(large)
    );
}

/// Generate `profile`'s tree, attach it in a child, and hand back the peak
/// resident set the kernel accounted to that child.
///
/// The harness binary is installed into the sandbox before it runs, because the
/// artifact cargo built is a file a concurrent build may rewrite. The tree goes
/// inside the sandbox too, so it is removed with it.
fn attach_peak(label: &str, profile: &str) -> u64 {
    baselines::assert_the_profile_the_bars_were_authored_on();
    let documents = norn_fixtures::Profile::by_name(profile)
        .unwrap_or_else(|| panic!("no profile named `{profile}`"))
        .docs;

    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let harness = sandbox
        .install_binary(&std::env::current_exe().expect("this suite's own executable"))
        .expect("installing the harness");
    let root: PathBuf = sandbox.work_dir().join("attached");
    attach::Vault::generate(&root, profile);
    let token = attach::issue_harness_token(&root);

    let outcome = Run::new(&sandbox, &harness)
        // `--ignored` is what makes the filter reach the case at all: the case
        // the child re-executes is ignored, and a plain run would skip it.
        .args(["--exact", HARNESS_CASE, "--ignored", "--nocapture"])
        .env(HARNESS_ENV, &root)
        .env(HARNESS_TOKEN_ENV, &token)
        .deadline(ATTACH_DEADLINE)
        .wait()
        .expect("running the attach harness");
    outcome.assert_success();
    // What the parent judges is the child's own report, so a prefix of it is a
    // reading of a run whose end was cut off rather than a reading of the run.
    assert!(
        !outcome.stdout_truncated,
        "the harness wrote more than the capture limit, so what came back is a prefix"
    );

    // A bar is only a statement about an attachment that happened. The child
    // reports what it found derived, so an attach that converged over nothing
    // does not read as cheap.
    let reported = outcome.stdout_text();
    assert!(
        reported.contains(&report_line(documents)),
        "`{profile}` holds {documents} documents and the harness reported: {reported}"
    );
    outcome.peak_rss_bytes
}

/// The harness: adopt the tree at `root`, attach it, and report what the
/// attachment derived.
#[allow(clippy::disallowed_macros)] // The child's report is a machine-consumed stream its parent reads.
fn attach_and_report(root: &Path) {
    let vault = attach::Vault::adopt(root);
    {
        let host = vault.host();
        attach::attach_and_wait(&host, vault.name());
    }
    let mut store = vault.store();
    println!("{}", report_line(attach::derived_documents(&mut store)));
}

/// What the child prints and the parent looks for.
fn report_line(documents: usize) -> String {
    format!("attached {documents} documents")
}
