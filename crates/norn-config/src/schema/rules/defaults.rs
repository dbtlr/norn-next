//! The defaults fixpoint ([`VaultSchema::fill_rule_defaults`]): the rule
//! defaults a created document takes for the required fields its caller and
//! its creation rule left missing.

use std::collections::BTreeMap;
use std::fmt;

use norn_wire::{AuthoredValue, Binding, Captures, CaseFold, ValueMap};

use super::super::VaultSchema;
use super::super::template::LocalTimestamp;
use super::{Rule, RuleDefault, is_missing, named, value_in};

/// One value proposed for a field, and the rules proposing it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefaultCandidate {
    /// The value, filled.
    pub value: AuthoredValue,
    /// Every rule proposing it, in name order.
    pub rules: Vec<String>,
}

/// One field whose defaults disagree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefaultsConflict {
    /// The field.
    pub field: String,
    /// Every value proposed for it, in the order first proposed.
    pub candidates: Vec<DefaultCandidate>,
}

/// Why rule defaults do not settle on one frontmatter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuleDefaultsRefusal {
    /// Rules matching the created document default fields differently.
    Conflict {
        /// Each conflicting field, in key order.
        fields: Vec<DefaultsConflict>,
    },
    /// A default reads a capture its rule's `match.path` binds several ways
    /// in the created path.
    AmbiguousCapture {
        /// The rule whose default reads the capture.
        rule: String,
        /// The field the default is for.
        field: String,
        /// Two of the bindings.
        bindings: Box<[Captures; 2]>,
    },
}

impl fmt::Display for RuleDefaultsRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuleDefaultsRefusal::Conflict { fields } => {
                formatter.write_str("rule defaults disagree: ")?;
                for (at, conflict) in fields.iter().enumerate() {
                    if at > 0 {
                        formatter.write_str("; ")?;
                    }
                    write!(formatter, "`{}` is defaulted to ", conflict.field)?;
                    for (at, candidate) in conflict.candidates.iter().enumerate() {
                        if at > 0 {
                            formatter.write_str(" and ")?;
                        }
                        write!(
                            formatter,
                            "{:?} by {}",
                            candidate.value,
                            named(&candidate.rules)
                        )?;
                    }
                }
                Ok(())
            }
            RuleDefaultsRefusal::AmbiguousCapture {
                rule,
                field,
                bindings,
            } => {
                let spelled = |captures: &Captures| {
                    captures
                        .iter()
                        .map(|(name, segment)| format!("{name}={segment}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                write!(
                    formatter,
                    "the rule `{rule}` defaults `{field}` from a path capture its `match.path` binds several ways: {{{}}} and {{{}}}",
                    spelled(&bindings[0]),
                    spelled(&bindings[1])
                )
            }
        }
    }
}

impl std::error::Error for RuleDefaultsRefusal {}

impl VaultSchema {
    /// The rule defaults a document created at `path` with `frontmatter` —
    /// the caller's values, then its creation rule's defaults — takes, in
    /// the order filled; or why they do not settle ([ADR 0035]). See
    /// [`VaultSchema::selects`] for `case`.
    ///
    /// Each **round** matches rules against the frontmatter composed so far
    /// and fills every required field still missing — absent or null — whose
    /// selecting rules' defaults all agree as filled values. A default filled
    /// may bring in a rule, which may default another field, so rounds repeat
    /// until one fills nothing. They end: a round only fills missing fields
    /// and selectors have no negation or presence test, so the rules matched
    /// only grow, and every round but the last fills a field some default
    /// names, which bounds the rounds at the defaulted fields plus one.
    ///
    /// **Fills are provisional.** A later round can bring in a rule
    /// defaulting a field an earlier round filled, so once settled every
    /// filled field is judged again against every rule matching the final
    /// frontmatter and path, and any default for it differing from the value
    /// filled is a conflict. Disagreement in a round is one too. A conflict
    /// names each conflicting field and every candidate value with the rules
    /// proposing it. The caller's values and its creation rule's are never
    /// judged again and never overwritten; a null is no value, so a field
    /// either writes as null is missing and filled. A required field nothing
    /// defaults stays missing, which is the write gate's to refuse, not
    /// this.
    ///
    /// **One clock reading, `at`, fills every default**, and each
    /// `{{path.<name>}}` reads what its own rule's `match.path` bound in
    /// `path`. A default read from a capture the match binds several ways is
    /// refused, naming two of the bindings.
    ///
    /// **A dormant carrier.** Its consumer is `new` and inbox capture, which
    /// take the fixpoint with NORN-359; the current call graph reaches it
    /// from no write, because no write fills rule defaults yet.
    ///
    /// [ADR 0035]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0035-a-schema-rule-selects-documents-by-their-frontmatter.md
    pub fn fill_rule_defaults(
        &self,
        frontmatter: &ValueMap,
        path: &str,
        at: LocalTimestamp,
        case: CaseFold,
    ) -> Result<Vec<(String, AuthoredValue)>, RuleDefaultsRefusal> {
        let mut composed: Vec<(String, AuthoredValue)> = frontmatter.entries().to_vec();
        let mut filled: Vec<(String, AuthoredValue)> = Vec::new();
        let mut bindings: BTreeMap<&str, Captures> = BTreeMap::new();
        let defaulted = self
            .rules
            .values()
            .flat_map(|rule| rule.required.iter())
            .filter(|(_, default)| default.is_some())
            .map(|(field, _)| field.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let mut rounds = 0;
        loop {
            rounds += 1;
            debug_assert!(rounds <= defaulted + 1, "the rounds outran their bound");
            let mut proposed: BTreeMap<&str, Vec<DefaultCandidate>> = BTreeMap::new();
            for rule in self.rules.values() {
                if !self.selects_in(rule, path, &composed, case) {
                    continue;
                }
                for (field, default) in &rule.required {
                    let Some(default) = default else {
                        continue;
                    };
                    if !is_missing(value_in(&composed, field)) {
                        continue;
                    }
                    let value = fill(rule, field, default, path, at, case, &mut bindings)?;
                    propose(proposed.entry(field).or_default(), value, &rule.name);
                }
            }
            if proposed.is_empty() {
                break;
            }
            let conflicts = disagreeing(&proposed);
            if !conflicts.is_empty() {
                return Err(RuleDefaultsRefusal::Conflict { fields: conflicts });
            }
            for (field, mut candidates) in proposed {
                let value = candidates.remove(0).value;
                match composed.iter_mut().find(|(held, _)| held == field) {
                    Some((_, held)) => *held = value.clone(),
                    None => composed.push((field.to_string(), value.clone())),
                }
                filled.push((field.to_string(), value));
            }
        }

        // The settled frontmatter: every filled field judged again against
        // every rule matching it.
        let mut judged: BTreeMap<&str, Vec<DefaultCandidate>> = BTreeMap::new();
        for rule in self.rules.values() {
            if !self.selects_in(rule, path, &composed, case) {
                continue;
            }
            for (field, value) in &filled {
                let Some(Some(default)) = rule.required.get(field) else {
                    continue;
                };
                let candidates = judged.entry(field).or_insert_with(|| {
                    vec![DefaultCandidate {
                        value: value.clone(),
                        rules: Vec::new(),
                    }]
                });
                let proposal = fill(rule, field, default, path, at, case, &mut bindings)?;
                propose(candidates, proposal, &rule.name);
            }
        }
        let conflicts = disagreeing(&judged);
        if !conflicts.is_empty() {
            return Err(RuleDefaultsRefusal::Conflict { fields: conflicts });
        }
        Ok(filled)
    }
}

/// `default` filled at `at`, its captures read from what `rule`'s match binds
/// in `path`, each rule's binding found once.
fn fill<'s>(
    rule: &'s Rule,
    field: &str,
    default: &RuleDefault,
    path: &str,
    at: LocalTimestamp,
    case: CaseFold,
    bindings: &mut BTreeMap<&'s str, Captures>,
) -> Result<AuthoredValue, RuleDefaultsRefusal> {
    let captures = if default.reads_captures() {
        match bindings.get(rule.name.as_str()) {
            Some(captures) => captures.clone(),
            None => {
                let captures = match rule
                    .selector
                    .path
                    .as_ref()
                    .map(|glob| glob.bind(path, case))
                {
                    Some(Binding::Unique(captures)) => captures,
                    Some(Binding::Several(several)) => {
                        return Err(RuleDefaultsRefusal::AmbiguousCapture {
                            rule: rule.name.clone(),
                            field: field.to_string(),
                            bindings: several,
                        });
                    }
                    // A rule that selects the path matches it; a default
                    // reading a capture is refused at read where the rule
                    // has no `match.path`.
                    Some(Binding::Unmatched) | None => Captures::default(),
                };
                bindings.insert(&rule.name, captures.clone());
                captures
            }
        }
    } else {
        Captures::default()
    };
    Ok(default.fill(at, captures).expect(
        "a rule default's tokens are the clock's and its own rule's captures, judged at read",
    ))
}

/// Adds `value`, proposed by `rule`, to `candidates`.
fn propose(candidates: &mut Vec<DefaultCandidate>, value: AuthoredValue, rule: &str) {
    match candidates
        .iter_mut()
        .find(|candidate| candidate.value == value)
    {
        Some(candidate) => {
            candidate.rules.push(rule.to_string());
            candidate.rules.sort();
        }
        None => candidates.push(DefaultCandidate {
            value,
            rules: vec![rule.to_string()],
        }),
    }
}

/// Every field proposed more than one value, with its candidates.
fn disagreeing(proposed: &BTreeMap<&str, Vec<DefaultCandidate>>) -> Vec<DefaultsConflict> {
    proposed
        .iter()
        .filter(|(_, candidates)| candidates.len() > 1)
        .map(|(field, candidates)| DefaultsConflict {
            field: field.to_string(),
            candidates: candidates.clone(),
        })
        .collect()
}
