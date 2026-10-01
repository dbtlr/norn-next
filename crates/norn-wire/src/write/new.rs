//! `new`: create one document at an explicit path.
//!
//! **A new document is exactly the content sent.** Composing a document from
//! fields and a body template is a surface's or a creation rule's, so the
//! request carries the document's full text and the path it is created at.
//! Nothing may stand at the path; the folders above it are always made.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::address::VaultAddress;
use crate::apply::ApplyMode;
use crate::document::DocumentPath;
use crate::plan::document::AuthoredPlan;
use crate::plan::document::is_false;
use crate::plan::operation::{AuthorCondition, Operation, OperationKind};

/// What a `new` request carries.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct NewParams {
    /// The vault written.
    pub vault: VaultAddress,
    /// Whether to preview the write or apply it. There is no default.
    pub mode: ApplyMode,
    /// Where the document is created. Nothing may stand there.
    pub path: DocumentPath,
    /// The document's full text.
    pub content: String,
    /// What the author observed and requires to hold.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<AuthorCondition>,
    /// Whether the write applies past the schema check. Absent is `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub force: bool,
}

impl NewParams {
    /// A request to `mode` the creation of the document at `path` in `vault`,
    /// holding `content`, with no condition and not forced.
    pub fn new(
        vault: VaultAddress,
        mode: ApplyMode,
        path: DocumentPath,
        content: impl Into<String>,
    ) -> Self {
        NewParams {
            vault,
            mode,
            path,
            content: content.into(),
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

    /// The plan the request compiles to: one `create_document` operation
    /// carrying the request's conditions, forced as the request is.
    pub fn plan(self) -> AuthoredPlan {
        let NewParams {
            vault,
            mode: _,
            path,
            content,
            conditions,
            force,
        } = self;
        let operation = Operation::new(OperationKind::CreateDocument { path, content })
            .with_conditions(conditions);
        AuthoredPlan::new(vault, vec![operation]).with_force(force)
    }
}
