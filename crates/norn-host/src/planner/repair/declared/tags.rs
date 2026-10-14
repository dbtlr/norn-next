//! One-edit suggestions from the literal declared tag vocabulary.

use super::{Document, Fix, Fixing, State, cited, skip};
use norn_wire::{Confidence, FindingRow, OperationKind, SkipReason};

/// The fixed score wording of a declared-tag suggestion.
fn score_note(tag: &str) -> String {
    format!("declared tag `{tag}` is one edit away by optimal string alignment under the tag fold")
}

/// Rewrite a whole undeclared name only when exactly one declared name is
/// one edit away; the document's ordinary judge admits its composed result.
pub(super) fn fix(
    document: &Document<'_>,
    state: &State,
    row: &FindingRow,
    finding: &crate::applier::Held,
) -> Result<Fix, Box<norn_wire::SkippedFinding>> {
    let from = row
        .target
        .as_deref()
        .expect("an undeclared tag names its tag");
    let folded: Vec<_> = norn_wire::fold_tag(from).chars().collect();
    if folded.len() < 3 {
        return Err(Box::new(crate::planner::repair::skipped(row)));
    }
    let mut winners = Vec::new();
    for tag in document.schema().tags().declared() {
        let mut work = document.repairing.work.get();
        work.declared_tags += 1;
        document.repairing.work.set(work);
        let candidate: Vec<_> = norn_wire::fold_tag(tag).chars().collect();
        if candidate.len() < 3 {
            continue;
        }
        if norn_store::similarity::osa_distance(&folded, &candidate, 1) == Some(1) {
            winners.push(tag.to_string());
        }
    }
    let Some((first, rest)) = winners.split_first() else {
        return Err(Box::new(crate::planner::repair::skipped(row)));
    };
    let candidates = super::closed_set::derived_candidates(&winners);
    if !rest.is_empty() {
        return Err(Box::new(if document.admits(Confidence::Suggested) {
            skip(row, SkipReason::Tie)
                .with_candidates(candidates)
                .with_note(
                    winners
                        .iter()
                        .take(norn_wire::CANDIDATE_HEAD)
                        .map(|tag| score_note(tag))
                        .collect::<Vec<_>>()
                        .join("; "),
                )
        } else {
            crate::planner::repair::skipped(row)
        }));
    }
    let operation = OperationKind::rewrite_tag(state.at.clone(), from, first);
    let next = document.admit(
        state,
        std::slice::from_ref(&operation),
        Fixing {
            row,
            finding,
            candidates: &candidates,
        },
    )?;
    let note = score_note(first);
    document
        .within_threshold(Confidence::Suggested, row, &operation)
        .map_err(|skipped| Box::new(skipped.with_note(note.clone())))?;
    Ok(Fix {
        operations: vec![operation],
        cited: vec![cited(row, Confidence::Suggested, vec![note])],
        state: next,
        removed_field: None,
    })
}
