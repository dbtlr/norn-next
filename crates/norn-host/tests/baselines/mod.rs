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
//! attachment bands are the peak resident set the kernel accounted to the child
//! process that attached; the soak bands are that child's own samples of itself
//! over a long mixed load. Repeated local readings cover **macos-arm64**
//! natively.
//!
//! **The platform that gates is `ubuntu-latest` x86_64-glibc.** Every band
//! below carries its hosted readings beside the local ones, the same way the
//! generator's baselines carry both architectures they were measured on. The
//! soak bands' hosted readings come off the nightly lane's hour-long load at
//! the ≥5k profile; the local readings beside them are the same case at the
//! short default duration, which is what a developer runs.
//!
//! **Two bands here spell their calibration state.**
//! [`SOAK_PEAK_RSS_CEILING_BYTES`] and [`SOAK_SETTLE_CEILING`] are `Option`s:
//! `Some` bars the run, and `None` records the reading, bars nothing, and
//! stamps its own runs non-qualifying through the ledger's exit-bar registry
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
//! Four integration binaries compile this module — `memory.rs` for the per-PR
//! memory lane, `fd_budget.rs` for the per-PR workspace suite, `host_soak.rs`
//! for the scheduled lane and `settle.rs` for the scheduled lane's churn
//! clock — and each asserts against the values its lane owns, so the remainder
//! being unused in any one of them is the layout rather than a defect. The
//! values stay in one file because the file is the trend's whole memory, and a
//! reviewer reads it as one diff.
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
/// Observed over 5 runs: **20.26–20.60 MiB on macos-arm64** — an indicative
/// band from repeated local runs, not a fixed measurement, so a rerun lands
/// near an edge rather than the middle. On `ubuntu-latest` x86_64-glibc the
/// hosted band across this lane's two attaches at this profile is **16.73–18.82
/// MiB** over ten readings: 17.69 and 18.26 at the first hosted run, 16.73 and
/// 17.00 in the Layer 1 acceptance pass, and 17.73/18.30, 17.96/18.18,
/// 18.82/18.37 and 17.95/17.91 across four consecutive `memory invariant` runs.
/// Every hosted reading sits below the local band, as 4 KiB pages against 16
/// KiB predict. **The hosted spread is about two MiB wide and is not a trend to
/// read**: one run's two attaches differ by as much as 0.57 MiB, which is the
/// same order as the spread across runs, so no ordering of these readings
/// carries a direction and the number that bounds them is the top one.
///
/// The ceiling stays at 40 MiB, which is 1.94x the local band's top and 2.13x
/// the highest hosted reading. The readings it holds are whole-process peaks,
/// so each carries the child binary and its runtime as a fixed addend that a
/// page-size or allocator change moves without the attachment costing more —
/// and a bar that flakes teaches people to rerun rather than to look. What a
/// vault-shaped cost would read here is multiples of the band, not the 2 MiB of
/// spread between platforms.
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
/// of those changesets are committed. The observed ratios are **1.17–1.20 on
/// macos-arm64** (5 runs per profile), against attach peaks of 17.09–17.35 MiB
/// at `ambiguous` and 20.26–20.60 MiB at `realistic`; the hosted band on
/// `ubuntu-latest` x86_64-glibc is **1.13–1.26** over five runs — 1.21 (14.60
/// against 17.69 MiB) at the first, then 1.19, 1.17, 1.26 and 1.13 across four
/// consecutive `memory invariant` runs, against bases of 14.83–15.82 MiB. A
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

/// How much of the first quartile's mean resident set the last quartile's mean
/// may reach, per mille.
///
/// **The flat-memory-slope term of lockdown.** A leak under a load that runs
/// for an hour shows as a rising resident set, so the sample series is split in
/// four and the last quartile's mean is held against the first's. Comparing
/// quartile means rather than endpoints is what keeps one sample taken during a
/// changeset commit from deciding the run.
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
/// **What it is the peak of is the load, and the attach is not in it.** The
/// series starts once the attachment reads ready, so the heal walk over the
/// ≥5k tree completes before the first sample and its cost is outside every
/// reading taken here. [`ATTACH_PEAK_RSS_CEILING_BYTES`] bars that phase at the
/// 2k profile and nothing bars it at ≥5k — an attach peak at soak scale is a
/// third reading, needing an instrument that survives the phase rather than
/// samples it, and it is not taken yet.
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
/// first-quartile means of 18.86–19.09. Observed on macos-arm64 at the
/// 90-second default duration a developer runs: **22.31–23.20 MiB over three
/// runs** — three to four MiB above the hosted band, as 16 KiB pages against
/// 4 KiB predict, with a spread six times the hosted one, which is what a
/// short run's cache-filling first minute does.
///
/// The ceiling is 40 MiB: 2.07x the highest hosted reading and 1.72x the
/// highest local one, the stance [`ATTACH_PEAK_RSS_CEILING_BYTES`] takes and
/// for the same reason. The readings are whole-process peaks, so each carries
/// the binary and its runtime as a fixed addend that a runner image, a page
/// size or an allocator moves without the load costing more — the sixteen-run
/// series has already stepped 0.72 MiB overnight once, with the slope flat
/// through it — and a bar that flakes on the next such step
/// teaches people to rerun rather than to look. What a vault-shaped cost would
/// read here is multiples of the band: a load that held the ≥5k profile's
/// documents resident would clear this many times over, not by the 4 MiB
/// between platforms.
///
/// **Platform scope: the Linux measurement lane.** The sixteen-run series is
/// the scheduled lane's on `ubuntu-latest` x86_64-glibc; the macos-arm64
/// readings are a developer's and gate nothing.
pub const SOAK_PEAK_RSS_CEILING_BYTES: Option<u64> = Some(40 * 1024 * 1024);

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
/// `Ready` at the instant equivalence was reached — because the entry does not
/// leave `Ready` under churn and a duration to it would be the cost of a
/// `state()` call rather than a fact about the subject.
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
///   load-bearing: the same instrument reads two to three times higher at ≥5k
///   than at the 120-document `small` profile, so a ceiling calibrated at
///   `small` would pass every scheduled run trivially and could not fail a
///   settle that had regressed toward vault-proportional. A local run defaults
///   to `small`, which is a developer's reading and gates nothing.
/// - **Observations.** *Unauthored.* The value here is authored by a commit
///   that records the runs behind it — the lane, the dates, the run ids, and
///   the per-leg band each reading fell in — the way
///   [`SOAK_PEAK_RSS_CEILING_BYTES`] records its sixteen. Those runs come off
///   the scheduled platform at the scheduled duration, under ADR 0007.
/// - **Safety rationale.** *Unauthored.* The same commit states the multiple
///   the ceiling stands at over the widest observed leg and why that multiple
///   is the right one for a reading whose spread is a scheduler's rather than
///   an allocator's: a settle is a clock, so a runner under load moves it much
///   further than a page size moves a resident set, and a bar that flakes
///   teaches people to rerun rather than to look.
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
///   it non-qualifying rather than letting the window count.
pub const SOAK_SETTLE_CEILING: Option<Duration> = None;

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

/// Whether a reading fits under an authored ceiling.
///
/// **The one comparison every measurement ceiling in this crate makes**, and
/// all seven of them make it: the two attach bars, the three soak bars, the
/// descriptor budget and the settle ceiling. What "under the ceiling" means is
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
