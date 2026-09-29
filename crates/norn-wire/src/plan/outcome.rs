//! Why an apply did not end applied: the typed reasons its outcome codes
//! carry.
//!
//! **A refusal, an interruption and a fault are three different facts.** A
//! check that refuses before publication writes nothing, and answers with a
//! fresh resolved plan: [`RefusedCheck`] names each check, and
//! [`UnresolvedOperation`] each operation the fresh plan leaves for the caller
//! to dispose of. An interruption is a failure that partly landed, not a
//! refusal: [`InterruptionCause`] names what stopped publication, and sending
//! the plan again finishes it. A fault is in the plan's own shape, judged
//! apart from the vault: [`PlanFault`] names it and the operations involved,
//! by their position in the plan's operation list, because an operation need
//! not carry an identifier.
//!
//! **A schema violation is spelled in the finding vocabulary.** What a plan
//! introduces is what a finding over the result would be filed under, so a
//! refused check carries the finding kind, the subject inside the document and
//! the message a finding would, rather than a second vocabulary for the same
//! facts.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::document::DocumentPath;
use crate::finding::FindingKind;
use crate::plan::document::{FileState, PlanCondition};
use crate::plan::operation::{Operation, OperationId};

/// One check that refused an apply before anything was published.
///
/// On the wire a check is an object tagged `check`:
/// `{"check":"drifted","path":"notes/a.md","holds":{"state":"absent"}}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "check", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RefusedCheck {
    /// A target holds neither its before-state nor its after-state.
    #[non_exhaustive]
    Drifted {
        /// The target.
        path: DocumentPath,
        /// What it holds.
        holds: FileState,
    },
    /// A condition the plan carries does not hold.
    #[non_exhaustive]
    ConditionFailed {
        /// The condition.
        condition: PlanCondition,
    },
    /// A target's result would violate the vault schema where the plan writes,
    /// or where no violation stood before the plan.
    #[non_exhaustive]
    SchemaViolation {
        /// The target whose result violates the schema.
        path: DocumentPath,
        /// The finding kind the violation would be filed under.
        kind: FindingKind,
        /// What the violation is about inside the document, as the document
        /// writes it — a field key, a tag — and `null` where it is about the
        /// whole of it.
        target: Option<String>,
        /// The violation in words, for a person reading a report.
        message: String,
    },
    /// Something stands at the path a create would publish at.
    #[non_exhaustive]
    NameTaken {
        /// The path the create names.
        path: DocumentPath,
    },
}

impl RefusedCheck {
    /// The target at `path` drifted, holding `holds`.
    pub const fn drifted(path: DocumentPath, holds: FileState) -> Self {
        RefusedCheck::Drifted { path, holds }
    }

    /// The plan's `condition` does not hold.
    pub const fn condition_failed(condition: PlanCondition) -> Self {
        RefusedCheck::ConditionFailed { condition }
    }

    /// The result at `path` would be filed under `kind`, about `target`,
    /// described by `message`.
    pub fn schema_violation(
        path: DocumentPath,
        kind: FindingKind,
        target: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        RefusedCheck::SchemaViolation {
            path,
            kind,
            target,
            message: message.into(),
        }
    }

    /// Something stands at `path`, where a create would publish.
    pub const fn name_taken(path: DocumentPath) -> Self {
        RefusedCheck::NameTaken { path }
    }
}

/// Why an operation is left out of a fresh plan, for the caller to dispose
/// of.
///
/// On the wire a reason is an object tagged `kind`: `{"kind":"part_landed"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum UnresolvedReason {
    /// One of its targets holds its after-state and another does not.
    PartLanded {},
    /// It no longer resolves against what the vault holds: an edit whose
    /// text no longer occurs once, or a move whose destination is taken.
    #[non_exhaustive]
    NoLongerResolves {
        /// What no longer resolves, in words, for a person reading a report.
        detail: String,
    },
    /// It requires an operation that is unresolved, directly or through
    /// others.
    #[non_exhaustive]
    RequiresUnresolved {
        /// The identifier of the unresolved operation it requires.
        requires: OperationId,
    },
}

impl UnresolvedReason {
    /// One target landed and another did not.
    pub const fn part_landed() -> Self {
        UnresolvedReason::PartLanded {}
    }

    /// The operation no longer resolves, described by `detail`.
    pub fn no_longer_resolves(detail: impl Into<String>) -> Self {
        UnresolvedReason::NoLongerResolves {
            detail: detail.into(),
        }
    }

    /// The operation requires the unresolved operation `requires`.
    pub const fn requires_unresolved(requires: OperationId) -> Self {
        UnresolvedReason::RequiresUnresolved { requires }
    }
}

/// An operation a fresh plan leaves out, and why.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct UnresolvedOperation {
    /// The operation, as the plan carried it.
    pub operation: Operation,
    /// Why it is left out.
    pub reason: UnresolvedReason,
}

impl UnresolvedOperation {
    /// `operation`, left out for `reason`.
    pub const fn new(operation: Operation, reason: UnresolvedReason) -> Self {
        UnresolvedOperation { operation, reason }
    }
}

/// What stopped publication after at least one target landed.
///
/// On the wire a cause is an object tagged `kind`:
/// `{"kind":"io_failure","detail":"…"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum InterruptionCause {
    /// The filesystem refused a publication.
    #[non_exhaustive]
    IoFailure {
        /// The failure in words, for a person reading a message or a log.
        detail: String,
    },
    /// Another writer took a create's name after it was staged.
    #[non_exhaustive]
    NameTaken {
        /// The path the create names.
        path: DocumentPath,
    },
    /// Another writer changed a target after it was staged.
    #[non_exhaustive]
    ForeignEdit {
        /// The target.
        path: DocumentPath,
    },
}

impl InterruptionCause {
    /// The filesystem refused, described by `detail`.
    pub fn io_failure(detail: impl Into<String>) -> Self {
        InterruptionCause::IoFailure {
            detail: detail.into(),
        }
    }

    /// Another writer took the name at `path`.
    pub const fn name_taken(path: DocumentPath) -> Self {
        InterruptionCause::NameTaken { path }
    }

    /// Another writer changed the target at `path`.
    pub const fn foreign_edit(path: DocumentPath) -> Self {
        InterruptionCause::ForeignEdit { path }
    }
}

/// What is wrong with a plan's own shape, whatever vault it is for. An
/// operation is named by its position in the plan's operation list, counting
/// from 0.
///
/// On the wire a fault is an object tagged `kind`:
/// `{"kind":"duplicate_id","id":"move-a","positions":[0,3]}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlanFault {
    /// More than one operation carries one identifier.
    #[non_exhaustive]
    DuplicateId {
        /// The identifier.
        id: OperationId,
        /// The positions of the operations carrying it.
        positions: Vec<usize>,
    },
    /// An operation requires an identifier no operation of the plan carries.
    #[non_exhaustive]
    UnknownRequirement {
        /// The position of the operation.
        position: usize,
        /// The identifier it requires.
        requires: OperationId,
    },
    /// Operations require each other in a cycle.
    #[non_exhaustive]
    RequiresCycle {
        /// The positions of the operations in the cycle, in the order each
        /// requires the next.
        positions: Vec<usize>,
    },
    /// Operations draw content from each other in a cycle, such as two
    /// documents exchanging places. Split the plan into plans that each
    /// finish.
    #[non_exhaustive]
    ContentCycle {
        /// The positions of the operations in the cycle.
        positions: Vec<usize>,
    },
}

impl PlanFault {
    /// The operations at `positions` all carry `id`.
    pub const fn duplicate_id(id: OperationId, positions: Vec<usize>) -> Self {
        PlanFault::DuplicateId { id, positions }
    }

    /// The operation at `position` requires `requires`, which no operation
    /// carries.
    pub const fn unknown_requirement(position: usize, requires: OperationId) -> Self {
        PlanFault::UnknownRequirement { position, requires }
    }

    /// The operations at `positions` require each other in a cycle.
    pub const fn requires_cycle(positions: Vec<usize>) -> Self {
        PlanFault::RequiresCycle { positions }
    }

    /// The operations at `positions` draw content from each other in a cycle.
    pub const fn content_cycle(positions: Vec<usize>) -> Self {
        PlanFault::ContentCycle { positions }
    }
}
