//! The fidelity seam: what a two-store comparison concluded, in a shape
//! something other than a failing assertion can read.
//!
//! **What this is.** The equivalence comparator answers one question per case
//! and answers it by passing or failing. That is the right shape for a gate and
//! the wrong shape for a trend: a suite that has passed a hundred times says
//! nothing about whether the populations it stood on shrank, and a case that
//! fails names one field with no record of which field the last twenty runs
//! named. This module is the seam that record goes through — a
//! [`FidelityReading`] per comparison, written as one JSON line under a run.
//!
//! **What it is not.** Nothing reads these lines yet. The seam exists so that
//! the consumers Layer 2 commits to have somewhere to attach: the drift scans
//! that ask whether a divergence class is recurring, and the post-lockdown
//! comparison of one candidate's readings against the run before it. Both want
//! the same record from every comparison the suites make, and a record invented
//! per consumer is two vocabularies for one judgment.
//!
//! **Why it is off unless asked for.** Writing a line per comparison on every
//! developer's machine is noise, and a suite that failed because it could not
//! write a telemetry line would report the wrong thing. So the sink is named by
//! the environment — `NORN_FIDELITY_LOG` — and a reading with no sink is
//! rendered to standard error and dropped, exactly the way a measurement
//! reading with no job summary is.

use crate::equivalence::{Comparison, Divergence, Population};

/// The environment variable naming the file readings are appended to.
///
/// One JSON object per line, appended: a run's readings are the file's lines in
/// the order the cases produced them, and a reader needs no framing to take one.
pub const SINK: &str = "NORN_FIDELITY_LOG";

/// One comparison, as the record a drift scan reads.
///
/// Serializable and named by field rather than by position, because the
/// consumers arrive later than the producer: a reading written by this build has
/// to be readable by a scan written against a build that added a field.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FidelityReading {
    /// What was compared, as the suite named it.
    pub subject: String,
    /// Whether the two projections agreed about every field.
    pub equal: bool,
    /// How much the first projection stood on.
    pub left: Population,
    /// How much the second stood on.
    pub right: Population,
    /// The first field the two disagreed about, and nothing where they agreed.
    ///
    /// One field rather than every field: what a scan asks of a history is which
    /// field keeps being named, and a comparison that reported every downstream
    /// consequence of one divergence would answer that with noise.
    pub first_divergence: Option<Divergence>,
}

impl FidelityReading {
    /// Read a comparison as the record of it.
    pub fn of(subject: &str, comparison: &Comparison) -> Self {
        FidelityReading {
            subject: subject.to_string(),
            equal: comparison.is_equal(),
            left: comparison.left,
            right: comparison.right,
            first_divergence: comparison.divergence.clone(),
        }
    }
}

/// Record what a comparison concluded, where a scan will find it.
///
/// A reading always reaches standard error, so a case's own output says what it
/// judged whether or not a sink was named. A sink that could not be written is a
/// lost reading rather than a failed comparison, and failing the run here would
/// report the wrong thing — the same ruling [`crate::readings::record`] makes
/// about a job summary.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Harness scaffolding: appends this run's fidelity readings to the file the environment names.
pub fn record(subject: &str, comparison: &Comparison) {
    let reading = FidelityReading::of(subject, comparison);
    let line = match serde_json::to_string(&reading) {
        Ok(line) => line,
        Err(problem) => {
            eprintln!("could not render a fidelity reading: {problem}");
            return;
        }
    };
    eprintln!("fidelity: {line}");

    let Some(path) = std::env::var_os(SINK) else {
        return;
    };
    use std::io::Write;
    let appended = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| writeln!(file, "{line}"));
    if let Err(problem) = appended {
        eprintln!("could not append to the fidelity log: {problem}");
    }
}

#[cfg(test)]
mod tests {
    use super::FidelityReading;
    use crate::equivalence::{Comparison, Divergence, Population};

    fn population(documents: usize) -> Population {
        Population {
            documents,
            facts: documents,
            findings: 0,
            indexed_terms: documents,
            vault_schema_pinned: true,
        }
    }

    #[test]
    fn an_equal_comparison_reads_as_equal_with_both_populations() {
        let reading = FidelityReading::of(
            "two derivations",
            &Comparison {
                divergence: None,
                left: population(3),
                right: population(3),
            },
        );
        assert!(reading.equal);
        assert_eq!(reading.first_divergence, None);
        assert_eq!(reading.left.documents, 3);
        assert_eq!(reading.right.documents, 3);
    }

    /// The record survives a round trip through its own rendering, which is what
    /// a later reader depends on: a scan reads lines this build wrote.
    #[test]
    fn a_reading_round_trips_through_its_rendering() {
        let reading = FidelityReading::of(
            "two derivations",
            &Comparison {
                divergence: Some(Divergence {
                    field: "document[docs/a.md].content_hash".to_string(),
                    left: "hash-1".to_string(),
                    right: "hash-2".to_string(),
                }),
                left: population(3),
                right: population(2),
            },
        );
        let rendered = serde_json::to_string(&reading).expect("rendering a reading");
        let read: FidelityReading = serde_json::from_str(&rendered).expect("reading it back");
        assert_eq!(read, reading);
        assert!(!read.equal);
        assert_eq!(
            read.first_divergence.expect("a divergence").field,
            "document[docs/a.md].content_hash"
        );
    }
}
