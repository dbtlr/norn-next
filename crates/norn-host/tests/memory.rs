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
//! at `realistic` each report it, and a bar bounds how many bytes the
//! `realistic` reading may exceed the `ambiguous` one by. The reading repeats
//! to within eight bytes, so the bound is a few KiB, and an id of eight bytes
//! kept for each of the 1,700 documents between the two profiles fails it. A
//! whole-process peak can absorb a vault's rows inside the headroom the attach
//! left, and the heap count cannot, because it counts what the code holds
//! rather than what the kernel mapped. **The heap reading is a high-water**,
//! the highest the heap stood and not the sum of what each shape held, so a
//! retention a shape builds after the find's pages are gone shows only once it
//! climbs past them.
//!
//! **A plan is held three ways, over one planning child.** The child previews
//! a `set --where`, a hub move with its cascade and a delete, and then applies
//! each as previewed. A ceiling holds its whole-process peak at `realistic`,
//! and two heap pairs hold, across the two profiles, what the previews alone
//! raised the heap by and what the whole mix with its applies did, because a
//! plan's memory is its operations and a fixed record per target, never the
//! vault. The previews' pair is held at the read pair's resolution, so an
//! eight-byte id kept for every document while planning fails it; the mix's
//! is coarser, because an apply through a live host moves its reading by
//! about 31 KB. Both readings are high-waters, so neither sees what planning
//! builds and frees under its own peak: work proportional to the vault while
//! planning is the counter lane's to refuse. A move's size-independence,
//! holding no copy of the document it moves, is barred by NORN-345, not here.
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

use std::path::{Path, PathBuf};
use std::time::Duration;

use attach::read::{FIND_LIMIT, bounded_find};
use norn_host::Answered;
use norn_store::{
    Change as StoreChange, ContentModel, DocumentFacts, DocumentPath as StorePath,
    IncrementProvenance,
};
use norn_testkit::heap;
use norn_testkit::process::{Run, Sandbox};
use norn_wire::{
    ApplyMode, ApplyParams, ApplyReport, AuthoredValue, ChangesetOutcome, Column, CountParams,
    CountReport, DeleteParams, DescribeParams, DescribeReport, DocumentPath, DocumentRow,
    ErrorEnvelope, FieldChange, FindParams, FindReport, GetParams, GetReport, GroupKey, LinkFamily,
    LinkHealth, MoveParams, MoveSubject, PlanDocument, Predicate, ResolutionTarget, ResolvedPlan,
    SearchParams, SearchReport, SetParams, TargetResult, ValidateParams, ValidateReport,
    VaultAddress, WriteTarget,
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
        "ambiguous" => [200, 6, 1, 10, 10, 25, 25, 15],
        "realistic" => [200, 6, 1, 5, 5, 25, 25, 15],
        other => panic!("no read answers are pinned for the `{other}` profile"),
    }
}

/// The vault schema the read subjects attach `realistic` under: the minimal
/// schema and a tag vocabulary that leaves two of the generated tags out and
/// reports them.
///
/// Under the minimal schema a generated tree's only other findings are the
/// link-health findings its links raise. The undeclared tags give the validate
/// shape findings to page over either tree, the way a vault with a vocabulary
/// it has outgrown carries them. The attach-only child of the ratio attaches
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

/// The stem the memory bar's hub write targets, and the directory its
/// in-links stand under.
const HUB_WRITE_STEM: &str = "memory-gate-hub";
const HUB_WRITE_DIR: &str = "memory-gate-hub-links";

/// How many documents already hold a bare-stem link to the hub the memory
/// bar's write resolves, fixed at both per-PR scales: the claim under test is
/// that a hub's write costs its own in-links, never the vault around it
/// (ADR 0027's bounded-working-memory obligation).
const HUB_WRITE_IN_LINKS: usize = 200;

/// The case a hub-writing child re-executes, which is the one that reads this
/// constant.
const HUB_WRITE_HARNESS_CASE: &str =
    "the_hub_write_heap_grows_inside_its_allowance_from_the_ambiguity_profile_to_the_gate_profile";

/// Write [`HUB_WRITE_IN_LINKS`] documents linking the hub by its bare stem
/// into `vault`'s tree, before anything attaches it.
fn plant_hub_in_links(vault: &attach::Vault) {
    for at in 0..HUB_WRITE_IN_LINKS {
        let path = vault.path().join(format!("{HUB_WRITE_DIR}/{at:04}.md"));
        std::fs::create_dir_all(path.parent().expect("a planted document's folder"))
            .expect("creating a planted document's folder");
        std::fs::write(&path, format!("See [[{HUB_WRITE_STEM}]].\n"))
            .expect("writing a planted hub in-link");
    }
}

/// **What a hub write adds to a process that already attached, as a
/// difference in heap bytes across the two per-PR scales (ADR 0027's
/// bounded-working-memory obligation).**
///
/// With [`HARNESS_ENV`] and its token set this process is the child: it
/// attaches the tree the variable names — which already holds
/// [`HUB_WRITE_IN_LINKS`] documents linking a hub nothing has written yet, so
/// the attach heal derives every one of them as broken — marks the heap once
/// the attachment is ready, and then writes the hub itself directly through
/// the store, the same write ADR 0027's re-decision runs inside, instead of
/// measuring anything. What is reported is the most that write raised the
/// heap above the mark, so the reading is the write's own and not the
/// attach's.
///
/// **This is a difference in bytes, not a ratio over a whole-process peak**,
/// following [`the_read_mix_heap_grows_inside_its_allowance_from_the_ambiguity_profile_to_the_gate_profile`]:
/// a peak resident-set ratio over an attach-and-write child is dominated by
/// the attach and the fixed cost of the child binary, so a re-decision that
/// held every link of the vault in memory instead of the hub's own
/// [`HUB_WRITE_IN_LINKS`] moved that ratio from 1.05 to 1.31 against a 1.6x
/// bar and still passed. The heap mark is taken after the attach and before
/// the write, so it sees none of that — only what the write itself holds.
///
/// `ambiguous` and `realistic` differ by 1,700 documents; the planted
/// neighborhood does not differ between them at all. A re-decision whose
/// working memory were the vault's links rather than the hub's own would
/// hold about 1,700 documents' worth more of them at `realistic`; this bar
/// holds that it does not.
#[test]
#[ignore = "memory-lane case: runs in the ci memory job, not the workspace suite"]
fn the_hub_write_heap_grows_inside_its_allowance_from_the_ambiguity_profile_to_the_gate_profile() {
    if let Some(root) = std::env::var_os(HARNESS_ENV) {
        hub_write_and_report(&attach::accepted_harness_root(&root, HARNESS_TOKEN_ENV));
        return;
    }

    let small = hub_write_heap_growth("hub-write-heap-ambiguous", "ambiguous");
    let large = hub_write_heap_growth("hub-write-heap-realistic", "realistic");
    let growth = large.saturating_sub(small);

    baselines::record(
        "heap the hub write holds above the attach, across the two per-PR scales",
        &[
            ("ambiguous, 300 documents (bytes)", small.to_string()),
            ("realistic, 2000 documents (bytes)", large.to_string()),
            (
                "realistic minus ambiguous (bytes)",
                (i128::from(large) - i128::from(small)).to_string(),
            ),
            (
                "growth allowance (bytes)",
                baselines::HUB_WRITE_HEAP_GROWTH_ALLOWANCE_BYTES.to_string(),
            ),
        ],
    );

    assert!(
        small > 0 && large > 0,
        "a hub write reported no heap reading above the attach, so the pair compares nothing"
    );
    assert!(
        baselines::fits(growth, baselines::HUB_WRITE_HEAP_GROWTH_ALLOWANCE_BYTES),
        "going from `ambiguous` (300 documents) to `realistic` (2000 documents) grew the heap \
         the hub write holds above the attach by {growth} bytes, past the {} byte allowance: \
         `ambiguous` held {small} bytes and `realistic` {large}",
        baselines::HUB_WRITE_HEAP_GROWTH_ALLOWANCE_BYTES,
    );
}

/// Generate `profile`'s tree with [`HUB_WRITE_IN_LINKS`] planted beside it,
/// attach it in a child that then writes the hub they all name, and hand back
/// the most the write raised that child's heap above the mark taken once the
/// attachment was ready.
fn hub_write_heap_growth(label: &str, profile: &str) -> u64 {
    let documents = norn_fixtures::Profile::by_name(profile)
        .unwrap_or_else(|| panic!("no profile named `{profile}`"))
        .docs;
    baselines::assert_the_profile_the_bars_were_authored_on();
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let harness = sandbox
        .install_binary(&std::env::current_exe().expect("this suite's own executable"))
        .expect("installing the harness");
    let root: PathBuf = sandbox.work_dir().join("attached");
    let vault = attach::Vault::generate(&root, profile);
    plant_hub_in_links(&vault);
    let token = attach::issue_harness_token(&root);

    let outcome = Run::new(&sandbox, &harness)
        .args([
            "--exact",
            HUB_WRITE_HARNESS_CASE,
            "--ignored",
            "--nocapture",
        ])
        .env(HARNESS_ENV, &root)
        .env(HARNESS_TOKEN_ENV, &token)
        .deadline(ATTACH_DEADLINE)
        .wait()
        .expect("running the hub-write harness");
    outcome.assert_success();
    assert!(
        !outcome.stdout_truncated,
        "the harness wrote more than the capture limit, so what came back is a prefix"
    );
    let reported = outcome.stdout_text();
    let expected = documents + HUB_WRITE_IN_LINKS + 1;
    assert!(
        reported.contains(&report_line(expected)),
        "`{profile}` holds {documents} documents, {HUB_WRITE_IN_LINKS} in-links were planted \
         beside them and the hub itself is one more, and the harness reported: {reported}"
    );
    let prefix = "hub write heap peak ";
    let heap_line = reported
        .lines()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("the harness reported no heap reading: {reported}"));
    heap_line[prefix.len()..]
        .trim()
        .parse()
        .unwrap_or_else(|problem| {
            panic!("the harness reported `{heap_line}` as a heap reading: {problem}")
        })
}

/// The harness: adopt the tree at `root`, attach it, mark the heap once the
/// attachment is ready, then write the hub [`HUB_WRITE_IN_LINKS`] planted
/// documents already link by its bare stem — directly through the store,
/// which is the write ADR 0027's re-decision runs inside — and report what
/// the vault now derives and the most that write raised the heap above the
/// mark.
///
/// **The mark is taken after the attachment is dropped and before the
/// write, and the peak is read before anything else runs.** The attachment
/// is dropped the same way the counter gate's direct-store writes drop it, so
/// no watcher thread allocates beside the measurement and its teardown lands
/// before the mark; the window between the mark and the reading holds the
/// write's own heap alone.
#[allow(clippy::disallowed_macros)] // The child's report is a machine-consumed stream its parent reads.
fn hub_write_and_report(root: &Path) {
    let vault = attach::Vault::adopt(root);
    {
        let host = vault.host();
        attach::attach_and_wait(&host, vault.name());
    }
    let mark = heap::Mark::set();
    let mut store = vault.store();
    let mut request = store.begin_request();
    let declared = request
        .vault_schema_pin()
        .expect("reading the pinned schema")
        .map_or_else(ContentModel::none, |pin| {
            ContentModel::under(pin.fingerprint)
        });
    request
        .apply_increment(
            IncrementProvenance::Derived,
            [StoreChange::Upsert(DocumentFacts::new(
                StorePath::new(&format!("{HUB_WRITE_STEM}.md")).expect("a document path"),
                HUB_WRITE_STEM,
                "the hub\n",
                8,
            ))],
            &[],
            &declared,
        )
        .expect("writing the hub");
    let peak = mark.peak_above();
    println!("{}", report_line(attach::derived_documents(&mut store)));
    println!("hub write heap peak {peak}");
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
/// [`the_read_mix_heap_grows_inside_its_allowance_from_the_ambiguity_profile_to_the_gate_profile`]'s
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

/// **What the read shapes hold on the heap, as a difference across two
/// scales.**
///
/// A reading child at each per-PR profile runs the same read mix and reports
/// the most its shapes raised the live heap above the attach beneath them.
/// `realistic` holds 1,700 more documents than `ambiguous`. A shape that
/// answers a page, a target or what a narrowing part admits holds about the
/// same bytes at both scales, and a shape that held something for each
/// document of the vault holds that many more of it at `realistic`. The heap
/// reading counts bytes the code asked for and repeats to within eight bytes
/// from run to run, so the bar bounds the difference in bytes rather than a
/// ratio: a ratio over the find's pages moves only once a retention climbs
/// past a fraction of them.
#[test]
#[ignore = "memory-lane case: runs in the ci memory job, not the workspace suite"]
fn the_read_mix_heap_grows_inside_its_allowance_from_the_ambiguity_profile_to_the_gate_profile() {
    let small = read_child("read-heap-ambiguous", "ambiguous").heap_peak;
    let large = read_child("read-heap-realistic", "realistic").heap_peak;
    let growth = large.saturating_sub(small);

    baselines::record(
        "heap the read mix holds across the two per-PR scales",
        &[
            ("ambiguous, 300 documents (bytes)", small.to_string()),
            ("realistic, 2000 documents (bytes)", large.to_string()),
            (
                "realistic minus ambiguous (bytes)",
                (i128::from(large) - i128::from(small)).to_string(),
            ),
            (
                "growth allowance (bytes)",
                baselines::READ_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES.to_string(),
            ),
        ],
    );

    assert!(
        small > 0 && large > 0,
        "a reading child reported no heap reading above its attach, so the pair compares nothing"
    );
    assert!(
        baselines::fits(growth, baselines::READ_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES),
        "going from `ambiguous` (300 documents) to `realistic` (2000 documents) grew the heap the \
         read mix holds by {growth} bytes, past the {} byte allowance: `ambiguous` held {small} \
         bytes and `realistic` {large}",
        baselines::READ_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES,
    );
}

/// The case a planning child re-executes, which is the one that reads this
/// constant.
const PLAN_HARNESS_CASE: &str = "the_gate_profile_plans_inside_its_memory_bar";

/// The frontmatter field the plan mix's `set --where` matches and rewrites.
/// No generated document carries it, so the documents it matches are the
/// planted ones alone, at both profiles.
const PLAN_SET_FIELD: &str = "memory_plan_wave";

/// How many documents the plan mix's `set --where` matches, fixed at both
/// per-PR scales.
const PLAN_SET_MATCHES: usize = 10;

/// The stem of the hub the plan mix moves, the folder its in-links stand
/// under, and the stem it is moved to. No generated document shares either
/// stem.
const PLAN_HUB_STEM: &str = "memory-plan-hub";
const PLAN_HUB_LINKS_DIR: &str = "memory-plan-hub-links";
const PLAN_MOVED_HUB_STEM: &str = "memory-plan-moved-hub";

/// How many documents link the plan mix's hub by its bare stem, so its move
/// plans a cascade of as many rewrites, fixed at both per-PR scales.
const PLAN_HUB_IN_LINKS: usize = 20;

/// The document the plan mix deletes, which no link names.
const PLAN_DELETED: &str = "memory-plan-delete/memory-plan-lonely.md";

/// What each planned write of the planning child writes, as the apply
/// answers its targets: the set's matches, the hub with its in-links' rewrites
/// and its new place, and the deleted document.
const PLAN_WRITES: [(&str, usize); 3] = [
    ("set", PLAN_SET_MATCHES),
    ("move", PLAN_HUB_IN_LINKS + 2),
    ("delete", 1),
];

/// Plant the planning child's subjects into `vault`'s tree, before anything
/// attaches it: [`PLAN_SET_MATCHES`] documents carrying [`PLAN_SET_FIELD`],
/// the hub with [`PLAN_HUB_IN_LINKS`] documents linking it by its bare stem,
/// and the document the mix deletes.
fn plant_plan_subjects(vault: &attach::Vault) {
    let plant = |at: &str, content: &[u8]| {
        let path = vault.path().join(at);
        std::fs::create_dir_all(path.parent().expect("a planted document's folder"))
            .expect("creating a planted document's folder");
        std::fs::write(&path, content).expect("writing a planted document");
    };
    for at in 0..PLAN_SET_MATCHES {
        plant(
            &format!("memory-plan-set/{at:04}.md"),
            format!("---\n{PLAN_SET_FIELD}: flip\n---\na matched document\n").as_bytes(),
        );
    }
    plant(&format!("memory-plan-hub/{PLAN_HUB_STEM}.md"), b"the hub\n");
    for at in 0..PLAN_HUB_IN_LINKS {
        plant(
            &format!("{PLAN_HUB_LINKS_DIR}/{at:04}.md"),
            format!("See [[{PLAN_HUB_STEM}]].\n").as_bytes(),
        );
    }
    plant(PLAN_DELETED, b"no link names me\n");
}

/// **The plan ceiling**, and the harness a planning child runs.
///
/// With [`HARNESS_ENV`] and its token set this process is the child: it
/// attaches the tree the variable names with the plan subjects planted beside
/// it, keeps it attached, and previews and applies every write of
/// [`PLAN_WRITES`] through the host's write verbs instead of measuring
/// anything. The peak this bars is the highest the process reached across the
/// attach and every plan.
#[test]
#[ignore = "memory-lane case: runs in the ci memory job, not the workspace suite"]
fn the_gate_profile_plans_inside_its_memory_bar() {
    if let Some(root) = std::env::var_os(HARNESS_ENV) {
        plan_and_report(&attach::accepted_harness_root(&root, HARNESS_TOKEN_ENV));
        return;
    }

    let peak = plan_child("plan-gate", "realistic").peak_rss;
    baselines::record(
        "gate profile plans",
        &[
            ("peak resident set (MiB)", baselines::mebibytes(peak)),
            (
                "peak resident set ceiling (MiB)",
                baselines::mebibytes(baselines::PLAN_PEAK_RSS_CEILING_BYTES),
            ),
        ],
    );
    assert!(
        peak > 0,
        "the planning child reported no peak resident set, so the ceiling holds no reading"
    );
    assert!(
        baselines::fits(peak, baselines::PLAN_PEAK_RSS_CEILING_BYTES),
        "attaching `realistic` and planning over it peaked at {} MiB against a {} MiB bar",
        baselines::mebibytes(peak),
        baselines::mebibytes(baselines::PLAN_PEAK_RSS_CEILING_BYTES)
    );
}

/// **Planning holds its operations and a fixed record per target, never the
/// vault**, as a difference in heap bytes across the two per-PR scales, read
/// before anything is applied.
///
/// A planning child at each profile marks the heap once the attachment is
/// ready, previews a `set --where` matching [`PLAN_SET_MATCHES`] documents, a
/// move of a hub whose [`PLAN_HUB_IN_LINKS`] in-links its cascade rewrites,
/// and a delete, holding the three resolved plans, and reads the most those
/// previews raised the live heap above the mark before its first apply. Every
/// write's targets are planted, the same at both profiles, so planning that
/// holds its operations and a record per target holds the same bytes at
/// `ambiguous` (300 documents) as at `realistic` (2000), and planning that
/// held something for every document of the vault holds 1,700 more of it at
/// `realistic`.
///
/// **No apply runs in the window**, so no watcher echo or commit moves the
/// reading, and it is held at the read pair's resolution: an eight-byte id
/// kept for every document while planning fails it, as
/// [`PLAN_PREVIEW_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES`](baselines::PLAN_PREVIEW_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES)
/// records.
///
/// **The reading is a high-water**, the blind spot
/// [`the_read_mix_heap_grows_inside_its_allowance_from_the_ambiguity_profile_to_the_gate_profile`]
/// declares: memory planning builds and frees under the previews' own peak,
/// about 164 KB above the attach, never raises the reading. Work proportional to the vault while planning is
/// the counter lane's to refuse, where
/// `counter_gate::a_where_apply_costs_the_same_at_both_scales` holds a
/// `set --where`'s steps flat across the same two scales. The high-water's
/// limit is watched under NORN-131, as the read pair's is.
#[test]
#[ignore = "memory-lane case: runs in the ci memory job, not the workspace suite"]
fn the_plan_previews_heap_grows_inside_its_allowance_from_the_ambiguity_profile_to_the_gate_profile()
 {
    let small = plan_child("plan-previews-ambiguous", "ambiguous").previews;
    let large = plan_child("plan-previews-realistic", "realistic").previews;
    hold_plan_pair(
        "the plan previews",
        small,
        large,
        baselines::PLAN_PREVIEW_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES,
    );
}

/// **A plan applied through a live host holds its operations and a fixed
/// record per target, never the vault**, as a difference in heap bytes across
/// the two per-PR scales.
///
/// The same planning child as
/// [`the_plan_previews_heap_grows_inside_its_allowance_from_the_ambiguity_profile_to_the_gate_profile`]
/// goes on to apply each plan exactly as previewed, and reports the most the
/// six requests raised the live heap above the same mark. An apply's
/// publications reach the watcher's threads while it commits, which moves
/// this reading between two states about 31 KB apart, so its allowance is
/// coarser than the previews': it fails a retention of about 24 bytes a
/// document where both readings sit in one state and about 43 where the
/// excursion lands on `ambiguous` alone, so an id kept for every document
/// passes it and only the previews' pair refuses one kept while planning, as
/// [`PLAN_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES`](baselines::PLAN_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES)
/// records.
///
/// **The reading is a high-water, and the mix's peak is high.** Memory
/// planning or applying builds and frees under it never raises the reading,
/// which at these profiles leaves a working set of about 166 KB unseen, up to
/// about 81 bytes for each of `realistic`'s documents. Work proportional to the vault is the counter
/// lane's to refuse, where
/// `counter_gate::a_where_apply_costs_the_same_at_both_scales` holds a
/// `set --where`'s steps flat across the same two scales. The high-water's
/// limit is watched under NORN-131, as the read pair's is.
#[test]
#[ignore = "memory-lane case: runs in the ci memory job, not the workspace suite"]
fn the_plan_mix_heap_grows_inside_its_allowance_from_the_ambiguity_profile_to_the_gate_profile() {
    let small = plan_child("plan-heap-ambiguous", "ambiguous").mix;
    let large = plan_child("plan-heap-realistic", "realistic").mix;
    hold_plan_pair(
        "the plan mix",
        small,
        large,
        baselines::PLAN_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES,
    );
}

/// Record a plan stretch's heap readings at the two per-PR scales and hold
/// the `realistic` one to at most `allowance` bytes over the `ambiguous` one.
fn hold_plan_pair(stretch: &str, small: u64, large: u64, allowance: u64) {
    let growth = large.saturating_sub(small);
    baselines::record(
        &format!("heap {stretch} raised above the attach, across the two per-PR scales"),
        &[
            ("ambiguous, 300 documents (bytes)", small.to_string()),
            ("realistic, 2000 documents (bytes)", large.to_string()),
            (
                "realistic minus ambiguous (bytes)",
                (i128::from(large) - i128::from(small)).to_string(),
            ),
            ("growth allowance (bytes)", allowance.to_string()),
        ],
    );
    assert!(
        small > 0 && large > 0,
        "a planning child reported no heap reading of {stretch} above its attach, so the pair \
         compares nothing"
    );
    assert!(
        baselines::fits(growth, allowance),
        "going from `ambiguous` (300 documents) to `realistic` (2000 documents) grew the heap \
         {stretch} raised by {growth} bytes, past the {allowance} byte allowance: `ambiguous` held \
         {small} bytes and `realistic` {large}",
    );
}

/// What a planning child's run comes to: the peak resident set the kernel
/// accounted to it, and the most each measured stretch raised the heap above
/// the mark set before it.
struct PlanReading {
    peak_rss: u64,
    previews: u64,
    mix: u64,
}

/// Generate `profile`'s tree with the plan subjects planted beside it, run the
/// planning child over it, and hand back what the child's run came to.
///
/// **A reading is only a statement about plans that landed.** The child
/// reports how many targets each write's apply wrote, and the parent holds
/// that to [`PLAN_WRITES`]: a write that resolved nothing, or a report
/// missing one, fails here rather than reading as a cheap plan.
fn plan_child(label: &str, profile: &str) -> PlanReading {
    let documents = norn_fixtures::Profile::by_name(profile)
        .unwrap_or_else(|| panic!("no profile named `{profile}`"))
        .docs;
    let (peak_rss, reported) = child_run(label, profile, PLAN_HARNESS_CASE, plant_plan_subjects);
    let report = PlanReport::from_stdout(&reported).unwrap_or_else(|problem| panic!("{problem}"));
    if let Err(problem) = report.writes_as_pinned() {
        panic!("over `{profile}`, {problem}");
    }
    // Every planted subject stands once the plans land, save the one the
    // mix deletes.
    let planted = PLAN_SET_MATCHES + 1 + PLAN_HUB_IN_LINKS + 1;
    assert!(
        reported.contains(&report_line(documents + planted - 1)),
        "`{profile}` holds {documents} documents, {planted} plan subjects were planted beside \
         them and the mix deletes one, and the harness reported: {reported}"
    );
    let heap =
        |subject: &str| u64::try_from(report.heap(subject)).expect("a heap reading fits a u64");
    let reading = PlanReading {
        peak_rss,
        previews: heap("previews"),
        mix: heap("mix"),
    };
    baselines::record(
        &format!("heap each plan stretch held ({label}, {profile})"),
        &[
            ("previews (bytes)", reading.previews.to_string()),
            ("mix (bytes)", reading.mix.to_string()),
            (
                "peak resident set (MiB)",
                baselines::mebibytes(reading.peak_rss),
            ),
        ],
    );
    reading
}

/// The planning harness: adopt the tree at `root`, attach it, keep it
/// attached, and preview then apply each write of [`PLAN_WRITES`] through the
/// host's write verbs, each apply landing exactly the plan its preview
/// answered.
///
/// **One heap mark, read twice.** It is set once the attachment is ready and
/// read once the three previews have resolved, before the first apply, and
/// again once the three applies have landed. Each reading is the most the
/// requests up to it raised the live heap above what the attached host held
/// as the first began.
///
/// **The mix previews all three writes before it applies any.** An apply's
/// publication comes back to the live host as a watcher echo, which the host
/// reads to confirm it as the apply's own, and that confirmation runs as a
/// job on the entry's one worker slot whenever the watcher delivers it. A
/// preview runs on this thread beside it, so a preview after an apply stacks
/// on that echo or not by timing alone, and the mix read up to 66 KB apart
/// across runs that way. With every preview ahead of every apply, the
/// previews' reading holds no apply's work at all, and what is left to move
/// the mix's is the watcher's own threads taking an apply's publications in
/// while it commits, which moves it between two states about 31 KB apart.
/// The three resolved plans are held together across the applies, which is
/// what the mix holds of them.
#[allow(clippy::disallowed_macros)] // The child's report is a machine-consumed stream its parent reads.
fn plan_and_report(root: &Path) {
    let vault = attach::Vault::adopt(root);
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let address = || VaultAddress::name(vault.name().clone());
    let document = |at: &str| DocumentPath::new(at).expect("a document path");
    let mut lines = Vec::new();
    let mut landed = |write: &str, plan: ResolvedPlan| {
        let targets = applied_plan(write, &host, plan);
        lines.push(format!("plan {write} wrote {targets}"));
    };

    let mark = heap::Mark::set();
    let set = previewed_plan(
        "set",
        host.set(SetParams::new(
            address(),
            ApplyMode::Preview,
            WriteTarget::matching([Predicate::equal_to(PLAN_SET_FIELD, "flip")]),
            vec![FieldChange::set(
                PLAN_SET_FIELD,
                AuthoredValue::string("flipped"),
            )],
        )),
    );
    let hub = previewed_plan(
        "move",
        host.move_path(MoveParams::new(
            address(),
            ApplyMode::Preview,
            MoveSubject::document(
                document(&format!("memory-plan-hub/{PLAN_HUB_STEM}.md")),
                document(&format!("memory-plan-moved/{PLAN_MOVED_HUB_STEM}.md")),
            ),
        )),
    );
    assert_eq!(
        hub.operations[0].cascade.len(),
        PLAN_HUB_IN_LINKS,
        "the hub's move planned a cascade other than one rewrite per in-link"
    );
    let deleted = previewed_plan(
        "delete",
        host.delete(DeleteParams::new(
            address(),
            ApplyMode::Preview,
            document(PLAN_DELETED),
        )),
    );
    let previews = mark.peak_above();
    landed("set", set);
    landed("move", hub);
    landed("delete", deleted);
    let mix = mark.peak_above();

    let mut store = vault.store();
    println!("{}", report_line(attach::derived_documents(&mut store)));
    for line in lines {
        println!("{line}");
    }
    println!("plan heap previews {previews}");
    println!("plan heap mix {mix}");
}

/// The plan a write verb's preview answered.
fn previewed_plan(
    write: &str,
    answered: Result<norn_host::PendingApply, ErrorEnvelope>,
) -> ResolvedPlan {
    let answered = answered
        .unwrap_or_else(|refusal| panic!("the {write} preview was refused: {refusal:?}"))
        .wait()
        .unwrap_or_else(|refusal| panic!("the {write} preview was refused: {refusal:?}"));
    let ApplyReport::Previewed { plan, .. } = answered.report else {
        panic!("the {write} preview answered {:?}", answered.report);
    };
    plan
}

/// Apply `plan` through `host`, exactly as previewed, and hand back how many
/// targets it wrote.
///
/// **Every target must come back as written.** An apply answers a target
/// that already stood at its after-state as found rather than written, and
/// such a target cost the apply no write; counting it would let a plan that
/// landed nothing pass as one that landed its targets.
fn applied_plan(write: &str, host: &attach::ServingHost, plan: ResolvedPlan) -> usize {
    let answered = host
        .apply(ApplyParams::new(
            ApplyMode::Apply,
            PlanDocument::resolved(plan),
        ))
        .unwrap_or_else(|refusal| panic!("the {write} apply was refused: {refusal:?}"))
        .wait()
        .unwrap_or_else(|refusal| panic!("the {write} apply was refused: {refusal:?}"));
    let ApplyReport::Applied {
        changeset, targets, ..
    } = answered.report
    else {
        panic!("the {write} apply answered {:?}", answered.report);
    };
    assert_eq!(
        changeset,
        ChangesetOutcome::Committed,
        "the {write} apply committed no changeset"
    );
    let unwritten: Vec<_> = targets
        .iter()
        .filter(|target| target.result != TargetResult::Wrote)
        .collect();
    assert!(
        unwritten.is_empty(),
        "the {write} apply came back with targets it did not write: {unwritten:?}"
    );
    targets.len()
}

/// What a planning child reports: how many targets each write of
/// [`PLAN_WRITES`] wrote, and the heap each measured stretch raised.
#[derive(Debug, Default, Eq, PartialEq)]
struct PlanReport {
    wrote: std::collections::BTreeMap<String, usize>,
    heap: std::collections::BTreeMap<String, usize>,
}

impl PlanReport {
    /// The heap stretches a planning child measures.
    const STRETCHES: [&str; 2] = ["previews", "mix"];

    /// The report a child's output carries, refused where a `plan` line is
    /// no report, where a write or a stretch is missing, or where one the
    /// child does not run is present.
    fn from_stdout(stdout: &str) -> Result<Self, String> {
        let mut report = PlanReport::default();
        for line in stdout.lines().filter(|line| line.starts_with("plan ")) {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            let number = |text: &str| {
                text.parse::<usize>()
                    .map_err(|problem| format!("the plan harness reported `{line}`: {problem}"))
            };
            match tokens.as_slice() {
                ["plan", "heap", stretch, bytes] => {
                    report.heap.insert((*stretch).to_string(), number(bytes)?);
                }
                ["plan", write, "wrote", targets] => {
                    report.wrote.insert((*write).to_string(), number(targets)?);
                }
                _ => {
                    return Err(format!(
                        "the plan harness reported `{line}`, which is no plan report"
                    ));
                }
            }
        }
        let writes: Vec<&str> = PLAN_WRITES.iter().map(|(write, _)| *write).collect();
        let named = |names: &[&str], held: &std::collections::BTreeMap<String, usize>| {
            names.len() == held.len() && names.iter().all(|name| held.contains_key(*name))
        };
        if !named(&writes, &report.wrote) || !named(&Self::STRETCHES, &report.heap) {
            return Err(format!(
                "the plan harness reported writes {:?} and heap stretches {:?} where it runs \
                 {writes:?} and {:?}: {stdout}",
                report.wrote.keys().collect::<Vec<_>>(),
                report.heap.keys().collect::<Vec<_>>(),
                Self::STRETCHES,
            ));
        }
        Ok(report)
    }

    /// Refuse a report whose writes did not write [`PLAN_WRITES`], naming the
    /// first that wrote otherwise.
    fn writes_as_pinned(&self) -> Result<(), String> {
        for (write, expected) in PLAN_WRITES {
            let wrote = self.wrote.get(write).copied().unwrap_or(0);
            if wrote != expected {
                return Err(format!(
                    "the plan harness's {write} wrote {wrote} targets where its plan writes \
                     {expected}, so the reading is not a reading over the plan: {:?}",
                    self.wrote
                ));
            }
        }
        Ok(())
    }

    /// The heap `stretch` raised, which a parsed report carries.
    fn heap(&self, stretch: &str) -> usize {
        self.heap[stretch]
    }
}

/// **The parent refuses a plan reading it cannot tie to every write landing
/// as pinned.** A report missing a write or a heap stretch, or one whose write
/// landed other than its plan writes, fails the bar rather than passing as a
/// cheap plan.
#[test]
fn a_plan_reading_stands_only_on_a_report_of_every_write_landing_as_pinned() {
    let printed = |wrote: &dyn Fn(&str, usize) -> Option<usize>, stretches: &[&str]| {
        PLAN_WRITES
            .iter()
            .filter_map(|(write, targets)| {
                wrote(write, *targets).map(|targets| format!("plan {write} wrote {targets}"))
            })
            .chain(
                stretches
                    .iter()
                    .map(|stretch| format!("plan heap {stretch} 4096")),
            )
            .collect::<Vec<_>>()
            .join("\n")
    };

    let whole = PlanReport::from_stdout(&printed(
        &|_, targets| Some(targets),
        &PlanReport::STRETCHES,
    ))
    .expect("a report of every write and stretch");
    assert_eq!(whole.writes_as_pinned(), Ok(()));
    assert_eq!(whole.heap("previews"), 4096);

    let short = PlanReport::from_stdout(&printed(
        &|write, targets| (write != "delete").then_some(targets),
        &PlanReport::STRETCHES,
    ))
    .expect_err("a write the child never ran");
    assert!(short.contains("where it runs"), "{short}");

    let unmeasured = PlanReport::from_stdout(&printed(&|_, targets| Some(targets), &["mix"]))
        .expect_err("a stretch the child never measured");
    assert!(unmeasured.contains("heap stretches"), "{unmeasured}");

    let nothing = PlanReport::from_stdout(&printed(
        &|write, targets| Some(if write == "set" { 0 } else { targets }),
        &PlanReport::STRETCHES,
    ))
    .expect("a report of every write and stretch")
    .writes_as_pinned()
    .expect_err("a write that wrote nothing");
    assert!(nothing.contains("set wrote 0 targets"), "{nothing}");

    let garbled = PlanReport::from_stdout("plan everything\n").expect_err("no plan report");
    assert!(garbled.contains("no plan report"), "{garbled}");
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
        .heap_peak()
        .expect("a parsed report carries its heap reading");
    let mut readings: Vec<(&str, String)> = READ_SHAPES
        .iter()
        .map(|shape| (*shape, read.rows(shape).to_string()))
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
fn child_peak(label: &str, profile: &str, schema: &[u8], case: &str) -> (u64, String) {
    child_run(label, profile, case, |vault| {
        std::fs::write(vault.path().join(".norn/schema.yaml"), schema)
            .expect("write the vault schema");
    })
}

/// Generate `profile`'s tree, let `prepare` add to it, run `case` over it in a
/// child, and hand back the peak resident set the kernel accounted to that
/// child with what it printed.
///
/// The harness binary is installed into the sandbox before it runs, because the
/// artifact cargo built is a file a concurrent build may rewrite. The tree goes
/// inside the sandbox too, so it is removed with it.
fn child_run(
    label: &str,
    profile: &str,
    case: &str,
    prepare: impl FnOnce(&attach::Vault),
) -> (u64, String) {
    baselines::assert_the_profile_the_bars_were_authored_on();
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let harness = sandbox
        .install_binary(&std::env::current_exe().expect("this suite's own executable"))
        .expect("installing the harness");
    let root: PathBuf = sandbox.work_dir().join("attached");
    let vault = attach::Vault::generate(&root, profile);
    prepare(&vault);
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

    read.measured(mark.peak_above());
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
/// **The report type ties a shape to its verb.** [`ReadReport::answered`] is
/// the only way the child records a shape's rows, and it takes them from a
/// report of the type the shape names. A report of another verb recorded
/// under a shape's name answers another number of rows, which
/// [`pinned_answers`] refuses; the pins are also what tell apart the two pairs
/// of shapes that share a verb (find and backlinks, count and links-to).
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

/// The reading child's report, behind a module boundary so the child can
/// record a shape's rows only through the typed [`ReadReport::answered`]: the
/// tally by name that reading a report back uses is private to this module.
mod read_report {
    use std::collections::BTreeMap;

    use super::{READ_SHAPES, Shape};

    /// What a reading child reports: how many rows each shape of [`READ_SHAPES`]
    /// answered, one line per shape, and the most the shapes raised the live heap
    /// above the mark set once the attachment was ready.
    #[derive(Debug, Default, Eq, PartialEq)]
    pub struct ReadReport {
        answered: BTreeMap<String, usize>,
        heap_peak: Option<usize>,
    }

    impl ReadReport {
        /// Add the rows `report` answered to shape `S`'s tally.
        pub fn answered<S: Shape>(&mut self, report: &S::Report) {
            self.tally(S::NAME, S::rows(report));
        }

        /// Record the most the shapes raised the live heap above the mark.
        pub fn measured(&mut self, heap_peak: usize) {
            self.heap_peak = Some(heap_peak);
        }

        /// How many rows `shape` answered, zero where it answered none.
        pub fn rows(&self, shape: &str) -> usize {
            self.answered.get(shape).copied().unwrap_or(0)
        }

        /// The most the shapes raised the live heap above the mark, where the
        /// report carries it.
        pub fn heap_peak(&self) -> Option<usize> {
            self.heap_peak
        }

        /// Add `rows` to `shape`'s tally, by name. Only a report read back from
        /// the child's output tallies this way; the child tallies through
        /// [`ReadReport::answered`].
        fn tally(&mut self, shape: &str, rows: usize) {
            *self.answered.entry(shape.to_string()).or_default() += rows;
        }

        /// The lines the child prints.
        pub fn lines(&self) -> String {
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
        pub fn from_stdout(stdout: &str) -> Result<Self, String> {
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
        pub fn answers_as_pinned(&self, pinned: &[usize; 8]) -> Result<(), String> {
            for (shape, expected) in READ_SHAPES.iter().zip(pinned) {
                let answered = self.rows(shape);
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
}

use read_report::ReadReport;

/// **The parent refuses a reading it cannot tie to every shape of the mix
/// answering as pinned.** A child whose output carries no report, a report
/// missing a shape or its heap reading, or one of a shape answering other than
/// its request answers fails the bar rather than passing it as a cheap read; a
/// report the child printed is read back as printed.
#[test]
fn a_read_reading_stands_only_on_a_report_of_every_shape_answering_as_pinned() {
    let pinned = pinned_answers("realistic");
    let printed = |rows: &dyn Fn(&str, usize) -> Option<usize>, heap: Option<usize>| {
        READ_SHAPES
            .iter()
            .zip(pinned)
            .filter_map(|(shape, pinned)| {
                rows(shape, pinned).map(|rows| format!("read {shape} answered {rows}"))
            })
            .chain(heap.map(|bytes| format!("read heap peak {bytes}")))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let read = ReadReport::from_stdout(&format!(
        "running 1 test\n{}\ntest ok\n",
        printed(&|_, rows| Some(rows), Some(4096))
    ))
    .expect("a report of every shape");
    assert_eq!(read.answers_as_pinned(&pinned), Ok(()));
    assert_eq!(read.heap_peak(), Some(4096));
    assert_eq!(ReadReport::from_stdout(&read.lines()), Ok(read));

    let missing = ReadReport::from_stdout("running 1 test\ntest ok\n").expect_err("no report");
    assert!(missing.contains("reported no read"), "{missing}");

    let short = printed(&|shape, _| (shape != Find::NAME).then_some(3), Some(4096));
    let absent = ReadReport::from_stdout(&short).expect_err("a shape the child never ran");
    assert!(absent.contains("ran no find shape"), "{absent}");

    let unmeasured = ReadReport::from_stdout(&printed(&|_, _| Some(3), None))
        .expect_err("a report with no heap reading");
    assert!(unmeasured.contains("no heap reading"), "{unmeasured}");

    // A search swapped for a count that answered one tally still reports a
    // search line; its number is not the search's.
    let swapped = printed(
        &|shape, rows| Some(if shape == Search::NAME { 1 } else { rows }),
        Some(4096),
    );
    let swapped = ReadReport::from_stdout(&swapped)
        .expect("a report of every shape")
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
