//! `delete`: remove one document.
//!
//! **A delete says out loud what becomes of the links naming its document.**
//! It rewrites them to name `rewrite_to`, or leaves them broken where
//! `allow_broken_links` says so; saying neither, a document any link names is
//! not removed, and the operation is left unresolved naming every document
//! holding such a link. Saying both is refused at the read. The flag is
//! written only when `true`, as `force` is, so a request that omits it asks
//! for the strict reading.
//!
//! **`rewrite_to` names a document, never a place inside one.** It is read
//! through the one resolution grammar, and an anchor on it is refused at the
//! read: each rewritten link keeps the anchor it was written with.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::address::VaultAddress;
use crate::apply::ApplyMode;
use crate::document::DocumentPath;
use crate::plan::backlinks::Backlinks;
use crate::plan::document::AuthoredPlan;
use crate::plan::document::is_false;
use crate::plan::operation::{AuthorCondition, Operation, OperationKind};
use crate::plan::write_target::settle_flattened_target;
use crate::target::ResolutionTarget;
use crate::write::document_target;

/// What a `delete` request carries.
///
/// On the wire: `{"vault":…,"mode":"apply","path":"notes/b.md","rewrite_to":"notes/c"}`.
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(transform = settle_flattened_target)]
#[non_exhaustive]
pub struct DeleteParams {
    /// The vault written.
    pub vault: VaultAddress,
    /// Whether to preview the write or apply it. There is no default.
    pub mode: ApplyMode,
    /// The document removed.
    pub path: DocumentPath,
    /// What becomes of the links naming it: forbidden, rewritten to
    /// `rewrite_to`, or left broken where `allow_broken_links` says so.
    #[serde(flatten)]
    pub backlinks: Backlinks,
    /// What the author observed and requires to hold.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<AuthorCondition>,
    /// Whether the write applies past the schema check. Absent is `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub force: bool,
}

impl DeleteParams {
    /// A request to `mode` the removal of the document at `path` in `vault`,
    /// which no link may name, with no condition and not forced.
    pub const fn new(vault: VaultAddress, mode: ApplyMode, path: DocumentPath) -> Self {
        DeleteParams {
            vault,
            mode,
            path,
            backlinks: Backlinks::Forbidden,
            conditions: Vec::new(),
            force: false,
        }
    }

    /// The request rewriting every link naming the removed document to name
    /// `rewrite_to`, in place of leaving any broken.
    #[must_use]
    pub fn rewriting_to(mut self, rewrite_to: ResolutionTarget) -> Self {
        self.backlinks = Backlinks::RewrittenTo(rewrite_to);
        self
    }

    /// The request leaving every link naming the removed document broken, in
    /// place of rewriting any.
    #[must_use]
    pub fn breaking_links(mut self) -> Self {
        self.backlinks = Backlinks::LeftBroken;
        self
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

    /// The plan the request compiles to: one `delete_document` saying what
    /// the request says of the links naming its document, carrying the
    /// request's conditions, forced as the request is.
    pub fn plan(self) -> AuthoredPlan {
        let DeleteParams {
            vault,
            mode: _,
            path,
            backlinks,
            conditions,
            force,
        } = self;
        let operation = Operation::new(OperationKind::DeleteDocument { path, backlinks })
            .with_conditions(conditions);
        AuthoredPlan::new(vault, vec![operation]).with_force(force)
    }
}

/// A `delete` request as it arrives: every key it may carry and no other.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteKeys {
    vault: VaultAddress,
    mode: ApplyMode,
    path: DocumentPath,
    #[serde(default, deserialize_with = "written_document_target")]
    rewrite_to: Option<ResolutionTarget>,
    #[serde(default)]
    allow_broken_links: bool,
    #[serde(default)]
    conditions: Vec<AuthorCondition>,
    #[serde(default)]
    force: bool,
}

/// A `rewrite_to` that was written, read as the document it names: `null` is
/// refused rather than read as left out.
fn written_document_target<'de, D>(deserializer: D) -> Result<Option<ResolutionTarget>, D::Error>
where
    D: Deserializer<'de>,
{
    document_target(deserializer).map(Some)
}

impl<'de> Deserialize<'de> for DeleteParams {
    /// Every key is read by the derive, refusing any the request does not
    /// name and any written twice; what becomes of the links is then read
    /// from its two keys, refusing a request that rewrites them and leaves
    /// them broken at once.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let DeleteKeys {
            vault,
            mode,
            path,
            rewrite_to,
            allow_broken_links,
            conditions,
            force,
        } = DeleteKeys::deserialize(deserializer)?;
        let backlinks =
            Backlinks::from_keys(rewrite_to, allow_broken_links).map_err(|problem| {
                D::Error::custom(format_args!(
                    "a `delete` request says what becomes of its links: {problem}"
                ))
            })?;
        Ok(DeleteParams {
            vault,
            mode,
            path,
            backlinks,
            conditions,
            force,
        })
    }
}
