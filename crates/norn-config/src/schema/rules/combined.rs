//! The combined constraint: what every rule selecting one document requires
//! of it at once ([ADR 0035]).
//!
//! Rules are additive and none overrides another. Per field: a closed set is
//! the intersection of every contributing rule's, a field is required where
//! any rule requires it and forbidden where any forbids it, and its length
//! limit is the smallest stated. A path is allowed only where every
//! contributing rule's `allowed_paths` admits it. The severity is the highest
//! of the contributing rules'.
//!
//! [ADR 0035]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0035-a-schema-rule-selects-documents-by-their-frontmatter.md

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use norn_wire::{CaseFold, Severity};

use super::super::{TypedValue, VaultSchema};
use super::{Rule, RuleWork, higher, named, placement};

/// What a set of rules requires of a document they all select.
#[derive(Clone, Debug)]
pub struct CombinedConstraint<'s> {
    rules: Vec<&'s Rule>,
    fields: BTreeMap<&'s str, FieldConstraint<'s>>,
}

/// What a set of rules requires of one field.
#[derive(Clone, Debug, Default)]
pub struct FieldConstraint<'s> {
    required_by: Vec<&'s Rule>,
    forbidden_by: Vec<&'s Rule>,
    one_of: Option<OneOfIntersection<'s>>,
    max_length: Option<(u64, Vec<&'s Rule>)>,
}

/// The values every rule closing a field over a set admits.
#[derive(Clone, Debug)]
pub struct OneOfIntersection<'s> {
    /// Each member's equality key, and its spelling in the first rule, by
    /// name, that writes it.
    members: BTreeMap<TypedValue, &'s str>,
    rules: Vec<&'s Rule>,
}

impl VaultSchema {
    /// What `rules` require together of a document they all select.
    ///
    /// Read at schema read by the statically unavoidable conflict check, and
    /// by the rule findings (NORN-358) and the write gate (NORN-359), which
    /// are not built.
    pub fn combined<'s>(&'s self, rules: &[&'s Rule]) -> CombinedConstraint<'s> {
        let mut rules = rules.to_vec();
        rules.sort_by(|left, right| left.name.cmp(&right.name));
        rules.dedup_by(|left, right| left.name == right.name);
        let mut fields: BTreeMap<&'s str, FieldConstraint<'s>> = BTreeMap::new();
        for rule in &rules {
            for field in rule.required.keys() {
                fields.entry(field).or_default().required_by.push(rule);
            }
            for field in rule.forbidden.keys() {
                fields.entry(field).or_default().forbidden_by.push(rule);
            }
            for (field, set) in &rule.one_of {
                let constraint = fields.entry(field).or_default();
                let intersection = constraint.one_of.get_or_insert_with(|| OneOfIntersection {
                    members: set
                        .members
                        .iter()
                        .map(|(key, written)| (key.clone(), written.as_str()))
                        .collect(),
                    rules: Vec::new(),
                });
                intersection
                    .members
                    .retain(|key, _| set.members.contains_key(key));
                intersection.rules.push(rule);
            }
            for (field, limit) in &rule.max_length {
                let constraint = fields.entry(field).or_default();
                let (smallest, limiting) = constraint
                    .max_length
                    .get_or_insert_with(|| (*limit, Vec::new()));
                *smallest = (*smallest).min(*limit);
                limiting.push(rule);
            }
        }
        CombinedConstraint { rules, fields }
    }
}

impl<'s> CombinedConstraint<'s> {
    /// The contributing rules, in name order, each once.
    pub fn rules(&self) -> &[&'s Rule] {
        &self.rules
    }

    /// The highest severity of the contributing rules; `warning` where there
    /// is none.
    pub fn severity(&self) -> Severity {
        self.rules.iter().fold(Severity::Warning, |severity, rule| {
            higher(severity, rule.severity)
        })
    }

    /// Each field some contributing rule constrains, in key order.
    pub fn fields(&self) -> impl Iterator<Item = (&'s str, &FieldConstraint<'s>)> {
        self.fields
            .iter()
            .map(|(field, constraint)| (*field, constraint))
    }

    /// What the contributing rules require of `field`, where any constrains
    /// it.
    pub fn field(&self, field: &str) -> Option<&FieldConstraint<'s>> {
        self.fields.get(field)
    }

    /// The contributing rules that state allowed paths, in name order.
    pub fn placement_rules(&self) -> impl Iterator<Item = &'s Rule> + '_ {
        self.rules
            .iter()
            .copied()
            .filter(|rule| rule.allowed_paths.is_some())
    }

    /// Whether every contributing rule's allowed paths admit `path`, under
    /// `case` (see [`VaultSchema::selects`]).
    pub fn admits_path(&self, path: &str, case: CaseFold) -> bool {
        self.admits_path_counted(path, case, &mut RuleWork::default())
    }

    /// [`CombinedConstraint::admits_path`], tallying in `work` the characters
    /// of each glob matched, up to the first rule admitting nothing.
    pub(super) fn admits_path_counted(
        &self,
        path: &str,
        case: CaseFold,
        work: &mut RuleWork,
    ) -> bool {
        self.placement_rules().all(|rule| {
            rule.allowed_paths
                .as_ref()
                .is_some_and(|allowed| allowed.admits(path, case, work))
        })
    }

    /// Every way the combined constraint is empty — no document could meet
    /// it — whatever the document holds: a field required and forbidden, a
    /// closed-set intersection with no member, and allowed paths sharing no
    /// document path under `case`. Each names every contributing rule in name order.
    ///
    /// An empty intersection is reported whether or not the field is
    /// required, carrying which: schema read refuses one on a required field
    /// alone, and a finding reports one on an unrequired field only where the
    /// document holds a value.
    pub fn conflicts(&self, case: CaseFold) -> Vec<RulesConflict> {
        let mut conflicts = self.field_conflicts();
        let placed: Vec<&Rule> = self.placement_rules().collect();
        if placed.len() > 1 && !placement::share_a_path(&placed, case) {
            conflicts.push(RulesConflict::DisjointPlacement {
                rules: names(placed.iter()),
            });
        }
        conflicts
    }

    /// The conflicts [`CombinedConstraint::conflicts`] reports on fields, in
    /// key order: a set operation over the contributing rules' constraints,
    /// which walks no path. Rule judgment reads them per document, and walks
    /// placement only where a document's path is not admitted.
    pub(super) fn field_conflicts(&self) -> Vec<RulesConflict> {
        let mut conflicts = Vec::new();
        for (field, constraint) in &self.fields {
            if !constraint.required_by.is_empty() && !constraint.forbidden_by.is_empty() {
                conflicts.push(RulesConflict::RequiredAndForbidden {
                    field: field.to_string(),
                    rules: names(
                        constraint
                            .required_by
                            .iter()
                            .chain(&constraint.forbidden_by),
                    ),
                });
            }
            if let Some(intersection) = &constraint.one_of
                && intersection.members.is_empty()
            {
                conflicts.push(RulesConflict::EmptyOneOf {
                    field: field.to_string(),
                    required: !constraint.required_by.is_empty(),
                    rules: names(intersection.rules.iter().chain(&constraint.required_by)),
                });
            }
        }
        conflicts
    }
}

/// Each rule's name, once, in name order.
fn names<'a>(rules: impl IntoIterator<Item = &'a &'a Rule>) -> Vec<String> {
    rules
        .into_iter()
        .map(|rule| rule.name.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

impl<'s> FieldConstraint<'s> {
    /// Whether some contributing rule requires the field.
    pub fn is_required(&self) -> bool {
        !self.required_by.is_empty()
    }

    /// The rules requiring the field, in name order.
    pub fn required_by(&self) -> impl Iterator<Item = &'s Rule> + '_ {
        self.required_by.iter().copied()
    }

    /// Whether some contributing rule forbids the field.
    pub fn is_forbidden(&self) -> bool {
        !self.forbidden_by.is_empty()
    }

    /// The rules forbidding the field, in name order.
    pub fn forbidden_by(&self) -> impl Iterator<Item = &'s Rule> + '_ {
        self.forbidden_by.iter().copied()
    }

    /// The values every rule closing the field admits, where any closes it.
    pub fn one_of(&self) -> Option<&OneOfIntersection<'s>> {
        self.one_of.as_ref()
    }

    /// The smallest length limit a contributing rule states, where any does.
    pub fn max_length(&self) -> Option<u64> {
        self.max_length.as_ref().map(|(limit, _)| *limit)
    }

    /// The rules limiting the field's length, in name order.
    pub fn max_length_by(&self) -> impl Iterator<Item = &'s Rule> + '_ {
        self.max_length
            .iter()
            .flat_map(|(_, rules)| rules.iter().copied())
    }
}

impl<'s> OneOfIntersection<'s> {
    /// Whether no value is in every closed set.
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Each value in every closed set, spelled as the first rule by name
    /// writes it, in the order of their equality keys.
    pub fn members(&self) -> impl Iterator<Item = &'s str> + '_ {
        self.members.values().copied()
    }

    /// Whether the value whose equality key is `key` is in every closed set
    /// (see [`VaultSchema::equality_key`]).
    pub fn admits(&self, key: &TypedValue) -> bool {
        self.members.contains_key(key)
    }

    /// The rules closing the field, in name order.
    pub fn rules(&self) -> impl Iterator<Item = &'s Rule> + '_ {
        self.rules.iter().copied()
    }
}

/// A combined constraint no document can meet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RulesConflict {
    /// A field one rule requires and another, or the same, forbids.
    RequiredAndForbidden {
        /// The field.
        field: String,
        /// Every rule requiring or forbidding it, in name order.
        rules: Vec<String>,
    },
    /// Closed sets over one field that share no value.
    EmptyOneOf {
        /// The field.
        field: String,
        /// Whether some contributing rule requires the field.
        required: bool,
        /// Every rule closing or requiring it, in name order.
        rules: Vec<String>,
    },
    /// Allowed paths that share no document path.
    DisjointPlacement {
        /// Every rule stating allowed paths, in name order.
        rules: Vec<String>,
    },
}

impl RulesConflict {
    /// Every contributing rule, in name order.
    pub fn rules(&self) -> &[String] {
        match self {
            RulesConflict::RequiredAndForbidden { rules, .. }
            | RulesConflict::EmptyOneOf { rules, .. }
            | RulesConflict::DisjointPlacement { rules } => rules,
        }
    }
}

impl fmt::Display for RulesConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RulesConflict::RequiredAndForbidden { field, rules } => write!(
                formatter,
                "`{field}` is both required and forbidden by the rules {}",
                named(rules)
            ),
            RulesConflict::EmptyOneOf {
                field,
                required,
                rules,
            } => write!(
                formatter,
                "the `one_of` values of `{field}`{} share no value under the rules {}",
                if *required {
                    ", which is required,"
                } else {
                    ""
                },
                named(rules)
            ),
            RulesConflict::DisjointPlacement { rules } => write!(
                formatter,
                "the allowed paths of the rules {} share no document path",
                named(rules)
            ),
        }
    }
}
