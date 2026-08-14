//! What an induced fault says about itself, and how a parent reads it.
//!
//! A bar over a process that dies has one hard problem: the process that met
//! the condition is not the process that judges it. What is left at rest says
//! what the outcome *was*, and it says nothing about whether the arm the case
//! aimed at is the arm that fired — a hook deleted from the protocol leaves an
//! unarmed write, and an unarmed write leaves a destination that also satisfies
//! "the old document or the complete new one". The outcome assertion passes
//! either way, and the case stops carrying anything.
//!
//! So every arm records itself where it fires, in the child, before it answers.
//! The record names the seam and the checkpoint, and the parent asserts on it
//! beside the outcome: a hook that is bypassed leaves no record, and the case
//! fails on the missing record rather than on state that never changed.
//!
//! **The writers are not here.** A record is written by the crate whose
//! protocol fired — `norn-fs` at its write stages, `norn-store` at its
//! changeset boundaries — and neither may depend on this crate. What lives here
//! is the reading and the vocabulary: one record per line, space-separated
//! `key=value` fields, `seam` naming the protocol and the rest naming whatever
//! that protocol has to say about where it stood.

use std::collections::BTreeMap;
use std::path::Path;

/// The field every record carries: which protocol's seam fired.
pub const SEAM: &str = "seam";

/// One record an arm wrote when it fired.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArmHit {
    fields: BTreeMap<String, String>,
}

impl ArmHit {
    /// What this record says under `field`, or nothing where it says nothing.
    pub fn get(&self, field: &str) -> Option<&str> {
        self.fields.get(field).map(String::as_str)
    }

    /// Whether every one of `expected` stands in this record with that value.
    pub fn carries(&self, expected: &[(&str, &str)]) -> bool {
        expected
            .iter()
            .all(|(field, value)| self.get(field) == Some(*value))
    }
}

impl std::fmt::Display for ArmHit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rendered: Vec<String> = self
            .fields
            .iter()
            .map(|(field, value)| format!("{field}={value}"))
            .collect();
        write!(f, "{}", rendered.join(" "))
    }
}

/// Every record a run left behind, in the order the arms fired.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Attestation {
    hits: Vec<ArmHit>,
}

impl Attestation {
    /// Read what is at `path`, which is nothing at all where no arm fired.
    ///
    /// A missing file and an empty one are one answer here — an arm that never
    /// fired never opened the file — and the difference between them is not one
    /// any case is stated over.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the record file a child wrote.
    pub fn read(path: &Path) -> Attestation {
        let contents = std::fs::read_to_string(path).unwrap_or_default();
        Attestation {
            hits: contents.lines().filter_map(parse).collect(),
        }
    }

    /// Every record read.
    pub fn hits(&self) -> &[ArmHit] {
        &self.hits
    }

    /// Whether any record carries every one of `expected`.
    pub fn holds(&self, expected: &[(&str, &str)]) -> bool {
        self.hits.iter().any(|hit| hit.carries(expected))
    }

    /// Fail unless some record carries every one of `expected`.
    ///
    /// This is the assertion that separates "the outcome is right" from "the
    /// checkpoint this case names is the one that produced it".
    #[track_caller]
    pub fn assert_reached(&self, subject: &str, expected: &[(&str, &str)]) {
        assert!(
            self.holds(expected),
            "{subject}: no arm recorded {}\nrecorded: {}",
            render(expected),
            self.render_all(),
        );
    }

    /// Fail where some record carries every one of `expected`.
    #[track_caller]
    pub fn assert_never_reached(&self, subject: &str, expected: &[(&str, &str)]) {
        assert!(
            !self.holds(expected),
            "{subject}: an arm recorded {}, and this case is stated over its absence\nrecorded: {}",
            render(expected),
            self.render_all(),
        );
    }

    /// Fail unless exactly `count` records were written.
    ///
    /// A checkpoint bar is about one stage. A run that recorded two fired
    /// somewhere the case did not name, which is a different run from the one
    /// being judged.
    #[track_caller]
    pub fn assert_count(&self, subject: &str, count: usize) {
        assert_eq!(
            self.hits.len(),
            count,
            "{subject}: recorded {}",
            self.render_all()
        );
    }

    fn render_all(&self) -> String {
        if self.hits.is_empty() {
            return "nothing".to_string();
        }
        self.hits
            .iter()
            .map(ArmHit::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    }
}

fn render(expected: &[(&str, &str)]) -> String {
    expected
        .iter()
        .map(|(field, value)| format!("{field}={value}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Read one line, or nothing where it carries no field at all.
fn parse(line: &str) -> Option<ArmHit> {
    let fields: BTreeMap<String, String> = line
        .split_whitespace()
        .filter_map(|field| field.split_once('='))
        .map(|(field, value)| (field.to_string(), value.to_string()))
        .collect();
    (!fields.is_empty()).then_some(ArmHit { fields })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_is_read_field_by_field() {
        let hit = parse("seam=norn-fs/write stage=swap answer=ends").expect("a record");
        assert_eq!(hit.get(SEAM), Some("norn-fs/write"));
        assert_eq!(hit.get("stage"), Some("swap"));
        assert_eq!(hit.get("missing"), None);
        assert!(hit.carries(&[("stage", "swap"), ("answer", "ends")]));
        assert!(!hit.carries(&[("stage", "create")]));
    }

    /// A line saying nothing is not a record. A child that wrote a blank line,
    /// or a trailing newline, has not attested to reaching anything.
    #[test]
    fn a_line_with_no_field_is_no_record() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("   "), None);
        assert_eq!(parse("no-fields-here"), None);
    }

    /// The absence assertion is what a bypassed-hook pin rests on, so it has to
    /// hold over a reading that found nothing at all.
    #[test]
    fn nothing_recorded_reaches_nothing() {
        let empty = Attestation::default();
        assert!(!empty.holds(&[(SEAM, "norn-fs/write")]));
        empty.assert_never_reached("an unarmed run", &[(SEAM, "norn-fs/write")]);
        empty.assert_count("an unarmed run", 0);
    }
}
