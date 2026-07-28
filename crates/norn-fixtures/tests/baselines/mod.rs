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
//! The peak-memory values are **ratchets**: the only edit that needs no new
//! argument is one that lowers them. Raising one is a claim that the
//! generator now costs more, and it is made in the open.
//!
//! The wall-clock value is not a ratchet and not a gate. It is a **sanity
//! ceiling**, and it lives in the soak lane for that reason: a hosted runner's
//! throughput varies by more than any interesting regression, so the ceiling
//! is set where a run past it is broken rather than slow, and the value worth
//! reading is the recorded number in the job summary rather than the pass.
//!
//! Each integration binary compiles the whole module and uses part of it, so
//! the unused remainder is expected rather than a defect.
#![allow(dead_code)]

use std::time::Duration;

/// Peak resident set the `realistic` profile's generation must stay under.
///
/// Readings of the pinned toolchain's unoptimized build: about 3.0 MiB on
/// linux and about 4.3 MiB on macOS, varying by under a tenth of a mebibyte
/// across runs. The ceiling is set well above the higher of the two because
/// an allocator this suite has not been run against is entitled to arrange a
/// few megabytes differently, and a bar that flakes teaches people to rerun
/// rather than to look. What it forbids is a generation whose cost is the
/// tree's total bytes: holding this profile's emitted files rather than
/// writing each as it is drawn reads at about 10 MiB.
pub const GATE_PEAK_RSS_CEILING_BYTES: u64 = 12 * 1024 * 1024;

/// Peak resident set the `soak` profile's generation must stay under.
///
/// Readings of the same build: about 4.7 MiB on linux and about 6.2 MiB on
/// macOS. The profile emits three times the documents and about five times
/// the bytes of the gate profile, and costs a little over a mebibyte more to
/// generate — which is the shape [`SOAK_TO_GATE_PEAK_RSS_PER_MILLE`] states
/// as a ratio. Holding the emitted files instead reads at about 55 MiB.
pub const SOAK_PEAK_RSS_CEILING_BYTES: u64 = 16 * 1024 * 1024;

/// How much of the gate profile's peak the soak profile's peak may reach, per
/// mille.
///
/// **This is the memory invariant measured rather than asserted.** The soak
/// profile holds three times the documents and about five times the bytes;
/// the generator's peak grows by neither, because it writes each file as it
/// draws it and holds one path and one digest per emitted file rather than
/// the files themselves. What is left growing is that per-file bookkeeping
/// and the largest single document, so the ratio observed is about 1.42 on
/// macOS and 1.56 on linux against a document count that trebled.
///
/// The bar is 2.2, which leaves better than a third of headroom over the
/// worse reading and still fails an order of magnitude short of the 5.7 a
/// generation holding its output reads.
pub const SOAK_TO_GATE_PEAK_RSS_PER_MILLE: u64 = 2_200;

/// Wall clock the `soak` profile's generation must finish inside.
///
/// A sanity ceiling, not a bar on speed: the generation bills about 3.2
/// seconds locally, and this is roughly fifty times that. A hosted runner
/// competing for a disk is slow in ways no regression is, so anything tight
/// enough to catch a regression here would fail for reasons that are not
/// about this repository. The number the lane exists to record is the
/// measured duration in the job summary.
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

/// Record a soak reading where a person will find it: the job summary GitHub
/// renders under the run, and this process's standard error either way.
///
/// The lane's product is the trend, and a trend nobody can read is a pass with
/// nothing behind it. Outside a workflow there is no summary file, so the
/// readings go to standard error alone and the suite is unaffected.
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
