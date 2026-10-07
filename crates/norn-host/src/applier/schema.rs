//! The schema check on a composed result: a plan refuses exactly the
//! violations it introduces, unless it is forced (ADR 0037).
//!
//! **A force bypasses this check and nothing else**, and it is loud: a forced
//! plan's violations are listed, in the shape a refusal carries them in, on
//! the forecast of its preview and on its applied report. A forced plan whose
//! results are all valid lists nothing.
//!
//! **The judge is the derivation's own.** What a finding over a document would
//! be filed under is what [`plan_document`] concludes from its bytes under the
//! pinned declaration, so the check runs that one derivation on the composed
//! bytes and on the bytes the target held before, and compares the document
//! findings each concludes. No second reading of the schema is written for
//! the applier, so `new` without a required field, a `set` outside a
//! `one_of` and a move to a disallowed path each refuse by the finding the
//! derivation would file. Every schema finding gates whatever its severity,
//! the field declarations' type and shape mismatches included. A link's
//! health is not a schema violation: the store judges it with the changeset,
//! and a link a plan breaks surfaces as a finding.
//!
//! **A violation's identity** is its kind, the field it stands on, the
//! offending value it names and the combined constraint it breaches:
//!
//! - a schema rule's or a field declaration's finding is the identity the
//!   rule judge mints for it ([`FindingIdentity`]): an element by its
//!   equality key, the whole value a forbidden field, a shape mismatch or a
//!   conflict names by its spelling, and its combined constraint by value,
//!   never by the rules stating it;
//! - an undeclared tag is its kind and the tag under the tag fold, wherever
//!   the document writes it, in frontmatter or body;
//! - every other kind is about the whole document — its path, its bytes, its
//!   frontmatter block as a whole — and is its kind alone.
//!
//! **A violation stood before** when the same identity is concluded from the
//! bytes the target was composed from — the target's own before-state, or,
//! where it was absent, a document the plan takes away, which is where a moved
//! document's content came from. A standing violation whose identity is
//! unchanged does not refuse, even on a field the plan writes or for a tag
//! the plan writes a different number of times: a repair rewriting one
//! element of a list leaves every other element's violation as it stood.

use std::collections::BTreeSet;
use std::path::Path;

use norn_config::schema::FindingIdentity;
use norn_store::HeldBlock;
use norn_wire::{CaseFold, DocumentPath, FindingKind, RefusedCheck, RuleSet, SchemaViolation};

use crate::derivation::{Cause, Declared, PlannedFinding, plan_document};
use crate::evidence::count_rule_work;

/// What the derivation concludes about one document's bytes: each violation,
/// with what tells it from another and what the wire says of it.
pub(super) struct Judged {
    violations: Vec<Violation>,
}

/// One violation a judgment concluded.
struct Violation {
    identity: Identity,
    kind: FindingKind,
    target: Option<String>,
    message: String,
    /// The offending value as the store keeps a field value, where the
    /// violation names one.
    value: Option<String>,
    /// The rules the violation cites, by name; empty where it cites none.
    rules: BTreeSet<String>,
}

/// What tells one violation from another. See the [module](self).
#[derive(Debug, PartialEq)]
enum Identity {
    /// A schema rule's or a field declaration's finding.
    Rule(FindingIdentity),
    /// An undeclared tag, under the tag fold.
    Tag(String),
    /// A violation about the whole document.
    Document(FindingKind),
}

impl Violation {
    /// The violation a planned finding states.
    fn of(finding: PlannedFinding) -> Self {
        let kind = finding.cause.kind();
        let identity = match (&finding.identity, finding.cause, &finding.target) {
            (Some(identity), _, _) => Identity::Rule(identity.clone()),
            (None, Cause::TagBreach(_), Some(tag)) => {
                Identity::Tag(norn_wire::fold_tag(tag).to_string())
            }
            _ => Identity::Document(kind),
        };
        Violation {
            identity,
            kind,
            message: finding.cause.message(&finding.subject),
            target: finding.target,
            value: finding.value,
            rules: finding.rules,
        }
    }
}

/// Judge `bytes` as the document at `path` under `declared`, its rules' path
/// globs comparing letters as `case` says. What the rules' judgment paid is
/// tallied as the derivation's is.
pub(super) fn judge(
    path: &DocumentPath,
    bytes: &[u8],
    declared: &Declared,
    case: CaseFold,
) -> Judged {
    let hash = norn_fs::ContentHash::of(bytes).to_string();
    let plan = plan_document(
        Path::new(path.as_str()),
        path.as_str(),
        bytes,
        hash,
        None,
        declared,
        case,
    );
    count_rule_work(plan.rule_work);
    Judged {
        violations: plan.findings.into_iter().map(Violation::of).collect(),
    }
}

/// Judge the frontmatter block `block` as the document at `path`'s, by the
/// schema rules alone ([`judge_block`](crate::derivation::judge_block)): what
/// a document a move carries byte for byte is judged by at the place it
/// leaves and the place it lands. What the judgment paid is tallied as the
/// derivation's is.
pub(super) fn judge_block(
    path: &DocumentPath,
    block: &HeldBlock,
    declared: &Declared,
    case: CaseFold,
) -> Judged {
    let (findings, work) = crate::derivation::judge_block(&path.into(), block, declared, case);
    count_rule_work(work);
    Judged {
        violations: findings.into_iter().map(Violation::of).collect(),
    }
}

/// The violations `after`, the composed result at `path`, introduces against
/// what stood in `before`, each document it was composed from: each refuses
/// an unforced plan, and a forced plan lets each through and lists it, citing
/// its rules through `citations`.
pub(super) fn introduced(
    path: &DocumentPath,
    after: Judged,
    before: &[Judged],
    citations: &mut Citations,
) -> Vec<SchemaViolation> {
    after
        .violations
        .into_iter()
        .filter(|violation| {
            !before.iter().any(|judged| {
                judged
                    .violations
                    .iter()
                    .any(|stood| stood.identity == violation.identity)
            })
        })
        .map(|violation| {
            let mut wire = SchemaViolation::new(
                path.clone(),
                violation.kind,
                violation.target,
                violation.message,
            );
            if let Some(value) = violation.value {
                wire = wire.with_value(norn_store::value_head(&value));
            }
            match citations.cite(violation.rules) {
                Some(rule_set) => wire.citing(rule_set),
                None => wire,
            }
        })
        .collect()
}

/// The rule sets one response's violations cite, each numbered once, from 1,
/// in the order first cited.
///
/// **One numbering per response.** An apply or a preview judges its plan, and
/// a refusal its fresh plan, through the one numbering, so an identity names
/// one set wherever it stands in the answer: in a refusal's checks and in its
/// forecast's forced violations alike. Each list of violations is answered
/// beside exactly the sets it cites ([`Citations::cited_by`]), as a page of
/// finding rows is.
#[derive(Debug, Default)]
pub(crate) struct Citations {
    sets: Vec<BTreeSet<String>>,
}

impl Citations {
    /// The identity of the set `rules` in this response, numbering it where
    /// it is new; `None` where it names no rule, since a violation citing no
    /// rule cites no set.
    fn cite(&mut self, rules: BTreeSet<String>) -> Option<u64> {
        if rules.is_empty() {
            return None;
        }
        let position = match self.sets.iter().position(|set| *set == rules) {
            Some(position) => position,
            None => {
                self.sets.push(rules);
                self.sets.len() - 1
            }
        };
        Some(position as u64 + 1)
    }

    /// Every set `violations` cite, each once, in the order of its identity.
    pub(crate) fn cited_by<'a>(
        &self,
        violations: impl IntoIterator<Item = &'a SchemaViolation>,
    ) -> Vec<RuleSet> {
        let cited: BTreeSet<u64> = violations
            .into_iter()
            .filter_map(|violation| violation.rule_set)
            .collect();
        cited
            .into_iter()
            .filter_map(|id| {
                let rules = self.sets.get(usize::try_from(id).ok()?.checked_sub(1)?)?;
                RuleSet::new(id, rules.iter().cloned()).ok()
            })
            .collect()
    }

    /// Every set the schema violations among `checks` cite, each once, in the
    /// order of its identity.
    pub(crate) fn cited_by_checks(&self, checks: &[RefusedCheck]) -> Vec<RuleSet> {
        self.cited_by(checks.iter().filter_map(|check| match check {
            RefusedCheck::SchemaViolation { violation, .. } => Some(violation),
            _ => None,
        }))
    }
}
