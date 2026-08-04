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
/// near an edge rather than the middle. The ceiling sits at roughly twice the
/// band's top, because a bar that flakes teaches people to rerun rather than to
/// look.
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
/// at `ambiguous` and 20.26–20.60 MiB at `realistic`.
///
/// The bar is 1.6, which leaves a third of headroom over the worse reading
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
/// Observed over 90-second local runs: **0.99–1.02 on macos-arm64**. The bar is
/// 1.25, which is loose enough that a settling allocator does not fail the lane
/// and tight enough that a load leaking per reconciliation does.
pub const SOAK_RSS_SLOPE_PER_MILLE: u64 = 1_250;

/// How a reading is rendered and where it is recorded is the harness's, and
/// every measurement lane in the workspace writes the same table under its run.
/// What is authored per crate is the numbers above, which is what a reviewer
/// reads as one diff.
pub use norn_testkit::readings::{mebibytes, multiple, per_mille, record};
