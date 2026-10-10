//! Synonyms: the member a rule's `one_of` maps an offending value to.
//!
//! The candidates for a selected `field/not-one-of` finding are the members
//! the contributing rules' synonyms for the offending value map it to. A rule
//! that declares a `one_of` on the field but no synonym for the value
//! proposes nothing. The value is compared with a synonym's written value by
//! the field's typed equality, as the judge compares an element
//! ([`VaultSchema::equality_key`]): `4` and `4.0` are one number under a
//! `number` field, and `Play` and `#play` one tag under a tag fold. The
//! candidates agree when they name one member by the same equality. No
//! candidate is [`SkipReason::NoDeclaredFix`], with the offending value;
//! candidates that disagree are [`SkipReason::Tie`], with every member and the
//! rule proposing it; and a member the judge refuses, such as one outside the
//! combined closed set the co-selecting rules narrow the field to, is
//! [`SkipReason::JudgeWouldRefuse`], with its candidates.
//!
//! **A field's offending values are fixed together.** A scalar field's one
//! finding is a `set_frontmatter` of the member. A list field's findings — one
//! per distinct offending element, by the field's typed equality — compose
//! into one `set_frontmatter` of the whole list in its written order, each
//! element's fix replacing every occurrence of its offending value and judged
//! on the state the earlier elements' fixes left. An element whose fix is
//! skipped stands where it is.

use norn_config::schema::VaultSchema;
use norn_wire::{AuthoredValue, FindingRow, SkipReason, SkippedFinding};

use super::{Document, Fix, Fixing, Proposal, State, candidates, cited, field_of, skip};
use crate::applier::Held;
use crate::planner::edit::edited;

/// What the synonyms of one field's selected findings come to: the one fix
/// that replaces the elements whose synonym is admitted, and the skip of each
/// element whose is not, by the position of its row.
pub(super) struct Mapped {
    pub(super) fix: Option<Fix>,
    pub(super) skipped: Vec<(usize, SkippedFinding)>,
}

/// One selected `field/not-one-of` finding of a field: its position among
/// the document's rows, the row, and the finding it names as the
/// before-state holds it.
pub(super) struct Element<'e> {
    pub(super) at: usize,
    pub(super) row: &'e FindingRow,
    pub(super) finding: &'e Held,
}

/// Fix the `field` of the document from `state`, for `elements` — the
/// selected `field/not-one-of` findings of that field, in finding order.
pub(super) fn fix_field(
    document: &Document<'_>,
    state: &State,
    field: &str,
    elements: &[Element<'_>],
) -> Mapped {
    let schema = document.schema();
    let mut skipped = Vec::new();
    let Some(Some(mut working)) = field_of(&state.bytes, field) else {
        // The document no longer holds the field, or its fields cannot be
        // read: no element of it is there to replace.
        for element in elements {
            if state.holding(element.finding).is_some() {
                skipped.push((
                    element.at,
                    skip(element.row, SkipReason::JudgeWouldRefuse).with_note(format!(
                        "the document's `{field}` cannot be read to replace an element of"
                    )),
                ));
            }
        }
        return Mapped { fix: None, skipped };
    };
    let mut current = state.clone();
    let mut cites = Vec::new();
    for &Element { at, row, finding } in elements {
        let Some(held) = current.holding(finding) else {
            continue;
        };
        let Some(offending) = held.value.clone() else {
            skipped.push((at, crate::planner::repair::skipped(row)));
            continue;
        };
        let proposals = proposals(
            schema,
            field,
            &held.rules.iter().cloned().collect::<Vec<_>>(),
            &offending,
        );
        let Some((first, rest)) = proposals.split_first() else {
            skipped.push((at, crate::planner::repair::skipped(row)));
            continue;
        };
        if rest
            .iter()
            .any(|proposal| !same_member(schema, field, &proposal.value, &first.value))
        {
            skipped.push((
                at,
                skip(row, SkipReason::Tie).with_candidates(candidates(&proposals)),
            ));
            continue;
        }
        let Some(replaced) = replaced(&working, schema, field, &offending, &first.value) else {
            skipped.push((
                at,
                skip(row, SkipReason::JudgeWouldRefuse)
                    .with_candidates(candidates(&proposals))
                    .with_note(format!("no element of `{field}` is `{offending}`")),
            ));
            continue;
        };
        let set = document.set(&state.at, field, replaced.clone());
        match document.admit(
            &current,
            std::slice::from_ref(&set),
            Fixing {
                row,
                finding,
                candidates: &candidates(&proposals),
            },
        ) {
            Ok(next) => {
                working = replaced;
                current = next;
                cites.push(cited(row, field, false));
            }
            Err(skip) => skipped.push((at, *skip)),
        }
    }
    if cites.is_empty() {
        return Mapped { fix: None, skipped };
    }
    let set = document.set(&state.at, field, working);
    if cites.len() > 1 {
        // Each element was composed onto the one before it; the operation
        // the plan carries is one set of the field, so the state is what that
        // one set composes to.
        if let Ok(bytes) = edited(&set, &state.bytes) {
            current.bytes = bytes;
        }
    }
    Mapped {
        fix: Some(Fix {
            operations: vec![set],
            cited: cites,
            state: current,
        }),
        skipped,
    }
}

/// The member each of the rules `contributing` to the field maps `offending`
/// to, in rule name order.
fn proposals(
    schema: &VaultSchema,
    field: &str,
    contributing: &[String],
    offending: &str,
) -> Vec<Proposal> {
    let mut proposals = Vec::new();
    for name in contributing {
        let Some(rule) = schema.rule(name) else {
            continue;
        };
        for (_, set) in rule.one_of().filter(|(declared, _)| *declared == field) {
            for (written, member) in set.synonym_values() {
                if same_value(schema, field, written, offending) {
                    proposals.push(Proposal {
                        value: member.clone(),
                        rule: name.clone(),
                        clocked: false,
                    });
                }
            }
        }
    }
    proposals
}

/// Whether the texts `left` and `right` are one value of `field`: one
/// equality key where the field's type reads them, otherwise one spelling.
fn same_value(schema: &VaultSchema, field: &str, left: &str, right: &str) -> bool {
    match (
        schema.equality_key(field, left),
        schema.equality_key(field, right),
    ) {
        (Some(left), Some(right)) => left == right,
        (None, None) => left == right,
        _ => false,
    }
}

/// Whether the members `left` and `right` are one value of `field`.
fn same_member(
    schema: &VaultSchema,
    field: &str,
    left: &AuthoredValue,
    right: &AuthoredValue,
) -> bool {
    match (left.scalar_text(), right.scalar_text()) {
        (Some(left), Some(right)) => same_value(schema, field, &left, &right),
        _ => left == right,
    }
}

/// `value`, a field's value, with each element that is `offending` replaced by
/// `member`: a list's elements in their order, or a scalar as the one element
/// it is; `None` where no element is.
fn replaced(
    value: &AuthoredValue,
    schema: &VaultSchema,
    field: &str,
    offending: &str,
    member: &AuthoredValue,
) -> Option<AuthoredValue> {
    let is_offending = |element: &AuthoredValue| {
        element
            .scalar_text()
            .is_some_and(|text| same_value(schema, field, &text, offending))
    };
    match value {
        AuthoredValue::List(elements) => {
            let mut any = false;
            let elements = elements
                .iter()
                .map(|element| {
                    if is_offending(element) {
                        any = true;
                        member.clone()
                    } else {
                        element.clone()
                    }
                })
                .collect();
            any.then_some(AuthoredValue::List(elements))
        }
        scalar => is_offending(scalar).then(|| member.clone()),
    }
}
