//! Derived fixes: the answers a built-in rule proves unique from the
//! document and the declaration alone, with no rule of the schema writing
//! them.
//!
//! **A derived fix is made at [`Confidence::Derived`], below any declared
//! one.** It is composed and judged as a declared fix is (`Document::admit`),
//! so a fix the applier would refuse, or that brings required fields in, is
//! skipped with the reason the judge gives. A fix whose level the request's
//! threshold does not admit is judged too, then skipped as
//! [`SkipReason::BelowThreshold`], carrying the operation the plan would have
//! held: lowering the threshold writes exactly that.
//!
//! - **Wrap and unwrap** answer a `field/shape-mismatch`. A field declared a
//!   list holding a scalar is set to the one-item list of that scalar, its
//!   YAML type kept; a field declared single holding a one-item list is set
//!   to that item. A list of any other length under single has no derived
//!   fix.

use norn_config::schema::Shape;
use norn_wire::{
    AuthoredValue, Confidence, FindingRow, SkipReason, SkippedCandidates, SkippedFinding,
    ValueCandidate, ValueCandidateHead,
};

use super::{Document, Fix, Fixing, State, cited, field_of, head, skip};
use crate::applier::Held;

/// The value a field declared `shape` holds once re-shaped, or `None` where
/// no re-shape is proved unique: a scalar under a list is the one item of a
/// list, and a list of one item under single is that item.
fn reshaped(shape: Shape, value: &AuthoredValue) -> Option<AuthoredValue> {
    match (shape, value) {
        (Shape::List, scalar) if scalar.scalar_text().is_some() => {
            Some(AuthoredValue::List(vec![scalar.clone()]))
        }
        (Shape::Single, AuthoredValue::List(items)) => match items.as_slice() {
            [only] => Some(only.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// The candidates a derived fix skipped by the judge carries: the one value
/// it would have written, proposed by no named rule.
pub(super) fn candidate_of(value: &AuthoredValue) -> SkippedCandidates {
    SkippedCandidates::values(
        ValueCandidateHead::new([ValueCandidate::new(head(value))], 1)
            .expect("a head of the one candidate it names"),
    )
}

/// The fix of the `field/shape-mismatch` finding `row` — `finding`, as the
/// before-state holds it — on `field`, made from `state`; or the skip it is
/// left alone with.
pub(super) fn reshape(
    document: &Document<'_>,
    state: &State,
    row: &FindingRow,
    finding: &Held,
    field: &str,
) -> Result<Fix, Box<SkippedFinding>> {
    let Some(shape) = document.schema().field(field).and_then(|f| f.shape()) else {
        return Err(Box::new(crate::planner::repair::skipped(row)));
    };
    let Some(Some(value)) = field_of(&state.bytes, field) else {
        return Err(Box::new(skip(row, SkipReason::JudgeWouldRefuse).with_note(
            format!("the document's `{field}` cannot be read to re-shape"),
        )));
    };
    let Some(reshaped) = reshaped(shape, &value) else {
        return Err(Box::new(crate::planner::repair::skipped(row)));
    };
    let set = document.set(&state.at, field, reshaped.clone());
    let composed = document.admit(
        state,
        std::slice::from_ref(&set),
        Fixing {
            row,
            finding,
            candidates: &candidate_of(&reshaped),
        },
    )?;
    document.written_at(Confidence::Derived, row, &set)?;
    Ok(Fix {
        operations: vec![set],
        cited: vec![cited(row, Confidence::Derived, Vec::new())],
        state: composed,
    })
}
