//! Name fixes share one batched text proof and one ordinary judgment per attempt.

use super::{Composed, Document, Made, Reckoned, cited, skip};
use crate::planner::edit::{self, NameRefusal};
use norn_wire::{
    AuthorCondition, Confidence, Operation, OperationKind, SkipReason, SkippedCandidates,
    SkippedFinding,
};

/// One operation and the selected findings its original name group answers.
pub(super) struct NameFix {
    /// The name operation, written where its holder ends.
    pub operation: OperationKind,
    /// The row positions it cites or skips together.
    pub rows: Vec<usize>,
    /// The evidence observed for this operation.
    pub conditions: Vec<AuthorCondition>,
    /// The winning spelling's decision data.
    pub candidates: SkippedCandidates,
    /// The family's fixed score wording.
    pub note: String,
}

/// Map a text refusal to the family's existing individual skip.
fn refused(reckoned: &Reckoned<'_>, fix: &NameFix, reason: NameRefusal) -> SkippedFinding {
    let row = &reckoned.rows[fix.rows[0]];
    match &fix.operation {
        OperationKind::RewriteAnchor { .. } => {
            let note = match reason {
                NameRefusal::Text(reason) => edit::rewrite_skip(reason).to_string(),
                NameRefusal::NoMatch => "no matching link remains".to_string(),
                other => other.detail(&fix.operation),
            };
            skip(row, SkipReason::LinkNotRewritable).with_note(note)
        }
        OperationKind::RewriteTag { .. } => skip(row, SkipReason::JudgeWouldRefuse)
            .with_candidates(fix.candidates.clone())
            .with_note(format!(
                "the fix cannot be set into the document: {}",
                reason.detail(&fix.operation)
            )),
        _ => unreachable!("a name fix"),
    }
}

/// Compose a family's proposals together and isolate only refused fixes by halving.
/// A below-threshold attempt proves the same writes without publishing its state.
pub(super) fn compose(
    reckoned: &Reckoned<'_>,
    document: &Document<'_>,
    composed: &mut Composed,
    fixes: Vec<NameFix>,
) {
    if fixes.is_empty() {
        return;
    }
    let mut state = composed.state.clone();
    let outcomes = edit::compose_set(&mut state, &fixes, &mut |state, fixes| {
        let mut work = document.repairing.work.get();
        work.compositions += 1;
        document.repairing.work.set(work);
        let kinds: Vec<_> = fixes.iter().map(|fix| &fix.operation).collect();
        let (bytes, results) = edit::rewritten_names(&kinds, &state.bytes)
            .map_err(|reason| Box::new(refused(reckoned, &fixes[0], reason)))?;
        let outcomes: Vec<_> = fixes
            .iter()
            .zip(results)
            .map(|(fix, result)| result.map_err(|reason| Box::new(refused(reckoned, fix, reason))))
            .collect();
        let Some(fix) = fixes
            .iter()
            .zip(&outcomes)
            .find_map(|(fix, outcome)| outcome.is_ok().then_some(fix))
        else {
            return Ok((state.clone(), outcomes));
        };
        let row = &reckoned.rows[fix.rows[0]];
        let next = document.judged_change(state, &state.at, bytes, row, &fix.candidates)?;
        for (fix, outcome) in fixes.iter().zip(&outcomes) {
            if outcome.is_err() {
                continue;
            }
            for &at in &fix.rows {
                if let Some(finding) = &reckoned.found[at]
                    && next.holding(finding).is_some()
                {
                    return Err(Box::new(
                        skip(&reckoned.rows[at], SkipReason::JudgeWouldRefuse)
                            .with_candidates(fix.candidates.clone())
                            .with_note("the fix leaves it still standing"),
                    ));
                }
            }
        }
        Ok((next, outcomes))
    });
    if document.admits(Confidence::Suggested) {
        composed.state = state;
    }
    for (fix, outcome) in fixes.into_iter().zip(outcomes) {
        let skipped = match outcome {
            Err(skip) => Some(*skip),
            Ok(()) if !document.admits(Confidence::Suggested) => Some(
                skip(&reckoned.rows[fix.rows[0]], SkipReason::BelowThreshold)
                    .with_proposed(
                        Operation::new(fix.operation.clone())
                            .with_conditions(fix.conditions.clone()),
                    )
                    .with_note(fix.note.clone()),
            ),
            Ok(()) => None,
        };
        for &at in &fix.rows {
            composed.decided[at] = skipped.as_ref().map(|skip| {
                let mut skip = skip.clone();
                skip.finding = reckoned.rows[at].id;
                skip
            });
        }
        if skipped.is_none() {
            composed.made.push(Made::Fixed {
                operations: vec![fix.operation],
                conditions: fix.conditions,
                cited: fix
                    .rows
                    .iter()
                    .map(|&at| {
                        cited(
                            &reckoned.rows[at],
                            Confidence::Suggested,
                            vec![fix.note.clone()],
                        )
                    })
                    .collect(),
                removed_field: None,
            });
        }
    }
}
