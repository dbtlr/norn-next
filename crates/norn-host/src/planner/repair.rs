//! `repair`: the operations a batch of findings plans to, the findings each
//! fixes, and the findings it leaves alone.
//!
//! **Today no finding has a fix, so every finding is skipped.** A finding of
//! a document that does not read — a quarantined path or body, or a
//! frontmatter block nothing read — is skipped as
//! [`SkipReason::Unreadable`]; every other finding is skipped as
//! [`SkipReason::NoDeclaredFix`], with the value it judged where it carries
//! one. Declared fixes (NORN-374) and derived fixes (NORN-375) add operations
//! and citations here, and the skips that remain keep the batch's order.
//!
//! The function is pure: it reads the batch's rows and nothing else, so the
//! host's `repair` handler (`crate::apply`) owns the hold, the resolution and
//! the provenance around it.

use norn_wire::{Citation, FindingKind, FindingRow, Operation, SkipReason, SkippedFinding};

/// What a batch of findings plans to.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Planned {
    /// The operations, in plan order. A repair plan numbers them `repair-1`,
    /// `repair-2`, and so on.
    pub(crate) operations: Vec<Operation>,
    /// The findings each operation fixes, keyed by the operation's id.
    pub(crate) citations: Vec<Citation>,
    /// The findings left alone, in batch order.
    pub(crate) skipped: Vec<SkippedFinding>,
}

/// Plan the findings `rows`, a repair batch's, in the order it read them.
pub(crate) fn plan(rows: &[FindingRow]) -> Planned {
    Planned {
        operations: Vec::new(),
        citations: Vec::new(),
        skipped: rows.iter().map(skipped).collect(),
    }
}

/// The finding `row` left alone, for the reason no fix exists yet: it is
/// about a document nothing read, or no rule declares a fix for it.
fn skipped(row: &FindingRow) -> SkippedFinding {
    let reason = if leaves_document_unread(row.kind) {
        SkipReason::Unreadable
    } else {
        SkipReason::NoDeclaredFix
    };
    let skipped = SkippedFinding::new(row.id, reason);
    match &row.value {
        Some(value) => skipped.with_value(value.clone()),
        None => skipped,
    }
}

/// Whether a finding of `kind` states that the document's path, bytes or
/// frontmatter block could not be read. A quarantined file stands under the
/// first three: an undecodable path or body.
fn leaves_document_unread(kind: FindingKind) -> bool {
    matches!(
        kind,
        FindingKind::PathBytesNotUtf8
            | FindingKind::PathNamesNoDocument
            | FindingKind::BodyBytesNotUtf8
            | FindingKind::FrontmatterTooLarge
            | FindingKind::FrontmatterUnclosed
            | FindingKind::FrontmatterUnreadable
    )
}

#[cfg(test)]
mod tests {
    use norn_wire::{
        CandidateHead, DocumentPath, FindingKind, FindingRow, Severity, SkipReason, ValueHead,
    };

    use super::*;
    use crate::planner::compose::content_hash;

    fn row(id: u64, kind: FindingKind, at: &str, value: Option<&str>) -> FindingRow {
        let row = FindingRow::new(
            id,
            kind,
            Severity::Warning,
            DocumentPath::new(at).expect("a document path"),
            None,
            None,
            CandidateHead::new([], 0).expect("an empty head"),
            None,
            "a finding",
            1,
        );
        match value {
            Some(value) => row.with_value(ValueHead::of(value, content_hash(value.as_bytes()))),
            None => row,
        }
    }

    /// **No fix exists yet, so a finding is skipped as one no rule declares a
    /// fix for, with the value it judged.**
    #[test]
    fn a_finding_no_rule_declares_a_fix_for_is_skipped_with_its_value() {
        let rows = [row(7, FindingKind::UndeclaredTag, "a.md", Some("draft"))];

        let planned = plan(&rows);

        assert!(planned.operations.is_empty());
        assert!(planned.citations.is_empty());
        let expected = SkippedFinding::new(7, SkipReason::NoDeclaredFix)
            .with_value(ValueHead::of("draft", content_hash(b"draft")));
        assert_eq!(planned.skipped, vec![expected]);
    }

    /// **A finding about a document nothing read is skipped as unreadable,
    /// whichever of the six kinds a quarantine or an unread block files it
    /// under.** A quarantined file (undecodable path or body) stands under
    /// the first three; a frontmatter block nothing read, under the rest.
    #[test]
    fn a_finding_about_a_document_nothing_read_is_skipped_as_unreadable() {
        let unreadable = [
            FindingKind::PathBytesNotUtf8,
            FindingKind::PathNamesNoDocument,
            FindingKind::BodyBytesNotUtf8,
            FindingKind::FrontmatterTooLarge,
            FindingKind::FrontmatterUnclosed,
            FindingKind::FrontmatterUnreadable,
        ];
        for kind in unreadable {
            let planned = plan(&[row(1, kind, "a.md", None)]);
            assert_eq!(
                planned.skipped,
                vec![SkippedFinding::new(1, SkipReason::Unreadable)],
                "{kind:?}"
            );
        }
        let readable: Vec<_> = FindingKind::ALL
            .into_iter()
            .filter(|kind| !unreadable.contains(kind))
            .collect();
        for kind in readable {
            let planned = plan(&[row(1, kind, "a.md", None)]);
            assert_eq!(
                planned.skipped,
                vec![SkippedFinding::new(1, SkipReason::NoDeclaredFix)],
                "{kind:?}"
            );
        }
    }

    /// **The skipped findings keep the batch's order.**
    #[test]
    fn the_skipped_findings_keep_the_batch_order() {
        let rows = [
            row(9, FindingKind::Broken, "a.md", None),
            row(2, FindingKind::Misplaced, "a.md", None),
            row(5, FindingKind::UndeclaredTag, "b.md", None),
        ];

        let skipped: Vec<u64> = plan(&rows)
            .skipped
            .into_iter()
            .map(|skipped| skipped.finding)
            .collect();

        assert_eq!(skipped, vec![9, 2, 5]);
    }
}
