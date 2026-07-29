//! The authored baselines the measurement suites assert against, and the
//! trace a soak run leaves behind.
//!
//! **Every value here is authored, and it moves only by a reviewed edit.**
//! That is the same discipline `probe::CALIBRATION` holds for tree shape,
//! applied to runtime cost: the file is the trend's whole memory, so a
//! measurement that drifts fails rather than quietly redefining what normal
//! is. There is no history to fetch and no store to consult — a number moves
//! when somebody changes it in a diff, with the grounds beside it.
//!
//! What binds mechanically is the comparison: a reading past the value here
//! fails. The direction the values travel is review-held — nothing forbids
//! raising one, and a reviewer reading the diff is what asks for the claim
//! that the subject now costs more. Lowering one needs no new argument.
//!
//! The wall-clock value is not a gate. It is a **sanity ceiling**, and it
//! lives in the soak lane for that reason: a hosted runner's throughput varies
//! by more than any interesting regression, so the ceiling is set where a run
//! past it is broken rather than slow, and the value worth reading is the
//! recorded number in the job summary rather than the pass.
//!
//! # Where the readings come from
//!
//! Every band below is the pinned toolchain's unoptimized build, peak resident
//! set as the kernel accounted it to the generator process. Repeated local
//! readings cover **linux-arm64** under docker and **macos-arm64** natively;
//! the first `ubuntu-latest` **x86_64-glibc** run measured the `realistic`
//! profile at 3.50–3.62 MiB with a 1.20 flatness ratio. The scheduled soak
//! profile's bands remain the two repeated local sets stated below.
//!
//! Two integration binaries compile this module — `memory.rs` for the per-PR
//! lane and `memory_soak.rs` for the scheduled one — and each asserts against
//! the values its lane owns, so the remainder being unused in either is the
//! layout rather than a defect. The values stay in one file because the file
//! is the trend's whole memory, and a reviewer reads it as one diff.
#![allow(dead_code)]

use std::time::Duration;

/// Peak resident set the `realistic` profile's generation must stay under.
///
/// Observed over 8 runs per architecture: **2.79–3.11 MiB on linux-arm64** and
/// **3.7–4.2 MiB on macos-arm64** — indicative bands from repeated local
/// runs, not a fixed measurement, so a rerun lands near an edge rather than
/// the middle. The ceiling sits at roughly twice the higher band's top,
/// because a bar that flakes teaches people to rerun rather than to look.
///
/// What it forbids is a generation whose cost is the tree's total bytes.
/// Holding this profile's clutter set rather than writing each file as it is
/// drawn reads **8.40–8.67 MiB on linux-arm64 and 9.50–9.73 MiB on
/// macos-arm64**, which is outside this bar on both. The margin on
/// linux-arm64 is thin — about 5% — so the shape's real detector is
/// [`GATE_PAIR_PEAK_RSS_PER_MILLE`], which reads it at better than 1.5x on
/// both architectures.
pub const GATE_PEAK_RSS_CEILING_BYTES: u64 = 8 * 1024 * 1024;

/// How much of the `ambiguous` profile's peak the `realistic` profile's peak
/// may reach, per mille.
///
/// **The flatness invariant in the per-PR lane.** Both scales are under 5k
/// documents, so both are per-PR work under ADR 0004's by-kind split, and the
/// pair is what a ceiling cannot be: a ceiling passes anything that fits under
/// it, and a ratio fails the moment the two scales stop moving together.
///
/// `ambiguous` emits 300 documents and 129 KiB of clutter; `realistic` emits
/// 2000 and 4.34 MiB — a 35x spread in the bytes a tree-shaped cost would be
/// a function of. The generator's peak is not that: it writes each file as it
/// draws it and holds one path and one digest per emitted file, so the
/// observed ratios are **1.18–1.40 on linux-arm64 and 1.29–1.57 on
/// macos-arm64** (8 runs per profile per architecture, banded worst-case
/// across runs).
///
/// The bar is 2.0, which leaves better than a quarter of headroom over the
/// worse reading. A generation holding its clutter set adds those bytes to
/// each side — about 0.12 MiB at `ambiguous` against about 4.34 MiB at
/// `realistic`, against a floor of a couple of megabytes either way — so its
/// ratio is **3.12–3.70**, better than 1.5x past this bar on both
/// architectures.
pub const GATE_PAIR_PEAK_RSS_PER_MILLE: u64 = 2_000;

/// Peak resident set the `soak` profile's generation must stay under.
///
/// Observed over 6 runs per architecture: **4.55–4.76 MiB on linux-arm64** and
/// **5.48–5.89 MiB on macos-arm64**. The profile emits three times the
/// documents and about five times the bytes of the gate profile, and costs
/// about a mebibyte and a half more to generate — which is the shape
/// [`SOAK_TO_GATE_PEAK_RSS_PER_MILLE`] states as a ratio. Holding the emitted
/// clutter instead reads 51.6–51.8 MiB on linux-arm64 and 54.5–54.7 MiB on
/// macos-arm64.
pub const SOAK_PEAK_RSS_CEILING_BYTES: u64 = 16 * 1024 * 1024;

/// How much of the gate profile's peak the soak profile's peak may reach, per
/// mille.
///
/// The same instrument as [`GATE_PAIR_PEAK_RSS_PER_MILLE`] one scale up, and a
/// soak case because the ≥5k profile is the scheduled lane's work whatever it
/// costs to run. The soak profile holds three times the documents and about
/// five times the bytes; the generator's peak grows by neither, so the
/// observed ratios are **1.47–1.71 on linux-arm64 and 1.32–1.55 on
/// macos-arm64** against a document count that trebled.
///
/// The bar is 2.2, which leaves better than a quarter of headroom over the
/// worse reading. A generation holding its output reads **5.60–6.17** here,
/// about 2.5x the bar.
pub const SOAK_TO_GATE_PEAK_RSS_PER_MILLE: u64 = 2_200;

/// Wall clock the `soak` profile's generation must finish inside.
///
/// A sanity ceiling, not a bar on speed: the generation bills roughly
/// **3.2–3.7 s** locally, over 6 runs on each of linux-arm64 and macos-arm64,
/// and this is roughly fifty times that. The band is indicative rather than a
/// fixed measurement — a rerun lands near an edge rather than the middle — and
/// a hosted runner competing for a disk is slower still, in ways no regression
/// is, so anything tight enough to catch a regression here would fail for
/// reasons that are not about this repository. The number the lane exists to
/// record is the measured duration in the job summary.
pub const SOAK_WALL_CLOCK_CEILING: Duration = Duration::from_secs(180);

/// Bytes, rendered as mebibytes to two places.
pub fn mebibytes(bytes: u64) -> String {
    format!(
        "{}.{:02}",
        bytes / (1024 * 1024),
        bytes * 100 / (1024 * 1024) % 100
    )
}

/// A ratio expressed per mille, rendered as a decimal multiple.
pub fn multiple(per_mille: u64) -> String {
    format!("{}.{:02}", per_mille / 1000, per_mille / 10 % 100)
}

/// `numerator / denominator`, per mille, with a zero denominator reported as
/// zero rather than reached for.
pub fn per_mille(numerator: u64, denominator: u64) -> u64 {
    (numerator * 1000).checked_div(denominator).unwrap_or(0)
}

/// Record a reading where a person will find it: the job summary GitHub
/// renders under the run, and this process's standard error either way.
///
/// A measurement lane's product is the trend, and a trend nobody can read is a
/// pass with nothing behind it. Outside a workflow there is no summary file,
/// so the readings go to standard error alone and the suite is unaffected.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Appends this run's readings to the job-summary file the workflow names.
pub fn record(heading: &str, readings: &[(&str, String)]) {
    let mut block = format!("### {heading}\n\n| reading | value |\n| --- | --- |\n");
    for (label, value) in readings {
        block.push_str(&format!("| {label} | {value} |\n"));
    }
    block.push('\n');
    eprintln!("{block}");

    let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY") else {
        return;
    };
    use std::io::Write;
    let appended = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| file.write_all(block.as_bytes()));
    if let Err(problem) = appended {
        // A summary that could not be written is a lost reading, not a failed
        // measurement, and failing the run here would report the wrong thing.
        eprintln!("could not append to the job summary: {problem}");
    }
}
