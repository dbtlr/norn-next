//! The authored baselines this crate's measurement suites assert against.
//!
//! **Every value here is authored, and it moves only by a reviewed edit.** The
//! file is the trend's whole memory: a measurement that drifts fails rather
//! than quietly redefining what normal is. There is no history to fetch and no
//! store to consult — a number moves when somebody changes it in a diff, with
//! the grounds beside it.
//!
//! What binds mechanically is the comparison: a reading past the value here
//! fails. The direction the values travel is review-held — nothing forbids
//! raising one, and a reviewer reading the diff is what asks for the claim that
//! the subject now costs more. Lowering one needs no new argument.
//!
//! # Where the readings come from
//!
//! Every band below is the pinned toolchain's unoptimized build. The
//! attachment and read bands are the peak resident set the kernel accounted to
//! the child process that attached, or attached and read, read with no cargo
//! feature named, and the read heap band is the difference between two
//! reading children's own counts of the live heap their read shapes raised;
//! the soak bands are
//! that child's own samples of itself over a long mixed load, read with
//! `induced-failure` on, which is what arms the recovery that load is required
//! to trip. The two subjects are set out beside
//! [`assert_the_profile_the_bars_were_authored_on`]. Repeated local readings
//! cover **macos-arm64** natively.
//!
//! **The platform that gates is `ubuntu-latest` x86_64-glibc.** Every authored
//! band below carries its hosted readings beside the local ones, the same way
//! the generator's baselines carry both architectures they were measured on,
//! except [`FD_BUDGET`] and [`READ_GATE_ROUNDS_AFTER_THE_FIRST_PER_ACQUISITION`],
//! counts read on macos-arm64 that hold the same on both platforms, and the
//! seven `APPLY_*` read budgets, counts of the acts an apply performs read on
//! linux-x86_64, which no platform moves. The folding-root spelling-listing
//! bar runs on macOS ARM64, with its distinct-root control on Linux. A
//! band un-authored back to `None` for recalibration carries the readings it
//! had, because what a recalibration window gathers is the next set. The soak
//! bands' hosted readings come off the nightly lane's hour-long load at the
//! ≥5k profile; the local
//! readings beside them are the same case at the short default duration, which
//! is what a developer runs.
//!
//! **Four bands here can spell a calibration state, and none of them is in
//! one.** [`SOAK_PEAK_RSS_CEILING_BYTES`], [`SOAK_HIGH_WATER_RSS_CEILING_BYTES`],
//! [`SOAK_SETTLE_CEILING`] and [`SOAK_QUIESCENT_FD_RETENTION`] are `Option`s:
//! `Some` bars the run, and `None` records the reading, bars nothing, and
//! stamps its own runs non-qualifying
//! through the ledger's exit-bar registry
//! (`norn_testkit::certification::ledger::NAMED_EXIT_BARS`, held to these
//! constants by a test in `settle.rs`) — so a calibration window never counts
//! toward lockdown's five.
//!
//! **Every constant in this file is in that registry, and so is every constant
//! in the other crates' baselines files.** A value here is a value a
//! measurement suite asserts a reading against, which is what an exit bar is,
//! and the registry's sweep reads every crate's baselines file and refuses a
//! constant it does not name — so a bar cannot escape the roll by being
//! always-authored. **The second escape is closed by a rule, not by the
//! sweep.** A threshold declared in a suite file rather than in a baselines
//! file sits where the sweep does not read, and what keeps one out of there is
//! the rule that a bar lives in a baselines file — held by review, not by a
//! test. An always-authored value such as [`FD_BUDGET`] carries
//! `armed: true` and is held to it; what the registry is for is the roll of
//! what the exit contract measures, and a roll that listed only the bars
//! mid-calibration would be a roll of the exceptions.
//!
//! # Platform scope
//!
//! **The bands below are authored against `ubuntu-latest` x86_64-glibc — the
//! Linux measurement lane — except where a constant says otherwise, and each
//! one says which.** A reading taken on another machine is a reading of that
//! machine: page size, allocator and runner image move these numbers without
//! the subject costing more. Local macos-arm64 readings stand beside the hosted
//! ones as the band a developer sees, and nothing gates on them.
//!
//! **The macOS certification lane evaluates no bar here at all.** It runs the
//! certification cases and no measurement step, which the comment over
//! `.github/workflows/certify.yml`'s `certification-macos` job states in those
//! words: the bars in this file are authored against `ubuntu-latest`, and a
//! second platform reading them would judge one machine's numbers on another's.
//! That lane's record therefore carries a watcher-backend answer and no
//! reading, and nothing in it should be read as a measurement of the candidate.
//!
//! Five integration binaries compile this module: `memory.rs` for the per-PR
//! memory lane, `counter_gate.rs` for the per-PR counter lane, `fd_budget.rs`
//! for the per-PR workspace suite, `host_soak.rs` for the scheduled lane and
//! `settle.rs` for the scheduled lane's churn clock. Each asserts against
//! the values its lane owns, so the remainder being unused in any one of them
//! is the layout rather than a defect. The values stay in one file because the
//! file is the trend's whole memory, and a reviewer reads it as one diff.
#![allow(dead_code)]
// The rendering helpers are re-exported for every lane at once, so a binary
// that uses two of the four is the layout rather than a stale import.
#![allow(unused_imports)]

use std::time::Duration;

/// Peak resident set attaching the `realistic` profile must stay under.
///
/// The subject is a child process that adopts a tree the parent generated,
/// attaches it through a production host, waits for the attachment to become
/// ready, and detaches — so the reading is the attach's cost plus the test
/// binary that carried it, and generation is charged to nobody.
///
/// Observed on macos-arm64 on 2026-09-26: **23.78–24.87 MiB**, this host's
/// readings of this lane's attaches at this profile across five runs: 23.78
/// to 24.73 from this case, 24.17 to 24.71 from the pair's `realistic` child,
/// and 23.79 to 24.87 from the attach-only child of
/// [`READ_OVER_ATTACH_PEAK_RSS_PER_MILLE`]. On 2026-09-23 the same host read
/// **22.40–23.39 MiB** over eight runs. An indicative band, not a fixed
/// measurement, so a rerun lands near an edge rather than the middle. On
/// `ubuntu-latest` x86_64-glibc the hosted readings of this lane's attaches at
/// this profile span **16.73–24.24 MiB** over nineteen readings: 17.69 and
/// 18.26 at the first hosted run, 16.73 and 17.00 in the Layer 1 acceptance
/// pass, 17.73/18.30, 17.96/18.18, 18.82/18.37 and 17.95/17.91 across four
/// consecutive `memory invariant` runs, then 21.40 from this case and 21.53
/// from the attach-only child of [`READ_OVER_ATTACH_PEAK_RSS_PER_MILLE`] in
/// `memory invariant` run 35858094556, 23.95 from this case, 24.13 from the
/// pair's `realistic` child and 24.24 from that attach-only child in run
/// 36215052561, and 24.22 from this case and 24.19 from that attach-only child
/// in run 36218303281. Every hosted reading before run 36215052561 sits below
/// the local band of its day, as 4 KiB pages against 16 KiB predict; the
/// readings of that run and the one after it, 23.95–24.24, sit inside the
/// local band above.
///
/// **The hosted spread is 7.5 MiB wide, and it has risen twice.** The first
/// twelve readings sit inside 16.73–18.82, where one run's two attaches differ by
/// as much as 0.57 MiB; the next run reads about 2.7 MiB above that, and the
/// one after it about 2.7 MiB above that again, where the run after that
/// stays. The local band rose by the same order between its two days. The rise
/// is recorded for the trend this file keeps, and the number that bounds the
/// readings is the top one.
///
/// The ceiling stays at 40 MiB, which is 1.61x the highest reading, local or
/// hosted, 24.87 MiB, and 1.65x the highest hosted one, 24.24 MiB. The readings it holds are
/// whole-process peaks, so each carries the child binary and its runtime as a
/// fixed addend that a page-size or allocator change moves without the
/// attachment costing more — and a bar that flakes teaches people to rerun
/// rather than to look. What a vault-shaped cost would read here is multiples
/// of the band, not the 2 MiB of spread between platforms.
///
/// **Platform scope: the Linux measurement lane.** The per-PR `memory
/// invariant` job on `ubuntu-latest` x86_64-glibc is where this gates; the
/// macos-arm64 band above is what a developer running the same case sees, and
/// no scheduled lane evaluates it on that platform.
///
/// What it forbids is an attachment whose cost is the vault. The heal walks the
/// tree and commits it in bounded changesets, so what stays resident is one
/// changeset's documents rather than the vault's — an attachment that held its
/// walk would carry all 2000 documents' facts at once instead.
pub const ATTACH_PEAK_RSS_CEILING_BYTES: u64 = 40 * 1024 * 1024;

/// How much of the `ambiguous` profile's attach peak the `realistic` profile's
/// attach peak may reach, per mille.
///
/// **The flatness invariant in the per-PR lane.** Both scales are under 5k
/// documents, so both are per-PR work under ADR 0004's by-kind split, and the
/// pair is what a ceiling cannot be: a ceiling passes anything that fits under
/// it, and a ratio fails the moment the two scales stop moving together.
///
/// `ambiguous` emits 300 documents and `realistic` 2000 — a 6.7x spread in the
/// documents a vault-shaped cost would be a function of. Attachment's peak is
/// not that: the walk streams, the store commits in changesets bounded well
/// below either profile, and what is left growing with the vault is how many
/// of those changesets are committed. The observed ratios on 2026-09-26 are
/// **1.08–1.10 on macos-arm64** over five runs, against attach peaks of
/// 22.12–22.50 MiB at `ambiguous` and 24.17–24.71 MiB at `realistic`, and on
/// 2026-09-23 read 1.09–1.15 against 20.31–20.64 and 22.40–23.39 MiB; the
/// hosted band on `ubuntu-latest` x86_64-glibc is **1.10–1.26** over seven
/// runs — 1.21 (14.60 against 17.69 MiB) at the first, then 1.19, 1.17, 1.26
/// and 1.13 across four consecutive `memory invariant` runs, against bases of
/// 14.83–15.82 MiB, 1.12 (21.54 against 24.13 MiB) in run 36215052561, and
/// 1.10 in run 36218303281. A
/// lower base and a wider ratio band, the shape a smaller page size predicts:
/// the fixed addend both readings carry shrinks, so the same difference between
/// the two scales is a larger multiple of it.
///
/// The bar is 1.6, which leaves a quarter of headroom over the highest reading
/// while staying far below the 6.7x a vault-shaped cost would show.
///
/// **Platform scope: the Linux measurement lane**, the same one
/// [`ATTACH_PEAK_RSS_CEILING_BYTES`] gates in — a ratio between two readings
/// cancels the fixed addend but not the allocator that produced both.
pub const ATTACH_PAIR_PEAK_RSS_PER_MILLE: u64 = 1_600;

/// Peak resident set attaching the `realistic` profile and running the read
/// mix over it must stay under.
///
/// The subject is a child process that adopts a tree the parent generated
/// under a schema whose tag vocabulary leaves two of the generated tags out
/// and reports them, attaches it through a production host, keeps it
/// attached, and runs the read mix through the host's read verbs, each shape
/// under its own live hold: a find narrowed to the documents whose `type` is
/// not `meeting`, eight pages of 25 rows newest `created` first, each row
/// carrying its fields, its tags and its links; a count grouped by `type`; a
/// get of a bare stem resolved as a suffix, answered as its whole record; a
/// links-to count and a backlinks find narrowed to the documents linking to
/// that stem; a validate page of findings narrowed by a path part to one
/// top-level directory; a lexical search page of 25 hits; and a describe page
/// of facets. So the reading is the attach's cost, the live host's, and the
/// highest any shape reached, plus the test binary that carried them. The
/// kernel reports one peak per child, which is why the shapes share one.
///
/// Observed on macos-arm64 on 2026-09-26: **27.56–29.04 MiB** over ten
/// readings, five from this bar's own case and five from the reading child of
/// the ratio beside it, across five runs of the lane. **The local band is
/// indicative, not a fixed measurement**: the same subject with an unfiltered
/// find and a validate over the whole vault read 28.43–28.95 MiB over four
/// runs on 2026-09-25 and 29.28 MiB in a fifth, the highest read reading on
/// either platform. On `ubuntu-latest` x86_64-glibc the `memory invariant`
/// run 36218303281 read the mix at **26.59 MiB** from this case and 26.67 from
/// the ratio's reading child, and run 36215052561 read the unfiltered subject
/// at 26.06 and 26.12. Trend: a find-only read, one child reading eight pages
/// and no other shape, read 25.10–25.85 MiB over twelve local readings
/// through 2026-09-23, and 21.83 and 21.67 MiB hosted in run 35858094556.
///
/// **This ceiling bounds the process.** It is 48 MiB, by the rule that it
/// keeps the proportion [`ATTACH_PEAK_RSS_CEILING_BYTES`] keeps over its
/// highest reading: that ceiling is 1.61x the highest attach reading, local or
/// hosted, 24.87 MiB, and the highest read reading, local or hosted, is 29.28
/// MiB, which at 1.61x is 47.14 MiB, rounded up to a whole MiB. The local
/// readings it is scaled from sit above the hosted ones; from the hosted
/// readings alone the rule gives 45 MiB (26.67 at 1.65x is 44.01). It is
/// 1.80x the highest hosted reading. The readings are whole-process peaks
/// carrying the child binary and its runtime as a fixed addend, and a bar
/// that flakes teaches people to rerun rather than to look.
///
/// **It is not what refuses a read whose cost is the vault.** Every shape
/// answers a page, a single target or what a narrowing part admits, so what
/// a read adds to a process that attached is a page's rows. A read that held
/// the vault's rows with their bodies is refused by the ratio below, and one
/// that held them without is refused only by [`READ_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES`];
/// both pass under this ceiling.
///
/// **Platform scope: the Linux measurement lane.** The per-PR `memory
/// invariant` job on `ubuntu-latest` x86_64-glibc is where this gates.
pub const READ_PEAK_RSS_CEILING_BYTES: u64 = 48 * 1024 * 1024;

/// How much of the attach-only peak at the `realistic` profile the peak of
/// attaching it and running the read mix may reach, per mille.
///
/// **What a read adds, as a ratio.** Both children attach the same tree under
/// the same schema the same way, and the reading one then runs the read mix
/// [`READ_PEAK_RSS_CEILING_BYTES`] names, so the ratio cancels the attach and
/// the fixed addend both readings carry and is left with the live host and
/// the highest any shape reached. A ceiling passes a read that grew the
/// process by anything that fits under it; this fails a shape that grows the
/// resident set by a large fraction of the attach.
///
/// Observed ratios on macos-arm64 on 2026-09-26: **1.11–1.18** over five
/// runs, 1.11 (24.75 against 27.56 MiB), 1.13 (24.87 against 28.21), 1.17
/// (24.35 against 28.70), 1.18 (23.79 against 28.23) and 1.15 (24.51 against
/// 28.31). On `ubuntu-latest` x86_64-glibc the `memory invariant` run
/// 36218303281 read the mix at **1.10** (24.19 against 26.67 MiB), and run
/// 36215052561 read the subject with an unfiltered find and a validate over
/// the whole vault at 1.07 (24.24 against 26.12 MiB). Trend:
/// that unfiltered subject read 1.15–1.18 locally over four runs on
/// 2026-09-25, and a find-only read read 1.07–1.13 locally through 2026-09-23
/// and 1.00 hosted in run 35858094556.
///
/// The bar is 1.5, by the rule [`ATTACH_PAIR_PEAK_RSS_PER_MILLE`] is authored
/// under: a quarter of headroom over the highest reading, local or hosted,
/// rounded up to the next twentieth. The highest is the 1.18 above, 1.187
/// before the display truncates it, and a quarter over it is 1.48. A read
/// that doubled the process reads 2.0 and fails it.
///
/// **It refuses a read that holds the vault's rows with their bodies, and not
/// one that holds them without.** Its negative control is the read mix with a
/// find inserted before the backlinks shape that hydrates every document of
/// the vault with its fields, its tags and its body, 25 rows a page, and keeps
/// every page alive: on 2026-09-26 on macos-arm64 it read **1.60, 1.62 and
/// 1.63** over three runs, ratio children at 39.07–39.25 MiB, which fails this
/// bar, while its ceiling children peaked at 39.07–39.32 MiB, under
/// [`READ_PEAK_RSS_CEILING_BYTES`]. The same find without the bodies read
/// 1.24, 1.31 and 1.31 here, which passes; the whole-process peak absorbs
/// 2000 rows of fields and tags, and [`READ_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES`] is the
/// bar that refuses them.
///
/// **Platform scope: the Linux measurement lane**, the same one
/// [`READ_PEAK_RSS_CEILING_BYTES`] gates in.
pub const READ_OVER_ATTACH_PEAK_RSS_PER_MILLE: u64 = 1_500;

/// How many more bytes of heap the read mix may hold over the `realistic`
/// profile than over the `ambiguous` profile.
///
/// **The bar that sees the rows a read holds.** Each child of the pair
/// attaches its profile under the read schema and runs the read mix
/// [`READ_PEAK_RSS_CEILING_BYTES`] names, and a counting global allocator in
/// the child reads the most the shapes raised the live heap above the mark it
/// set once the attachment was ready. The reading counts the bytes the code
/// asked the allocator for: page size, allocator slack and what the attach
/// left resident do not move it, and memory SQLite takes from `malloc` for its
/// own caches is outside it. What is inside it is where a hydrated row lives.
/// `realistic` holds 1,700 more documents than `ambiguous`, so a shape that
/// answers a page holds about the same bytes at both, and a shape that holds
/// something for every document of the vault holds 1,700 more of it at
/// `realistic`.
///
/// **The bar is a difference in bytes, not a ratio**, because the reading is
/// exact. The find's pages set the high-water at both profiles, near 1.2 MB,
/// and a ratio over that floor moves only once a retention climbs past a
/// fraction of it. A difference moves by every byte a retention adds at one
/// scale and not at the other. A difference below zero is no growth, and the
/// bar compares it as zero.
///
/// Observed on macos-arm64 on 2026-09-25 and 2026-09-26, over nine runs:
/// 1,193,975 bytes at `ambiguous` in all nine readings, and 1,185,220 at
/// `realistic` in eighteen of nineteen, the other reading 1,185,212, so a
/// difference of **-8,755 bytes**. On `ubuntu-latest` x86_64-glibc the
/// `memory invariant` run 36218303281 read 1,248,626 bytes at `ambiguous` and
/// 1,241,653 at `realistic`, a difference of **-6,973 bytes**. Each hosted
/// reading sits about 55 KB above the local one at its profile, and the two
/// platforms' differences sit 1,782 bytes apart. The trees and the requests
/// are fixed by the seed, and what the host's own threads allocate while the
/// shapes run moves a reading by bytes rather than kilobytes. The find's pages
/// set the high-water at both profiles; every other shape raises the heap by
/// under 80 KB above where it began.
///
/// The allowance is **4 KiB**, by the rule that a retention of eight bytes a
/// document, the size of one id, fails it on both platforms. That retention
/// adds 13,600 bytes to the difference over the 1,700 documents between the
/// profiles, so it fails any allowance under 4,845 bytes locally (13,600 less
/// 8,755) and under 6,627 hosted (13,600 less 6,973); 4 KiB is the largest
/// whole KiB under both. It sits 11,069 bytes over the highest reading, which
/// is over six times the 1,782 bytes the two platforms' differences part by,
/// and each platform's readings repeat to within eight bytes.
///
/// **What it catches.** A retention of some bytes for every document, held
/// while the find's pages are, raises the difference by 1,700 times that many
/// bytes, so the smallest it fails is **7.6 bytes a document on macos-arm64**
/// ((4,096 + 8,755) / 1,700) and **6.5 bytes a document hosted** ((4,096 +
/// 6,973) / 1,700). **What it does not see is a retention that stays under
/// the find's high-water.** The reading is the highest the heap stood, not
/// the sum of what each shape held, so a retention built after the find's
/// pages are gone moves neither reading until it climbs past them: a
/// `Vec<String>` of every path in the vault, built before the backlinks shape
/// and kept to the end, reads -8,755 bytes in each of two runs on
/// macos-arm64 on 2026-09-26, the unmutated mix's reading, and passes.
///
/// **Its negative controls**, each read in two runs on macos-arm64 on
/// 2026-09-26:
///
/// - A `Vec<String>` of every path in the vault, built right after the mark
///   and kept to the end, reads 1,212,902 bytes at `ambiguous` in both runs
///   and 1,321,574 and 1,321,582 at `realistic`, a difference of **108,672 and
///   108,680 bytes**, which fails.
/// - A find inserted before the backlinks shape that hydrates every document
///   with its fields and its tags, 25 rows a page, and keeps every page alive
///   reads 1,193,975 bytes at `ambiguous` and 2,734,799 at `realistic` in both
///   runs, a difference of **1,540,824 bytes**, which fails. The whole-process
///   bars absorb the same find, as [`READ_OVER_ATTACH_PEAK_RSS_PER_MILLE`]
///   records.
/// - A `Vec<u64>` holding one id for every document, allocated at the vault's
///   exact count right after the mark and kept to the end, reads 1,196,375
///   bytes at `ambiguous` and 1,201,220 at `realistic` in both runs, a
///   difference of **4,845 bytes**, which fails by 749 bytes.
///
/// **Platform scope: the Linux measurement lane**, the same one
/// [`READ_PEAK_RSS_CEILING_BYTES`] gates in. One hosted run stands beside the
/// local readings, and no negative control has been read there.
pub const READ_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES: u64 = 4 * 1024;

/// How many more bytes of heap a hub write may hold above the attach beneath
/// it over the `realistic` profile than over the `ambiguous` profile.
///
/// **The bar that sees what a hub write's own working memory holds, apart
/// from the attach beneath it and the vault around it.** A child attaches its
/// profile with [`HUB_WRITE_IN_LINKS`](../../../tests/memory.rs) fixed
/// in-links planted beside it, marks the heap once the attachment is ready,
/// writes the hub those in-links name directly through the store, and reports
/// the most that write raised the live heap above the mark. Peak resident set
/// is not this bar: a whole-process ratio over an attach-and-write child is
/// dominated by the attach and the fixed cost of the child binary, so a
/// re-decision that held every link of the vault in a `Vec` until it returned
/// — about 0.5 KiB per link — moved the `realistic` attach-and-write peak from
/// 25.9 to 32.4 MiB and the ratio over `ambiguous` from 1.05 to 1.31, both
/// under the 1.6x [`ATTACH_PAIR_PEAK_RSS_PER_MILLE`] bar. The heap mark is
/// taken after the attach and before the write, so it never sees the heal's
/// own allocations, and it is a difference in bytes rather than a ratio for
/// the same reason [`READ_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES`] is: the reading
/// is exact, and a ratio over a small floor moves only once a retention
/// climbs past a fraction of it.
///
/// `realistic` holds 1,700 more documents than `ambiguous`; the planted
/// in-links are the same 200 at both scales. A write whose own working memory
/// followed the vault's link count rather than its neighborhood's would hold
/// bytes for every one of those 1,700 extra documents' links; one that
/// followed only the re-decided set holds the same bytes at both scales.
///
/// Observed on macos-arm64 on 2026-09-27, over two runs: **0 bytes** in
/// both. Each profile held 225,588 bytes above its mark, the same
/// 200-in-link write's own working set at both scales. The mark is taken
/// after the attachment is dropped and the peak is read as the write
/// returns, so neither the host's teardown nor a read of the vault falls
/// inside the window.
///
/// The allowance is **4 KiB**, the same order
/// [`READ_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES`] is authored at. A retention of
/// some bytes for every one of the 1,700 extra documents fails it from
/// **2.5 bytes a document** (4,096 / 1,700) at the observed difference.
///
/// **Platform scope: the Linux measurement lane.** The per-PR `memory
/// invariant` job on `ubuntu-latest` x86_64-glibc is where this gates; no
/// hosted reading stands beside the local ones yet.
pub const HUB_WRITE_HEAP_GROWTH_ALLOWANCE_BYTES: u64 = 4 * 1024;

/// Peak resident set attaching the `realistic` profile with the plan subjects
/// planted beside it and running every plan of the planning child must stay
/// under.
///
/// The subject is a child process that adopts a tree the parent generated with
/// the plan subjects planted beside it, attaches it through a production
/// host, keeps it attached, and previews then applies, each apply landing the
/// plan its preview answered: a `set --where` matching ten planted documents,
/// a move of a planted hub whose twenty in-links its cascade rewrites, and a
/// delete of a document no link names. So the reading is the attach's cost,
/// the live host's, and the highest any plan reached, plus the test binary
/// that carried them; the kernel reports one peak per child, which is why the
/// plans share one.
///
/// Observed on x86_64-linux-glibc locally on 2026-10-04: **31.67–32.18 MiB**
/// over eighteen readings of the planning child at `realistic` across six runs
/// of the lane, six from this bar's own case and six from each heap pair's
/// `realistic` child; the `ambiguous` children read 29.45–29.91 MiB.
/// Five earlier runs of the same mix read 31.03–31.52 MiB over fifteen
/// readings. The same runs read the attach ceiling's child at 29.00–30.36
/// MiB, so the plans and the live host add about 2 MiB over the attach. On
/// `ubuntu-latest` x86_64-glibc the `memory invariant` jobs 111331931343 and
/// 111384985926 (runs 37166270607 and 37185003929) read the same mix at
/// **32.31–32.46 MiB** over six `realistic` readings and 30.00–30.20 over four
/// `ambiguous` ones.
///
/// **The readings are the plans', not the allocator's.** With glibc's mmap
/// threshold fixed at 512 KiB (`GLIBC_TUNABLES=glibc.malloc.mmap_threshold=524288`
/// passed to every child, one run of the lane on 2026-10-04) the planning
/// children read 31.62–31.88 MiB at `realistic` and 29.50–29.59 at
/// `ambiguous`, inside the default band, and every heap reading was
/// unchanged. The size subjects are kept out of this child for that reason:
/// while they shared it, its 4 MiB documents set its peak at 56–83 MiB by
/// default, 64.06–82.76 in the hosted run 37201706982, and a review's run of
/// that shared child with the threshold fixed read 47.6–48.7 MiB at both
/// profiles with the same heap readings, so what set the peak was how many of
/// their freed allocations glibc's dynamic threshold kept resident, and a
/// ceiling sized over it was sized on the allocator. The size child carries
/// no ceiling of its own, for the same reason: its readings span **56.98–82.69
/// MiB** by default over twenty-four readings across those six runs, in modes
/// about 57, 65 and 82 MiB that no heap reading shares, and 48.39–48.66 MiB
/// over four with the threshold fixed. Every stretch it measures is barred in
/// heap bytes instead, by
/// [`PLAN_MOVE_PREVIEW_SIZE_HEAP_GROWTH_ALLOWANCE_BYTES`],
/// [`PLAN_MOVE_APPLY_OVER_DERIVATION_HEAP_ALLOWANCE_BYTES`],
/// [`DERIVATION_SIZE_HEAP_GROWTH_ALLOWANCE_BYTES`] and
/// [`PLAN_RELINKING_MOVE_PREVIEW_SIZE_HEAP_GROWTH_ALLOWANCE_BYTES`], which
/// count what the code holds and not what the allocator keeps.
///
/// **This ceiling bounds the process.** It is 53 MiB, by the rule
/// [`READ_PEAK_RSS_CEILING_BYTES`] is authored under: the proportion
/// [`ATTACH_PEAK_RSS_CEILING_BYTES`] keeps over its highest reading, 1.61x,
/// over the highest plan reading, local or hosted, 32.46 MiB, which is 52.26
/// MiB, rounded up to a whole MiB. From the local readings alone the rule
/// gives 52 MiB (32.18 at 1.61x is 51.81). It is the coarse backstop: what
/// refuses a plan whose memory is the vault is
/// [`PLAN_PREVIEW_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES`] and
/// [`PLAN_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES`].
///
/// **Platform scope: the Linux measurement lane.** The per-PR `memory
/// invariant` job on `ubuntu-latest` x86_64-glibc is where this gates.
pub const PLAN_PEAK_RSS_CEILING_BYTES: u64 = 53 * 1024 * 1024;

/// How many more bytes of heap the plan previews may hold above the attach
/// over the `realistic` profile than over the `ambiguous` profile.
///
/// **The bar that planning holds its operations and a fixed record per
/// target, never the vault, at the read pair's resolution.** A planning child
/// attaches its profile with the plan subjects planted beside it, the same
/// subjects at both profiles, marks the heap once the attachment is ready and
/// the heap has settled, and previews three writes through the host: a `set --where` matching ten
/// planted documents, a move of a planted hub whose twenty in-links its
/// cascade rewrites, and a delete of a document no link names. It holds the
/// three resolved plans and reads the most the previews raised the live heap
/// above the mark before it applies any of them. No apply has run, so no
/// watcher echo or commit stands in the window, and planning that held
/// something for every document of the vault holds 1,700 more of it at
/// `realistic`. It is a difference in bytes rather than a ratio for the
/// reason [`READ_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES`] gives: the reading is
/// exact.
///
/// Observed on x86_64-linux-glibc locally on 2026-10-04, over five runs of
/// the pair: 164,780 bytes at both profiles in four runs, and 164,780 at
/// `ambiguous` with 164,788 at `realistic` in the fifth, a difference of **0
/// bytes in four runs and 8 in one**. Across the five runs of the lane every
/// planning child read its previews at 164,750 to 164,788 bytes, ten readings
/// at `ambiguous` and fifteen at `realistic`; a reading moves by a few bytes
/// with the sandbox's name, and the pair's two children carry names of one
/// length. **Re-read after NORN-345's planning changes landed and with the
/// mark set on a settled heap**, over six runs of the lane on 2026-10-04:
/// 171,323 bytes at both profiles in all six, a difference of **0 bytes**, and
/// every planning child read its previews at 171,293 to 171,323 bytes, twelve
/// readings at `ambiguous` and eighteen at `realistic`. On `ubuntu-latest`
/// x86_64-glibc the `memory invariant` job 111331931343, before those changes,
/// read 164,752 bytes at both profiles, and job 111384985926, after them and
/// before the settle, read 171,287 at both, a difference of **0 bytes** in
/// each: the rise of about 6.5 KB is those changes', on both platforms.
///
/// The allowance is **4 KiB**, the figure
/// [`READ_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES`] is authored at, by the same rule:
/// a retention of eight bytes a document, the size of one id, fails it. That
/// retention adds 13,600 bytes over the 1,700 documents between the profiles,
/// so it fails any allowance under 13,592 bytes at the highest difference
/// read. A retention of some bytes for every document, held at the previews'
/// peak, fails it from **2.4 bytes a document** ((4,096 + 0) / 1,700).
///
/// **What it does not see is memory planning builds and frees under the
/// previews' own high-water**, about 171 KB above its mark at both
/// profiles: a working set proportional to the vault that never climbs past
/// it, up to about 84 bytes for each of `realistic`'s 2,032 documents, reads
/// as nothing. Work proportional to the vault while planning is the counter
/// lane's to refuse, where `a_where_apply_costs_the_same_at_both_scales`
/// holds a `set --where`'s steps flat across the same two scales. The
/// high-water's limit is watched under NORN-131, as the read pair's is.
///
/// **Its negative control**, read in two runs locally on 2026-10-04, before
/// NORN-345's planning changes landed: a `Vec<u64>` holding one id for every
/// document of the vault, sized by the snapshot's own count, allocated as the
/// `set --where` begins matching and leaked so it outlives the plan, reads
/// 167,434 bytes at `ambiguous` and 181,034 at `realistic` in both runs, a
/// difference of **13,600 bytes**, which fails. The mix pair beside it read
/// the same mutant at a difference of 13,600 bytes and passed, as its
/// allowance says it does.
///
/// **Platform scope: the Linux measurement lane.** The per-PR `memory
/// invariant` job on `ubuntu-latest` x86_64-glibc is where this gates, and
/// its readings above stand beside the local ones; no negative control has
/// been read there.
pub const PLAN_PREVIEW_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES: u64 = 4 * 1024;

/// How many more bytes of heap the plan mix may hold above the attach over
/// the `realistic` profile than over the `ambiguous` profile.
///
/// **The bar that a plan applied through a live host holds its operations and
/// a fixed record per target, never the vault.** The planning child of
/// [`PLAN_PREVIEW_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES`] goes on to apply each of
/// its three plans exactly as previewed, and reports the most the six
/// requests raised the live heap above the same mark. A plan that holds its
/// operations and a record for each target it writes holds the same bytes at
/// both profiles; one that held something for every document of the vault
/// holds 1,700 more of it at `realistic`. It is a difference in bytes rather
/// than a ratio for the reason [`READ_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES`]
/// gives: the reading is exact.
///
/// Observed on x86_64-linux-glibc locally on 2026-10-04, over five runs of
/// the pair: 166,262 bytes at `ambiguous` and 166,266 at `realistic` in three
/// runs, a difference of **4 bytes**; 197,812 and 166,266 in one, **-31,546
/// bytes**; and 166,262 and 197,665 in one, **31,403 bytes**. The previews
/// all run before the first apply: an apply's watcher echo is confirmed by a
/// job beside a later preview or not by timing alone, and a mix that
/// interleaved them read 164,110 to 232,512 bytes at one profile across runs.
/// **Re-read after NORN-345's planning changes landed and with the mark set
/// on a settled heap**, over six runs of the lane on 2026-10-04: 172,191
/// bytes at both profiles in five runs, a difference of **0 bytes**, and
/// 172,191 at `ambiguous` with 198,306 at `realistic` in the sixth, **26,115
/// bytes**. On `ubuntu-latest` x86_64-glibc the `memory invariant` job
/// 111331931343, before those changes, read 166,206 bytes at `ambiguous` and
/// 166,210 at `realistic`, and job 111384985926, after them and before the
/// settle, read 172,211 and 172,215, a difference of **4 bytes** in each.
///
/// **The reading moves by about 31 KB between two states.** The watcher's
/// threads take an apply's own publications in while the apply is still
/// committing, and whether their event buffers stand at the apply's peak is
/// timing. Across the five runs of the lane the planning children read their
/// mix at 166,222 to 166,282 bytes in the lower state and 192,388 to 197,812
/// in the upper, the same work on every thread each time, an excursion of at
/// most **31,550 bytes** over the same child's lower reading. Either profile
/// can land in either state, and nothing holds them together. The six runs
/// of the re-reading read twenty-six children at 172,147 to 172,207 bytes and
/// four at 193,045 to 198,775, an excursion of at most 26,628 bytes over the
/// same child's lower reading, inside the one the allowance is derived from.
///
/// The allowance is **40 KiB**: the excursion with a quarter of headroom,
/// 39,438 bytes, rounded up to the next 8 KiB. A retention held for each of
/// the 1,700 documents between the profiles fails it from **24.1 bytes a
/// document** (40,960 / 1,700) where both readings sit in one state, and from
/// **42.7 bytes a document** ((40,960 + 31,550) / 1,700) where the excursion
/// lands on `ambiguous` alone; where it lands on `realistic` alone the
/// unmutated pair sits about 9.5 KB under the allowance. That is coarser than
/// the 4 KiB [`PLAN_PREVIEW_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES`] is authored at,
/// and it is the price of a plan applied through a live host: **an id kept
/// for every document passes it**, and is refused by the previews' pair where
/// planning keeps it. A path kept for every document fails it in either
/// state.
///
/// **What it does not see is memory a plan builds and frees under the mix's
/// high-water**, about 172 KB above its mark and set by the previews, which
/// the applies climb barely past: a working set proportional to the vault that
/// never climbs past it, up to about 84 bytes for each of `realistic`'s 2,032
/// documents, reads as nothing. Work proportional to the vault while planning
/// is the counter lane's to refuse, where
/// `a_where_apply_costs_the_same_at_both_scales` holds a `set --where`'s steps
/// flat across the same two scales. The high-water's limit is watched under
/// NORN-131, as the read pair's is. **A move's size-independence, holding no
/// copy of the document it moves, is barred by
/// [`PLAN_MOVE_PREVIEW_SIZE_HEAP_GROWTH_ALLOWANCE_BYTES`] and
/// [`PLAN_MOVE_APPLY_OVER_DERIVATION_HEAP_ALLOWANCE_BYTES`], not here.**
///
/// **Its negative controls**, each read locally on 2026-10-04, before
/// NORN-345's planning changes landed:
///
/// - The `where` matcher paging through every document of the vault and
///   keeping each path past the plan reads 184,441 bytes at `ambiguous` and
///   522,370 at `realistic`, a difference of **337,929 bytes**, which fails.
///   The previews' pair reads the same mutant at 339,421 bytes and fails.
/// - A `Vec<u64>` holding one id for every document of the vault, sized by
///   the snapshot's own count, allocated as the `set --where` begins matching
///   and leaked so it outlives the plan, reads 168,918 bytes at `ambiguous`
///   and 182,518 at `realistic` in two runs, a difference of **13,600
///   bytes**, which passes, as the arithmetic above says it does.
///
/// **Platform scope: the Linux measurement lane.** The per-PR `memory
/// invariant` job on `ubuntu-latest` x86_64-glibc is where this gates, and
/// its readings above stand beside the local ones; neither caught the
/// excursion, and no negative control has been read there.
pub const PLAN_PAIR_HEAP_GROWTH_ALLOWANCE_BYTES: u64 = 40 * 1024;

/// How many more bytes of heap previewing the move of a 4 MiB document may
/// raise above its mark than previewing the move of a 4 KiB one, where
/// neither move changes a byte.
///
/// **The bar that a move holds no copy of the document it moves once the
/// vault has indexed it.** The size child at `realistic` previews two moves
/// right after its attach, before any apply of the child has run: one of a
/// 4 KiB document and one of a 4 MiB document, both bodies plain lines naming
/// no link, each path of one the length of the other's. Each preview runs
/// under its own mark, set once the live heap has held still for 750 ms, and
/// reads the most the preview raised the heap above it. A move that changes
/// no bytes is two paths and a fixed record of each one's state, the same for
/// either document, and planning reads the moved document streamed and takes
/// its links from the store's index where the index derived them from those
/// very bytes, so the 4,190,208 bytes between the two bodies are bytes the
/// plan neither authored nor holds.
///
/// Observed on x86_64-linux-glibc locally on 2026-10-04, over twenty-four
/// size children across six runs of the lane, six of them this bar's own: the
/// small move's preview read 71,503 to 72,189 bytes and the large one's
/// 72,127 to 72,155, the small one in states about 690 bytes apart that
/// neither size sets, differences of **-34 to 636 bytes**, and -34, 630, 630,
/// 630, -34 and 630 in this bar's own six. On `ubuntu-latest` x86_64-glibc the `memory
/// invariant` run 37201706982 read differences of -34 to 597 bytes over eight
/// children, while the size subjects still shared the planning child and ran
/// ahead of its mix as they run here. Neither reading moves with the body:
/// both sizes' previews hold the same buffers, among them the one 64 KiB
/// chunk `norn-fs`'s streamed hash reads at a time, and the plan's fixed
/// records. Each reading moves by up to a few hundred bytes from run to run
/// and with the length of the sandbox's path, so another checkout's readings
/// land near these ranges rather than inside them.
///
/// The allowance is **64 KiB**, headroom equal to one chunk of the streamed
/// hash: a preview that held one more such buffer for the large document
/// than for the small one fits it, and one holding even one whole copy of the
/// large document, 4 MiB more, fails it sixty-four times over.
///
/// **Its negative control**, read once locally on 2026-10-04 while the size
/// subjects shared the planning child: the plan's one carried rule answering
/// that no file is carried, so planning and the applier read the moved
/// document whole as a move did before it carried anything, reads 91,884 bytes
/// for the small move and 16,797,891 for the large, a difference of
/// **16,706,007 bytes**, about four bytes held for each byte moved, which
/// fails.
///
/// **What it does not bar.** A move over a vault whose index does not vouch
/// for the moved document, which planning reads whole once at the hash it
/// streamed, the declared limit the module docs of `norn_host::planner`
/// state; a move that rewrites the moved document's own links, which
/// [`PLAN_RELINKING_MOVE_PREVIEW_SIZE_HEAP_GROWTH_ALLOWANCE_BYTES`] bars at
/// its floor; and anything planning builds and frees under the preview's own
/// high-water. The apply's cost is
/// [`PLAN_MOVE_APPLY_OVER_DERIVATION_HEAP_ALLOWANCE_BYTES`]'s.
///
/// **Platform scope: the Linux measurement lane.** The per-PR `memory
/// invariant` job on `ubuntu-latest` x86_64-glibc is where this gates; its
/// readings above are of the shared child, and no negative control has been
/// read there.
pub const PLAN_MOVE_PREVIEW_SIZE_HEAP_GROWTH_ALLOWANCE_BYTES: u64 = 64 * 1024;

/// How many more bytes of heap applying the move of a 4 MiB document may
/// raise above the move of a 4 KiB one than the live host's derivation of the
/// same 4 MiB document raises above its derivation of the 4 KiB one.
///
/// **The bar that a move's apply holds no more than the commit's derivation
/// of the document it lands.** The size child at `realistic`, once its
/// previews have run, applies the two moves
/// [`PLAN_MOVE_PREVIEW_SIZE_HEAP_GROWTH_ALLOWANCE_BYTES`] previewed, each under
/// its own mark set on a settled heap and read as the apply answers. The
/// apply stages each move as a streamed copy, so it holds no copy of the
/// moved document while it writes; then its commit reads the landed document
/// back and derives it through the one derivation every heal runs, which
/// holds about four times the body at once
/// ([`DERIVATION_SIZE_HEAP_GROWTH_ALLOWANCE_BYTES`]). **That cost is the
/// derivation's, not the plan's**, so the bar is held against it rather than
/// against zero: `apply_large - apply_small <= derive_large - derive_small +
/// allowance`. Both sides are differences between the two sizes, so what each
/// stretch holds whatever the document's size, a plan's records on one side
/// and the watcher's event handling on the other, cancels, and what is
/// compared is what each holds for the body alone.
///
/// **The control is a write from outside the vault, and the live host's own
/// derivation of it.** Once the applies have landed and the heap has
/// settled, the child writes a document of each size beside the vault, the
/// same bytes the moves landed, makes its folder inside the vault, settles,
/// marks, and renames it in whole; the watcher delivers it, the host derives
/// it on the entry's worker through that same derivation, and the mark is
/// read once the store holds the document at its length and the heap has
/// settled again. That is the cleanest single derivation of one document the
/// production host runs: the attach heal derives the planted documents too,
/// but beside the whole vault under one peak, so no mark isolates one of
/// them, and a create through the host would hold the bytes it authors. The
/// case refuses a control whose difference is under one body, which is a
/// control that did not derive the document it stands for.
///
/// Observed on x86_64-linux-glibc locally on 2026-10-04, over twenty-four
/// size children across six runs of the lane, six of them this bar's own: the
/// applies read 85,553 to 85,651 bytes small and 16,797,889 to 16,798,107
/// large, a difference of 16,712,246 to 16,712,464; the derivations read
/// 122,062 to 122,182 small and 16,835,891 to 16,836,203 large, a difference
/// of 16,713,717 to 16,714,037. **The apply stood 1,463 to 1,775 bytes under
/// the derivation**, and -1,775, -1,463, -1,701, -1,767, -1,775 and -1,679
/// in this bar's own six.
/// On `ubuntu-latest` x86_64-glibc the `memory invariant` run 37201706982
/// read the apply 689 to 1,016 bytes over the derivation over eight
/// children, while the size subjects still shared the planning child and
/// their applies ran after its mix; locally that arrangement read 590 to
/// 1,027 over across the PR's own runs and a replication of them, and the
/// small apply reads about 2.5 KB more run straight after the previews. Each reading moves by up to a few hundred bytes from run to
/// run and with the length of the sandbox's path, so another checkout's
/// readings land near these ranges rather than inside them.
///
/// The allowance is **64 KiB**, the one chunk `norn-fs`'s streamed hash reads
/// at a time: the watcher's confirmation of the apply's own publication
/// streams the landed file through one such chunk on its own thread, and may
/// stand at it while the commit derives. Any whole copy of the 4 MiB document
/// the apply held past the derivation's own fails it sixty-four times over.
///
/// **Its negative controls**, each read once locally on 2026-10-04 while the
/// size subjects shared the planning child:
///
/// - The plan's one carried rule answering that no file is carried, so
///   planning and staging read the moved document whole, reads the applies at
///   83,566 and 16,797,889 bytes against derivations of 122,174 and
///   16,835,891, **606 bytes** over the derivation, which passes. **The
///   derivation dominates**: staging's whole read is freed before the commit
///   reads the document back, so the apply's high-water is the commit's either
///   way, and a preview that reads it whole is
///   [`PLAN_MOVE_PREVIEW_SIZE_HEAP_GROWTH_ALLOWANCE_BYTES`]'s to refuse.
/// - A copy of each landed document the commit reads, kept until its
///   changeset flushes, reads the applies at 87,251 and 20,992,193 bytes
///   against derivations of 122,174 and 16,836,195, **4,190,921 bytes** over
///   the derivation, which fails.
///
/// **What it does not bar.** The derivation's own cost: a derivation that
/// held more for each byte raises the control and the apply alike, which
/// [`DERIVATION_SIZE_HEAP_GROWTH_ALLOWANCE_BYTES`] bars instead. **Anything
/// the apply builds and frees under the commit's own high-water**, which is
/// about four bodies: the first negative control above, an apply that reads
/// the moved document whole while staging, passes it. What holds the applier
/// to no copy of a carried document is its seam tests in
/// `norn-host`'s `applier/tests/carried.rs`: their counting view asserts a
/// fresh apply reads such a document whole zero times (a re-send that finds
/// the change already landed reads the landed document whole once)
/// (`a_chain_of_moves_carries_each_document_where_it_lands`,
/// `a_carried_name_refilled_by_a_composed_document_is_read_streamed`,
/// `a_carried_name_a_respell_refills_is_read_streamed`), and
/// `the_applier_observes_a_carried_move_without_holding_its_document` that it
/// observes one streamed, holding no bytes. The write kernel's streamed copy
/// (`norn_fs::Content::CopyOf`) holds one chunk by the construction of its one
/// streaming loop; `norn-fs`'s copy tests hold the bytes it writes across
/// chunk boundaries and the files it reads, and no test bounds its buffer. The
/// apply's reading is taken as it answers, so the watcher's echo of the landed
/// document, which reads about its body on the host's own threads after the
/// apply answers, is not charged to it, and the next mark is set only once it
/// has been handled; an echo that came to overlap the commit would raise the
/// apply by about a body and fail the bar.
///
/// **Platform scope: the Linux measurement lane.** The per-PR `memory
/// invariant` job on `ubuntu-latest` x86_64-glibc is where this gates; its
/// readings above are of the shared child, and no negative control has been
/// read there.
pub const PLAN_MOVE_APPLY_OVER_DERIVATION_HEAP_ALLOWANCE_BYTES: u64 = 64 * 1024;

/// How many more bytes of heap the live host's derivation of a 4 MiB document
/// written into the vault from outside it may raise above its mark than its
/// derivation of a 4 KiB one.
///
/// **A regression bar at a declared floor, not a target.** These are the
/// derivation controls [`PLAN_MOVE_APPLY_OVER_DERIVATION_HEAP_ALLOWANCE_BYTES`]
/// holds a move's apply against, which that bar cancels by design: a
/// derivation that held one more copy of each body raises the control and the
/// apply alike, and passes it. This bar holds the controls themselves to what
/// the derivation holds today, so a change that adds a copy of the body to
/// the one derivation every heal and every commit runs fails here; it does
/// not say that floor is what a derivation should hold. The size child at
/// `realistic`, once its moves have landed, writes a 4 KiB and a 4 MiB
/// document of plain lines naming no link beside the vault, renames each in
/// under its own mark set on a settled heap, and reads the mark once the store
/// holds the document at its length and the heap has settled again.
///
/// **The floor is about four bodies, two allocations live together** while
/// the derivation scans the body (`norn_host::derivation::map_document`),
/// traced locally on 2026-10-04 by logging each allocation of a mebibyte or
/// more:
///
/// - the file's bytes, read whole and hashed
///   (`norn_fs::read::read_optional_and_hash` through `scoped_increment`),
///   one body;
/// - the first-pass tree pulldown-cmark builds over the body (`BodyScan::new`,
///   through `Document::scan_body`), which reserves a 48-byte node for every
///   32 bytes of body and doubles once on body text of short lines, about
///   three bodies.
///
/// The tree is dropped inside the scan, before the facts take their own copy
/// of the body, so that copy stands beside the bytes alone, two bodies, under
/// the floor. Both figures are this fixture's: the tree grows with how densely
/// a document packs Markdown nodes, so a document of shorter lines or of lists
/// holds more than four bodies.
///
/// Observed on x86_64-linux-glibc locally on 2026-10-04, over twenty-four
/// size children across six runs of the lane, six of them this bar's own: the
/// small derivation read 122,062 to 122,182 bytes and the large 16,835,891 to
/// 16,836,203, differences of **16,713,717 to 16,714,037 bytes**, about 3.98
/// bodies, and 16,714,021 in four of this bar's own six, 16,713,717 and
/// 16,713,925 in the other two. On
/// `ubuntu-latest` x86_64-glibc the `memory invariant` run 37201706982 read
/// differences of 16,713,717 to 16,714,021 bytes over eight children, while
/// the size subjects still shared the planning child. Each reading moves by
/// up to a few hundred bytes from run to run and with the length of the
/// sandbox's path, so another checkout's readings land near these ranges
/// rather than inside them.
///
/// The allowance is **four bodies and 64 KiB**, 16,842,752 bytes: the two
/// allocations' four bodies of the 4 MiB document, and the one chunk
/// `norn-fs`'s streamed hash reads at a time as headroom, 128,715 bytes over
/// the highest reading. A derivation that held one more copy of the body
/// beside the tree, 4 MiB, fails it.
///
/// **Its negative control**, read once locally on 2026-10-04: `map_document`
/// keeping a copy of the bytes it derives from until it returns, reads
/// 126,270 bytes for the small derivation and 21,030,403 for the large, a
/// difference of **20,904,133 bytes**, about five bodies, which fails by
/// 4,061,381. The apply bar read the same mutant at 1,767 bytes under the
/// derivation and passed, as it is built to.
///
/// **What it does not bar.** Anything the derivation builds and frees under
/// the tree's high-water, such as the facts' copy of the body, since the
/// reading is a high-water; and a derivation inside the attach heal, which
/// derives beside the whole vault under one peak.
///
/// **Platform scope: the Linux measurement lane.** The per-PR `memory
/// invariant` job on `ubuntu-latest` x86_64-glibc is where this gates; its
/// readings above are of the shared child, and no negative control has been
/// read there.
pub const DERIVATION_SIZE_HEAP_GROWTH_ALLOWANCE_BYTES: u64 = 4 * 4 * 1024 * 1024 + 64 * 1024;

/// How many more bytes of heap previewing the move of a 4 MiB document that
/// rewrites its own links may raise above its mark than previewing the same
/// move of a 4 KiB one.
///
/// **A regression bar at a declared floor, not a target.** It holds a move
/// that rewrites the moved document's own links to the heap that move holds
/// today, so a change that adds a copy of the body fails it; it does not say
/// that floor is what such a move should hold. The size child at
/// `realistic` previews, before any apply and each under its own mark set on a
/// settled heap, the moves of a 4 KiB and a 4 MiB document whose bodies open
/// with a relative link to a document beside them, which the move into
/// another folder breaks, so each plan rewrites its own document's link. Each
/// path of the large document is the length of the small one's.
///
/// **The floor is about five bodies, three allocations live together** while
/// the rewrite verifies that the rewritten bytes read as the document did with
/// only its links respelled (`Document::reads_as_rewritten`):
///
/// - the before-body planning read whole and holds (`TreeView::entry`), one
///   body;
/// - the rewritten bytes, which `splice_all` allocates once at their exact
///   length, one body;
/// - the first-pass tree pulldown-cmark builds over the rewritten bytes
///   (`BodyScan::new`, through `rewrite::Reading::of`), which reserves a
///   48-byte node for every 32 bytes of body and doubles once on body text of
///   short lines, about three bodies.
///
/// Both figures are this fixture's: the tree grows with how densely a
/// document packs Markdown nodes, so a document of shorter lines or of lists
/// holds more than five bodies.
///
/// **Why the floor stands.** The verification needs a full re-scan of the
/// rewritten bytes: a backtick, an HTML block, a reference definition or the
/// frontmatter can change how content far from the edit reads, so no window
/// around the edit is proof. pulldown-cmark materialises its first-pass tree
/// for the whole input before it yields an event, so that scan holds the tree
/// at once, beside the bytes it scans and the body they replace.
///
/// Observed on x86_64-linux-glibc locally on 2026-10-04, over twenty-four
/// size children across six runs of the lane, six of them this bar's own: the
/// small preview read 106,981 to 106,989 bytes and the large 21,007,273 to
/// 21,007,289, differences of **20,900,292 to 20,900,300 bytes**, about 4.98
/// bodies, and 20,900,292 in four of this bar's own six and 20,900,300 in
/// two.
/// On `ubuntu-latest` x86_64-glibc the `memory invariant` run 37201706982
/// read differences of 20,900,292 to 20,900,300 bytes over eight children,
/// while the size subjects still shared the planning child and ran ahead of
/// its mix as they run here. Each reading moves by a few bytes from run to
/// run and with the length of the sandbox's path, so another checkout's
/// readings land near these ranges rather than inside them.
///
/// The allowance is **five bodies and 64 KiB**, 21,037,056 bytes: the three
/// allocations' five bodies of the 4 MiB document, and the one chunk
/// `norn-fs`'s streamed hash reads at a time as headroom, 136,756 bytes over
/// the highest reading. A preview that held one more copy of the body, 4 MiB,
/// fails it.
///
/// **Its negative control**, read once locally on 2026-10-04 while the size
/// subjects shared the planning child: `splice_all` allocating the rewritten bytes at the source's length, as it did before it
/// allocated their exact length, so a rewrite a few bytes longer doubles that
/// allocation to about two bodies, reads 111,057 bytes for the small preview
/// and 25,201,557 for the large, a difference of **25,090,500 bytes**, about
/// six bodies, which fails by 4,053,444.
///
/// **What it does not bar.** Anything the rewrite builds and frees under the
/// three allocations' high-water, such as the scans for the links to rewrite
/// before the splice, since the reading is a high-water; and the apply of
/// such a move.
///
/// **Platform scope: the Linux measurement lane.** The per-PR `memory
/// invariant` job on `ubuntu-latest` x86_64-glibc is where this gates; its
/// readings above are of the shared child, and no negative control has been
/// read there.
pub const PLAN_RELINKING_MOVE_PREVIEW_SIZE_HEAP_GROWTH_ALLOWANCE_BYTES: u64 =
    5 * 4 * 1024 * 1024 + 64 * 1024;

/// How many descriptors a long mixed load may add to the count taken once the
/// attachment is ready.
///
/// **The no-file-descriptor-growth term of lockdown.** The count is taken in
/// the process that attached, after it is ready and again when the load ends,
/// and the difference is what this bounds. Zero is what the subject is expected
/// to hold and what every observed run reads; the allowance is for a descriptor
/// a run legitimately holds at the sampling instant — a watcher re-subscribing,
/// a store file reopened — rather than headroom for a leak, which grows with
/// the load and passes no allowance this small.
///
/// Observed on `ubuntu-latest` x86_64-glibc over **every hour-long nightly the
/// hosted lane has run — thirteen of them, 2026-08-05 through 2026-08-17**:
/// growth of zero in every one, at 14 descriptors first-to-last in the first
/// three and 15 in the other ten. The count a load holds moves with the runner
/// image, which is what that step from 14 to 15 is; what this bar reads is the
/// difference across one run, which does not.
///
/// **Platform scope: the Linux measurement lane.** The hour-long load this
/// reads is the scheduled lane's on `ubuntu-latest` x86_64-glibc, and the
/// macOS certification lane runs no load at all.
pub const SOAK_FD_GROWTH_ALLOWANCE: usize = 4;

/// How many descriptors a host may still hold once its load has stopped, above
/// the load's first sample — or `None` while no ceiling is authored.
///
/// The baseline is the first sample of the load loop, which the harness takes
/// after tick 0 has churned and read, not a reading at the instant the
/// attachment became ready. Anything tick 0 opened and later handed back is
/// therefore inside the baseline and cancels out of the retention, which biases
/// the term low. [`SOAK_FD_GROWTH_ALLOWANCE`] reads the same sample, so the two
/// descriptor bars are stated over one baseline.
///
/// **The post-quiescence retention term of lockdown**, and the second of the
/// two descriptor bars the exit contract names.
/// [`SOAK_FD_GROWTH_ALLOWANCE`] beside this reads the count while the load is
/// still working, so a descriptor the host legitimately holds *for* the work —
/// a store file open for a changeset, a watch re-subscribing — sits inside
/// both the first reading and the last and cancels out of the difference. What
/// that bar cannot see is a descriptor the work acquired and the host never
/// handed back, because under a continuous load the two are the same number.
/// This is the reading that separates them: the load stops churning and stops
/// reading, and the descriptors still open once the host has nothing left to do
/// are the ones held for no work at all.
///
/// **The reading is taken when the host is observed quiet, never after a fixed
/// sleep.** The harness waits on the host's own account of what is running
/// against the entry — nothing claimed, no leg registered, no job queued, no
/// release in flight, nothing pinned — and reads the descriptors the moment that
/// holds. A fixed sleep states the wrong thing in both directions: a drain that
/// outruns it on a loaded runner is counted as descriptors the host kept, which
/// fails this bar and resets the five-run count over a machine that was merely
/// busy, and a host that settles at once still pays the rest of the window. The
/// quiescent window is therefore a **bound** on that wait, and a wait that
/// reaches it is a typed failure naming what was still in flight rather than a
/// reading taken mid-drain. How long the host took to go quiet is recorded in
/// the run's summary beside the count, so a bar met by a host that settled in
/// two seconds and one met by a host that took twenty-nine are not the same
/// record.
///
/// **It is not an idle detach.** The entry stays attached and stays watched:
/// the reap interval the soak harness configures is deliberately longer than
/// any load it runs, so what this measures is a host at rest under coverage
/// rather than a host taken down. A run whose count comes back to the first
/// sample has handed back everything the hour acquired past it.
///
/// # Platform scope
///
/// **The Linux measurement lane**, the same one [`SOAK_FD_GROWTH_ALLOWANCE`]
/// gates in: the hour-long load is the scheduled lane's on `ubuntu-latest`
/// x86_64-glibc, and the macOS certification lane runs no load at all. The
/// count itself is the process's own open-descriptor table, which is a
/// different number on every runner image — so the bar is stated as a
/// retention above this run's own first sample rather than as a count.
///
/// # Observations
///
/// Observed on `ubuntu-latest` x86_64-glibc, the `workflow_dispatch` reading
/// run (run 34905110033, at 974dd86): 16 descriptors at the load's first
/// sample, 15 at the last, 15 read once the host was observed quiet 30 s after
/// the load stopped — a retention of **0** above the first sample. The same
/// run's peak and high-water resident-set readings (19.50 MiB each), its
/// slope (1.00) and its recovery dose (1, tripped in 4 ticks / 3003 ms against
/// a 1000 ms baseline tick) sit inside the bands the three prior calibration
/// runs of 2026-09-14 set, so this run adds a fourth data point to those bars
/// rather than moving any of them.
///
/// # Safety rationale
///
/// The observed retention is zero: a host at rest holds exactly what it held
/// at the load's first sample, with nothing left open for work that has
/// finished. The ceiling is authored at 4 rather than 0, the same allowance
/// [`SOAK_FD_GROWTH_ALLOWANCE`] carries for a descriptor a run legitimately
/// holds at the sampling instant — a watcher re-subscribing, a store file
/// mid-reopen — so that a transient caught at rest does not reset the count
/// this bar builds toward lockdown's five. The allowance is headroom for the
/// sampling instant, not for a leak: a leak grows with the load and a
/// four-descriptor allowance passes none of that growth.
///
/// # Review trigger
///
/// A run past this ceiling is a claim that the host now keeps descriptors past
/// the work that needed them, and it is answered by reading what still holds
/// them rather than by rerunning. Moving the value is a reviewed edit carrying
/// that claim beside it; lowering it needs no new argument. Un-authoring it
/// back to `None` reopens the calibration window.
pub const SOAK_QUIESCENT_FD_RETENTION: Option<usize> = Some(4);

/// How much of the first quartile's mean resident set the last quartile's mean
/// may reach, per mille.
///
/// **The flat-memory-slope term of lockdown.** A leak under a load that runs
/// for an hour shows as a rising resident set, so the sample series is split in
/// four and the last quartile's mean is held against the first's. Comparing
/// quartile means rather than endpoints is what keeps one sample taken during a
/// changeset commit from deciding the run.
///
/// **The series this is read over excludes the load's recovery windows.** The
/// load trips one deliberate recovery, and the re-attach behind it walks and
/// heals the ≥5k tree from inside the run — attach cost, in the first quartile
/// every time, because the arm is owed to the first change the load churns.
/// Left in the series it would raise the denominator of a ratio that is only
/// ever failed by a large one, which desensitizes the bar rather than
/// stretching it. The quartile-means reasoning above is about an incidental
/// spike; a cost placed in one quartile by construction is not one, so it is
/// dropped rather than averaged over. `host_soak.rs` marks each sample with
/// whether a recovery was outstanding across its tick, and the peak beside this
/// keeps every sample because the height a run reached is the height it
/// reached.
///
/// Observed on `ubuntu-latest` x86_64-glibc, which is the platform that gates:
/// **1.00 on each of the thirteen hour-long nightlies the hosted lane has run**,
/// 2026-08-05 through 2026-08-17, with first-quartile means of 17.27–17.99 MiB
/// and last-quartile means of 17.28–18.01. The comparison is in integer per
/// mille, so a displayed 1.00 is a true ratio under 1.010; computing each run's
/// two means directly puts the worst of the thirteen at 1.0012. The quartile
/// means themselves drift up about 4% across the fortnight — a runner image
/// moving, not a leak, because it is a change in where each run starts rather
/// than a rise inside any one of them, which is exactly the distinction a
/// run-against-itself comparison exists to make.
///
/// Observed feature-on, with the deliberate recovery in the load, over the
/// three calibration runs of 2026-09-14 (runs 34877528262, 34877987791 and
/// 34884665687): **1.00 in each**, against first-quartile means of 19.48–19.68
/// MiB and last-quartile means of 19.49–19.69. Each is read over the samples
/// outside the one recovery window the run trips — 3586–3596 of the 3590–3599
/// samples the run took — which is the series this bar is stated over.
///
/// Observed on macos-arm64 at the short default duration: **1.04–1.07 over
/// three 90-second runs, and 0.96 over a 300-second one**, against
/// first-quartile means of 21.50–22.31 MiB. A short run reads higher because
/// its first quartile covers the minute after an attach, where a store's caches
/// are still filling; the hour-long run spends that inside its first quartile
/// and reads flat.
///
/// The bar is 1.15, and **what sets it is the short run rather than the hosted
/// one**: the same constant judges a 90-second local run, whose worst reading
/// is 1.07, so the bar keeps 0.15 over 1.00 — a little over twice the 0.07 that
/// reading stands above it. Against the hour-long load that allowance is 125
/// times the 0.0012 the worst of the thirteen nightlies shows, which is the
/// price of one bar over both durations — and it is still tight enough to fail
/// a load paying for each reconciliation, because a leak at that scale
/// compounds over an hour rather than levelling off.
///
/// **Platform scope: the Linux measurement lane**, which is where the
/// hour-long load runs — though the bar is a run against itself and the
/// macos-arm64 short-duration readings above are the ones that set it, so this
/// is the one band whose value a second platform argued for. It is still
/// evaluated on `ubuntu-latest` alone.
pub const SOAK_RSS_SLOPE_PER_MILLE: u64 = 1_150;

/// Peak resident set the host may reach **under** the long mixed load at the
/// ≥5k profile, or `None` while no ceiling is authored.
///
/// **The peak term of the memory invariant at soak scale.** The slope beside
/// this reads the trend and says nothing about the height the series reaches;
/// the maximum of the same samples is what says a load that stayed flat stayed
/// flat somewhere reasonable. The reading is recorded every run either way, and
/// the comparison happens only where a ceiling is authored: `Some` bars the
/// run, `None` records the reading and bars nothing — a calibration state the
/// qualification ledger types every such run non-qualifying under, so the
/// readings accumulate without the runs counting.
///
/// **What it is the peak of is the load and the one re-attach inside it.** The
/// series starts once the attachment reads ready, so the walk the load's first
/// attach makes over the ≥5k tree completes before the first sample and is
/// outside every reading taken here. The deliberate recovery's re-attach is
/// inside them: it re-walks and content-hash heals the same tree while the
/// series is being sampled, so that walk's own high-water mark is a candidate
/// for this maximum on every run. [`ATTACH_PEAK_RSS_CEILING_BYTES`] bars the
/// first-attach phase at the 2k profile, and at ≥5k that phase is
/// [`SOAK_HIGH_WATER_RSS_CEILING_BYTES`]'s: the kernel's mark for the whole
/// run, which survives the phase rather than sampling it. The two are read as
/// a pair.
///
/// **The band the bar stands on is the feature-on series, and the re-attach is
/// inside it.** The sixteen nightlies below were taken before the load armed a
/// recovery; the three calibration runs of 2026-09-14 are the first hosted
/// readings under the `induced-failure` build the lane now runs, whose load
/// trips one deliberate recovery and re-walks the ≥5k tree while the series is
/// being sampled. The step between the two sets is 0.4 MiB, so at this profile
/// the re-attach's walk reaches no higher than the height the load already
/// holds. The local reading at the short duration with the recovery in the
/// series is **22.84 MiB**, inside the 22.31–23.20 band below.
///
/// Observed on `ubuntu-latest` x86_64-glibc at the scheduled hour, which is the
/// lane that gates: the peak has been recorded on **every nightly since the
/// instrument landed — sixteen of them, 2026-08-18 through 2026-09-02**. The
/// first seven read 18.02–18.32 MiB; the series then stepped 0.72 MiB between
/// the 2026-08-24 and 2026-08-25 nightlies (18.32 → 19.04, runs 32690822466
/// and 32809428376), and every reading since sits at 19.04–19.32. The seven
/// runs of the suite this ceiling is authored under — 2026-08-27 through
/// 2026-09-02, after the engine crates merged — read **19.17–19.32 MiB** (runs
/// 33085019370, 33187194082, 33248400528, 33304650396, 33382242484,
/// 33490462147 and 33608123948), each with a displayed slope of 1.00 against
/// first-quartile means of 18.86–19.09. **The three feature-on runs the value
/// is re-read on** — 2026-09-14, runs 34877528262 (b2c2e84), 34877987791 and
/// 34884665687 (b8f56b5), each an hour of load that recovered once — read
/// **19.71, 19.76 and 19.78 MiB**, with a displayed slope of 1.00 against
/// first-quartile means of 19.48–19.68. Observed on macos-arm64 at the
/// 90-second default duration a developer runs: **22.31–23.20 MiB over three
/// runs** — three to four MiB above the hosted band, as 16 KiB pages against
/// 4 KiB predict, with a spread six times the hosted one, which is what a
/// short run's cache-filling first minute does.
///
/// The ceiling is 40 MiB, read against the feature-on series above: 2.02x its
/// highest hosted reading and 1.72x the highest local one, the stance
/// [`ATTACH_PEAK_RSS_CEILING_BYTES`] takes and for the same reason. The
/// readings are whole-process peaks, so each carries the binary and its
/// runtime as a fixed addend that a runner image, a page size or an allocator
/// moves without the load costing more — the sixteen-run series has already
/// stepped 0.72 MiB overnight once, with the slope flat through it — and a bar
/// that flakes on the next such step teaches people to rerun rather than to
/// look. What a vault-shaped cost would read here is multiples of the band: a
/// load that held the ≥5k profile's documents resident would clear this many
/// times over, not by the 4 MiB between platforms.
///
/// **Platform scope: the Linux measurement lane.** The sixteen-run series is
/// the scheduled lane's on `ubuntu-latest` x86_64-glibc; the macos-arm64
/// readings are a developer's and gate nothing.
pub const SOAK_PEAK_RSS_CEILING_BYTES: Option<u64> = Some(40 * 1024 * 1024);

/// The highest resident set the **whole** soak run may reach — the first attach
/// and its heal included — or `None` while no ceiling is authored.
///
/// **The attach-and-heal term of the memory invariant at soak scale.**
/// [`SOAK_PEAK_RSS_CEILING_BYTES`] is the maximum of a sampled series that
/// begins once the attachment reads ready, so the first attach's walk over the
/// ≥5k tree finishes before its first sample and a cost paid there and released
/// is outside it. This band is the kernel's own high-water mark for that same
/// process, read once at the end of the run, so what it covers is the phase the
/// series cannot. [`ATTACH_PEAK_RSS_CEILING_BYTES`] bars that phase at 2k, and
/// the reading this band judges covers it at the profile the soak lane runs —
/// barred here once a value is authored, and recorded meanwhile.
///
/// # Profile
///
/// The soak lane's ≥5k `soak` tree under the long mixed load, `induced-failure`
/// on, which is the subject the soak bands above judge. **The deliberate
/// recovery is inside the mark as well.** The re-attach re-walks and
/// content-hash heals the same tree mid-run, so the mark covers the first
/// attach, that re-attach and every tick between them; the sampled peak beside
/// this covers the re-attach only where a tick lands inside it. The mark is
/// therefore never below the sampled peak, and the pair is read together: a
/// mark far above the peak is an attach or a re-attach, which is the cost this
/// band exists to see.
///
/// # Platform scope
///
/// **The Linux measurement lane**, which is the lane that gates. The reading is
/// `VmHWM` from `/proc/self/status`, beside the `VmRSS` every sample is taken
/// from, and the kernel keeps it for the life of the process. macOS publishes
/// no equivalent through the accounting that lane uses, so a run there records
/// no high-water reading and this ceiling judges nothing on it.
///
/// # Observations
///
/// Observed on `ubuntu-latest` x86_64-glibc, the lane that gates, over the
/// three hour-long `workflow_dispatch` runs of the certification lane this
/// value is authored from — 2026-09-14, runs 34877528262 (b2c2e84),
/// 34877987791 and 34884665687 (b8f56b5), `induced-failure` on, one deliberate
/// recovery in each: **19.71, 19.76 and 19.78 MiB**.
///
/// **The mark equalled the sampled peak in every one of the three.** The run's
/// whole-process high-water mark is therefore a height the load reached again
/// inside the sampled window: neither the first attach's walk over the ≥5k tree
/// nor the recovery's re-attach cost more than the load's own working set at
/// this profile. That is the reading the pair exists to make visible, and it
/// reads the same on each of the three.
///
/// macOS publishes no equivalent mark through the accounting this lane uses, so
/// there are no local readings to stand beside these.
///
/// # Safety rationale
///
/// The ceiling is 40 MiB — 2.02x the highest of the three readings — which is
/// [`SOAK_PEAK_RSS_CEILING_BYTES`]'s value and its headroom rule, and the two
/// coincide because the readings do. The reading is a whole-process high-water
/// mark, so it carries the binary and its runtime as a fixed addend that a
/// runner image, a page size or an allocator moves without the attach costing
/// more — the sampled peak beside it has already stepped 0.72 MiB overnight
/// once — and a bar that flakes on such a step teaches people to rerun rather
/// than to look. What a vault-shaped attach would read here is multiples of the
/// band rather than the megabytes between runner images.
///
/// **A mark that rises above the sampled peak is a reading to chase even under
/// this ceiling**, because what rose is an attach or a re-attach rather than
/// the load, and the pair is what says which.
///
/// **The consequence of the two ceilings being equal: the sampled peak can no
/// longer fail alone.** The mark is at or above every sample by construction,
/// so any run that fails the peak fails this bar too. What the pair splits is
/// the subject — the sampled series against the whole run — rather than the
/// threshold, and the two failure messages are what say which phase reached the
/// height. A run where only this bar fails is the attach, the heal or the
/// re-attach; there is no run where only the peak fails.
///
/// # Review trigger
///
/// A run past this ceiling is a claim that attaching and healing the ≥5k
/// profile now costs more, and it is answered by reading the attach rather than
/// by rerunning. Moving the value is a reviewed edit carrying that claim beside
/// it; lowering it needs no new argument. Un-authoring it back to `None`
/// reopens the calibration window, and the registry entry naming this constant
/// keeps those runs from counting toward lockdown's five.
pub const SOAK_HIGH_WATER_RSS_CEILING_BYTES: Option<u64> = Some(40 * 1024 * 1024);

/// How long a churn family's settle may take, or `None` while no ceiling is
/// authored.
///
/// **The clock term of rung 1. The reading is the wall clock from a workload's
/// final change to the derived store holding what a build from zero over the
/// same tree holds** — the full equivalence comparator, findings included,
/// rather than a cheaper projection of it. The churn suite's own settle budgets
/// are runaway bounds and say so — a settle that reaches one is stuck rather
/// than slow — so until this is authored nothing in the workspace states how
/// long convergence after an edit may take. `settle.rs` is the instrument: it
/// runs every leg of every churn family the driver's roll carries, times each
/// from its own final act, and records the reading as it lands. Beside each
/// reading it records one boolean — whether the attachment was publishing
/// `Ready` at the poll that confirmed the reading — because the entry does not
/// leave `Ready` under churn and a duration to it would be the cost of a
/// `state()` call rather than a fact about the subject. That poll is one
/// projection read and one poll gap after the reading itself, so the boolean
/// says the churn withdrew no trust across the settle, not that the entry held
/// `Ready` at the reading's own instant.
///
/// **The instant a calibration reads.** The clock stops where the confirming
/// full projection read *began*, not where it returned: a read observes the
/// store as of its start, so a read that comes back holding the settled state
/// says the settle had already happened by then, and the several hundred
/// milliseconds it takes to materialise a ≥5k-document projection are the
/// instrument's cost. A ceiling authored over the returning instant would gate
/// `StoreProjection::read` more than it gates settle. What is left is a
/// resolution rather than a bias: the settle lies between the start of the last
/// poll that found the store unsettled and that instant, and the reading is the
/// top of that window. `settle.rs` measures the window and records it beside
/// every reading, so the commit that authors this value states its multiple
/// over the widest observed leg with each leg's resolution in view.
///
/// **At the `soak` profile the readings are bounds rather than measurements**,
/// and they say so: every leg's recorded resolution equals its reading, which
/// is `settle.rs` reporting that its first look already found the store
/// agreeing. A census of a ≥5k tree takes longer than the settle it is looking
/// for, so what the readings there bound is a settle that had already happened
/// somewhere inside the first look. A ceiling authored over them is a ceiling
/// on that bound — it fails a settle that grew past one census read of the
/// vault, and it says nothing finer.
///
/// **What the bar is, then, is one census walk, and that is the resolution
/// this value accepts.** The clock starts at the delivered change and the next
/// thing the instrument does is walk the whole ≥5k tree to build the census the
/// polls compare against, so that walk is inside every reading before the first
/// poll. The value below therefore states "the settle finishes inside one
/// filesystem walk of the vault, with headroom": its floor tracks the runner's
/// filesystem rather than the subject, so what it can fail is a regression
/// large against that walk rather than against the settle. The safety
/// rationale below states the sensitivity in seconds. Sharpening it means
/// narrowing the first look — the workload
/// script already implies the tree, which is what
/// `Census::assert_the_script_read_the_tree_the_same_way` checks, so the
/// expected census is derivable rather than walkable and the walk can verify
/// after the clock stops. The review trigger below names that work as what this
/// value is re-authored under, because a narrower first look moves the floor
/// rather than the subject.
///
/// The comparison happens only where a ceiling is authored: `Some` bars the
/// run, `None` records the readings and bars nothing, and the qualification
/// ledger types every such run non-qualifying, so the readings accumulate
/// without the runs counting toward lockdown's five.
///
/// # The constant rules
///
/// - **Profile.** Filled. The ceiling is stated over the `soak` profile — the
///   ≥5k-document scale every other soak bar in the workspace is authored over
///   — and the scheduled lane names it in the step's environment. The scale is
///   load-bearing: every leg reads higher at ≥5k than at the 120-document
///   `small` profile, and the ≥5k readings are floor-bound besides, so a
///   ceiling calibrated at `small` would sit below the instrument's own floor
///   at the gating scale. [`fits`] admits a reading only at or under the
///   ceiling, so such a value would **fail every scheduled run** — a red lane
///   about the scale the number came from rather than about the candidate, and
///   the failure a calibration must not manufacture. A local run defaults to
///   `small`, which is a developer's reading and gates nothing.
/// - **Observations.** Filled. Three `workflow_dispatch` runs of the
///   certification lane on `ubuntu-latest` x86_64-glibc at the `soak` profile,
///   2026-09-14, every leg of every family timed in each: run **34877528262**
///   (b2c2e84) read **1415–1470 ms** over its eight legs, run **34877987791**
///   (b8f56b5) **796–824 ms**, and run **34884665687** (b8f56b5)
///   **1104–1130 ms**. The widest leg of the series is `a burst` at 1470 ms and
///   the narrowest `a case-renamed parent over a save` at 796 ms. **Every leg
///   of every run recorded a resolution equal to its reading**, which is the
///   instrument reporting that its first look already found the store agreeing:
///   each number is a bound on a settle that had happened somewhere inside one
///   census walk, not a measurement of it. A local run defaults to `small` and
///   is a developer's reading, so no band stands beside these.
/// - **Safety rationale.** Filled. The ceiling is 5 s: three times the widest
///   observed leg — 1470 ms, so 4.41 s — rounded up to a whole second. **The
///   floor under every one of those readings is one census walk of the
///   6000-document `soak` tree**, so a reading is that walk plus whatever
///   settle ran past it. **What the bar's sensitivity is, exactly**: at 5 s the
///   smallest settle regression it catches is about 3.5 s on the slowest
///   observed runner, whose walk is 1.47 s, and about 4.2 s on the fastest,
///   whose walk is 0.8 s. A 2x bar — 3 s — would catch a 1.5 s regression on
///   two of the three runners, and a slow night inside the observed 1.8x spread
///   would sit at 2.6–2.9 s against it. Three is chosen anyway, for three
///   reasons: a false failure resets a five-night count, so the cost of
///   flaking is a campaign rather than a rerun; the subject this bar is stated
///   over is a vault-proportional settle regression, which at the ≥5k profile
///   is multiple seconds rather than one; and sensitivity under one walk is the
///   census-from-script sharpening's to give, which is the review trigger
///   below. Rounding 4.41 s up to 5 s rather than to 4.5 s keeps the value in
///   the unit it is argued in, and the 0.6 s it adds is smaller than the
///   run-to-run spread already in the series.
/// - **Platform scope.** Filled. The Linux measurement lane, `ubuntu-latest`
///   x86_64-glibc, is where this gates. The macOS certification lane runs no
///   measurement step, so it neither takes this reading nor evaluates this bar.
///   A local run at the default profile is a developer's reading and gates
///   nothing.
/// - **Review trigger.** Filled. The value moves only by a reviewed edit that
///   states its grounds, under
///   [ADR 0007](../../../docs/decisions/0007-authored-measurement-thresholds.md);
///   raising it is what asks a reviewer for the claim that convergence now
///   costs more, and lowering it needs no new argument. Un-authoring it back to
///   `None` is the recalibration state, and the ledger stamps every run under
///   it non-qualifying rather than letting the window count. **The other
///   trigger is the sharpening above.** A census derived from the workload
///   script rather than walked drops the instrument's floor from one vault walk
///   to the settle itself, and a value authored over walk-bound readings is
///   then a bar over a floor that no longer exists — so that change re-authors
///   this number over readings of the subject.
pub const SOAK_SETTLE_CEILING: Option<Duration> = Some(Duration::from_secs(5));

/// How many descriptors one served attachment may hold.
///
/// **The no-descriptor-cost-per-document term.** The count is taken in the
/// process that attached, after the entry reaches `Ready`, so what this bounds
/// is the steady-state cost of a served attachment rather than the high-water
/// mark of the heal walk that got there. `fd_budget.rs` is the instrument, and
/// the bar the vault-size claim really rests on is not this ceiling: the
/// one-document and 2000-document deltas are asserted **equal**, which fails
/// the moment the cost starts moving with the vault at any height under the
/// budget.
///
/// Observed on macos-arm64 over two consecutive runs, identical in both: **4
/// descriptors per attachment at 1 document and 4 at 2000** — the readings
/// `fd_budget.rs` records beside this value. The budget is 12, three times the
/// measured cost, so a subject that starts holding one more handle per
/// subscription or per store file is caught while a run whose runner hands the
/// process an extra descriptor at the sampling instant is not.
///
/// **Platform scope: every lane that runs the workspace suite, on both
/// platforms.** It is the one bar in this file that is not the Linux
/// measurement lane's, and the reason is what it reads: a descriptor count is a
/// count rather than a clock or a resident set, so under ADR 0004 it may gate a
/// pull request, and it says the same thing on a slow runner as on a fast one.
/// What differs between platforms is which handles a backend holds — FSEvents
/// and inotify are not the same subscription — and the budget is three times
/// the measured cost so that both fit under one number rather than two.
pub const FD_BUDGET: usize = 12;

/// How many recoveries the long mixed load must trip, at the fewest, per run.
///
/// **The deliberate-recovery term of lockdown, and the only authored value here
/// that is a floor rather than a ceiling.** The other bands bound a cost; this
/// one bounds a *dose* — the condition the run is required to have met, so that
/// "the host kept serving under load" is a claim about a load that met one
/// rather than about a load nothing ever disturbed. The load arms `norn-fs`'s
/// watcher fault seam at the stream stage, budgeted to the one establishment
/// its attach makes, so the entry loses coverage once mid-load and the demand
/// the load already holds is what brings it back.
///
/// **The floor is compared with the same [`fits`] every ceiling here uses**, by
/// reading the dose as what the run's count must reach: `fits(dose, count)` is
/// the floor the way `fits(reading, ceiling)` is the ceiling, so the negative
/// control covers this bar's comparison too.
///
/// **It is a count and not a clock**, which is what lets it be authored ahead
/// of any reading: a run that trips no recovery measured nothing about
/// recovering, on a fast runner as on a slow one. What the load bounds with a
/// clock is elsewhere and unchanged — a recovery attempt has `RECOVERY_LIMIT`
/// to bring the entry back, and a run that needs more than `RECOVERY_ATTEMPTS`
/// of them fails as a host that will not come back.
///
/// Observed on `ubuntu-latest` x86_64-glibc at the scheduled hour, which is the
/// lane that gates: **zero on each of the six scheduled runs at ed25f3b**, the
/// suite as it stood before the injection landed — the load recovered only
/// where the host was observed not serving and never arranged for that, so
/// every one of those runs reported `attachment recoveries | 0`. There is no
/// band to read here and there will not be one: the injection is arranged
/// rather than observed, so what the value tracks is what the load arms, and
/// the platform scope is the Linux measurement lane the same way the bands
/// above are scoped, with a local macOS run at the default duration reading the
/// same count because a count does not move with the machine.
///
/// **What one dose buys, exactly.** A run that met it recovered once: the arm
/// fired, the entry came back inside `RECOVERY_LIMIT` of a bounded number of
/// attempts, and across the window's wall clock the load kept churning and
/// taking warm read-only requests at the doses its cadence owes for that much
/// time, charged at the tick period the load was achieving before the window
/// opened. The last of those is a floor with slack in it, and the arranged
/// recovery's window — two or three ticks, the re-attach over the ≥5k tree — is
/// usually short enough to owe nothing at all. So what a passing dose proves
/// about starvation on a typical night is that the load was *still running its
/// cadence across the window*, not that a stalled load would have been caught
/// by this window; `recovery windows, taken/owed` in the run's summary carries
/// what each window actually charged, and a night whose term compared nothing
/// says so there. The shape the term does catch is a window that stretched in
/// wall clock with the load not working across it, which is what starvation
/// looks like when it is real.
///
/// The dose is 1: the fewest that makes the term measured, and exactly what the
/// load arms. The run's count is recorded either way, so a run that reports
/// more has recovered from something it did not arrange — which stands in the
/// summary as the reading it is, beside the one record the seam's own arm
/// wrote.
///
/// **A window this dose admits can be narrow.** One recovery is one window, and
/// the arranged window is about two ticks wide, so the no-starvation term it
/// carries charges one churn turn or none and no warm read at all. What the
/// dose makes true is that the run recovered and kept ticking through it; what
/// it does not make true is that a long stretch of load was served across a
/// recovery. Each run's window prints the doses it charged, so the strength of
/// that night's evidence is read off the summary rather than assumed from the
/// bar.
///
/// **Withdrawing the dose takes a reviewed edit to the value here**, under
/// [ADR 0007](../../../docs/decisions/0007-authored-measurement-thresholds.md).
/// A count has no unauthored state to sit in — the injection is arranged, not
/// measured — so the registry holds this bar `armed` and there is no
/// calibration window for it to open.
pub const SOAK_RECOVERY_DOSE: u32 = 1;

/// How many rounds of its entry gate any one read acquisition may take after
/// its first.
///
/// **The contention-rounds term of the read-concurrency bar.** No acquisition
/// waits for its entry's connection while it holds the entry gate: one that
/// finds the connection taken gives the gate back, waits, and takes the gate
/// again, and in that round it reads afresh the demand its entry publishes,
/// because the demand it read first describes an instant it no longer answers
/// under. That round, a hold of the gate taken again, is the priced cost of
/// contention. Each acquisition counts its own rounds and records them where
/// it leaves, and `counter_gate.rs`'s `overlapping reads on one entry`
/// workload reads the widest any one acquisition took off the host's read
/// account and holds it to this. Beside the ceiling the workload holds a
/// floor: the window's rounds after the first equal its waits, so a contended
/// acquisition that answered under the demand it read before it waited fails
/// too.
///
/// Observed: **one round after the first per contended acquisition**, a widest
/// of 1 and 8 such rounds over 8 waits, in every local run of that case on
/// macos-arm64. It is a count rather than a clock or a resident set, so it
/// reads the same on a loaded runner as on an idle one, and there is no band
/// to author around.
///
/// The ceiling is 1, which is what the acquisition performs. A widest above it
/// is an acquisition that took a round the read path does not have, and the
/// bar fails it rather than pricing it, however many acquisitions beside it
/// took fewer.
///
/// **Platform scope: every platform.** A count is not a machine's reading, so
/// under ADR 0004 it may gate a pull request. The per-PR `counter gates` job on
/// `ubuntu-latest` x86_64-glibc is where it gates.
pub const READ_GATE_ROUNDS_AFTER_THE_FIRST_PER_ACQUISITION: u64 = 1;

/// **The write-through read budgets: how many times an apply reads each file
/// its plan touches, by the protocol that reads it and by what the plan does
/// to the file.** Three protocols read a touched file, and each has a budget
/// of its own per replaced, created and removed target, so a read moved from
/// one protocol to another — or onto a file the plan does not touch — fails
/// as surely as a read added:
///
/// - **document reads** ([`norn_fs::reads::ReadTally::document_opens`]) —
///   `norn-fs`'s contained read of a file's content, by planning for its
///   before-state, by the applier's recomposition, and by the changeset's
///   read-back of what landed;
/// - **target reads** ([`norn_fs::reads::ReadTally::target_reads`]) — the
///   write kernel's no-follow read of a target, by staging and by
///   publication's verification again;
/// - **shadow reads** ([`norn_fs::reads::ReadTally::shadow_reads`]) — the
///   write kernel's confirmation of the staged shadow a write publishes from.
///
/// Each read is one read of the file's bytes and one hash of what it read,
/// counted by the act that hashes, so the count of hashes is this count: a
/// file hashed twice through one descriptor counts twice.
///
/// **What these budgets do not see.** The tally is the job's thread's: a read
/// the apply moved onto another thread would reach no account, a limit every
/// counter-lane read bar shares since Layer 3. And the watcher's echo check of
/// the apply's own writes (`norn-fs`'s `matches_expected`) opens and hashes
/// each written file once more, on the watcher's thread, outside any job's
/// account: that read is excluded from these budgets, not counted in them.
///
/// Observed: every budget below, in every local run on linux-x86_64 of
/// `counter_gate.rs`'s write-through workloads — a `set` by path, a hub's
/// move, two deletes, a `set --where`'s ten matches and a mass delete's two
/// hundred removals — at both per-PR scales, field by field and file by file.
/// They are counts of acts the apply performs, so they read the same on any
/// runner, and the workloads hold each field to its budget exactly, floor and
/// ceiling.
///
/// **Platform scope: every platform.** A count is not a machine's reading, so
/// under ADR 0004 it may gate a pull request. The per-PR `counter gates` job on
/// `ubuntu-latest` x86_64-glibc is where it gates.
///
/// A replaced document's document reads: three — **planning** reads the file
/// for its before-state, **the applier's recomposition** reads it again to
/// run the plan's operations before staging anything, and **the changeset**
/// reads the landed file back once to derive it without the applier holding
/// its bytes, deriving it only where those bytes hash to what was published.
pub const APPLY_DOCUMENT_READS_PER_REPLACED_TARGET: u64 = 3;

/// A replaced document's target reads: two — **staging** reads it through the
/// write kernel's no-follow open and checks it holds the before-state, and
/// **publication** reads it again just before the rename and checks it still
/// does. Budgeted with [`APPLY_DOCUMENT_READS_PER_REPLACED_TARGET`].
pub const APPLY_TARGET_READS_PER_REPLACED_TARGET: u64 = 2;

/// Complete sibling listings per freshly replaced target on a folding root.
/// Staging checks the before-state's spelling once, and publication checks it
/// again before the rename. Each pass reads every non-dot entry, so N targets
/// in a folder holding S total entries cost exactly 2 x N x S write dirents.
/// A root that distinguishes case pays zero. This is a declared listing
/// limit, separate from the per-target content-read budgets above.
///
/// `folding_cost.rs` crosses 32 and 128 targets with 256 and 2048 siblings.
/// Observed on hosted macOS ARM64 (`macos-15`), CI run 37231397772 at
/// 9318cef: 16,384, 65,536, 131,072 and 524,288 write dirents, respectively.
/// The same run's Linux control (`ubuntu-latest`) read zero at all four sizes.
/// Reproduce with `LANE_FEATURES=induced-failure .github/scripts/lane-suite.sh
/// norn-host folding_cost --nocapture --test-threads=1` on a folding volume.
/// The job-thread tally excludes the walk's path confirmation, normalization
/// probes and watcher-thread echoes. This bar covers fresh replacements, not
/// creates, removals, respells or interrupted-plan reapplication.
pub const APPLY_SPELLING_LISTINGS_PER_REPLACED_TARGET: u64 = 2;

/// A created document's document reads: one — **the changeset's read-back**
/// of the landed file. Planning and the recomposition find the name holding
/// no file, and a name with no file opens nothing to read. Budgeted with
/// [`APPLY_DOCUMENT_READS_PER_REPLACED_TARGET`].
pub const APPLY_DOCUMENT_READS_PER_CREATED_TARGET: u64 = 1;

/// A created document's target reads: none — staging and publication find the
/// name holding no file, and an absent target opens nothing to hash. Budgeted
/// with [`APPLY_DOCUMENT_READS_PER_REPLACED_TARGET`].
pub const APPLY_TARGET_READS_PER_CREATED_TARGET: u64 = 0;

/// A removed document's document reads: two — **planning**'s read of its
/// before-state and **the recomposition**'s. The changeset reads none: it asks
/// whether a document stands at the name, which opens no file. Budgeted with
/// [`APPLY_DOCUMENT_READS_PER_REPLACED_TARGET`].
pub const APPLY_DOCUMENT_READS_PER_REMOVED_TARGET: u64 = 2;

/// A removed document's target reads: two — **staging**'s check of its
/// before-state and **publication**'s check again just before the unlink.
/// Budgeted with [`APPLY_DOCUMENT_READS_PER_REPLACED_TARGET`].
pub const APPLY_TARGET_READS_PER_REMOVED_TARGET: u64 = 2;

/// A document a move carries byte for byte, its source's document reads,
/// where the index vouches for the links it holds: two — **planning**'s
/// streamed read of its before-state and **the recomposition**'s, each a hash
/// and a decode verdict from one pass that keeps no byte. Where the index does
/// not vouch for them, planning and the applier each read the file whole once
/// more, for its links — the declared limit of a carried move — which the
/// lane's vaults, indexed before each apply, never meet. Its destination, a
/// created document, is never read for its content before the changeset's
/// read-back. The same count as a removed
/// document's ([`APPLY_DOCUMENT_READS_PER_REMOVED_TARGET`]), kept apart so a
/// move's source is held to its own fate. Budgeted with
/// [`APPLY_DOCUMENT_READS_PER_REPLACED_TARGET`].
pub const APPLY_DOCUMENT_READS_PER_COPIED_AWAY_TARGET: u64 = 2;

/// A document a move carries byte for byte, its source's target reads: three
/// — **staging the destination** streams the source into the destination's
/// shadow through the write kernel's own open, hashing it as it copies
/// (`norn_fs::Content::CopyOf`), then **staging** and **publication** check
/// its before-state before its unlink, as a removed document's do
/// ([`APPLY_TARGET_READS_PER_REMOVED_TARGET`]). The copy is the one more read
/// a carried move pays for holding no copy of the document it moves. Budgeted
/// with [`APPLY_DOCUMENT_READS_PER_REPLACED_TARGET`].
pub const APPLY_TARGET_READS_PER_COPIED_AWAY_TARGET: u64 = 3;

/// **The staged shadows an apply reads per document it writes.** One: the
/// write kernel confirms, just before the publication act, that the shadow a
/// replacement or a creation is published from is still the file staging
/// made and still holds the after-state, opening it and hashing it once. A
/// shadow lives in the shadow home under no name of the vault's, and it is
/// the apply's own file: it is counted apart from the reads of the vault's
/// files so neither count stands in for the other, and each shadow is read
/// once, under a name of its own. A removal stages no shadow. Budgeted, and
/// limited, with [`APPLY_DOCUMENT_READS_PER_REPLACED_TARGET`].
pub const APPLY_SHADOW_READS_PER_WRITTEN_TARGET: u64 = 1;

/// Whether a reading fits under an authored ceiling.
///
/// **The one comparison every measurement bar in this crate makes**, and every
/// one of them makes it: the attach bars, the read bars, the soak bars, the
/// descriptor budget, the settle ceiling, and the recovery dose — which reads
/// the dose as the reading and the run's count as the ceiling, so a floor and a
/// ceiling are the same comparison with the arguments in the order each states
/// its bound in. What "under the ceiling" means is
/// therefore one line rather than one per bar, and the negative control in
/// `settle.rs` feeds that line a reading past a ceiling of each shape the bars
/// are stated in — bytes, a duration, a count and a per-mille ratio — and
/// requires a refusal. A bar that compared with a bare operator would be a bar
/// the control says nothing about, so the coverage claim and the routing are
/// one fact.
///
/// A reading exactly at the ceiling fits: a bar states the most a subject may
/// cost, and costing exactly that is not costing more.
///
/// The two arguments share one type, so a duration is never compared against a
/// count. What this adds over the operator is not arithmetic; it is being the
/// seam a control can grab.
pub fn fits<T: PartialOrd>(reading: T, ceiling: T) -> bool {
    reading <= ceiling
}

/// Every band above is a reading of the unoptimized build. An optimized one
/// allocates differently enough that the bars would be measuring a subject they
/// were not authored against, and a bar authored high enough to hold either
/// would pass quietly rather than fail.
///
/// It fails at run time rather than at compile time on purpose: a release build
/// of the workspace suite is a normal thing to want, and it is only the
/// measurement cases that are wrong under it.
///
/// **The build's feature set is the other half of that subject, and the two
/// halves of this file are read on different ones.** A cargo feature changes
/// what is compiled the way a profile does, so it is stated here rather than
/// left to a reader to recompute off the checkout:
///
/// - The `ATTACH_*` bands are read **feature-off**. `memory.rs` compiles under
///   the plain workspace build and the per-PR lane's attach step names no
///   feature.
/// - The `SOAK_*` bands are read **with `induced-failure` on**. `host_soak.rs`
///   is a whole file behind that feature — the load arms `norn-fs`'s watcher
///   seam to trip its deliberate recovery, and the seam has a reader nowhere
///   else — so its feature set is closed by construction and there is no
///   run-time guard here to match the profile assertion below. The lane that
///   reads them names the feature in its step's `LANE_FEATURES`, and the
///   suite-manifest digest closes over the workflow that does, so the
///   feature-on transition moves the digest and restarts lockdown's count.
///
/// A qualification record carries the platform and the runner but no
/// build-configuration field, so what the measured build was is read here.
#[allow(clippy::assertions_on_constants)] // The constant is the build profile, and the point is to fail the run under the wrong one.
pub fn assert_the_profile_the_bars_were_authored_on() {
    assert!(
        cfg!(debug_assertions),
        "the host measurement baselines are debug-profile values; this suite was built with \
         optimizations, so its bars describe a different subject"
    );
}

/// How a reading is rendered and where it is recorded is the harness's, and
/// every measurement lane in the workspace writes the same table under its run.
/// What is authored per crate is the numbers above, which is what a reviewer
/// reads as one diff.
pub use norn_testkit::readings::{mebibytes, milliseconds, multiple, per_mille, record};
