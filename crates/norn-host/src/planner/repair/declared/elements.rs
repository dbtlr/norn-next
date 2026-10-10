//! A field's offending elements, fixed one by one into one write of the
//! field.
//!
//! **One way to write a list's fixed elements.** The selected findings of one
//! kind on one field — one per distinct offending element, by the field's
//! typed equality — compose into one `set_frontmatter` of the whole field in
//! its written order, each element's replacement judged on the state the
//! earlier ones left. A scalar field is the one element it is. An element
//! whose fix is skipped stands where it is, and never blocks its neighbours'.
//! The one operation cites every finding it fixes, each at the level of its
//! own fix.
//!
//! What each element is replaced by is a kind's own ([`closed_set`](super::closed_set)
//! and [`derived`](super::derived)): they answer a [`Proposed`], and this
//! module composes, judges and writes it as one.

use norn_config::schema::VaultSchema;
use norn_wire::{
    AuthoredValue, Confidence, FindingRow, SkipReason, SkippedCandidates, SkippedFinding,
};

use super::{Document, Fix, Fixing, State, cited, field_of, skip};
use crate::applier::Held;
use crate::planner::edit::edited;

/// What the elements of one field come to: the one fix that replaces the
/// elements whose replacement is admitted, and the skip of each element whose
/// is not, by the position of its row.
pub(super) struct Mapped {
    pub(super) fix: Option<Fix>,
    pub(super) skipped: Vec<(usize, SkippedFinding)>,
}

/// One selected finding of a field: its position among the document's rows,
/// the row, and the finding it names as the before-state holds it.
pub(super) struct Element<'e> {
    pub(super) at: usize,
    pub(super) row: &'e FindingRow,
    pub(super) finding: &'e Held,
}

/// One offending element a kind's proposals are asked for: its row, the
/// finding as the composed state holds it, and the offending value whole.
pub(super) struct Offending<'o> {
    pub(super) row: &'o FindingRow,
    pub(super) held: &'o Held,
    pub(super) text: &'o str,
}

/// What a kind makes of one offending element.
pub(super) enum Proposed {
    /// The element is left alone, for the reason and with the data the skip
    /// names.
    Skipped(Box<SkippedFinding>),
    /// Every occurrence of the offending element is replaced.
    Replace(Replacement),
}

/// The replacement of an offending element.
pub(super) struct Replacement {
    /// The value written in the element's place.
    pub(super) member: AuthoredValue,
    /// How sure the repair is of it.
    pub(super) level: Confidence,
    /// The candidates a skip of it carries, where the judge refuses it.
    pub(super) candidates: SkippedCandidates,
    /// Words about the fix, carried by the citation.
    pub(super) notes: Vec<String>,
}

/// Fix the `field` of the document from `state`, for `elements` — the
/// selected findings of that field, in finding order — each as `propose`
/// answers it.
pub(super) fn fix_field(
    document: &Document<'_>,
    state: &State,
    field: &str,
    elements: &[Element<'_>],
    propose: &mut dyn FnMut(&Offending<'_>) -> Proposed,
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
        let replacement = match propose(&Offending {
            row,
            held,
            text: &offending,
        }) {
            Proposed::Skipped(skip) => {
                skipped.push((at, *skip));
                continue;
            }
            Proposed::Replace(replacement) => replacement,
        };
        let Some(replaced) = replaced(&working, schema, field, &offending, &replacement.member)
        else {
            skipped.push((
                at,
                skip(row, SkipReason::JudgeWouldRefuse)
                    .with_candidates(replacement.candidates)
                    .with_note(format!("no element of `{field}` is `{offending}`")),
            ));
            continue;
        };
        let set = document.set(&state.at, field, replaced.clone());
        let admitted = document
            .admit(
                &current,
                std::slice::from_ref(&set),
                Fixing {
                    row,
                    finding,
                    candidates: &replacement.candidates,
                },
            )
            .and_then(|next| {
                document
                    .written_at(replacement.level, row, &set)
                    .map(|()| next)
            });
        match admitted {
            Ok(next) => {
                working = replaced;
                current = next;
                cites.push(cited(row, replacement.level, replacement.notes));
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

/// Whether the texts `left` and `right` are one value of `field`: one
/// equality key where the field's type reads them, otherwise one spelling.
pub(super) fn same_value(schema: &VaultSchema, field: &str, left: &str, right: &str) -> bool {
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
pub(super) fn same_member(
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
