//! `repair`: the operations a batch of findings plans to, the findings each
//! fixes, and the findings it leaves alone.
//!
//! **Three kinds of finding have a declared fix; every other is skipped.** A
//! selected `field/required-missing` finding fills from the default the rules
//! requiring the field declare; a `field/not-one-of` finding is replaced by
//! the member a rule's synonym maps its offending value to, a list field's
//! elements fixed one by one into one `set_frontmatter` of the field; a
//! `field/forbidden` finding is removed or renamed as a rule declares. All are
//! filled and judged by [`declared`], one operation per fix (a rename is two),
//! numbered `repair-1`, `repair-2` and on in plan order, each cited at
//! [`Confidence::Declared`](norn_wire::Confidence::Declared) with the
//! findings it fixes. A fix that cannot be made is skipped with its reason and
//! its decision data — no fix declared, candidates that tie or defaults that
//! disagree, a capture bound several ways, a rename onto an occupied field, a
//! fix that would bring in required fields, or one the write gate would
//! refuse. A finding the rules conflict over (`field/rules-conflict`,
//! `document/rules-conflict`) is skipped as [`SkipReason::RulesConflict`]. A
//! finding of a document that does not read — a path derived state cannot
//! hold (not UTF-8, or spelling no document path), a body that does not
//! decode, or a frontmatter block nothing read — is skipped as
//! [`SkipReason::Unreadable`]; an ambiguous link is skipped as
//! [`SkipReason::AmbiguousLink`], with the candidates its row carries; every
//! other finding is skipped as [`SkipReason::NoDeclaredFix`], with the value
//! it judged where it carries one. Routes (NORN-374's later steps) and derived
//! fixes (NORN-375) add operations here, beside these.
//!
//! **A document is composed in finding order.** Its findings are taken in
//! kind, field and offending value order, each fix composed onto the bytes the
//! earlier ones left and judged against the document's own before-state by
//! the applier's one judge ([`crate::applier::verdict`]); a fix the judge
//! refuses is skipped and the next composes without it, so the plan stays
//! applicable. A selected finding the composed document no longer holds is
//! dropped: it is neither fixed nor skipped. The skipped findings keep the
//! batch's order.
//!
//! **The planning is pure**: a function of the batch's rows, the pinned
//! declaration, the case its globs compare under, the bytes each document
//! with a fix to make held, read once through `read`, and the plan's one clock
//! reading ([`OneReading`]), taken only where a default reads `{{now}}`,
//! `{{date}}` or `{{time}}`. **A clock that gives no reading refuses the
//! repair as it refuses a creation**: the operation of each default that
//! reads the clock is left in [`Planned::unresolved`], and the host's handler
//! answers `vault/plan-refused` in both modes. The host's `repair` handler
//! (`crate::apply`) owns the hold, the view the bytes are read from, the
//! resolution and the provenance around it.

mod declared;

use std::sync::Arc;

use norn_wire::{
    CaseFold, Citation, DocumentPath, FindingKind, FindingRow, Operation, SkipReason,
    SkippedCandidates, SkippedFinding, UnresolvedOperation,
};

use crate::clock::OneReading;
use crate::derivation::Declared;

/// What a batch of findings plans to.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Planned {
    /// The operations, in plan order, numbered `repair-1`, `repair-2` and on.
    pub(crate) operations: Vec<Operation>,
    /// The findings each operation fixes, keyed by the operation's id.
    pub(crate) citations: Vec<Citation>,
    /// The findings left alone, in batch order.
    pub(crate) skipped: Vec<SkippedFinding>,
    /// The operations a fix would have been that cannot be resolved because
    /// the clock gives no reading, numbered in the operations' sequence. The
    /// repair is refused where there is any, as a creation is.
    pub(crate) unresolved: Vec<UnresolvedOperation>,
}

/// What a repair plans under: the declaration the applier judges its result
/// by, how that declaration's path globs compare letters, and the plan's one
/// clock reading.
pub(crate) struct Repairing<'a> {
    /// The declaration the entry's store pins.
    pub(crate) declared: &'a Declared,
    /// How the rules' path globs compare letters with a document's path, as
    /// the root's recorded path order names it.
    pub(crate) case: CaseFold,
    /// The plan's one clock reading, shared with the planning its operations
    /// resolve through.
    pub(crate) clock: &'a OneReading<'a>,
}

/// What one read of a batch document's bytes found.
pub(crate) enum Before {
    /// The document, whole.
    Held(Arc<[u8]>),
    /// No document whose bytes can be read: the document was taken away, or
    /// something that is no document stands there now.
    Unread,
}

/// Plan the findings `rows`, a repair batch's, in the order it read them,
/// under `repairing`, reading the bytes of each document a fix may be made
/// to through `read`, once, and no other document's.
pub(crate) fn plan<E>(
    rows: &[FindingRow],
    repairing: &Repairing<'_>,
    read: &mut dyn FnMut(&DocumentPath) -> Result<Before, E>,
) -> Result<Planned, E> {
    let mut planned = Planned::default();
    let mut decided: Vec<Option<SkippedFinding>> = Vec::with_capacity(rows.len());
    // A batch never splits a document, and it reads in path order, so one
    // document's findings stand together.
    for document in rows.chunk_by(|left, right| left.path == right.path) {
        if !document.iter().any(declared::has_fix) {
            decided.extend(document.iter().map(|row| Some(skipped(row))));
            continue;
        }
        match read(&document[0].path)? {
            Before::Held(bytes) => {
                decided.extend(declared::compose(document, &bytes, repairing, &mut planned));
            }
            Before::Unread => decided.extend(document.iter().map(|row| {
                Some(if declared::has_fix(row) {
                    SkippedFinding::new(row.id, SkipReason::Unreadable)
                } else {
                    skipped(row)
                })
            })),
        }
    }
    planned.skipped = decided.into_iter().flatten().collect();
    Ok(planned)
}

/// The finding `row` left alone, for the reason no fix is made from the row
/// alone: it is about a document nothing read, it is an ambiguous link a
/// repair does not choose between, its rules conflict, or no rule declares a
/// fix for it.
fn skipped(row: &FindingRow) -> SkippedFinding {
    if row.kind == FindingKind::Ambiguous {
        // A repair reads the finding at its own snapshot. Link health is
        // re-decided inside every changeset (ADR 0027), so the head the row
        // carries is the class at that snapshot.
        return SkippedFinding::new(row.id, SkipReason::AmbiguousLink)
            .with_candidates(SkippedCandidates::documents(row.head.clone()));
    }
    let reason = if leaves_document_unread(row.kind) {
        SkipReason::Unreadable
    } else if matches!(
        row.kind,
        FindingKind::FieldRulesConflict | FindingKind::DocumentRulesConflict
    ) {
        SkipReason::RulesConflict
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
/// first three: a path derived state cannot hold (not UTF-8, or spelling no
/// document path), or a body that does not decode.
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
        Candidate, CandidateHead, DocumentPath, FindingKind, FindingRow, Severity, SkipReason,
        ValueHead,
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

    /// `rows` planned under a declaration stating nothing, no document read:
    /// none of the rows the cases here plan has a fix to make.
    fn plan(rows: &[FindingRow]) -> Planned {
        let declared = Declared::unpinned();
        let clock = || panic!("a plan of no fix read the clock");
        let reading = OneReading::of(&clock);
        let repairing = Repairing {
            declared: &declared,
            case: CaseFold::Exact,
            clock: &reading,
        };
        let read = &mut |path: &DocumentPath| -> Result<Before, std::convert::Infallible> {
            panic!("`{path}` was read for a plan of no fix")
        };
        match super::plan(rows, &repairing, read) {
            Ok(planned) => planned,
        }
    }

    fn twins(names: &[&str]) -> CandidateHead {
        let candidates = names.iter().map(|name| {
            Candidate::new(
                DocumentPath::new(format!("{name}/twin.md")).expect("a path"),
                *name,
            )
        });
        CandidateHead::new(candidates, names.len() as u64).expect("a head")
    }

    /// **A finding no rule declares a fix for is skipped as such, with the
    /// value it judged.**
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
    /// under.** A quarantined file (a path derived state cannot hold, or a
    /// body that does not decode) stands under the first three; a
    /// frontmatter block nothing read, under the rest.
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
        // A missing required field, a value outside a closed set and a
        // forbidden field may have a fix, which reads its document
        // (`declared`), and a rules conflict is skipped as one.
        let readable: Vec<_> = FindingKind::ALL
            .into_iter()
            .filter(|kind| {
                !unreadable.contains(kind)
                    && !matches!(
                        kind,
                        FindingKind::Ambiguous
                            | FindingKind::RequiredMissing
                            | FindingKind::NotOneOf
                            | FindingKind::Forbidden
                            | FindingKind::FieldRulesConflict
                            | FindingKind::DocumentRulesConflict
                    )
            })
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

    /// **A finding the rules conflict over is skipped as a rules conflict,
    /// with the value it judged where it names one**: the field's whole
    /// value, and none for where the document stands.
    #[test]
    fn rules_conflict_findings_skip_as_rules_conflict() {
        let rows = [
            row(3, FindingKind::FieldRulesConflict, "a.md", Some("x")),
            row(4, FindingKind::DocumentRulesConflict, "a.md", None),
        ];

        let planned = plan(&rows);

        assert!(planned.operations.is_empty());
        assert_eq!(
            planned.skipped,
            vec![
                SkippedFinding::new(3, SkipReason::RulesConflict)
                    .with_value(ValueHead::of("x", content_hash(b"x"))),
                SkippedFinding::new(4, SkipReason::RulesConflict),
            ]
        );
    }

    /// **An ambiguous link is skipped as ambiguous, naming the candidates its
    /// row carries.**
    #[test]
    fn an_ambiguous_link_is_skipped_with_its_rows_candidates() {
        let mut row = row(4, FindingKind::Ambiguous, "pointer.md", None);
        row.head = twins(&["a", "b"]);

        let planned = plan(&[row.clone()]);

        let expected = SkippedFinding::new(4, SkipReason::AmbiguousLink)
            .with_candidates(SkippedCandidates::documents(row.head));
        assert_eq!(planned.skipped, vec![expected]);
    }
}
