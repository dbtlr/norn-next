//! `repair`: the operations a batch of findings plans to, the findings each
//! fixes, and the findings it leaves alone.
//!
//! **Today no finding has a fix, so every finding is skipped.** A finding of
//! a document that does not read — a path derived state cannot hold (not
//! UTF-8, or spelling no document path), a body that does not decode, or a
//! frontmatter block nothing read — is skipped as
//! [`SkipReason::Unreadable`]; an ambiguous link is skipped as
//! [`SkipReason::AmbiguousLink`], with the documents its address names now;
//! every other finding is skipped as [`SkipReason::NoDeclaredFix`], with the
//! value it judged where it carries one. Declared fixes (NORN-374) and derived
//! fixes (NORN-375) add operations and citations here, and the skips that
//! remain keep the batch's order.
//!
//! The function reads the batch's rows, and the one thing it asks beyond them
//! is the live ambiguity class of an ambiguous link, through the caller's
//! `live_candidates`: a repair reads the class as it stands, never a
//! finding's snapshot of it. The host's `repair` handler (`crate::apply`)
//! owns the hold, the resolution and the provenance around it.

use norn_wire::{
    CandidateHead, Citation, FindingKind, FindingRow, Operation, SkipReason, SkippedCandidates,
    SkippedFinding,
};

/// What a batch of findings plans to.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Planned {
    /// The operations, in plan order. Their ids are minted when fixes add
    /// operations (NORN-374).
    pub(crate) operations: Vec<Operation>,
    /// The findings each operation fixes, keyed by the operation's id.
    pub(crate) citations: Vec<Citation>,
    /// The findings left alone, in batch order.
    pub(crate) skipped: Vec<SkippedFinding>,
}

/// Plan the findings `rows`, a repair batch's, in the order it read them.
///
/// `live_candidates` answers what a suffix address names now, headed, and
/// `None` where it names several no longer; an error it returns ends the
/// planning with it.
pub(crate) fn plan<E>(
    rows: &[FindingRow],
    live_candidates: &mut dyn FnMut(&str) -> Result<Option<CandidateHead>, E>,
) -> Result<Planned, E> {
    let skipped = rows
        .iter()
        .map(|row| skipped(row, live_candidates))
        .collect::<Result<_, E>>()?;
    Ok(Planned {
        operations: Vec::new(),
        citations: Vec::new(),
        skipped,
    })
}

/// The finding `row` left alone, for the reason no fix exists yet: it is
/// about a document nothing read, it is an ambiguous link a repair does not
/// choose between, or no rule declares a fix for it.
fn skipped<E>(
    row: &FindingRow,
    live_candidates: &mut dyn FnMut(&str) -> Result<Option<CandidateHead>, E>,
) -> Result<SkippedFinding, E> {
    if row.kind == FindingKind::Ambiguous {
        return ambiguous_link(row, live_candidates);
    }
    let reason = if leaves_document_unread(row.kind) {
        SkipReason::Unreadable
    } else {
        SkipReason::NoDeclaredFix
    };
    let skipped = SkippedFinding::new(row.id, reason);
    Ok(match &row.value {
        Some(value) => skipped.with_value(value.clone()),
        None => skipped,
    })
}

/// The ambiguous link `row` is about, skipped with the documents its address
/// names now. An ambiguous link is a wikilink's suffix address, written with
/// no protocol, so the finding's target less its anchor is the address. A
/// class that no longer names several leaves the head the finding was filed
/// with, which the same snapshot judged.
fn ambiguous_link<E>(
    row: &FindingRow,
    live_candidates: &mut dyn FnMut(&str) -> Result<Option<CandidateHead>, E>,
) -> Result<SkippedFinding, E> {
    let written = row.target.as_deref().unwrap_or_default();
    let address = written
        .split_once('#')
        .map_or(written, |(address, _)| address);
    let head = match live_candidates(address)? {
        Some(live) => live,
        None => row.head.clone(),
    };
    Ok(SkippedFinding::new(row.id, SkipReason::AmbiguousLink)
        .with_candidates(SkippedCandidates::documents(head)))
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
    use std::convert::Infallible;

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

    /// Plan `rows` over a vault where no ambiguity class is read.
    fn plan(rows: &[FindingRow]) -> Planned {
        let Ok(planned) = super::plan::<Infallible>(rows, &mut |_| Ok(None));
        planned
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
        let readable: Vec<_> = FindingKind::ALL
            .into_iter()
            .filter(|kind| !unreadable.contains(kind) && *kind != FindingKind::Ambiguous)
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

    /// **An ambiguous link is skipped as ambiguous, naming the documents its
    /// address names now rather than the head its finding was filed with.**
    /// The address asked is the finding's target less its anchor.
    #[test]
    fn an_ambiguous_link_is_skipped_naming_the_documents_its_address_names_now() {
        let mut row = row(4, FindingKind::Ambiguous, "pointer.md", None);
        row.target = Some("twin#Heading".to_string());
        row.head = twins(&["a", "b"]);
        let live = twins(&["a", "b", "c"]);
        let mut asked = Vec::new();

        let Ok(planned) = super::plan::<Infallible>(&[row], &mut |address| {
            asked.push(address.to_string());
            Ok(Some(live.clone()))
        });

        assert_eq!(asked, ["twin"]);
        let expected = SkippedFinding::new(4, SkipReason::AmbiguousLink)
            .with_candidates(SkippedCandidates::documents(live));
        assert_eq!(planned.skipped, vec![expected]);
    }

    /// **A class that names several no longer leaves the head the finding was
    /// filed with.**
    #[test]
    fn an_ambiguous_link_whose_class_names_several_no_longer_keeps_its_filed_head() {
        let mut row = row(4, FindingKind::Ambiguous, "pointer.md", None);
        row.target = Some("twin".to_string());
        row.head = twins(&["a", "b"]);

        let planned = plan(&[row.clone()]);

        let expected = SkippedFinding::new(4, SkipReason::AmbiguousLink)
            .with_candidates(SkippedCandidates::documents(row.head));
        assert_eq!(planned.skipped, vec![expected]);
    }

    /// **A refusal of the live read ends the planning with it.**
    #[test]
    fn a_refused_live_read_ends_the_planning() {
        let mut row = row(4, FindingKind::Ambiguous, "pointer.md", None);
        row.target = Some("twin".to_string());

        let planned = super::plan(&[row], &mut |_| Err("the read was refused"));

        assert_eq!(planned.map(|_| ()), Err("the read was refused"));
    }
}
