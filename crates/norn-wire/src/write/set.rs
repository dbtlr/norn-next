//! `set`: change the frontmatter of one document, or of every document a
//! predicate list matches.
//!
//! **A set names its documents the way its operations do.** The request
//! carries exactly one of `path` and `where`, the keys a frontmatter
//! operation names its documents by, and each change compiles to one
//! frontmatter operation with that target. A `where` target is expanded at
//! planning into one operation per matched document; a `where` matching no
//! document leaves its operations unresolved rather than doing nothing.
//!
//! **A change is written exactly.** A set writes the typed value sent; push
//! appends one element and pop removes every element equal to one, each read
//! against what the document holds when the plan is resolved, so a plan
//! resolved again after a foreign edit appends to that edit rather than
//! writing a list computed before it.
//!
//! **The request is read by hand, because its target is two keys.** The
//! target sits flattened among the request's own keys, and the derive cannot
//! both flatten it and refuse an unknown key. So the request is read by the
//! derive into a private shape holding every key it may carry and no other,
//! and the target is then built from its two keys by the rule an operation's
//! target is built by.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::address::VaultAddress;
use crate::apply::ApplyMode;
use crate::document::DocumentPath;
use crate::plan::document::AuthoredPlan;
use crate::plan::document::is_false;
use crate::plan::operation::{AuthorCondition, Operation, OperationKind, written};
use crate::plan::value::AuthoredValue;
use crate::plan::write_target::{WriteTarget, settle_flattened_target};
use crate::predicate::Predicate;
use crate::write::at_least_one;

/// One change to a frontmatter field.
///
/// On the wire a change is an object tagged `change`:
/// `{"change":"set","field":"status","value":"done"}`,
/// `{"change":"remove","field":"due"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "change", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum FieldChange {
    /// Set the field to exactly this value, adding it where the document does
    /// not carry it.
    #[non_exhaustive]
    Set {
        /// The frontmatter key.
        field: String,
        /// The value the field holds after the write, written exactly.
        value: AuthoredValue,
    },
    /// Remove a field the document carries.
    #[non_exhaustive]
    Remove {
        /// The frontmatter key removed.
        field: String,
    },
    /// Append a value to a list. A field the document does not carry becomes
    /// a list of the one value.
    #[non_exhaustive]
    Push {
        /// The frontmatter key.
        field: String,
        /// The value appended.
        value: AuthoredValue,
    },
    /// Remove every element equal to a value from a list. The list must hold
    /// the value.
    #[non_exhaustive]
    Pop {
        /// The frontmatter key.
        field: String,
        /// The value removed, wherever the list holds it.
        value: AuthoredValue,
    },
}

impl FieldChange {
    /// Set `field` to `value`.
    pub fn set(field: impl Into<String>, value: AuthoredValue) -> Self {
        FieldChange::Set {
            field: field.into(),
            value,
        }
    }

    /// Remove `field`.
    pub fn remove(field: impl Into<String>) -> Self {
        FieldChange::Remove {
            field: field.into(),
        }
    }

    /// Append `value` to the list `field` holds.
    pub fn push(field: impl Into<String>, value: AuthoredValue) -> Self {
        FieldChange::Push {
            field: field.into(),
            value,
        }
    }

    /// Remove every element equal to `value` from the list `field` holds.
    pub fn pop(field: impl Into<String>, value: AuthoredValue) -> Self {
        FieldChange::Pop {
            field: field.into(),
            value,
        }
    }

    /// The operation kind this change is on the documents `target` names.
    fn into_kind(self, target: WriteTarget) -> OperationKind {
        match self {
            FieldChange::Set { field, value } => OperationKind::SetFrontmatter {
                target,
                field,
                value,
            },
            FieldChange::Remove { field } => OperationKind::RemoveFrontmatter { target, field },
            FieldChange::Push { field, value } => OperationKind::PushFrontmatter {
                target,
                field,
                value,
            },
            FieldChange::Pop { field, value } => OperationKind::PopFrontmatter {
                target,
                field,
                value,
            },
        }
    }
}

/// What a `set` request carries.
///
/// On the wire the documents written are exactly one of `path` and `where`
/// beside the request's other keys:
/// `{"vault":…,"mode":"preview","path":"notes/a.md","changes":[…]}`.
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(transform = settle_flattened_target)]
#[non_exhaustive]
pub struct SetParams {
    /// The vault written.
    pub vault: VaultAddress,
    /// Whether to preview the write or apply it. There is no default.
    pub mode: ApplyMode,
    /// The documents written.
    #[serde(flatten)]
    pub target: WriteTarget,
    /// The changes, at least one, in the order they compose.
    #[schemars(length(min = 1))]
    pub changes: Vec<FieldChange>,
    /// What the author observed and requires to hold.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<AuthorCondition>,
    /// Whether the write applies past the schema check. Absent is `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub force: bool,
}

impl SetParams {
    /// A request to `mode` the `changes` to the documents `target` names in
    /// `vault`, with no condition and not forced.
    pub const fn new(
        vault: VaultAddress,
        mode: ApplyMode,
        target: WriteTarget,
        changes: Vec<FieldChange>,
    ) -> Self {
        SetParams {
            vault,
            mode,
            target,
            changes,
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

    /// The plan the request compiles to: one frontmatter operation per
    /// change, in the request's order, each with the request's target and
    /// carrying its conditions, forced as the request is.
    pub fn plan(self) -> AuthoredPlan {
        let SetParams {
            vault,
            mode: _,
            target,
            changes,
            conditions,
            force,
        } = self;
        let operations = changes
            .into_iter()
            .map(|change| {
                Operation::new(change.into_kind(target.clone())).with_conditions(conditions.clone())
            })
            .collect();
        AuthoredPlan::new(vault, operations).with_force(force)
    }
}

/// A change list read with its floor of one change.
fn at_least_one_change<'de, D>(deserializer: D) -> Result<Vec<FieldChange>, D::Error>
where
    D: Deserializer<'de>,
{
    at_least_one(deserializer, "change")
}

/// A `set` request as it arrives: every key it may carry and no other, its
/// target as the two keys it may be written as.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetFieldsKeys {
    vault: VaultAddress,
    mode: ApplyMode,
    #[serde(default, deserialize_with = "written")]
    path: Option<DocumentPath>,
    #[serde(default, rename = "where", deserialize_with = "written")]
    predicates: Option<Vec<Predicate>>,
    #[serde(deserialize_with = "at_least_one_change")]
    changes: Vec<FieldChange>,
    #[serde(default)]
    conditions: Vec<AuthorCondition>,
    #[serde(default)]
    force: bool,
}

impl<'de> Deserialize<'de> for SetParams {
    /// Every key is read by the derive, refusing any the request does not
    /// name and any written twice; the target is then built from exactly one
    /// of `path` and `where`.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let SetFieldsKeys {
            vault,
            mode,
            path,
            predicates,
            changes,
            conditions,
            force,
        } = SetFieldsKeys::deserialize(deserializer)?;
        let target = WriteTarget::from_keys(path, predicates).map_err(|problem| {
            D::Error::custom(format_args!(
                "a `set` request names its documents: {problem}"
            ))
        })?;
        Ok(SetParams {
            vault,
            mode,
            target,
            changes,
            conditions,
            force,
        })
    }
}
