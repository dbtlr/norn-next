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
//! What each element is replaced by is a kind's own ([`closed_set`]
//! and [`derived`]): they answer a [`Proposed`], and this
//! module composes, judges and writes it as one. A nested list element is
//! matched by the finding's structured spelling, so an element exactly
//! `[["s"]]`, read from `[[s]]`, can become the link string `"[[s]]"`;
//! a one-string list `["s"]` has no unquoted-link fix. A refused shape fix uses the same
//! proposals for the elements its reshape exposes, with no intermediate
//! addition admitted: the combined value is judged once from the state the
//! shape fix composes onto, at the weakest confidence and with every note.
//! An exposed element's skipped proposal keeps its reason and decision data,
//! keyed to the shape finding's id and value. A link type mismatch defers
//! that decision to the link phase; every other skip is the first in holds order.

use norn_config::schema::VaultSchema;
use norn_wire::{
    AuthoredValue, Confidence, FindingKind, FindingRow, SkipReason, SkippedCandidates,
    SkippedFinding,
};

use super::{Document, Fix, Fixing, State, cited, closed_set, derived, field_of, skip};
use crate::applier::Held;
use crate::derivation::stored_spelling;
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
/// finding as the composed state holds it, the offending value whole and
/// its first authored element, and whether every element the finding names
/// is a string ([`strings_only`]).
pub(super) struct Offending<'o> {
    pub(super) row: &'o FindingRow,
    pub(super) held: &'o Held,
    pub(super) text: &'o str,
    pub(super) value: &'o AuthoredValue,
    pub(super) string: bool,
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
/// answers it, an element whose admitted state `vetoed` names a reason the
/// plan would be refused for skipped as one the judge would refuse, noting
/// it.
pub(super) fn fix_field(
    document: &Document<'_>,
    state: &State,
    field: &str,
    elements: &[Element<'_>],
    propose: &mut dyn FnMut(&Offending<'_>) -> Proposed,
    vetoed: &dyn Fn(&State) -> Option<String>,
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
        let Some(value) = named_element(&working, schema, field, &offending) else {
            skipped.push((at, crate::planner::repair::skipped(row)));
            continue;
        };
        let replacement = match propose(&Offending {
            row,
            held,
            text: &offending,
            value,
            string: strings_only(&working, schema, field, &offending),
        }) {
            Proposed::Skipped(skip) => {
                skipped.push((at, *skip));
                continue;
            }
            Proposed::Replace(replacement) => replacement,
        };
        let Some(replaced) = replaced(&working, schema, field, value, &replacement.member) else {
            skipped.push((
                at,
                skip(row, SkipReason::JudgeWouldRefuse)
                    .with_candidates(replacement.candidates)
                    .with_note(format!("no element of `{field}` is `{offending}`")),
            ));
            continue;
        };
        let set = document.set(&state.at, field, replaced.clone());
        if replacement.level == Confidence::Suggested {
            let mut work = document.repairing.work.get();
            work.compositions += 1;
            document.repairing.work.set(work);
        }
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
            .and_then(|next| match vetoed(&next) {
                Some(note) => Err(Box::new(
                    skip(row, SkipReason::JudgeWouldRefuse)
                        .with_candidates(replacement.candidates.clone())
                        .with_note(note),
                )),
                None => Ok(next),
            })
            .and_then(|next| {
                document
                    .within_threshold(replacement.level, row, &set)
                    .map(|()| next)
            });
        match admitted {
            Ok(next) => {
                working = replaced;
                current = next;
                cites.push(cited(row, replacement.level, replacement.notes));
            }
            Err(mut skip) => {
                if skip.reason == SkipReason::BelowThreshold && !replacement.notes.is_empty() {
                    skip.note = Some(replacement.notes.join("; "));
                }
                skipped.push((at, *skip));
            }
        }
    }
    if cites.is_empty() {
        return Mapped { fix: None, skipped };
    }
    let set = document.set(&state.at, field, working);
    // The elements were composed one by one, and the plan carries one set
    // of the field: the two write the same bytes, since a flat list set over
    // a flat list is rewritten where it stands in its author's spelling and
    // any other set replaces the field's whole entry, so the state the
    // elements composed to is the one set's.
    debug_assert!(
        cites.len() == 1 || edited(&set, &state.bytes).ok().as_deref() == Some(&*current.bytes),
        "one set of `{field}` writes what its elements composed to"
    );
    Mapped {
        fix: Some(Fix {
            removed_field: None,
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

/// Whether every element of `value`, a field's value, that is `offending` —
/// a list's elements, or a scalar as the one element it is — is a string, and
/// there is one. A finding names a value by its text, so a YAML number,
/// boolean or null spelled as a string element is spelled is the same
/// finding: a derived fix rewriting text answers only a finding every
/// element of which is a string, and never coerces the others.
fn strings_only(value: &AuthoredValue, schema: &VaultSchema, field: &str, offending: &str) -> bool {
    let is_it = |element: &AuthoredValue| {
        element
            .scalar_text()
            .is_some_and(|text| same_value(schema, field, &text, offending))
    };
    let elements = match value {
        AuthoredValue::List(elements) => elements.as_slice(),
        scalar => std::slice::from_ref(scalar),
    };
    let mut named = elements.iter().filter(|element| is_it(element)).peekable();
    named.peek().is_some() && named.all(|element| matches!(element, AuthoredValue::String(_)))
}

/// `value`, a field's value, with each element that is `offending` replaced by
/// `member`: a list's elements in their order, or a scalar as the one element
/// it is; `None` where no element is.
fn replaced(
    value: &AuthoredValue,
    schema: &VaultSchema,
    field: &str,
    offending: &AuthoredValue,
    member: &AuthoredValue,
) -> Option<AuthoredValue> {
    let is_offending = |element: &AuthoredValue| same_member(schema, field, element, offending);
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

/// Whether a field element has the spelling the finding names, with typed
/// equality for scalars and the store's structured spelling for lists and maps.
fn matches_element(
    element: &AuthoredValue,
    schema: &VaultSchema,
    field: &str,
    offending: &str,
) -> bool {
    match element.scalar_text() {
        Some(text) => same_value(schema, field, &text, offending),
        None => stored_spelling(element).as_deref() == Some(offending),
    }
}

/// The first element of `value` the finding's spelling names.
fn named_element<'v>(
    value: &'v AuthoredValue,
    schema: &VaultSchema,
    field: &str,
    offending: &str,
) -> Option<&'v AuthoredValue> {
    let elements = match value {
        AuthoredValue::List(elements) => elements.as_slice(),
        scalar => std::slice::from_ref(scalar),
    };
    elements
        .iter()
        .find(|element| matches_element(element, schema, field, offending))
}

/// Whether a finding of `kind` on a reshaped field is one of the element
/// findings a reshape exposes.
pub(super) fn is_exposed(kind: FindingKind) -> bool {
    matches!(
        kind,
        FindingKind::TypeMismatch | FindingKind::NotOneOf | FindingKind::TooLong
    )
}

/// What the element fixes exposed by a reshape can answer together.
pub(super) enum Reshaped {
    Unanswered,
    Skipped(Box<SkippedFinding>),
    NeedsLinks,
    Replaced(Replacement),
}

/// Propose the element fixes a reshape exposes, without admitting an
/// intermediate addition. `state` is the plain reshape's already judged
/// state; the final combined value is judged from the state it reshaped.
pub(super) fn after_reshape(
    document: &Document<'_>,
    state: &State,
    row: &FindingRow,
    field: &str,
    value: &AuthoredValue,
    mut links: Option<&mut dyn FnMut(&Offending<'_>) -> Proposed>,
) -> Reshaped {
    let mut working = value.clone();
    let mut level = Confidence::Derived;
    let mut notes = Vec::new();
    let mut folds = derived::FoldCache::default();
    let mut any = false;
    for held in state
        .holds
        .iter()
        .filter(|held| held.field.as_deref() == Some(field) && is_exposed(held.kind))
    {
        let Some(text) = held.value.as_deref() else {
            return Reshaped::Unanswered;
        };
        let Some(element) = named_element(&working, document.schema(), field, text) else {
            // The judge orders an element's closed-set finding before its
            // length finding, so a member answering the first has replaced
            // the element the second names; the composed value is judged
            // afresh, so that finding needs no answer of its own.
            if named_element(value, document.schema(), field, text).is_some() {
                continue;
            }
            return Reshaped::Unanswered;
        };
        let mut element_row = row.clone();
        element_row.kind = held.kind;
        let offending = Offending {
            row: &element_row,
            held,
            text,
            value: element,
            string: strings_only(&working, document.schema(), field, text),
        };
        let proposal = match held.kind {
            FindingKind::NotOneOf => closed_set::propose(document, field, &mut folds, &offending),
            FindingKind::TypeMismatch if derived::is_boolean(document.schema(), field) => {
                derived::boolean_case(document, &offending)
            }
            FindingKind::TypeMismatch
                if document.schema().declared_type(field)
                    == norn_config::schema::FieldType::Link =>
            {
                match links.as_mut() {
                    Some(propose) => propose(&offending),
                    None => return Reshaped::NeedsLinks,
                }
            }
            _ => {
                return Reshaped::Skipped(Box::new(
                    derived::excluded(&element_row, document.schema())
                        .unwrap_or_else(|| crate::planner::repair::skipped(&element_row)),
                ));
            }
        };
        let replacement = match proposal {
            Proposed::Replace(replacement) => replacement,
            Proposed::Skipped(skip) => return Reshaped::Skipped(skip),
        };
        let Some(replaced) = replaced(
            &working,
            document.schema(),
            field,
            element,
            &replacement.member,
        ) else {
            return Reshaped::Unanswered;
        };
        working = replaced;
        // Confidence orders strongest first, so the maximum is the weaker level.
        level = level.max(replacement.level);
        notes.extend(replacement.notes);
        any = true;
    }
    if any {
        Reshaped::Replaced(Replacement {
            candidates: derived::candidate_of(&working),
            member: working,
            level,
            notes,
        })
    } else {
        Reshaped::Unanswered
    }
}
