//! `rewrite_wikilink`: respell every wikilink naming one document to name
//! another.
//!
//! **`old` need not name a document that stands.** A wikilink rewrite is how
//! a broken link is repaired, so `old` is read as a resolution target and
//! never checked against the vault here. Planning reads it as the vault
//! stands before the plan: naming one document, every wikilink resolving to
//! that document is retargeted, whatever its spelling; naming none, every
//! broken wikilink filed under `old` in any case is; naming several, the
//! operation is unresolved with the head of its candidates. `new` must name
//! one document where the plan leaves the vault. Each retargeted wikilink is
//! respelled in its own form, and the rewrites ride the operation as its
//! link cascade. A Markdown link is no wikilink, and is never rewritten here.
//!
//! **Both ends name documents, never places inside them.** Each rewritten
//! link keeps the anchor it was written with, so an anchor on `old` or `new`
//! is refused at the read.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::address::VaultAddress;
use crate::apply::ApplyMode;
use crate::plan::document::AuthoredPlan;
use crate::plan::document::is_false;
use crate::plan::operation::{AuthorCondition, Operation, OperationKind};
use crate::target::{ResolutionTarget, whole_document_schema};
use crate::write::document_target;

/// What a `rewrite_wikilink` request carries.
///
/// On the wire: `{"vault":…,"mode":"preview","old":"drafts/plan","new":"plans/2026"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RewriteWikilinkParams {
    /// The vault written.
    pub vault: VaultAddress,
    /// Whether to preview the write or apply it. There is no default.
    pub mode: ApplyMode,
    /// What the rewritten wikilinks name now. It need not name a document
    /// that stands, and naming several does not resolve.
    #[serde(deserialize_with = "document_target")]
    #[schemars(schema_with = "whole_document_schema")]
    pub old: ResolutionTarget,
    /// What they name after.
    #[serde(deserialize_with = "document_target")]
    #[schemars(schema_with = "whole_document_schema")]
    pub new: ResolutionTarget,
    /// What the author observed and requires to hold.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<AuthorCondition>,
    /// Whether the write applies past the schema check. Absent is `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub force: bool,
}

impl RewriteWikilinkParams {
    /// A request to `mode` the rewrite of every wikilink naming `old` to name
    /// `new` in `vault`, with no condition and not forced.
    pub const fn new(
        vault: VaultAddress,
        mode: ApplyMode,
        old: ResolutionTarget,
        new: ResolutionTarget,
    ) -> Self {
        RewriteWikilinkParams {
            vault,
            mode,
            old,
            new,
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

    /// The plan the request compiles to: one `rewrite_wikilink` carrying the
    /// request's conditions, forced as the request is.
    pub fn plan(self) -> AuthoredPlan {
        let RewriteWikilinkParams {
            vault,
            mode: _,
            old,
            new,
            conditions,
            force,
        } = self;
        let operation =
            Operation::new(OperationKind::RewriteWikilink { old, new }).with_conditions(conditions);
        AuthoredPlan::new(vault, vec![operation]).with_force(force)
    }
}
