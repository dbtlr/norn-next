//! The per-PR memory bars for attachment and for a read: what attaching a
//! vault costs, and what reading it through a live hold adds, measured rather
//! than asserted, at the profiles the per-PR lane owns.
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
//! **A read is held the same two ways.** A child attaches `realistic`, keeps
//! it attached, and reads pages of a find through a live hold, each row
//! carrying its fields and its tags; a ceiling holds that child's peak, and a
//! ratio holds it to the attach-only child's peak at the same profile. A find
//! hydrates a page at a time, so what a read adds to a process that attached
//! is a page's rows rather than the vault's — and the ratio is what catches a
//! read that grew the process under a ceiling generous enough to pass it.
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
//! [`the_gate_profile_attaches_inside_its_memory_bar`] and
//! [`the_gate_profile_reads_inside_its_memory_bar`] are each both a bar and a
//! harness entry point — the child re-executes one of them by name, the name
//! is what says whether it attaches or reads, and the environment variable is
//! what tells it to do that instead of to measure. One case doing both is what
//! keeps this suite from carrying a case that passes trivially in the lane
//! whenever nothing spawned it. **The variable alone does
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

use attach::read::{FIND_LIMIT, bounded_find, each_page, the_pinned_declaration};
use norn_testkit::process::{Run, Sandbox};
use norn_wire::Column;

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

/// The case an attaching child re-executes, which is the one that reads this
/// constant.
const HARNESS_CASE: &str = "the_gate_profile_attaches_inside_its_memory_bar";

/// The case a reading child re-executes, which is the one that reads this
/// constant.
const READ_HARNESS_CASE: &str = "the_gate_profile_reads_inside_its_memory_bar";

/// How many pages the reading child reads through the find's cursor.
const READ_PAGES: u64 = 8;

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
        baselines::fits(peak, baselines::ATTACH_PEAK_RSS_CEILING_BYTES),
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
        baselines::fits(observed, baselines::ATTACH_PAIR_PEAK_RSS_PER_MILLE),
        "going from `ambiguous` (300 documents) to `realistic` (2000 documents) moved the attach \
         peak by {}x, past the {}x bar: `ambiguous` peaked at {} MiB and `realistic` at {} MiB",
        baselines::multiple(observed),
        baselines::multiple(baselines::ATTACH_PAIR_PEAK_RSS_PER_MILLE),
        baselines::mebibytes(small),
        baselines::mebibytes(large)
    );
}

/// **The read ceiling**, and the harness a reading child runs.
///
/// With [`HARNESS_ENV`] and its token set this process is the child: it
/// attaches the tree the variable names, keeps it attached, and reads
/// [`READ_PAGES`] pages of a find through a live hold instead of measuring
/// anything.
#[test]
#[ignore = "memory-lane case: runs in the ci memory job, not the workspace suite"]
fn the_gate_profile_reads_inside_its_memory_bar() {
    if let Some(root) = std::env::var_os(HARNESS_ENV) {
        read_and_report(&attach::accepted_harness_root(&root, HARNESS_TOKEN_ENV));
        return;
    }

    let peak = read_peak("read-gate");
    baselines::record(
        "gate profile read",
        &[
            ("peak resident set (MiB)", baselines::mebibytes(peak)),
            (
                "peak resident set ceiling (MiB)",
                baselines::mebibytes(baselines::READ_PEAK_RSS_CEILING_BYTES),
            ),
        ],
    );
    assert!(
        baselines::fits(peak, baselines::READ_PEAK_RSS_CEILING_BYTES),
        "attaching and reading `realistic` peaked at {} MiB against a {} MiB bar",
        baselines::mebibytes(peak),
        baselines::mebibytes(baselines::READ_PEAK_RSS_CEILING_BYTES)
    );
}

/// **What a read adds to the process that attached**, as a ratio rather than
/// a point.
///
/// Both children attach `realistic` the same way; one then reads through a
/// live hold. A find hydrates a page at a time, so the reading child's peak
/// sits on the attaching child's, and a read that held the vault's rows would
/// show as a multiple of it however generous the ceiling above it is.
#[test]
#[ignore = "memory-lane case: runs in the ci memory job, not the workspace suite"]
fn reading_the_gate_profile_holds_the_process_near_its_attach_peak() {
    let attached = attach_peak("read-pair-attach", "realistic");
    let read = read_peak("read-pair-read");
    let observed = baselines::per_mille(read, attached);

    baselines::record(
        "peak memory reading the gate profile, against attaching it",
        &[
            ("attach only (MiB)", baselines::mebibytes(attached)),
            ("attach and read (MiB)", baselines::mebibytes(read)),
            ("observed ratio", baselines::multiple(observed)),
            (
                "ratio bar",
                baselines::multiple(baselines::READ_OVER_ATTACH_PEAK_RSS_PER_MILLE),
            ),
        ],
    );

    assert!(
        attached > 0 && read > 0,
        "a child reported no peak at all, so the pair compares nothing"
    );
    assert!(
        baselines::fits(observed, baselines::READ_OVER_ATTACH_PEAK_RSS_PER_MILLE),
        "reading `realistic` through a live hold moved the peak by {}x over attaching it, past \
         the {}x bar: the attach peaked at {} MiB and the read at {} MiB",
        baselines::multiple(observed),
        baselines::multiple(baselines::READ_OVER_ATTACH_PEAK_RSS_PER_MILLE),
        baselines::mebibytes(attached),
        baselines::mebibytes(read)
    );
}

/// Generate `profile`'s tree, attach it in a child, and hand back the peak
/// resident set the kernel accounted to that child.
fn attach_peak(label: &str, profile: &str) -> u64 {
    let documents = norn_fixtures::Profile::by_name(profile)
        .unwrap_or_else(|| panic!("no profile named `{profile}`"))
        .docs;
    let (peak, reported) = child_peak(label, profile, HARNESS_CASE);

    // A bar is only a statement about an attachment that happened. The child
    // reports what it found derived, so an attach that converged over nothing
    // does not read as cheap.
    assert!(
        reported.contains(&report_line(documents)),
        "`{profile}` holds {documents} documents and the harness reported: {reported}"
    );
    peak
}

/// Generate the `realistic` tree, attach and read it in a child, and hand back
/// the peak resident set the kernel accounted to that child.
///
/// **A peak is only a statement about a read that happened.** The child
/// reports the rows it hydrated and the pages it read, and a report that is
/// missing or read no row fails here rather than reading as a cheap read.
fn read_peak(label: &str) -> u64 {
    let (peak, reported) = child_peak(label, "realistic", READ_HARNESS_CASE);
    let read = ReadReport::from_stdout(&reported).unwrap_or_else(|problem| panic!("{problem}"));
    assert_eq!(
        read,
        ReadReport {
            rows: READ_PAGES * u64::from(FIND_LIMIT),
            pages: READ_PAGES,
        },
        "`realistic` holds more documents than {READ_PAGES} pages, so every page the harness \
         read was a whole one"
    );
    peak
}

/// Generate `profile`'s tree, run `case` over it in a child, and hand back the
/// peak resident set the kernel accounted to that child with what it printed.
///
/// The harness binary is installed into the sandbox before it runs, because the
/// artifact cargo built is a file a concurrent build may rewrite. The tree goes
/// inside the sandbox too, so it is removed with it.
fn child_peak(label: &str, profile: &str, case: &str) -> (u64, String) {
    baselines::assert_the_profile_the_bars_were_authored_on();
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
        .args(["--exact", case, "--ignored", "--nocapture"])
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

    (outcome.peak_rss_bytes, outcome.stdout_text())
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

/// The reading harness: adopt the tree at `root`, attach it, and read
/// [`READ_PAGES`] pages of a find through a live hold, each row carrying its
/// fields and its tags, one page resident at a time.
#[allow(clippy::disallowed_macros)] // The child's report is a machine-consumed stream its parent reads.
fn read_and_report(root: &Path) {
    let vault = attach::Vault::adopt(root);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let declared = the_pinned_declaration(&mut vault.store());

    let hold = host
        .begin_read(vault.name())
        .expect("a live attachment answers a read");
    let params = bounded_find(vault.name()).with_columns([Column::fields(), Column::tags()]);
    let mut read = ReadReport::default();
    each_page(hold.snapshot(), &params, &declared, READ_PAGES, |page| {
        read.rows += page.work.documents_hydrated;
        read.pages += 1;
    });
    println!("{}", read.line());
}

/// What a reading child reports: the rows its pages hydrated, and how many
/// pages it read.
#[derive(Debug, Default, Eq, PartialEq)]
struct ReadReport {
    rows: u64,
    pages: u64,
}

impl ReadReport {
    /// The line the child prints.
    fn line(&self) -> String {
        format!("read {} rows hydrated over {} pages", self.rows, self.pages)
    }

    /// The report a child's output carries, refused where it carries none or
    /// one of a read that hydrated nothing: a peak over a read that did not
    /// happen is a peak over an attach.
    fn from_stdout(stdout: &str) -> Result<Self, String> {
        let Some(line) = stdout.lines().find(|line| line.starts_with("read ")) else {
            return Err(format!("the read harness reported no read: {stdout}"));
        };
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let ["read", rows, "rows", "hydrated", "over", pages, "pages"] = tokens.as_slice() else {
            return Err(format!(
                "the read harness reported `{line}`, which is no read report"
            ));
        };
        let count = |text: &str| {
            text.parse::<u64>()
                .map_err(|problem| format!("the read harness reported `{line}`: {problem}"))
        };
        let report = ReadReport {
            rows: count(rows)?,
            pages: count(pages)?,
        };
        if report.rows == 0 || report.pages == 0 {
            return Err(format!(
                "the read harness reported `{line}`, a read that hydrated nothing, so its peak is \
                 an attach's"
            ));
        }
        Ok(report)
    }
}

/// **The parent refuses a peak it cannot tie to a read.** A child whose output
/// carries no report, or a report of a read that hydrated no row, fails the
/// bar rather than passing it as a cheap read; a report the child printed is
/// read back as printed.
#[test]
fn a_read_peak_stands_only_on_a_report_of_rows_read() {
    let printed = ReadReport {
        rows: 200,
        pages: 8,
    };
    assert_eq!(
        ReadReport::from_stdout(&format!("running 1 test\n{}\ntest ok\n", printed.line())),
        Ok(printed)
    );

    let missing = ReadReport::from_stdout("running 1 test\ntest ok\n").expect_err("no report");
    assert!(missing.contains("reported no read"), "{missing}");

    let empty = ReadReport::from_stdout(&ReadReport { rows: 0, pages: 1 }.line())
        .expect_err("a read of no row");
    assert!(empty.contains("hydrated nothing"), "{empty}");

    let garbled =
        ReadReport::from_stdout("read everything\n").expect_err("a line that is no report");
    assert!(garbled.contains("no read report"), "{garbled}");
}
