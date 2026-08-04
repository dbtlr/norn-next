//! Rendering a measurement, and recording it where a person will find it.
//!
//! A measurement lane's product is the trend, and a trend nobody can read is a
//! pass with nothing behind it. This module holds the rendering a reading is
//! written in and the one place it is written to; the numbers a reading is
//! compared against are authored per crate, beside the suite that asserts them.
//!
//! The renderings are integer arithmetic on purpose. A bar is a whole number of
//! bytes or a ratio per mille, so a reading formatted through a float would
//! print a value the comparison never made.

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
/// Outside a workflow there is no summary file, so the readings go to standard
/// error alone and the suite is unaffected.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Harness scaffolding: appends this run's readings to the job-summary file the workflow names.
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

#[cfg(test)]
mod tests {
    use super::{mebibytes, multiple, per_mille};

    #[test]
    fn bytes_render_as_mebibytes_to_two_places() {
        assert_eq!(mebibytes(0), "0.00");
        assert_eq!(mebibytes(1024 * 1024), "1.00");
        assert_eq!(mebibytes(3 * 1024 * 1024 + 512 * 1024), "3.50");
    }

    #[test]
    fn a_per_mille_ratio_renders_as_a_multiple() {
        assert_eq!(multiple(1_000), "1.00");
        assert_eq!(multiple(2_200), "2.20");
        assert_eq!(multiple(1_405), "1.40");
    }

    /// A zero denominator is a reading that measured nothing, and the caller
    /// asserting on the ratio is what says so — dividing by it here would end
    /// the run before the assertion could name what was wrong.
    #[test]
    fn a_ratio_against_nothing_is_zero_rather_than_a_panic() {
        assert_eq!(per_mille(4, 2), 2_000);
        assert_eq!(per_mille(4, 0), 0);
    }
}
