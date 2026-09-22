//! `validate`: the findings standing over a vault.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::address::VaultAddress;
use crate::cursor::{Cursor, Page};
use crate::finding::{FindingKind, Severity};
use crate::finding_row::FindingRow;
use crate::predicate::Predicate;

/// How many findings of one kind stand.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct KindTally {
    /// The kind the findings are filed under.
    pub kind: FindingKind,
    /// How urgently that kind is reported.
    pub severity: Severity,
    /// How many of them stand.
    pub count: u64,
}

impl KindTally {
    /// The `count` of findings of `kind`, reported at `severity`.
    pub const fn new(kind: FindingKind, severity: Severity, count: u64) -> Self {
        KindTally {
            kind,
            severity,
            count,
        }
    }
}

/// What `validate` answers with.
///
/// On the wire a report is an object tagged `shape`: the findings themselves,
/// or the tally of them one kind at a time.
///
/// `PartialEq` alone: a page carries the cursor it continues at, and a cursor
/// carries a relevance score.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ValidateReport {
    /// The findings, one row each.
    #[non_exhaustive]
    Findings {
        /// The page of finding rows.
        page: Page<FindingRow>,
    },
    /// How many findings stand, one tally per kind.
    #[non_exhaustive]
    Summary {
        /// The tallies, one per kind that has findings standing.
        by_kind: Vec<KindTally>,
    },
}

impl ValidateReport {
    /// The findings themselves.
    pub const fn findings(page: Page<FindingRow>) -> Self {
        ValidateReport::Findings { page }
    }

    /// The tally of them, `by_kind`.
    pub fn summary(by_kind: impl IntoIterator<Item = KindTally>) -> Self {
        ValidateReport::Summary {
            by_kind: by_kind.into_iter().collect(),
        }
    }
}

/// What a `validate` request carries.
///
/// **`validate` reports what stands; it runs no rule.** Findings are derived
/// where documents are derived and read back here, so a request neither
/// re-judges a vault nor waits on one being re-judged.
///
/// **`validate` emits no plan.** What to do about a finding is a repair's
/// question, and this verb answers only that the finding stands.
///
/// **A resolution predicate is not applicable.** `resolves` answers which
/// documents a target names, which is a `find`; a `validate` carrying one is
/// answered with the findings it earned and reports the part as
/// [`Unsatisfied::ResolvesNotApplicable`](crate::Unsatisfied::ResolvesNotApplicable)
/// rather than refusing.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ValidateParams {
    /// The vault to answer from.
    pub vault: VaultAddress,
    /// The conjunction a finding's document must satisfy. Empty reports every
    /// finding in the vault.
    pub predicates: Vec<Predicate>,
    /// The kinds to report. Empty reports every kind.
    pub kinds: Vec<FindingKind>,
    /// The severity to report at or above. `null` reports every severity.
    pub severity: Option<Severity>,
    /// Whether to answer the tally instead of the findings. `false` answers
    /// the findings.
    pub summary: bool,
    /// How many finding rows at most. `null` leaves the ceiling to the host,
    /// and a summary is not paged.
    pub limit: Option<u32>,
    /// Where to continue from. `null` starts at the first finding.
    pub after: Option<Cursor>,
}

impl ValidateParams {
    /// A `validate` over `vault`: every finding of every kind, as rows.
    pub const fn new(vault: VaultAddress) -> Self {
        ValidateParams {
            vault,
            predicates: Vec::new(),
            kinds: Vec::new(),
            severity: None,
            summary: false,
            limit: None,
            after: None,
        }
    }

    /// The request filtered by `predicates`.
    #[must_use]
    pub fn with_predicates(mut self, predicates: impl IntoIterator<Item = Predicate>) -> Self {
        self.predicates = predicates.into_iter().collect();
        self
    }

    /// The request reporting `kinds` alone.
    #[must_use]
    pub fn with_kinds(mut self, kinds: impl IntoIterator<Item = FindingKind>) -> Self {
        self.kinds = kinds.into_iter().collect();
        self
    }

    /// The request reporting at `severity` and above.
    #[must_use]
    pub const fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = Some(severity);
        self
    }

    /// The request answering the tally rather than the findings.
    #[must_use]
    pub const fn summarized(mut self) -> Self {
        self.summary = true;
        self
    }

    /// The request bounded at `limit` finding rows.
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
}
