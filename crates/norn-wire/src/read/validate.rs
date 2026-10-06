//! `validate`: the findings standing over a vault.
//!
//! **`PartialEq` alone on [`ValidateReport`].** A page carries the cursor it
//! continues at, and a cursor carries a relevance score.
//!
//! **`validate` reports what stands; it runs no rule.** Findings are derived
//! where documents are derived and read back here, so a request neither
//! re-judges a vault nor waits on one being re-judged.
//!
//! **`validate` emits no plan.** What to do about a finding is a repair's
//! question, and this verb answers only that the finding stands.
//!
//! **A finding cites its rules by set, and a page resolves each set once.**
//! A finding judged against the schema rules names the set of rules it cites
//! by one number ([`FindingRow::rule_set`]), and a page of findings carries
//! each set its rows cite once, as the names of its rules
//! ([`RuleSet`]) — exactly those sets, and none where no row cites one. So a
//! row's bytes stay the same however many rules it cites, and a page's grow
//! with the distinct sets it holds rather than with its rows.
//!
//! **A request may select the findings citing one rule.** `rule` names a rule
//! the pinned schema declares, and the page or tally answers only the
//! findings whose rule set holds it, composed with every other part of the
//! request. A rule the schema does not declare is refused by name rather than
//! answered with an empty page, because an empty page would say the rule
//! holds everywhere.
//!
//! **A resolution predicate is not applicable.** `resolves` answers which
//! documents a target names, which is a `find`; a `validate` carrying one is
//! answered with the findings it earned and reports the part as
//! [`Unsatisfied::ResolvesNotApplicable`](crate::Unsatisfied::ResolvesNotApplicable)
//! rather than refusing.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::address::VaultAddress;
use crate::cursor::{Cursor, Page};
use crate::finding::{FindingKind, Severity};
use crate::finding_row::FindingRow;
use crate::predicate::Predicate;

/// How many findings of one kind stand at one severity.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct KindTally {
    /// The kind the findings are filed under.
    pub kind: FindingKind,
    /// The severity those findings are reported at.
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

/// One set of schema rules a page's findings cite: its identity, which a
/// finding row cites it by, and the names of its rules.
///
/// On the wire a set is a plain object: `{"id":3,"rules":["open-tasks","tasks"]}`.
/// The names are in byte order, each once.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct RuleSet {
    /// The identity a finding row cites the set by.
    pub id: u64,
    /// The names of the rules in the set, in byte order, each once.
    pub rules: Vec<String>,
}

impl RuleSet {
    /// The set `id` of `rules`, named in byte order, each once, whatever order
    /// they are handed in.
    pub fn new(id: u64, rules: impl IntoIterator<Item = String>) -> Self {
        let mut rules: Vec<String> = rules.into_iter().collect();
        rules.sort_unstable();
        rules.dedup();
        RuleSet { id, rules }
    }
}

/// What `validate` answers with.
///
/// On the wire a report is an object tagged `shape`: the findings themselves,
/// or the tally of them one kind and severity at a time.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
#[non_exhaustive]
#[allow(clippy::large_enum_variant)] // One per answer, moved once: a page is the common answer, and boxing it would allocate for the rarer tally's sake.
pub enum ValidateReport {
    /// The findings, one row each.
    #[non_exhaustive]
    Findings {
        /// The page of finding rows.
        page: Page<FindingRow>,
        /// Every rule set the page's rows cite, each once, in the order of its
        /// identity; empty where no row cites one.
        rule_sets: Vec<RuleSet>,
    },
    /// How many findings stand, one tally per kind and severity.
    #[non_exhaustive]
    Summary {
        /// The tallies, one per kind and severity some finding stands under.
        by_kind: Vec<KindTally>,
    },
}

impl ValidateReport {
    /// The findings themselves, with the rule sets their rows cite.
    pub fn findings(page: Page<FindingRow>, rule_sets: impl IntoIterator<Item = RuleSet>) -> Self {
        ValidateReport::Findings {
            page,
            rule_sets: rule_sets.into_iter().collect(),
        }
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
/// A validate answers the findings standing over a vault, as rows or as one
/// tally per kind and severity. A `resolves` predicate is answered with the
/// findings the rest of the request earned and reported back as the
/// unsatisfied part `resolves_not_applicable`. A `rule` narrows either to the
/// findings citing that rule.
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
    /// The one schema rule whose findings to report: those citing it. `null`
    /// reports findings whatever rules they cite, and those citing none. A
    /// rule the pinned schema does not declare is refused.
    pub rule: Option<String>,
    /// Whether to answer the tally instead of the findings. `false` answers
    /// the findings.
    pub summary: bool,
    /// How many finding rows a page holds at most. `null` leaves the ceiling
    /// to the host. A summary is not paged, and ignores it.
    pub limit: Option<u32>,
    /// Where a page continues from. `null` starts at the first finding. A
    /// summary is not paged, and a request for one carrying a cursor is
    /// refused.
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
            rule: None,
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

    /// The request reporting the findings citing the rule `rule` alone.
    #[must_use]
    pub fn with_rule(mut self, rule: impl Into<String>) -> Self {
        self.rule = Some(rule.into());
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
