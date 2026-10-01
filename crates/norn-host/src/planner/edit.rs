//! The document-local kinds: each an edit of one document's bytes where it
//! stands, a pure function of those bytes through `norn-text`.
//!
//! **Where the edit itself lives.** `norn-text` owns every splice — a field
//! set, removed, pushed to or popped from, a body or a section replaced, text
//! appended or inserted at a heading — and proves each by reading the result
//! back, refusing rather than returning bytes that do not read as intended.
//! This module only names which splice a kind is, converts the author's
//! [`AuthoredValue`] into the value model one to one ([`text_value`]), and
//! states the planner's own refusals: an append or insert of empty content,
//! which a splice would otherwise perform as a silent no-op, and the values
//! `norn-text` does not write yet. Every refusal is an operation that does
//! not resolve, in words, never a fault in the plan's shape. An edit whose
//! result is the bytes it was given — a field set to the value it holds, a
//! body or a section replaced by itself — is no refusal: it composes to its
//! document unchanged, which lands found.
//!
//! **What the planner refuses before any splice.** A frontmatter kind whose
//! target is still a `where` list, which planning expands into path targets
//! before anything composes ([`super::expand`]); an append or insert with empty content, which would
//! land as a change of nothing; and a value that is nested — a map, or a list
//! holding a list or a map — which the value model holds and `norn-text`'s
//! writer does not write yet (NORN-317). An element pushed to or popped from a
//! list is itself a list's element, so a list or a map there is nested too.
//!
//! **An expected value is judged here too**, on a document's bytes
//! ([`expectation_unmet`]), so the field an author observed and the field an
//! operation writes are read by one rule.

use std::sync::Arc;

use norn_text::{Document, EditError, Mapping, SectionError, Value};
use norn_wire::{AuthoredValue, DocumentPath, ExpectedField, OperationKind, WriteTarget};

use super::compose::Unresolved;
use crate::derivation::document_source;

/// The document path a document-local kind names, or why it names none yet;
/// `None` for a kind that is not document-local.
pub(crate) fn local_target(kind: &OperationKind) -> Option<Result<&DocumentPath, Unresolved>> {
    let target = match kind {
        OperationKind::SetFrontmatter { target, .. }
        | OperationKind::RemoveFrontmatter { target, .. }
        | OperationKind::PushFrontmatter { target, .. }
        | OperationKind::PopFrontmatter { target, .. } => target,
        OperationKind::ReplaceBody { path, .. }
        | OperationKind::ReplaceSection { path, .. }
        | OperationKind::AppendToSection { path, .. }
        | OperationKind::DeleteSection { path, .. }
        | OperationKind::InsertBeforeHeading { path, .. }
        | OperationKind::InsertAfterHeading { path, .. } => return Some(Ok(path)),
        // NORN-297: a `rewrite_link` edits one document where it stands, but
        // its composition through `norn-text`'s link rewriter is not wired
        // yet, so it is not read here as a document-local edit.
        OperationKind::CreateDocument { .. }
        | OperationKind::StrReplace { .. }
        | OperationKind::MoveDocument { .. }
        | OperationKind::DeleteDocument { .. }
        | OperationKind::MoveFolder { .. }
        | OperationKind::RewriteLink { .. }
        | OperationKind::RewriteWikilink { .. } => return None,
    };
    Some(match target {
        WriteTarget::Path(path) => Ok(path),
        WriteTarget::Where(predicates) if predicates.is_empty() => Err(
            "a `where` target names at least one predicate, since a conjunction of none matches every document"
                .to_string(),
        ),
        // Planning expands every `where` target before composition
        // (`super::expand`), so only a plan resolved without expansion — a
        // refresh re-resolving a resolved plan's operations, which the
        // applier refuses first if one carries a `where` — could meet one.
        WriteTarget::Where(_) => Err(
            "a `where` target is not yet expanded into the documents it matches".to_string(),
        ),
    })
}

/// Why `kind` cannot act on any document, whatever it holds: a value it does
/// not write yet, or content that would change nothing. `None` where it can.
pub(crate) fn refused_whatever_the_document(kind: &OperationKind) -> Option<Unresolved> {
    match kind {
        OperationKind::SetFrontmatter { value, .. } => nested(value, false),
        OperationKind::PushFrontmatter { value, .. }
        | OperationKind::PopFrontmatter { value, .. } => nested(value, true),
        OperationKind::AppendToSection { content, .. }
        | OperationKind::InsertBeforeHeading { content, .. }
        | OperationKind::InsertAfterHeading { content, .. }
            if content.is_empty() =>
        {
            Some(format!(
                "a `{}` operation with empty content changes nothing",
                kind.name()
            ))
        }
        _ => None,
    }
}

/// Why `value` is not written yet: a map, or a list holding a list or a map;
/// and, where it is a list's element (`element`), a list at all.
fn nested(value: &AuthoredValue, element: bool) -> Option<Unresolved> {
    let is_nested = match value {
        AuthoredValue::Map(_) => true,
        AuthoredValue::List(items) => {
            element
                || items
                    .iter()
                    .any(|item| matches!(item, AuthoredValue::List(_) | AuthoredValue::Map(_)))
        }
        _ => false,
    };
    is_nested.then(|| {
        "the value is nested — a map, or a list holding a list or a map — and only scalars and flat lists of scalars are written until nested frontmatter values land (NORN-317)"
            .to_string()
    })
}

/// `bytes`, the document the operation names as it stands so far, edited as
/// the document-local `kind` edits it; or why it cannot be.
pub(crate) fn edited(kind: &OperationKind, bytes: &[u8]) -> Result<Arc<[u8]>, Unresolved> {
    let Ok(text) = document_source(bytes) else {
        return Err(
            "the document is not UTF-8 text, so its frontmatter and sections cannot be edited"
                .to_string(),
        );
    };
    let document = Document::parse(text);
    let result = match kind {
        OperationKind::SetFrontmatter { field, value, .. } => {
            document.set_field(field, &text_value(value))
        }
        OperationKind::RemoveFrontmatter { field, .. } => document.remove_field(field),
        OperationKind::PushFrontmatter { field, value, .. } => {
            document.push_to_list(field, &text_value(value))
        }
        OperationKind::PopFrontmatter { field, value, .. } => {
            document.pop_from_list(field, &text_value(value))
        }
        OperationKind::ReplaceBody { content, .. } => document.replace_body(content),
        OperationKind::ReplaceSection {
            heading, content, ..
        } => document.replace_section(heading.as_str(), content),
        OperationKind::AppendToSection {
            heading, content, ..
        } => document.append_to_section(heading.as_str(), content),
        OperationKind::DeleteSection { heading, .. } => document.delete_section(heading.as_str()),
        OperationKind::InsertBeforeHeading {
            heading, content, ..
        } => document.insert_before_heading(heading.as_str(), content),
        OperationKind::InsertAfterHeading {
            heading, content, ..
        } => document.insert_after_heading(heading.as_str(), content),
        OperationKind::CreateDocument { .. }
        | OperationKind::StrReplace { .. }
        | OperationKind::MoveDocument { .. }
        | OperationKind::DeleteDocument { .. }
        | OperationKind::MoveFolder { .. }
        | OperationKind::RewriteLink { .. }
        | OperationKind::RewriteWikilink { .. } => {
            unreachable!("only a document-local kind is edited here")
        }
    };
    result
        .map(|edited| Arc::from(edited.into_bytes()))
        .map_err(|error| refusal(&error))
}

/// An edit refusal in the words an unresolved operation carries.
///
/// A section refusal is restated: the wire addresses a section by its heading
/// alone, so the resolver's advice to address an occurrence is not the
/// caller's to take.
fn refusal(error: &EditError) -> Unresolved {
    match error {
        EditError::Section(SectionError::HeadingNotFound { heading }) => {
            format!("no heading in the document reads as {heading:?}")
        }
        EditError::Section(SectionError::HeadingAmbiguous { heading, count }) => format!(
            "{count} headings in the document read as {heading:?}, so it names no one section"
        ),
        other => other.to_string(),
    }
}

/// `value` in `norn-text`'s value model: one shape for one shape, the map
/// keeping its order.
pub(crate) fn text_value(value: &AuthoredValue) -> Value {
    match value {
        AuthoredValue::Null => Value::Null,
        AuthoredValue::Bool(value) => Value::Bool(*value),
        AuthoredValue::Integer(value) => Value::Int(*value),
        AuthoredValue::Float(value) => Value::Float(value.get()),
        AuthoredValue::String(value) => Value::String(value.clone()),
        AuthoredValue::List(items) => Value::Sequence(items.iter().map(text_value).collect()),
        AuthoredValue::Map(map) => {
            let mut mapping = Mapping::new();
            for (key, value) in map.entries() {
                mapping.insert(key.clone(), text_value(value));
            }
            Value::Map(mapping)
        }
    }
}

/// Why the document at `path`, holding `bytes`, does not hold `expect` under
/// `field`, or `None` where it does.
///
/// **`bytes` are the document as it stood before the plan**, never as the
/// operations ahead of the guarded one leave it, so the reason states that
/// rule: a field the plan's own earlier operation writes is judged at what
/// it held before.
///
/// **Absent means the document does not carry the field**: a document with
/// no frontmatter block, or an empty one, carries none. **Present means the
/// field reads as exactly the value**, under `norn-text`'s equality on its
/// value model: one shape for one shape — a string is never the number or the
/// boolean it spells, an integer never the float of its value, null never
/// absence — floats compared by their total order, so `-0.0` is not `0.0`,
/// and a list holding equal elements in the same order. A nested expected
/// value is not judged until nested values land (NORN-317), and a block that
/// cannot be read, or holds no mapping, holds no field to observe.
pub(crate) fn expectation_unmet(
    path: &DocumentPath,
    bytes: &[u8],
    field: &str,
    expect: &ExpectedField,
) -> Option<Unresolved> {
    if let ExpectedField::Present { value } = expect
        && let Some(refusal) = nested(value, false)
    {
        return Some(format!(
            "the expected value of `{field}` in `{path}` cannot be judged: {refusal}"
        ));
    }
    let Ok(text) = document_source(bytes) else {
        return Some(format!(
            "`{path}` is not UTF-8 text, so its field `{field}` cannot be observed"
        ));
    };
    let document = Document::parse(text);
    if document.frontmatter_refusal().is_some() {
        return Some(format!(
            "the frontmatter block of `{path}` cannot be read, so its field `{field}` cannot be observed"
        ));
    }
    let held = match document.frontmatter() {
        None | Some(Value::Null) => None,
        Some(Value::Map(map)) => map.get(field),
        Some(other) => {
            return Some(format!(
                "the frontmatter block of `{path}` holds a {}, and only a mapping has fields",
                other.kind()
            ));
        }
    };
    let holds = match (expect, held) {
        (ExpectedField::Absent {}, held) => held.is_none(),
        (ExpectedField::Present { value }, Some(held)) => *held == text_value(value),
        (ExpectedField::Present { .. }, None) => false,
    };
    (!holds).then(|| {
        format!(
            "the field `{field}` of `{path}` does not hold what its author expected, judged against the document as it stood before the plan"
        )
    })
}
