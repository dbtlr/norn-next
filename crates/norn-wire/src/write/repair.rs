//! `repair`: plan the fixes the vault's findings call for.
//!
//! **A repair is selected like a validate and planned in batches.** The
//! request carries validate's selection — a conjunction of predicates, the
//! kinds and severity to repair, and the one rule whose findings to repair —
//! and plans the fixes for the findings it selects, by document in path
//! order. `limit` is a soft target in selected findings, read as validate's
//! page limit is: `null` leaves it to the host's default page and the host's
//! ceiling caps the request. A batch is never cut inside a document, so it
//! extends through the last findings of its last document. The cursor
//! `after` continues from the last document path a batch covered.
//!
//! **A repair states its mode, as every write does.** There is no default: a
//! request naming neither `preview` nor `apply` does not read. It has no
//! `force` and no `conditions`: what a repair writes it derives from the
//! findings, and a result the schema refuses is a finding skipped, never a
//! write let through.
//!
//! **The threshold gates what the plan writes.** A fix is made at one
//! [`Confidence`]; the plan holds the fixes the threshold admits and skips
//! the others as below the threshold. A request naming none is read at
//! [`Confidence::DEFAULT_THRESHOLD`].

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::address::VaultAddress;
use crate::apply::ApplyMode;
use crate::cursor::Cursor;
use crate::finding::{FindingKind, Severity};
use crate::plan::document::Confidence;
use crate::predicate::Predicate;

// A dormant carrier: Layer 5B repair is the consuming layer, and NORN-373
// is the step that adds `Verb::Repair`, the host handler that plans it and a
// `plan` that builds its operations. Until then nothing in the call graph
// reads these params. The roadmap note lives here rather than in the doc
// comment schemars lifts into the published schema.
/// What a `repair` request carries.
///
/// On the wire the selection is validate's, beside the repair's own keys:
/// `{"vault":…,"mode":"preview","kinds":["document/undeclared-tag"],"threshold":"declared"}`.
/// The lists are left out where empty, and the optional keys where absent.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RepairParams {
    /// The vault repaired.
    pub vault: VaultAddress,
    /// Whether to preview the repair or apply it. There is no default.
    pub mode: ApplyMode,
    /// The conjunction a finding's document must satisfy. Empty repairs every
    /// finding in the vault.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub predicates: Vec<Predicate>,
    /// The kinds to repair. Empty repairs every kind.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<FindingKind>,
    /// The severity to repair at or above. `null` repairs every severity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,
    /// The one schema rule whose findings to repair: those citing it. `null`
    /// repairs findings whatever rules they cite, and those citing none. A
    /// rule the pinned schema does not declare is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// A soft target for how many selected findings a batch plans. `null`
    /// leaves it to the host's default page, and the host's ceiling caps the
    /// request. A batch is never cut inside a document, so it extends
    /// through the last findings of its last document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Where a batch continues from: the cursor a previous batch answered,
    /// which names the last document path it covered. `null` starts at the
    /// first document. A cursor from any other request is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Cursor>,
    /// The weakest confidence the plan writes a fix at. `null` is `derived`:
    /// declared and derived fixes are written and suggested ones are skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<Confidence>,
}

impl RepairParams {
    /// A request to `mode` the repair of every finding of every kind in
    /// `vault`, at the default threshold, from the first document.
    pub const fn new(vault: VaultAddress, mode: ApplyMode) -> Self {
        RepairParams {
            vault,
            mode,
            predicates: Vec::new(),
            kinds: Vec::new(),
            severity: None,
            rule: None,
            limit: None,
            after: None,
            threshold: None,
        }
    }

    /// The request filtered by `predicates`.
    #[must_use]
    pub fn with_predicates(mut self, predicates: impl IntoIterator<Item = Predicate>) -> Self {
        self.predicates = predicates.into_iter().collect();
        self
    }

    /// The request repairing `kinds` alone.
    #[must_use]
    pub fn with_kinds(mut self, kinds: impl IntoIterator<Item = FindingKind>) -> Self {
        self.kinds = kinds.into_iter().collect();
        self
    }

    /// The request repairing at `severity` and above.
    #[must_use]
    pub const fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = Some(severity);
        self
    }

    /// The request repairing the findings citing the rule `rule` alone.
    #[must_use]
    pub fn with_rule(mut self, rule: impl Into<String>) -> Self {
        self.rule = Some(rule.into());
        self
    }

    /// The request targeting a batch of `limit` selected findings.
    #[must_use]
    pub const fn with_limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// The request continuing from `after`.
    #[must_use]
    pub fn with_after(mut self, after: Cursor) -> Self {
        self.after = Some(after);
        self
    }

    /// The request writing fixes at `threshold` and stronger.
    #[must_use]
    pub const fn with_threshold(mut self, threshold: Confidence) -> Self {
        self.threshold = Some(threshold);
        self
    }

    /// The threshold the request is read at: the one it names, or
    /// [`Confidence::DEFAULT_THRESHOLD`].
    pub fn threshold(&self) -> Confidence {
        self.threshold.unwrap_or(Confidence::DEFAULT_THRESHOLD)
    }
}
