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
//! process that attached, read with no cargo feature named; the soak bands are
//! that child's own samples of itself over a long mixed load, read with
//! `induced-failure` on, which is what arms the recovery that load is required
//! to trip. The two subjects are set out beside
//! [`assert_the_profile_the_bars_were_authored_on`]. Repeated local readings
//! cover **macos-arm64** natively.
//!
//! **The platform that gates is `ubuntu-latest` x86_64-glibc.** Every authored
//! band below carries its hosted readings beside the local ones, the same way
//! the generator's baselines carry both architectures they were measured on. A
//! band un-authored back to `None` for recalibration carries the readings it
//! had, because what a recalibration window gathers is the next set. The soak
//! bands' hosted readings come off the nightly lane's hour-long load at the
//! ≥5k profile; the local
//! readings beside them are the same case at the short default duration, which
//! is what a developer runs.
//!
//! **Three bands here can spell a calibration state, and none of them is in
//! one.** [`SOAK_PEAK_RSS_CEILING_BYTES`], [`SOAK_HIGH_WATER_RSS_CEILING_BYTES`]
//! and [`SOAK_SETTLE_CEILING`] are `Option`s: `Some` bars the run, and `None`
//! records the reading, bars nothing, and stamps its own runs non-qualifying
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
/// [`ATTACH_PEAK_RSS_CEILING_BYTES`] takes and for the same reason. The readings are whole-process peaks, so each carries
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

/// Whether a reading fits under an authored ceiling.
///
/// **The one comparison every measurement bar in this crate makes**, and all
/// nine of them make it: the two attach bars, the four soak bars, the
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
