//! The document-local kinds: each an edit of one document's bytes where it
//! stands, a pure function of those bytes through `norn-text`.
//!
//! **Where the edit itself lives.** `norn-text` owns every splice — a field
//! set, removed, pushed to or popped from, a body or a section replaced, text
//! appended or inserted at a heading — and proves each by reading the result
//! back, refusing rather than returning bytes that do not read as intended.
//! This module only names which splice a kind is, converts the author's
//! [`AuthoredValue`] into the value model one to one ([`text_value`]), and
//! states the planner's own refusals: the ones a splice would otherwise
//! perform as a silent no-op, and the values `norn-text` does not write yet.
//! Every refusal is an operation that does not resolve, in words, never a
//! fault in the plan's shape.
//!
//! **What the planner refuses before any splice.** A frontmatter kind whose
//! target is a `where` list, which planning expands into path targets before
//! anything composes; an append or insert with empty content, which would
//! land as a change of nothing; and a value that is nested — a map, or a list
//! holding a list or a map — which the value model holds and `norn-text`'s
//! writer does not write yet (NORN-317). An element pushed to or popped from a
//! list is itself a list's element, so a list or a map there is nested too.

use std::sync::Arc;

use norn_text::{Document, EditError, Mapping, SectionError, Value};
use norn_wire::{AuthoredValue, DocumentPath, OperationKind, WriteTarget};

/// Why a document-local kind cannot act, in words.
pub(crate) type Refusal = String;

/// The document path a document-local kind names, or why it names none yet;
/// `None` for a kind that is not document-local.
pub(crate) fn local_target(kind: &OperationKind) -> Option<Result<&DocumentPath, Refusal>> {
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
        OperationKind::CreateDocument { .. }
        | OperationKind::StrReplace { .. }
        | OperationKind::MoveDocument { .. }
        | OperationKind::DeleteDocument { .. } => return None,
    };
    Some(match target {
        WriteTarget::Path(path) => Ok(path),
        WriteTarget::Where(predicates) if predicates.is_empty() => Err(
            "a `where` target names at least one predicate, since a conjunction of none matches every document"
                .to_string(),
        ),
        // NORN-296: where expansion lands with the host verbs (PR 4b), which
        // expands a `where` target before composition and replaces this.
        WriteTarget::Where(_) => Err(
            "a `where` target is not yet expanded into the documents it matches".to_string(),
        ),
    })
}

/// Why `kind` cannot act on any document, whatever it holds: a value it does
/// not write yet, or content that would change nothing. `None` where it can.
pub(crate) fn refused_whatever_the_document(kind: &OperationKind) -> Option<Refusal> {
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
fn nested(value: &AuthoredValue, element: bool) -> Option<Refusal> {
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
pub(crate) fn edited(kind: &OperationKind, bytes: &[u8]) -> Result<Arc<[u8]>, Refusal> {
    let Ok(text) = std::str::from_utf8(bytes) else {
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
        | OperationKind::DeleteDocument { .. } => {
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
fn refusal(error: &EditError) -> Refusal {
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
