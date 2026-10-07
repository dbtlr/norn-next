//! `apply`: preview or apply a plan.
//!
//! **Every write is a plan, and this is the verb a plan is sent through.** A
//! request carries operations or a resolved plan, and states whether it
//! previews or applies; there is no default mode, so a request naming none
//! does not read. The vault is the one the plan names by address, which is
//! why the verb's addressing is required while its params carry no vault of
//! their own.
//!
//! **An apply answers with one of two outcomes, or with a code.** A preview
//! answers with the resolved plan and its forecast and writes nothing; an
//! applied plan answers with the plan, whether its changeset committed or the
//! entry is healing from what the paths hold, what each target came to, and
//! the folders it made and removed. Both cross inside a
//! [`VaultAnswer`](crate::VaultAnswer), as a read's report does. Every other
//! outcome is an [`ErrorEnvelope`](crate::ErrorEnvelope): refused by a check
//! (`vault/plan-refused`, with a fresh plan) or by the root's identity
//! (`vault/root-changed`), interrupted after a target landed
//! (`vault/plan-interrupted`), stopped by I/O before any did
//! (`vault/write-failed`), not run for a lifecycle cause (`host/apply-not-run`),
//! dropped after publication began (`host/apply-outcome-unknown`), or a plan
//! whose own shape is wrong (`request/plan-invalid`). Admission refuses under
//! the codes a read would.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::document::DocumentPath;
use crate::plan::document::{PlanDocument, ResolvedPlan};
use crate::plan::forecast::{FolderPath, Forecast};
use crate::plan::outcome::SchemaViolation;
use crate::read::validate::RuleSet;

/// Whether a request previews a plan or applies it.
///
/// On the wire a mode is the flat string itself: `"preview"`, `"apply"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyMode {
    /// Resolve the plan and answer with it and its forecast, writing nothing.
    Preview,
    /// Apply the plan.
    Apply,
}

/// What an `apply` request carries.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ApplyParams {
    /// Whether to preview the plan or apply it. There is no default.
    pub mode: ApplyMode,
    /// The plan, naming the vault it is for.
    pub plan: PlanDocument,
}

impl ApplyParams {
    /// A request to `mode` the `plan`.
    pub const fn new(mode: ApplyMode, plan: PlanDocument) -> Self {
        ApplyParams { mode, plan }
    }
}

/// Whether an applied plan's changes are in the registration's store.
///
/// On the wire an outcome is the flat string itself: `"committed"`,
/// `"healing"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChangesetOutcome {
    /// The apply's changes committed as one changeset.
    Committed,
    /// The changeset did not commit, and the entry heals from what the paths
    /// hold.
    Healing,
}

/// What one target of an applied plan came to.
///
/// On the wire a result is the flat string itself: `"wrote"`, `"found"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TargetResult {
    /// This apply published it.
    Wrote,
    /// It already stood at its after-state.
    Found,
}

/// One target of an applied plan, and what it came to.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct AppliedTarget {
    /// The target.
    pub path: DocumentPath,
    /// What it came to.
    pub result: TargetResult,
}

impl AppliedTarget {
    /// The target at `path`, which came to `result`.
    pub const fn new(path: DocumentPath, result: TargetResult) -> Self {
        AppliedTarget { path, result }
    }
}

/// What `apply` answers with.
///
/// On the wire a report is an object tagged `outcome`:
/// `{"outcome":"previewed",…}`, `{"outcome":"applied",…}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ApplyReport {
    /// The plan was resolved and nothing was written.
    #[non_exhaustive]
    Previewed {
        /// The resolved plan. Sending it back applies it.
        plan: ResolvedPlan,
        /// What it would do beyond the transitions the plan names: the
        /// folders it makes and removes.
        forecast: Forecast,
    },
    /// Every target of the plan stands at its after-state.
    #[non_exhaustive]
    Applied {
        /// The resolved plan that was applied.
        plan: ResolvedPlan,
        /// Whether its changes committed as one changeset or the entry is
        /// healing from what the paths hold.
        changeset: ChangesetOutcome,
        /// Each target, and what it came to.
        targets: Vec<AppliedTarget>,
        /// The folders the plan made.
        folders_made: Vec<FolderPath>,
        /// The folders the plan's removals left empty, which it removed.
        folders_removed: Vec<FolderPath>,
        /// Every schema violation a written result introduces that the
        /// plan's force let through. Empty for a plan that is not forced, and
        /// for a forced plan that introduces no violation.
        forced: Vec<SchemaViolation>,
        /// Every rule set the violations in `forced` cite, each once, in the
        /// order of its identity; empty where none cites one.
        rule_sets: Vec<RuleSet>,
    },
}

impl ApplyReport {
    /// The `plan` a preview resolved, and its `forecast`.
    pub const fn previewed(plan: ResolvedPlan, forecast: Forecast) -> Self {
        ApplyReport::Previewed { plan, forecast }
    }

    /// The `plan` applied, its `changeset`'s outcome, its `targets`, and the
    /// folders it made and removed, forcing nothing through.
    pub const fn applied(
        plan: ResolvedPlan,
        changeset: ChangesetOutcome,
        targets: Vec<AppliedTarget>,
        folders_made: Vec<FolderPath>,
        folders_removed: Vec<FolderPath>,
    ) -> Self {
        ApplyReport::Applied {
            plan,
            changeset,
            targets,
            folders_made,
            folders_removed,
            forced: Vec::new(),
            rule_sets: Vec::new(),
        }
    }

    /// The report listing `violations` as those a forced plan lets through,
    /// citing `cited`: in a preview's forecast, or in the applied report
    /// itself.
    #[must_use]
    pub fn with_forced(mut self, violations: Vec<SchemaViolation>, cited: Vec<RuleSet>) -> Self {
        match &mut self {
            ApplyReport::Applied {
                forced, rule_sets, ..
            } => {
                *forced = violations;
                *rule_sets = cited;
            }
            ApplyReport::Previewed { forecast, .. } => {
                forecast.forced = violations;
                forecast.rule_sets = cited;
            }
        }
        self
    }
}
