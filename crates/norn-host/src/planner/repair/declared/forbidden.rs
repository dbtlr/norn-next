//! Forbidden fixes: what a rule declares for a field it forbids.
//!
//! The candidates for a selected `field/forbidden` finding are the fixes the
//! contributing rules declare for the field: to remove it, or to rename it to
//! another field. A rule that lists the field plainly declares no fix and
//! proposes nothing. The candidates agree when every one removes the field, or
//! every one renames it to the same field; any other mix is
//! [`SkipReason::Tie`], with each candidate as the schema spells it
//! (`remove`, `rename_to: <field>`) and the rule proposing it. No candidate is
//! [`SkipReason::NoDeclaredFix`], with the field's value.
//!
//! **A removal is a `remove_frontmatter`; a rename is a `set_frontmatter` of
//! the new field to the old field's whole value, then a `remove_frontmatter`
//! of the old field requiring that set**, both cited for the finding. A rename
//! onto a field the composed document already holds is
//! [`SkipReason::RenameOntoOccupiedField`]; the pair is judged as one change,
//! so a rename that brings the new field under a closed set it breaches, or
//! under a rule requiring fields, skips through the judge as any fix does.

use std::collections::BTreeSet;

use norn_config::schema::ForbiddenFix;
use norn_wire::{FindingRow, SkipReason, SkippedFinding, ValueHead};

use super::{Document, Fix, State, cited, field_of, spelled_candidates};

/// What one rule declares for a forbidden field.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Remedy {
    Remove,
    RenameTo(String),
}

impl Remedy {
    /// The remedy as the schema spells it, which a skip names it by.
    fn spelled(&self) -> String {
        match self {
            Remedy::Remove => "remove".to_string(),
            Remedy::RenameTo(field) => format!("rename_to: {field}"),
        }
    }
}

/// The fix of the forbidden `field`, the finding `row`'s, that `rules`, the
/// rules forbidding it on the composed document, declare, made from `state`;
/// or the skip it is left alone with.
pub(super) fn fix(
    document: &Document<'_>,
    state: &State,
    row: &FindingRow,
    field: &str,
    rules: &BTreeSet<String>,
) -> Result<Fix, Box<SkippedFinding>> {
    let schema = document.schema();
    let proposals: Vec<(Remedy, &str)> = rules
        .iter()
        .filter_map(|name| schema.rule(name).map(|rule| (name.as_str(), rule)))
        .filter_map(|(name, rule)| {
            let (_, declared) = rule
                .forbidden()
                .find(|(forbidden, _)| *forbidden == field)?;
            match declared {
                ForbiddenFix::Unfixed => None,
                ForbiddenFix::Remove => Some((Remedy::Remove, name)),
                ForbiddenFix::RenameTo(to) => Some((Remedy::RenameTo(to.clone()), name)),
            }
        })
        .collect();
    let candidates = spelled_candidates(
        proposals
            .iter()
            .map(|(remedy, rule)| (candidate_head(remedy), *rule)),
    );
    let Some(((remedy, _), rest)) = proposals.split_first() else {
        return Err(Box::new(crate::planner::repair::skipped(row)));
    };
    if rest.iter().any(|(other, _)| other != remedy) {
        return Err(Box::new(
            SkippedFinding::new(row.id, SkipReason::Tie).with_candidates(candidates),
        ));
    }
    let cannot_read = || {
        Box::new(
            SkippedFinding::new(row.id, SkipReason::JudgeWouldRefuse)
                .with_candidates(candidates.clone())
                .with_note(format!("the document's `{field}` cannot be read")),
        )
    };
    let edits = match remedy {
        Remedy::Remove => vec![document.remove(field)],
        Remedy::RenameTo(to) => {
            let Some(Some(value)) = field_of(&state.bytes, field) else {
                return Err(cannot_read());
            };
            let Some(held) = field_of(&state.bytes, to) else {
                return Err(cannot_read());
            };
            if held.is_some() {
                return Err(Box::new(
                    SkippedFinding::new(row.id, SkipReason::RenameOntoOccupiedField)
                        .with_note(format!("the document already holds `{to}`")),
                ));
            }
            vec![document.set(to, value), document.remove(field)]
        }
    };
    let composed = document.admit(&state.bytes, &edits, row, &candidates)?;
    Ok(Fix {
        operations: edits,
        cited: vec![cited(row, field, false)],
        state: composed,
    })
}

/// A remedy as a candidate a skip carries.
fn candidate_head(remedy: &Remedy) -> ValueHead {
    norn_store::value_head(&remedy.spelled())
}
