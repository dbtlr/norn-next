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
//! **One band here is unauthored.** [`SOAK_PEAK_RSS_CEILING_BYTES`] is `None`:
//! the host's peak resident set at the ≥5k profile is recorded each run and
//! barred against nothing, because no run before the one that records it
//! measured it. A ceiling authored from the slope's quartile means would be a
//! number with no reading behind it, and under [ADR
//! 0007](../../../../docs/decisions/0007-authored-measurement-thresholds.md) a
//! threshold is an authored constraint with stated grounds — so the bar waits
//! for calibration runs of the scheduled lane on the scheduled platform.
//!
//! Two integration binaries compile this module — `memory.rs` for the per-PR
//! lane and `host_soak.rs` for the scheduled one — and each asserts against the
//! values its lane owns, so the remainder being unused in either is the layout
//! rather than a defect. The values stay in one file because the file is the
//! trend's whole memory, and a reviewer reads it as one diff.
#![allow(dead_code)]

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
pub const SOAK_RSS_SLOPE_PER_MILLE: u64 = 1_150;

/// Peak resident set the host may reach **under** the long mixed load at the
/// ≥5k profile, or `None` while no ceiling is authored.
///
/// **The peak term of the memory invariant at soak scale.** The slope beside
/// this reads the trend and says nothing about the height the series reaches;
/// the maximum of the same samples is what says a load that stayed flat stayed
/// flat somewhere reasonable. The reading is recorded every run either way, and
/// the comparison happens only where a ceiling is authored: `Some` bars the
/// run, `None` records the reading and bars nothing.
///
/// **What it is the peak of is the load, and the attach is not in it.** The
/// series starts once the attachment reads ready, so the heal walk over the
/// ≥5k tree completes before the first sample and its cost is outside every
/// reading taken here. [`ATTACH_PEAK_RSS_CEILING_BYTES`] bars that phase at the
/// 2k profile and nothing bars it at ≥5k — an attach peak at soak scale is a
/// third reading, needing an instrument that survives the phase rather than
/// samples it, and it is not taken yet.
///
/// **It is `None` because no reading exists yet.** No run before the one that
/// records it took a peak at this profile — `host_soak` sampled the current
/// resident set for the slope and kept no maximum — so there is nothing to
/// author a ceiling from. The quartile means beside it are means of a sampled
/// series and not its height, so deriving a ceiling from them would state a
/// number no measurement stands behind, which is what [ADR
/// 0007](../../../../docs/decisions/0007-authored-measurement-thresholds.md)
/// refuses. The value it takes comes from calibration runs of the scheduled
/// lane on the scheduled platform at the scheduled duration, and it lands as a
/// reviewed edit carrying those readings.
pub const SOAK_PEAK_RSS_CEILING_BYTES: Option<u64> = None;

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
pub use norn_testkit::readings::{mebibytes, multiple, per_mille, record};
