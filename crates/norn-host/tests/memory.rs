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
//! **A read is held three ways, over one read mix.** A child attaches a
//! profile, keeps it attached, and runs the mix's eight shapes through the
//! host's read verbs, each verb taking its own live hold: a find under a
//! predicate, paged newest first, with each row carrying its fields, its tags
//! and its links; a count grouped by a field; a get of a bare stem, resolved
//! as a suffix; a links-to count and a backlinks find, each narrowed to the
//! documents linking to that stem; a validate page of findings narrowed by a
//! path part; a lexical search page; and a describe page of facets. Every
//! shape answers a page, a single target or what a narrowing part admits, so
//! what a read adds to a process that attached is a page's rows rather than
//! the vault's.
//!
//! Two of the three are whole-process peaks at `realistic`: a ceiling on the
//! reading child's, and a ratio of it to the peak of an attach-only child of
//! the same tree. The kernel reports one peak per child, so the mix is one
//! child, and what those bars bound is the highest any shape reached, over
//! the attach under all of them. **The third is the heap.** This binary's
//! global allocator counts the live heap, and the reading child marks it once
//! the attachment is ready and before the first shape runs, then reports the
//! most the shapes raised it above that mark. A child at `ambiguous` and one
//! at `realistic` each report it, and their ratio holds the heap the mix
//! holds flat across the two scales. A whole-process peak can absorb a
//! vault's rows inside the headroom the attach left, and the heap count
//! cannot, because it counts what the code holds rather than what the kernel
//! mapped.
//!
//! # What the measurement charges to whom
//!
//! Each reading is of a child process, spawned under the testkit's process
//! harness, whose peak resident set the kernel accounts and the harness reads.
//! The child is this test binary re-executed in a harness mode an environment
//! variable selects: it adopts a tree already on disk, attaches it, waits for
//! ready, and then either detaches and reports what it derived or runs the
//! read mix and reports the rows each shape answered and its heap reading.
//! **Generation happens in the parent**, so the child's peak is the
//! attachment's and not the generator's, and a peak read off the test process
//! itself would include cargo's runner and every case running beside it.
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
//! **Every measurement case here is `#[ignore]`d into the `memory-lane`
//! lane**, and the CI `memory invariant` job is the only thing that runs them.
//! A measurement running beside the workspace suite measures the workspace
//! suite too, so "build and test" stays free of measurement.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

mod attach;
mod baselines;
mod heap;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use attach::read::{FIND_LIMIT, bounded_find};
use norn_host::Answered;
use norn_testkit::process::{Run, Sandbox};
use norn_wire::{
    Column, CountParams, CountReport, DescribeParams, DescribeReport, DocumentPath, DocumentRow,
    ErrorEnvelope, FindParams, FindReport, GetParams, GetReport, GroupKey, LinkFamily, LinkHealth,
    Predicate, ResolutionTarget, SearchParams, SearchReport, ValidateParams, ValidateReport,
    VaultAddress,
};

/// Every allocation this binary makes goes through the counting allocator, so
/// a reading child can say how far its read shapes raised the heap above the
/// attach beneath them.
#[global_allocator]
static HEAP: heap::Counting = heap::Counting;

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

/// How many pages the reading child's find reads through its cursor.
const READ_PAGES: u64 = 8;

/// The shapes the reading child runs, in the order it runs them.
///
/// Each is bounded as its verb bounds it: the find at [`READ_PAGES`] pages,
/// the get at the one document its target names, the links-to count at the
/// documents its narrowing part admits, and every other shape at one page.
const READ_SHAPES: [&str; 8] = [
    Find::NAME,
    Count::NAME,
    Get::NAME,
    LinksTo::NAME,
    Backlinks::NAME,
    Validate::NAME,
    Search::NAME,
    Describe::NAME,
];

/// What each shape of the mix answers over `profile`'s tree, in
/// [`READ_SHAPES`] order.
///
/// The trees are generated from [`attach::SEED`] and the requests are fixed,
/// so every answer is a known number and the parent holds the child's report
/// to it: a shape swapped for a cheaper one, or one whose answer was spelled
/// rather than read, reports a number other than its own.
fn pinned_answers(profile: &str) -> [usize; 8] {
    match profile {
        "ambiguous" => [200, 6, 1, 10, 10, 18, 25, 15],
        "realistic" => [200, 6, 1, 5, 5, 25, 25, 15],
        other => panic!("no read answers are pinned for the `{other}` profile"),
    }
}

/// The vault schema the read subjects attach `realistic` under: the minimal
/// schema and a tag vocabulary that leaves two of the generated tags out and
/// reports them.
///
/// The generated tree carries no document a finding stands over, so under the
/// minimal schema a validate answers nothing. The undeclared tags are what give
/// the validate shape findings to page, the way a vault with a vocabulary it
/// has outgrown carries them. The attach-only child of the ratio attaches
/// under the same schema, so the ratio still cancels the attach.
const READ_SCHEMA: &[u8] = b"version: 1\ntags:\n  declared: [research, writing, infra, reading, \
personal, planning, review, reference]\n  undeclared: report\n";

/// The word the reading child's lexical search asks for, which the generated
/// bodies carry in a sentence tail.
const SEARCH_QUERY: &str = "baseline";

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
        peak > 0,
        "the attaching child reported no peak resident set, so the ceiling holds no reading"
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
/// attaches the tree the variable names, keeps it attached, and runs every
/// shape of [`READ_SHAPES`] through the host's read verbs instead of
/// measuring anything. The peak this bars is the highest the process reached
/// across the attach and all eight shapes.
#[test]
#[ignore = "memory-lane case: runs in the ci memory job, not the workspace suite"]
fn the_gate_profile_reads_inside_its_memory_bar() {
    if let Some(root) = std::env::var_os(HARNESS_ENV) {
        read_and_report(&attach::accepted_harness_root(&root, HARNESS_TOKEN_ENV));
        return;
    }

    let peak = read_child("read-gate", "realistic").peak_rss;
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
        peak > 0,
        "the reading child reported no peak resident set, so the ceiling holds no reading"
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
/// Both children attach `realistic` under [`READ_SCHEMA`] the same way; one
/// then runs the read mix. Every shape answers a bounded page, target or
/// narrowed set, so the reading child's peak sits on the attaching child's,
/// and a shape that grew the process by a large fraction of the attach shows
/// as a multiple of it however generous the ceiling above it is. A shape that
/// holds the vault's rows inside the headroom the attach left is
/// [`the_read_mix_holds_its_heap_flat_from_the_ambiguity_profile_to_the_gate_profile`]'s
/// to refuse.
#[test]
#[ignore = "memory-lane case: runs in the ci memory job, not the workspace suite"]
fn reading_the_gate_profile_holds_the_process_near_its_attach_peak() {
    let attached = attach_peak_under("read-pair-attach", "realistic", READ_SCHEMA);
    let read = read_child("read-pair-read", "realistic").peak_rss;
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

/// **What the read shapes hold on the heap, as a slope across two scales.**
///
/// A reading child at each per-PR profile runs the same read mix and reports
/// the most its shapes raised the live heap above the attach beneath them.
/// `realistic` holds 6.7x the documents `ambiguous` does. A shape that answers
/// a page, a target or what a narrowing part admits holds about the same rows
/// at both scales, and a shape that held the vault's rows would show the
/// spread. The heap reading counts bytes the code asked for, so neither page
/// size nor what the attach left resident moves it, which is what lets it see
/// a vault's rows a whole-process peak absorbs.
#[test]
#[ignore = "memory-lane case: runs in the ci memory job, not the workspace suite"]
fn the_read_mix_holds_its_heap_flat_from_the_ambiguity_profile_to_the_gate_profile() {
    let small = read_child("read-heap-ambiguous", "ambiguous").heap_peak;
    let large = read_child("read-heap-realistic", "realistic").heap_peak;
    let observed = baselines::per_mille(large, small);

    baselines::record(
        "heap the read mix holds across the two per-PR scales",
        &[
            ("ambiguous, 300 documents (bytes)", small.to_string()),
            ("realistic, 2000 documents (bytes)", large.to_string()),
            ("observed ratio", baselines::multiple(observed)),
            (
                "ratio bar",
                baselines::multiple(baselines::READ_PAIR_HEAP_PEAK_PER_MILLE),
            ),
        ],
    );

    assert!(
        small > 0 && large > 0,
        "a reading child reported no heap reading above its attach, so the pair compares nothing"
    );
    assert!(
        baselines::fits(observed, baselines::READ_PAIR_HEAP_PEAK_PER_MILLE),
        "going from `ambiguous` (300 documents) to `realistic` (2000 documents) moved the heap the \
         read mix holds by {}x, past the {}x bar: `ambiguous` held {small} bytes and \
         `realistic` {large}",
        baselines::multiple(observed),
        baselines::multiple(baselines::READ_PAIR_HEAP_PEAK_PER_MILLE),
    );
}

/// Generate `profile`'s tree under the minimal schema, attach it in a child,
/// and hand back the peak resident set the kernel accounted to that child.
fn attach_peak(label: &str, profile: &str) -> u64 {
    attach_peak_under(label, profile, attach::SCHEMA)
}

/// Generate `profile`'s tree under `schema`, attach it in a child, and hand
/// back the peak resident set the kernel accounted to that child.
fn attach_peak_under(label: &str, profile: &str, schema: &[u8]) -> u64 {
    let documents = norn_fixtures::Profile::by_name(profile)
        .unwrap_or_else(|| panic!("no profile named `{profile}`"))
        .docs;
    let (peak, reported) = child_peak(label, profile, schema, HARNESS_CASE);

    // A bar is only a statement about an attachment that happened. The child
    // reports what it found derived, so an attach that converged over nothing
    // does not read as cheap.
    assert!(
        reported.contains(&report_line(documents)),
        "`{profile}` holds {documents} documents and the harness reported: {reported}"
    );
    peak
}

/// What a reading child's run comes to: the peak resident set the kernel
/// accounted to it, and the most its read shapes raised the heap above the
/// attach beneath them.
struct ReadReading {
    peak_rss: u64,
    heap_peak: u64,
}

/// Generate `profile`'s tree under [`READ_SCHEMA`], attach it in a child and
/// run the read mix over it, and hand back what the child's run came to.
///
/// **A reading is only a statement about a read that happened.** The child
/// reports the rows each shape answered, and the parent holds that report to
/// [`pinned_answers`]: a report missing a shape, carrying one the mix does not
/// name, or answering any shape other than as pinned fails here rather than
/// reading as a cheap read.
fn read_child(label: &str, profile: &str) -> ReadReading {
    let (peak_rss, reported) = child_peak(label, profile, READ_SCHEMA, READ_HARNESS_CASE);
    let read = ReadReport::from_stdout(&reported).unwrap_or_else(|problem| panic!("{problem}"));
    if let Err(problem) = read.answers_as_pinned(&pinned_answers(profile)) {
        panic!("over `{profile}`, {problem}");
    }
    let heap_peak = read
        .heap_peak
        .expect("a parsed report carries its heap reading");
    let mut readings: Vec<(&str, String)> = READ_SHAPES
        .iter()
        .map(|shape| (*shape, read.answered[*shape].to_string()))
        .collect();
    readings.push(("heap peak above the attach (bytes)", heap_peak.to_string()));
    baselines::record(
        &format!("rows each read shape answered ({label}, {profile})"),
        &readings,
    );
    ReadReading {
        peak_rss,
        heap_peak: u64::try_from(heap_peak).expect("a heap reading fits a u64"),
    }
}

/// Generate `profile`'s tree under `schema`, run `case` over it in a child, and
/// hand back the peak resident set the kernel accounted to that child with what
/// it printed.
///
/// The harness binary is installed into the sandbox before it runs, because the
/// artifact cargo built is a file a concurrent build may rewrite. The tree goes
/// inside the sandbox too, so it is removed with it.
fn child_peak(label: &str, profile: &str, schema: &[u8], case: &str) -> (u64, String) {
    baselines::assert_the_profile_the_bars_were_authored_on();
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let harness = sandbox
        .install_binary(&std::env::current_exe().expect("this suite's own executable"))
        .expect("installing the harness");
    let root: PathBuf = sandbox.work_dir().join("attached");
    let vault = attach::Vault::generate(&root, profile);
    std::fs::write(vault.path().join(".norn/schema.yaml"), schema).expect("write the vault schema");
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

/// The reading harness: adopt the tree at `root`, attach it, keep it
/// attached, and run every shape of [`READ_SHAPES`] through the host's read
/// verbs, each bounded as its verb bounds it.
///
/// Each verb takes its own hold on the one live attachment and gives it back
/// before it returns, which is the path a client's read takes. **The heap mark
/// is set once the attachment is ready and before the first shape runs**, so
/// the heap reading the child reports is the most the shapes raised the live
/// heap above what the attached host already held.
///
/// The find reads [`READ_PAGES`] pages of the documents whose `type` is not
/// `meeting`, one resident at a time. Its pages are where the other shapes'
/// narrowing comes from. The first row across them that carries a wikilink
/// written as a bare stem resolving to one document names the target of the
/// three shapes that name one: the get resolves that stem as a suffix, and the
/// links-to count and the backlinks find narrow to the documents linking to
/// it, the row that carried the link among them. The first row across them
/// that sits in a directory names the path part the validate is narrowed to.
#[allow(clippy::disallowed_macros)] // The child's report is a machine-consumed stream its parent reads.
fn read_and_report(root: &Path) {
    let vault = attach::Vault::adopt(root);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let address = || VaultAddress::name(vault.name().clone());
    let mut read = ReadReport::default();
    let mark = heap::Mark::set();

    let find = bounded_find(vault.name())
        .with_predicates([Predicate::not_equal_to("type", "meeting")])
        .with_columns([Column::fields(), Column::tags(), Column::links()]);
    let mut linked = None;
    let mut directory = None;
    let mut after = None;
    for _ in 0..READ_PAGES {
        let request = match after.take() {
            None => find.clone(),
            Some(cursor) => find.clone().with_after(cursor),
        };
        let page = complete(Find::NAME, host.find(&request));
        read.answered::<Find>(&page);
        linked = linked.or_else(|| a_linked_stem(&page.rows));
        directory = directory.or_else(|| a_directory(&page.rows));
        after = page.next;
        if after.is_none() {
            break;
        }
    }
    let (stem, resolved, linking) = linked
        .expect("a row the find read carries a wikilink written as a stem naming one document");
    let directory = directory.expect("a row the find read sits in a directory");
    let target = ResolutionTarget::new(&stem).expect("a link's stem is a target");

    let tallies = complete(
        Count::NAME,
        host.count(
            &CountParams::new(address())
                .with_by([GroupKey::field("type")])
                .with_limit(FIND_LIMIT),
        ),
    );
    read.answered::<Count>(&tallies);

    let got = complete(
        Get::NAME,
        host.get(&GetParams::new(address(), target.clone())),
    );
    let GetReport::Record { document, .. } = &got else {
        panic!("a get of `{stem}` with no anchor answered no record");
    };
    assert_eq!(
        document.path, resolved,
        "the get resolved `{stem}` to another document than its link does"
    );
    read.answered::<Get>(&got);

    let linking_to = complete(
        LinksTo::NAME,
        host.count(
            &CountParams::new(address()).with_predicates([Predicate::links_to(target.clone())]),
        ),
    );
    read.answered::<LinksTo>(&linking_to);

    let backlinks = complete(
        Backlinks::NAME,
        host.find(
            &FindParams::new(address())
                .with_predicates([Predicate::links_to(target)])
                .with_limit(FIND_LIMIT),
        ),
    );
    assert!(
        backlinks.rows.iter().any(|row| row.path == linking),
        "the backlinks of `{stem}` leave out `{linking}`, whose link to it named the target"
    );
    // Both shapes narrow by the one links-to part, so where the backlinks fit
    // one page the count tallies exactly the documents the page lists.
    if backlinks.next.is_none() {
        assert_eq!(
            LinksTo::rows(&linking_to),
            backlinks.rows.len(),
            "the links-to count of `{stem}` and its backlinks page disagree on who links to it"
        );
    }
    read.answered::<Backlinks>(&backlinks);

    let findings = complete(
        Validate::NAME,
        host.validate(
            &ValidateParams::new(address())
                .with_predicates([Predicate::path(format!("{directory}/**"))])
                .with_limit(FIND_LIMIT),
        ),
    );
    assert!(
        matches!(findings, ValidateReport::Findings { .. }),
        "a validate asking for findings answered {findings:?}"
    );
    read.answered::<Validate>(&findings);

    let hits = complete(
        Search::NAME,
        host.search(&SearchParams::new(address(), SEARCH_QUERY).with_limit(FIND_LIMIT)),
    );
    read.answered::<Search>(&hits);

    let facets = complete(
        Describe::NAME,
        host.describe(&DescribeParams::new(address()).with_limit(FIND_LIMIT)),
    );
    read.answered::<Describe>(&facets);

    read.heap_peak = Some(mark.peak_above());
    println!("{}", read.lines());
}

/// The report of a read verb that answered `shape` with every part applied.
///
/// A part a verb could not apply is answered unsatisfied rather than refused,
/// so an answer with one is a shape that did not run as asked.
fn complete<R: std::fmt::Debug, W>(
    shape: &str,
    answered: Result<Answered<R, W>, ErrorEnvelope>,
) -> R {
    let answered =
        answered.unwrap_or_else(|refusal| panic!("the {shape} shape was refused: {refusal:?}"));
    assert!(
        answered.answer.is_complete(),
        "the {shape} shape left parts unapplied: {:?}",
        answered.answer.unsatisfied
    );
    answered.answer.report
}

/// A wikilink one of `rows` carries, written as a bare stem that resolves to
/// exactly one document: the stem, the document it resolves to, and the row
/// that carries it.
fn a_linked_stem(rows: &[DocumentRow]) -> Option<(String, DocumentPath, DocumentPath)> {
    rows.iter().find_map(|row| {
        let links = row.links.as_ref()?;
        links.items.iter().find_map(|link| {
            let bare = link.family == LinkFamily::Wikilink
                && link.protocol.is_none()
                && link.anchor.is_none()
                && !link.target.contains('/');
            match link.targets.candidates() {
                [only] if bare && link.health() == LinkHealth::Healthy => {
                    Some((link.target.clone(), only.path.clone(), row.path.clone()))
                }
                _ => None,
            }
        })
    })
}

/// The top-level directory the first of `rows` that sits in one sits in.
fn a_directory(rows: &[DocumentRow]) -> Option<String> {
    rows.iter()
        .find_map(|row| row.path.as_str().split_once('/'))
        .map(|(directory, _)| directory.to_string())
}

/// One read shape of the mix: the name its report line carries, the report
/// its verb answers with, and how many rows that report answered.
///
/// **The report type ties a shape to its verb.** The child records a shape's
/// rows only from the report its verb returned, so a count's tallies cannot
/// stand in for a search's hits or a describe's facets. The two pairs of
/// shapes that share a verb (find and backlinks, count and links-to) are told
/// apart by [`pinned_answers`].
trait Shape {
    /// The shape's name in the child's report.
    const NAME: &'static str;
    /// What the shape's verb answers with.
    type Report;
    /// How many rows `report` answered.
    fn rows(report: &Self::Report) -> usize;
}

/// A find page, newest `created` first, under a predicate.
struct Find;
/// A count grouped by a field.
struct Count;
/// A get of a bare stem, resolved as a suffix.
struct Get;
/// A count narrowed to the documents linking to one target, as a tally of them.
struct LinksTo;
/// A find narrowed to the documents linking to one target.
struct Backlinks;
/// A validate page of findings narrowed by a path part.
struct Validate;
/// A lexical search page.
struct Search;
/// A describe page of facets.
struct Describe;

impl Shape for Find {
    const NAME: &'static str = "find";
    type Report = FindReport;
    fn rows(report: &FindReport) -> usize {
        report.rows.len()
    }
}

impl Shape for Count {
    const NAME: &'static str = "count";
    type Report = CountReport;
    fn rows(report: &CountReport) -> usize {
        report.rows.len()
    }
}

impl Shape for Get {
    const NAME: &'static str = "get";
    type Report = GetReport;
    fn rows(report: &GetReport) -> usize {
        usize::from(matches!(report, GetReport::Record { .. }))
    }
}

impl Shape for LinksTo {
    const NAME: &'static str = "links-to";
    type Report = CountReport;
    fn rows(report: &CountReport) -> usize {
        let linking: u64 = report.rows.iter().map(|tally| tally.count).sum();
        usize::try_from(linking).expect("a tally fits a usize")
    }
}

impl Shape for Backlinks {
    const NAME: &'static str = "backlinks";
    type Report = FindReport;
    fn rows(report: &FindReport) -> usize {
        report.rows.len()
    }
}

impl Shape for Validate {
    const NAME: &'static str = "validate";
    type Report = ValidateReport;
    fn rows(report: &ValidateReport) -> usize {
        match report {
            ValidateReport::Findings { page, .. } => page.rows.len(),
            _ => 0,
        }
    }
}

impl Shape for Search {
    const NAME: &'static str = "search";
    type Report = SearchReport;
    fn rows(report: &SearchReport) -> usize {
        report.page.rows.len()
    }
}

impl Shape for Describe {
    const NAME: &'static str = "describe";
    type Report = DescribeReport;
    fn rows(report: &DescribeReport) -> usize {
        report.rows.len()
    }
}

/// What a reading child reports: how many rows each shape of [`READ_SHAPES`]
/// answered, one line per shape, and the most the shapes raised the live heap
/// above the mark set once the attachment was ready.
#[derive(Debug, Default, Eq, PartialEq)]
struct ReadReport {
    answered: BTreeMap<String, usize>,
    heap_peak: Option<usize>,
}

impl ReadReport {
    /// Add the rows `report` answered to shape `S`'s tally.
    fn answered<S: Shape>(&mut self, report: &S::Report) {
        self.tally(S::NAME, S::rows(report));
    }

    /// Add `rows` to `shape`'s tally.
    fn tally(&mut self, shape: &str, rows: usize) {
        *self.answered.entry(shape.to_string()).or_default() += rows;
    }

    /// The lines the child prints.
    fn lines(&self) -> String {
        self.answered
            .iter()
            .map(|(shape, rows)| format!("read {shape} answered {rows}"))
            .chain(
                self.heap_peak
                    .map(|bytes| format!("read heap peak {bytes}")),
            )
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The report a child's output carries, refused where it carries none,
    /// where a line is no report, where a shape of [`READ_SHAPES`] is missing
    /// or one the mix does not name is present, and where the heap reading is
    /// missing: a reading over a shape that did not run is a reading over the
    /// shapes that did.
    fn from_stdout(stdout: &str) -> Result<Self, String> {
        let mut report = ReadReport::default();
        for line in stdout.lines().filter(|line| line.starts_with("read ")) {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            let number = |text: &str| {
                text.parse::<usize>()
                    .map_err(|problem| format!("the read harness reported `{line}`: {problem}"))
            };
            match tokens.as_slice() {
                ["read", "heap", "peak", bytes] => report.heap_peak = Some(number(bytes)?),
                ["read", shape, "answered", rows] => report.tally(shape, number(rows)?),
                _ => {
                    return Err(format!(
                        "the read harness reported `{line}`, which is no read report"
                    ));
                }
            }
        }
        if report.answered.is_empty() {
            return Err(format!("the read harness reported no read: {stdout}"));
        }
        if let Some(shape) = READ_SHAPES
            .iter()
            .find(|shape| !report.answered.contains_key(**shape))
        {
            return Err(format!("the read harness ran no {shape} shape: {stdout}"));
        }
        if let Some(stray) = report
            .answered
            .keys()
            .find(|shape| !READ_SHAPES.contains(&shape.as_str()))
        {
            return Err(format!(
                "the read harness reported a `{stray}` shape the mix does not name"
            ));
        }
        if report.heap_peak.is_none() {
            return Err(format!(
                "the read harness reported no heap reading: {stdout}"
            ));
        }
        Ok(report)
    }

    /// Refuse a report whose shapes did not answer `pinned`, naming the first
    /// shape of [`READ_SHAPES`] that answered otherwise.
    fn answers_as_pinned(&self, pinned: &[usize; 8]) -> Result<(), String> {
        for (shape, expected) in READ_SHAPES.iter().zip(pinned) {
            let answered = self.answered.get(*shape).copied().unwrap_or(0);
            if answered != *expected {
                return Err(format!(
                    "the read harness's {shape} shape answered {answered} rows where its request \
                     answers {expected}, so the peak is not a peak over the mix: {:?}",
                    self.answered
                ));
            }
        }
        Ok(())
    }
}

/// **The parent refuses a reading it cannot tie to every shape of the mix
/// answering as pinned.** A child whose output carries no report, a report
/// missing a shape or its heap reading, or one of a shape answering other than
/// its request answers fails the bar rather than passing it as a cheap read; a
/// report the child printed is read back as printed.
#[test]
fn a_read_reading_stands_only_on_a_report_of_every_shape_answering_as_pinned() {
    let pinned = pinned_answers("realistic");
    let mut printed = ReadReport::default();
    for (shape, rows) in READ_SHAPES.iter().zip(pinned) {
        printed.tally(shape, rows);
    }
    printed.heap_peak = Some(4096);
    let read = ReadReport::from_stdout(&format!("running 1 test\n{}\ntest ok\n", printed.lines()));
    assert_eq!(read, Ok(printed));
    assert_eq!(read.map(|read| read.answers_as_pinned(&pinned)), Ok(Ok(())));

    let missing = ReadReport::from_stdout("running 1 test\ntest ok\n").expect_err("no report");
    assert!(missing.contains("reported no read"), "{missing}");

    let mut short = ReadReport::default();
    for shape in &READ_SHAPES[1..] {
        short.tally(shape, 3);
    }
    short.heap_peak = Some(4096);
    let absent = ReadReport::from_stdout(&short.lines()).expect_err("a shape the child never ran");
    assert!(absent.contains("ran no find shape"), "{absent}");

    let mut unmeasured = ReadReport::default();
    for shape in READ_SHAPES {
        unmeasured.tally(shape, 3);
    }
    let unmeasured =
        ReadReport::from_stdout(&unmeasured.lines()).expect_err("a report with no heap reading");
    assert!(unmeasured.contains("no heap reading"), "{unmeasured}");

    // A search swapped for a count that answered one tally still reports a
    // search line; its number is not the search's.
    let mut swapped = ReadReport::default();
    for (shape, rows) in READ_SHAPES.iter().zip(pinned) {
        swapped.tally(shape, if *shape == Search::NAME { 1 } else { rows });
    }
    let swapped = swapped
        .answers_as_pinned(&pinned)
        .expect_err("a shape answering other than pinned");
    assert!(
        swapped.contains("search shape answered 1 rows"),
        "{swapped}"
    );

    let garbled =
        ReadReport::from_stdout("read everything\n").expect_err("a line that is no report");
    assert!(garbled.contains("no read report"), "{garbled}");
}
