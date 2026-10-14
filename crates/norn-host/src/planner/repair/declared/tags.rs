//! One-edit suggestions from the literal declared tag vocabulary.

use super::names::{self, NameFix};
use super::{Composed, Document, Reckoned, State, skip};
use norn_wire::{Confidence, FindingRow, OperationKind, SkipReason};

/// The fixed score wording of a declared-tag suggestion.
fn score_note(tag: &str) -> String {
    format!("declared tag `{tag}` is one edit away by optimal string alignment under the tag fold")
}

/// The ruled whole-name tag measure, shared by the read gate and the fix.
pub(super) fn one_edit(from: &str, tag: &str) -> bool {
    let from: Vec<_> = norn_wire::fold_tag(from).chars().collect();
    let candidate: Vec<_> = norn_wire::fold_tag(tag).chars().collect();
    from.len() >= 3
        && candidate.len() >= 3
        && norn_store::similarity::osa_distance(&from, &candidate, 1) == Some(1)
}

/// Rewrite a whole undeclared name only when exactly one declared name is
/// one edit away; the document's ordinary judge admits its composed result.
fn propose(
    document: &Document<'_>,
    state: &State,
    row: &FindingRow,
    at: usize,
) -> Result<NameFix, Box<norn_wire::SkippedFinding>> {
    let from = row
        .target
        .as_deref()
        .expect("an undeclared tag names its tag");
    let mut winners = Vec::new();
    for tag in document.schema().tags().declared() {
        let mut work = document.repairing.work.get();
        work.declared_tags += 1;
        document.repairing.work.set(work);
        if one_edit(from, tag) {
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
    Ok(NameFix {
        operation,
        rows: vec![at],
        conditions: Vec::new(),
        candidates,
        note: score_note(first),
    })
}

/// Decide all of the holder's undeclared tags before composing their name batch.
pub(super) fn fixes(
    reckoned: &Reckoned<'_>,
    document: &Document<'_>,
    composed: &mut Composed,
    rows: &[usize],
) {
    let mut fixes = Vec::new();
    for &at in rows {
        let row = &reckoned.rows[at];
        let Some(finding) = &reckoned.found[at] else {
            continue;
        };
        if composed.state.holding(finding).is_none() {
            continue;
        }
        match propose(document, &composed.state, row, at) {
            Ok(fix) => fixes.push(fix),
            Err(skip) => composed.decided[at] = Some(*skip),
        }
    }
    names::compose(reckoned, document, composed, fixes);
}
