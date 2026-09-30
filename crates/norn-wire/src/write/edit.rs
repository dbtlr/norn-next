//! `edit`: change the text of one document.
//!
//! **An edit's anchor is exact.** A `str_replace` replaces text that occurs in
//! the document exactly once; a section edit names a heading by its exact
//! text without its `#` marks, which must head exactly one section at any
//! level. An anchor that is gone or ambiguous leaves its operation unresolved
//! rather than guessing.
//!
//! **Edits compose in the order written.** Each edit compiles to one
//! operation on the document, in the request's order, and each reads the
//! document as the edits before it left it.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::address::VaultAddress;
use crate::apply::ApplyMode;
use crate::document::DocumentPath;
use crate::plan::document::AuthoredPlan;
use crate::plan::operation::{AuthorCondition, Operation, OperationKind};
use crate::write::{at_least_one, is_false};

/// One change to a document's text.
///
/// On the wire an edit is an object tagged `edit`:
/// `{"edit":"str_replace","old_str":"draft","new_str":"final"}`,
/// `{"edit":"delete_section","heading":"Notes"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "edit", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum DocumentEdit {
    /// Replace one occurrence of a text. The text must occur exactly once.
    #[non_exhaustive]
    StrReplace {
        /// The text replaced, which must occur in the document exactly once.
        old_str: String,
        /// The text it is replaced with.
        new_str: String,
    },
    /// Replace the body of a section, keeping its heading line.
    #[non_exhaustive]
    ReplaceSection {
        /// The heading's exact text, without its `#` marks.
        heading: String,
        /// The section's new body.
        content: String,
    },
    /// Append text to the end of a section's body.
    #[non_exhaustive]
    AppendToSection {
        /// The heading's exact text, without its `#` marks.
        heading: String,
        /// The text appended.
        content: String,
    },
    /// Remove a section: its heading line and its body.
    #[non_exhaustive]
    DeleteSection {
        /// The heading's exact text, without its `#` marks.
        heading: String,
    },
    /// Insert text before a heading line.
    #[non_exhaustive]
    InsertBeforeHeading {
        /// The heading's exact text, without its `#` marks.
        heading: String,
        /// The text inserted.
        content: String,
    },
    /// Insert text after a heading line.
    #[non_exhaustive]
    InsertAfterHeading {
        /// The heading's exact text, without its `#` marks.
        heading: String,
        /// The text inserted.
        content: String,
    },
    /// Replace everything after the frontmatter block, leaving the block
    /// exactly as it is.
    #[non_exhaustive]
    ReplaceBody {
        /// The body's full text.
        content: String,
    },
}

impl DocumentEdit {
    /// Replace the one occurrence of `old_str` with `new_str`.
    pub fn str_replace(old_str: impl Into<String>, new_str: impl Into<String>) -> Self {
        DocumentEdit::StrReplace {
            old_str: old_str.into(),
            new_str: new_str.into(),
        }
    }

    /// Replace the body of the section `heading` heads with `content`.
    pub fn replace_section(heading: impl Into<String>, content: impl Into<String>) -> Self {
        DocumentEdit::ReplaceSection {
            heading: heading.into(),
            content: content.into(),
        }
    }

    /// Append `content` to the section `heading` heads.
    pub fn append_to_section(heading: impl Into<String>, content: impl Into<String>) -> Self {
        DocumentEdit::AppendToSection {
            heading: heading.into(),
            content: content.into(),
        }
    }

    /// Remove the section `heading` heads.
    pub fn delete_section(heading: impl Into<String>) -> Self {
        DocumentEdit::DeleteSection {
            heading: heading.into(),
        }
    }

    /// Insert `content` before the heading line `heading`.
    pub fn insert_before_heading(heading: impl Into<String>, content: impl Into<String>) -> Self {
        DocumentEdit::InsertBeforeHeading {
            heading: heading.into(),
            content: content.into(),
        }
    }

    /// Insert `content` after the heading line `heading`.
    pub fn insert_after_heading(heading: impl Into<String>, content: impl Into<String>) -> Self {
        DocumentEdit::InsertAfterHeading {
            heading: heading.into(),
            content: content.into(),
        }
    }

    /// Replace the body with `content`.
    pub fn replace_body(content: impl Into<String>) -> Self {
        DocumentEdit::ReplaceBody {
            content: content.into(),
        }
    }

    /// The operation kind this edit is on the document at `path`.
    fn into_kind(self, path: DocumentPath) -> OperationKind {
        match self {
            DocumentEdit::StrReplace { old_str, new_str } => OperationKind::StrReplace {
                path,
                old_str,
                new_str,
            },
            DocumentEdit::ReplaceSection { heading, content } => OperationKind::ReplaceSection {
                path,
                heading,
                content,
            },
            DocumentEdit::AppendToSection { heading, content } => OperationKind::AppendToSection {
                path,
                heading,
                content,
            },
            DocumentEdit::DeleteSection { heading } => {
                OperationKind::DeleteSection { path, heading }
            }
            DocumentEdit::InsertBeforeHeading { heading, content } => {
                OperationKind::InsertBeforeHeading {
                    path,
                    heading,
                    content,
                }
            }
            DocumentEdit::InsertAfterHeading { heading, content } => {
                OperationKind::InsertAfterHeading {
                    path,
                    heading,
                    content,
                }
            }
            DocumentEdit::ReplaceBody { content } => OperationKind::ReplaceBody { path, content },
        }
    }
}

/// An edit list read with its floor of one edit.
fn at_least_one_edit<'de, D>(deserializer: D) -> Result<Vec<DocumentEdit>, D::Error>
where
    D: Deserializer<'de>,
{
    at_least_one(deserializer, "edit")
}

/// What an `edit` request carries.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct EditParams {
    /// The vault written.
    pub vault: VaultAddress,
    /// Whether to preview the write or apply it. There is no default.
    pub mode: ApplyMode,
    /// The document edited.
    pub path: DocumentPath,
    /// The edits, at least one, in the order they compose.
    #[serde(deserialize_with = "at_least_one_edit")]
    #[schemars(length(min = 1))]
    pub edits: Vec<DocumentEdit>,
    /// What the author observed and requires to hold.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<AuthorCondition>,
    /// Whether the write applies past the schema check. Absent is `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub force: bool,
}

impl EditParams {
    /// A request to `mode` the `edits` to the document at `path` in `vault`,
    /// with no condition and not forced.
    pub const fn new(
        vault: VaultAddress,
        mode: ApplyMode,
        path: DocumentPath,
        edits: Vec<DocumentEdit>,
    ) -> Self {
        EditParams {
            vault,
            mode,
            path,
            edits,
            conditions: Vec::new(),
            force: false,
        }
    }

    /// The request requiring `conditions` to hold.
    #[must_use]
    pub fn with_conditions(mut self, conditions: Vec<AuthorCondition>) -> Self {
        self.conditions = conditions;
        self
    }

    /// The request applying past the schema check where `force` holds.
    #[must_use]
    pub const fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }

    /// The plan the request compiles to: one operation per edit on the
    /// document, in the request's order, each carrying the request's
    /// conditions, forced as the request is.
    pub fn plan(self) -> AuthoredPlan {
        let EditParams {
            vault,
            mode: _,
            path,
            edits,
            conditions,
            force,
        } = self;
        let operations = edits
            .into_iter()
            .map(|edit| {
                Operation::new(edit.into_kind(path.clone())).with_conditions(conditions.clone())
            })
            .collect();
        AuthoredPlan::new(vault, operations).with_force(force)
    }
}
