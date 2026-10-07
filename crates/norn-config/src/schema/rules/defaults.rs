//! The defaults fixpoint ([`VaultSchema::fill_rule_defaults`]): the rule
//! defaults a created document takes for the required fields its caller and
//! its creation rule left out.

use std::collections::BTreeMap;
use std::fmt;

use norn_wire::{AuthoredValue, Binding, Captures, CaseFold, ValueMap};

use super::super::VaultSchema;
use super::super::template::{LocalTimestamp, NotALocalTimestamp};
use super::{Rule, RuleDefault, RuleWork, named, value_in};

/// One value proposed for a field, and the rules proposing it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefaultCandidate {
    value: AuthoredValue,
    rules: Vec<String>,
}

impl DefaultCandidate {
    /// The value, filled.
    pub fn value(&self) -> &AuthoredValue {
        &self.value
    }

    /// Every rule proposing it, in name order.
    pub fn rules(&self) -> &[String] {
        &self.rules
    }
}

/// One field whose defaults disagree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefaultsConflict {
    field: String,
    candidates: Vec<DefaultCandidate>,
}

impl DefaultsConflict {
    /// The field.
    pub fn field(&self) -> &str {
        &self.field
    }

    /// Every value proposed for it, in the order first proposed.
    pub fn candidates(&self) -> &[DefaultCandidate] {
        &self.candidates
    }
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
    /// A clock-reading default was proposed or had to be compared, and the
    /// clock gives no reading a default can fill.
    NoClockReading(NotALocalTimestamp),
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
            RuleDefaultsRefusal::NoClockReading(unread) => write!(
                formatter,
                "the clock cannot be read as a local time the rule defaults can fill: {unread}"
            ),
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
    /// **Only an absent field is filled.** A key `frontmatter` holds is its
    /// caller's — the creation's caller or its creation rule — null
    /// included: a caller omits a key to take its default and sends null to
    /// ask for no value, which the write gate then judges, a required field
    /// held null refusing as missing. A caller that wants a null field
    /// filled, as repair would, leaves the key out: a null and an absent key
    /// select alike, and judge alike except for `forbidden`, which a null
    /// breaches by the key's presence.
    ///
    /// Each **round** matches rules against the frontmatter composed so far
    /// and fills every required field still absent whose selecting rules'
    /// defaults all agree as filled values. **Two filled
    /// values agree only where they are one written value**: `1` and `1.0`
    /// are an integer and a float, which write different bytes into the
    /// document, so they disagree though a `number` field compares them
    /// equal. A default filled may bring in a rule, which may default another
    /// field, so rounds repeat until one fills nothing. They end: a round only
    /// fills missing fields and selectors have no negation or presence test,
    /// so the rules matched only grow, and every round but the last fills a
    /// field some default names, which bounds the rounds at the defaulted
    /// fields plus one.
    ///
    /// **Fills are provisional.** A later round can bring in a rule
    /// defaulting a field an earlier round filled, so once settled every
    /// filled field is judged again against every rule matching the final
    /// frontmatter and path, and any default for it differing from the value
    /// filled is a conflict. Disagreement in a round is one too, refused in
    /// that round rather than settled by a pick: a value picked from several
    /// could bring in rules whose own defaults then disagree, a conflict the
    /// document never had. A conflict names each conflicting field and every
    /// candidate value with the rules proposing it. The caller's values and
    /// its creation rule's are never judged again and never overwritten. A
    /// required field nothing defaults stays missing, which is the write
    /// gate's to refuse, not this.
    ///
    /// **One clock reading serves every default**: `clock` is read the first
    /// time a default reading `{{now}}`, `{{date}}` or `{{time}}` is proposed
    /// for a field, or is compared with a filled value at the settled
    /// re-check, and at most once. It is read before the field is known to
    /// conflict, because deciding whether a clock default agrees with another
    /// default needs the reading: `2026-10-07` can equal `{{date}}`. A
    /// creation where no clock-reading default is proposed or compared never
    /// reads the clock. A clock giving no reading refuses the defaults naming
    /// the clock — even where the field would otherwise have been a conflict.
    /// Each `{{path.<name>}}` reads
    /// what its own rule's `match.path` bound in `path`. A default read from
    /// a capture the match binds several ways is refused, naming two of the
    /// bindings.
    ///
    /// **Its work is tallied in `work`** ([`RuleWork`]), refused or not:
    /// every rule evaluated in each round and at the re-check, with the
    /// selector terms, bytes and glob characters doing so read, the rules
    /// each selected, the rounds, the fields filled and the path bindings
    /// taken. A round matches every rule once, so the rules evaluated are
    /// the rule count times one more than the rounds.
    ///
    /// **Its consumer is every creation**: `new` by a creation rule, inbox
    /// capture and `new` at a bare path, which `norn-host`'s planner fills
    /// from it, its refusals answered as structured unresolved reasons.
    ///
    /// [ADR 0035]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0035-a-schema-rule-selects-documents-by-their-frontmatter.md
    pub fn fill_rule_defaults(
        &self,
        frontmatter: &ValueMap,
        path: &str,
        clock: &mut dyn FnMut() -> Result<LocalTimestamp, NotALocalTimestamp>,
        case: CaseFold,
        work: &mut RuleWork,
    ) -> Result<Vec<(String, AuthoredValue)>, RuleDefaultsRefusal> {
        let mut at = Reading { clock, read: None };
        let at = &mut at;
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
            work.defaults_rounds += 1;
            debug_assert!(rounds <= defaulted + 1, "the rounds outran their bound");
            let mut proposed: BTreeMap<&str, Vec<DefaultCandidate>> = BTreeMap::new();
            for rule in self.rules.values() {
                if !self.selects_counted(rule, path, &composed, case, work) {
                    continue;
                }
                work.rules_selected += 1;
                for (field, default) in &rule.required {
                    let Some(default) = default else {
                        continue;
                    };
                    if value_in(&composed, field).is_some() {
                        continue;
                    }
                    let value = fill(rule, field, default, path, at, case, &mut bindings, work)?;
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
                composed.push((field.to_string(), value.clone()));
                filled.push((field.to_string(), value));
                work.defaults_filled += 1;
            }
        }

        // The settled frontmatter: every filled field judged again against
        // every rule matching it.
        let mut judged: BTreeMap<&str, Vec<DefaultCandidate>> = BTreeMap::new();
        for rule in self.rules.values() {
            if !self.selects_counted(rule, path, &composed, case, work) {
                continue;
            }
            work.rules_selected += 1;
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
                let proposal = fill(rule, field, default, path, at, case, &mut bindings, work)?;
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

/// The clock a fixpoint fills its defaults from, read the first time a
/// clock-reading default is proposed for a field or compared with a filled
/// value, and kept from then on.
struct Reading<'c> {
    clock: &'c mut dyn FnMut() -> Result<LocalTimestamp, NotALocalTimestamp>,
    read: Option<LocalTimestamp>,
}

impl Reading<'_> {
    /// The one reading, taken now where none is yet.
    fn taken(&mut self) -> Result<LocalTimestamp, RuleDefaultsRefusal> {
        match self.read {
            Some(at) => Ok(at),
            None => {
                let at = (self.clock)().map_err(RuleDefaultsRefusal::NoClockReading)?;
                self.read = Some(at);
                Ok(at)
            }
        }
    }
}

/// `default` filled from `at` where it reads the clock, its captures read
/// from what `rule`'s match binds in `path`, each rule's binding found once
/// and tallied in `work`.
#[allow(clippy::too_many_arguments)] // One fill's whole context: splitting it would only rename the arguments.
fn fill<'s>(
    rule: &'s Rule,
    field: &str,
    default: &RuleDefault,
    path: &str,
    at: &mut Reading<'_>,
    case: CaseFold,
    bindings: &mut BTreeMap<&'s str, Captures>,
    work: &mut RuleWork,
) -> Result<AuthoredValue, RuleDefaultsRefusal> {
    let at = if default.reads_clock() {
        Some(at.taken()?)
    } else {
        None
    };
    let captures = if default.reads_captures() {
        match bindings.get(rule.name.as_str()) {
            Some(captures) => captures.clone(),
            None => {
                work.captures_bound += 1;
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
