//! The two plan documents a caller holds: operations it authored, and the
//! resolved plan a preview answers with.
//!
//! **A resolved plan is a self-contained value.** It names its vault by
//! address, carries the identity of the vault's root, its operations, one
//! transition per file they touch and the conditions its planning read. It
//! never carries the bytes of a file it did not author, and it carries no
//! local path to the vault and no format version: a plan is short-lived, and
//! a plan this build cannot read is refused rather than migrated.
//!
//! **Three kinds of fact, three places.** An author's condition sits on an
//! operation; the conditions a resolved plan carries are [`PlanCondition`]s,
//! checked when it is applied; and [`Provenance`] records what a plan was
//! planned from and is never checked. Keeping them three types means a
//! condition cannot be mistaken for provenance, and an author's condition
//! that became a before-state is not carried twice.
//!
//! **Provenance is a dormant carrier for Layer 5 repair.** Repair plans cite
//! the finding generation they read and the findings they skipped. No Layer 4
//! planner emits provenance — every Layer 4 plan is authored by a write verb
//! or a caller, and neither plans from findings — so the current call graph
//! reaches it only through a caller sending it back. It is spelled here so a
//! repair plan is the same document every other plan is.
//!
//! **The document is a newtype over each plan.** A resolved plan crosses on
//! its own inside every answer an apply gives, so it is a type of its own, and
//! the document a caller sends wraps it under the `plan` tag. The tagged-enum
//! rule that variants are struct-shaped exists because a newtype variant over
//! a non-object fails at serialize time; both payloads here are objects, so
//! the tag merges into them and the schema advertises it there.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::address::VaultAddress;
use crate::document::DocumentPath;
use crate::plan::hash::ContentHash;
use crate::plan::operation::Operation;
use crate::plan::root::RootIdentity;

/// What a file holds on one side of a transition: nothing, or exactly the
/// bytes with a hash.
///
/// On the wire a state is an object tagged `state`: `{"state":"absent"}`,
/// `{"state":"present","hash":"sha256:…"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum FileState {
    /// Nothing stands at the path.
    Absent {},
    /// The file at the path holds exactly the bytes with this hash.
    #[non_exhaustive]
    Present {
        /// The hash of what the file holds.
        hash: ContentHash,
    },
}

impl FileState {
    /// Nothing stands at the path.
    pub const fn absent() -> Self {
        FileState::Absent {}
    }

    /// The file holds the bytes whose hash is `hash`.
    pub const fn present(hash: ContentHash) -> Self {
        FileState::Present { hash }
    }
}

/// One file's change within a resolved plan: what it must hold before the
/// write and what it holds after. A file already holding its after-state has
/// landed.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Transition {
    /// The file changed.
    pub path: DocumentPath,
    /// What the file must hold before the write.
    pub before: FileState,
    /// What the file holds after it.
    pub after: FileState,
}

impl Transition {
    /// The file at `path` changing from `before` to `after`.
    pub const fn new(path: DocumentPath, before: FileState, after: FileState) -> Self {
        Transition {
            path,
            before,
            after,
        }
    }
}

/// A fact about a file the plan does not write, which must still hold when the
/// plan is applied.
///
/// On the wire a condition is an object tagged `condition`:
/// `{"condition":"content_hash","path":"notes/c.md","hash":"sha256:…"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "condition", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum PlanCondition {
    /// The file at the path holds exactly the bytes with this hash.
    #[non_exhaustive]
    ContentHash {
        /// The file the condition is about.
        path: DocumentPath,
        /// The hash of what it must hold.
        hash: ContentHash,
    },
}

impl PlanCondition {
    /// The file at `path` holds the bytes whose hash is `hash`.
    pub const fn content_hash(path: DocumentPath, hash: ContentHash) -> Self {
        PlanCondition::ContentHash { path, hash }
    }
}

/// A finding a repair plan left alone, and why.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct SkippedFinding {
    /// The finding's identity in the vault's findings.
    pub finding: u64,
    /// Why the plan left it alone, in words, for a person reading the plan.
    pub reason: String,
}

impl SkippedFinding {
    /// The finding `finding`, left alone for `reason`.
    pub fn new(finding: u64, reason: impl Into<String>) -> Self {
        SkippedFinding {
            finding,
            reason: reason.into(),
        }
    }
}

// A dormant carrier: Layer 5 repair is the consuming layer. Repair plans cite
// the finding generation they read and the findings they skipped; no Layer 4
// planner plans from findings, so nothing in the current call graph emits
// this, and it is reached only when a caller sends a plan carrying one back.
// Its published description stays wire-facing, so the roadmap note lives here
// rather than in the doc comment schemars lifts.
/// What a repair plan was planned from. It is a record, never checked when
/// the plan is applied.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Provenance {
    /// The write generation of the findings the plan was planned from.
    pub finding_generation: u64,
    /// The findings the plan left alone.
    pub skipped: Vec<SkippedFinding>,
}

impl Provenance {
    /// A plan planned from the findings at `finding_generation`, leaving
    /// `skipped` alone.
    pub const fn new(finding_generation: u64, skipped: Vec<SkippedFinding>) -> Self {
        Provenance {
            finding_generation,
            skipped,
        }
    }
}

/// A plan as its author writes it: the vault it is for and its operations,
/// not yet resolved against what the vault holds.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct AuthoredPlan {
    /// The vault the plan is for.
    pub vault: VaultAddress,
    /// The operations, in the order they compose.
    pub operations: Vec<Operation>,
    /// Words about the plan, for a person reading it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footnote: Option<String>,
}

impl AuthoredPlan {
    /// The plan for `vault`, made of `operations`.
    pub const fn new(vault: VaultAddress, operations: Vec<Operation>) -> Self {
        AuthoredPlan {
            vault,
            operations,
            footnote: None,
        }
    }

    /// The plan carrying `footnote`.
    #[must_use]
    pub fn with_footnote(mut self, footnote: impl Into<String>) -> Self {
        self.footnote = Some(footnote.into());
        self
    }
}

/// A plan resolved against what the vault held: its operations, one transition
/// per file they touch, the conditions its planning read, and the identity of
/// the root it was resolved against. Sending it back applies it; sending it
/// again after an interruption finishes it.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ResolvedPlan {
    /// The vault the plan is for.
    pub vault: VaultAddress,
    /// The identity of the vault's root when the plan was resolved. A vault
    /// whose root identity differs refuses the plan.
    pub root: RootIdentity,
    /// The operations, in the order they compose.
    pub operations: Vec<Operation>,
    /// One per file the operations touch. Folders are not transitions.
    pub transitions: Vec<Transition>,
    /// What must still hold of the files the plan does not write.
    pub conditions: Vec<PlanCondition>,
    /// What a repair plan was planned from. Absent from every other plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    /// Words about the plan, for a person reading it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footnote: Option<String>,
}

impl ResolvedPlan {
    /// The plan for `vault`, resolved against the root `root` into
    /// `transitions`, depending on `conditions`, with no provenance and no
    /// footnote.
    pub const fn new(
        vault: VaultAddress,
        root: RootIdentity,
        operations: Vec<Operation>,
        transitions: Vec<Transition>,
        conditions: Vec<PlanCondition>,
    ) -> Self {
        ResolvedPlan {
            vault,
            root,
            operations,
            transitions,
            conditions,
            provenance: None,
            footnote: None,
        }
    }

    /// The plan citing `provenance`.
    #[must_use]
    pub fn with_provenance(mut self, provenance: Provenance) -> Self {
        self.provenance = Some(provenance);
        self
    }

    /// The plan carrying `footnote`.
    #[must_use]
    pub fn with_footnote(mut self, footnote: impl Into<String>) -> Self {
        self.footnote = Some(footnote.into());
        self
    }
}

/// A plan a caller sends: operations it authored, or a resolved plan.
///
/// On the wire a document is the plan's own object tagged `plan`:
/// `{"plan":"operations","vault":…,"operations":[…]}`,
/// `{"plan":"resolved","vault":…,"root":…,…}`. A key the plan does not name is
/// refused, at every depth.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "plan", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlanDocument {
    /// Operations, planned and applied in one request. Sending them again is
    /// a new change.
    Operations(AuthoredPlan),
    /// A resolved plan, applied where every target holds its before-state or
    /// its after-state and every condition holds.
    Resolved(ResolvedPlan),
}

impl PlanDocument {
    /// The document carrying the operations of `plan`.
    pub const fn operations(plan: AuthoredPlan) -> Self {
        PlanDocument::Operations(plan)
    }

    /// The document carrying the resolved `plan`.
    pub const fn resolved(plan: ResolvedPlan) -> Self {
        PlanDocument::Resolved(plan)
    }

    /// The vault the plan is for.
    pub const fn vault(&self) -> &VaultAddress {
        match self {
            PlanDocument::Operations(plan) => &plan.vault,
            PlanDocument::Resolved(plan) => &plan.vault,
        }
    }
}
