//! What schema read judges of its rules once every rule is read: each rule
//! against itself, the allowed-paths ceiling across rules that may select
//! one document together, and the conflicts rules that always select
//! together cannot avoid.
//!
//! # A rule against itself
//!
//! An untemplated default element, a `one_of` member and a synonym's target
//! are each judged as one element value: they read as the field's declared
//! type, fit the rule's own `max_length`, and — a default element and a
//! synonym's target — are among the rule's own `one_of` values. A default's
//! shape is judged against the field's declared shape whatever it holds,
//! since a template fills a string and a list stays a list. Members are
//! scalars whatever the field's shape: a collection's shape is judged on the
//! composed field alone.
//!
//! **A route is judged as a folder some document could stand directly
//! inside.** The route's text, with each token standing as a `*` — a capture
//! takes one segment and a clock token fills text holding no `/` — and a file
//! name `*.md` appended, is a glob; the route stands outside its rule's
//! allowed paths where that glob shares no document path with them. An
//! untemplated route is so judged exactly.
//!
//! **A templated route is refused only where no independent filling of its
//! tokens is admitted — a declared limit.** Each token stands as its own `*`,
//! so two tokens reading one capture are filled apart: the route
//! `{{path.p}}/{{path.p}}/` passes against `red/blue/*.md`, though no real
//! fill writes `red` and `blue` from one capture. The exact judgment is of a
//! filled route, which repair makes per document when it moves one (Layer
//! 5B, NORN-351); schema read refuses only what no document could escape.
//!
//! **A route is judged under [`CaseFold::Exact`].** It is the author's own
//! spelling against the author's own globs, so `Notes/` against `notes/**`
//! is a route that disagrees with itself, refused whether or not the root
//! folds case.
//!
//! # Across rules
//!
//! **The ceiling.** Two rules may select one document together unless both
//! select on a shared `match.frontmatter` key, declared [`Shape::Single`],
//! whose value sets share no member under the selectors' comparison: a
//! single-shaped key holds one value, so it equals a value of at most one of
//! them, and a list there selects neither. Every other pair — path-only,
//! selectorless, selecting on unrelated keys or on a key that may hold a list
//! — may. For each rule `r` stating allowed paths, its weight times the
//! weights of every other such rule that may select beside it bounds every
//! set of rules selecting one document with `r`; the schema refuses where
//! that bound passes [`PLACEMENT_CEILING`]. The judgment compares each pair
//! once per rule, so it costs the square of the rule count times the
//! selector keys compared, at schema read.
//!
//! **Statically unavoidable conflicts.** Rules select together on every
//! document any of them selects where one is selectorless, or where their
//! selectors are identical in normal form: any-of and `exclude.path` lists as
//! sets, capture names erased, a `match.path` of `**` read as absent. So each
//! class of identical selectors, with every selectorless rule beside it, is
//! one group, and so is the selectorless set alone. Within a group the
//! combined constraint's conflicts ([`CombinedConstraint::conflicts`]) are
//! refused, except an empty closed-set intersection on a field none of the
//! group requires, which a document holding no value meets.
//!
//! **A conflict between rules holds on every root.** A refusal at read must
//! hold whichever root the schema is pinned for, so a group's allowed paths
//! are judged under [`CaseFold::Ascii`]: a document path the wider fold
//! cannot admit no root admits. Two rules spelling one folder in two cases
//! are two authors' spellings, which a folding root reads as one, so neither
//! is refused for the other's.
//!
//! # What judging costs
//!
//! **Every walk schema read takes is bounded by [`PLACEMENT_CEILING`] before
//! it starts.** The ceiling is judged first, before any rule is judged
//! against itself, so no walk runs over a schema past it. A route's walk is
//! the route's glob against its rule's allowed paths, so its weight is the
//! route glob's weight times the rule's, and a route weighing past the
//! ceiling is refused, naming the rule, before its walk. A group's walk is
//! over rules that may each select beside every other, so it weighs no more
//! than the neighbourhood of any of them, which the ceiling holds. Each walk
//! visits at most a constant times its weight in states
//! ([`norn_wire::sets_share_a_document_path`]), so a schema read costs at most
//! one walk per route and one per group, each under that constant times
//! `2^18`, beside the ceiling's own comparisons: the square of the rule count
//! times the selector keys compared.

use std::collections::BTreeMap;

use norn_wire::{CaseFold, DOCUMENT_EXTENSION, Pattern, sets_share_a_document_path};

use super::super::creation::DefaultValue;
use super::super::template::Part;
use super::super::{Shape, VaultSchema, VaultSchemaError};
use super::combined::{CombinedConstraint, RulesConflict};
use super::placement::{self, PLACEMENT_CEILING};
use super::{ElementProblem, ForbiddenFix, NormalSelector, Route, Rule, RuleProblem, scalar_text};

/// Every judgment schema read makes of its rules once each is read.
///
/// The ceiling is judged first, so no walk runs over a schema past it; see
/// the [module](self) for what judging costs.
pub(in crate::schema) fn check_rules(schema: &VaultSchema) -> Result<(), VaultSchemaError> {
    check_ceiling(schema)?;
    for rule in schema.rules.values() {
        check_rule(schema, rule)?;
    }
    check_unavoidable(schema)
}

fn refusal(at: impl Into<String>, problem: RuleProblem) -> VaultSchemaError {
    VaultSchemaError::Rule {
        at: at.into(),
        problem,
    }
}

/// One rule against itself: its defaults, its closed sets, its route and its
/// renames.
fn check_rule(schema: &VaultSchema, rule: &Rule) -> Result<(), VaultSchemaError> {
    let section = format!("rules.{}", rule.name);
    for (field, default) in &rule.required {
        let Some(default) = default else {
            continue;
        };
        let at_path = format!("{section}.required.{field}.default");
        let is_list = matches!(default.value, DefaultValue::List(_));
        match schema.field(field).and_then(|field| field.shape()) {
            Some(declared @ Shape::Single) if is_list => {
                return Err(refusal(&at_path, RuleProblem::DefaultShape { declared }));
            }
            Some(declared @ Shape::List) if !is_list => {
                return Err(refusal(&at_path, RuleProblem::DefaultShape { declared }));
            }
            _ => {}
        }
        let elements: Vec<&DefaultValue> = match &default.value {
            DefaultValue::List(items) => items.iter().collect(),
            value => vec![value],
        };
        for element in elements {
            let Some(raw) = untemplated_text(element) else {
                continue;
            };
            if let Some(problem) = element_problem(schema, rule, field, &raw, true) {
                return Err(refusal(
                    &at_path,
                    RuleProblem::Default {
                        value: raw,
                        problem,
                    },
                ));
            }
        }
    }
    for (field, set) in &rule.one_of {
        let at_path = format!("{section}.one_of.{field}");
        for member in &set.values {
            if let Some(problem) = element_problem(schema, rule, field, member, false) {
                return Err(refusal(
                    format!("{at_path}.values"),
                    RuleProblem::Member {
                        value: member.clone(),
                        problem,
                    },
                ));
            }
        }
        for (_, target) in &set.synonyms {
            if let Some(problem) = element_problem(schema, rule, field, target, true) {
                return Err(refusal(
                    format!("{at_path}.synonyms"),
                    RuleProblem::SynonymTarget {
                        value: target.clone(),
                        problem,
                    },
                ));
            }
        }
    }
    if let Some(allowed) = &rule.allowed_paths
        && let Some(route) = &allowed.route
    {
        check_route(rule, route, &allowed.paths)
            .map_err(|problem| refusal(format!("{section}.allowed_paths.route"), problem))?;
    }
    let mut renamed: BTreeMap<&str, &str> = BTreeMap::new();
    for (field, fix) in &rule.forbidden {
        let ForbiddenFix::RenameTo(target) = fix else {
            continue;
        };
        let at_path = format!("{section}.forbidden.{field}.rename_to");
        if rule.forbidden.contains_key(target) {
            return Err(refusal(
                at_path,
                RuleProblem::RenameOntoForbidden {
                    target: target.clone(),
                },
            ));
        }
        if let Some(other) = renamed.insert(target, field) {
            return Err(refusal(
                at_path,
                RuleProblem::RenameTwice {
                    target: target.clone(),
                    other: other.to_string(),
                },
            ));
        }
    }
    Ok(())
}

/// Why `rule` refuses `raw` as one element of `field`, or nothing: it must
/// read as the field's declared type and fit the rule's `max_length`, and,
/// where `in_closed_set`, be one of the rule's own `one_of` values.
fn element_problem(
    schema: &VaultSchema,
    rule: &Rule,
    field: &str,
    raw: &str,
    in_closed_set: bool,
) -> Option<ElementProblem> {
    let declared = schema.declared_type(field);
    if declared.read(raw).is_err() {
        return Some(ElementProblem::NotType(declared));
    }
    if let Some(limit) = rule.max_length.get(field)
        && u64::try_from(raw.chars().count()).unwrap_or(u64::MAX) > *limit
    {
        return Some(ElementProblem::TooLong(*limit));
    }
    if in_closed_set
        && let Some(set) = rule.one_of.get(field)
        && !schema
            .equality_key(field, raw)
            .is_some_and(|key| set.members.contains_key(&key))
    {
        return Some(ElementProblem::OutsideOneOf);
    }
    None
}

/// A default element's text where it holds no token, or nothing where it is
/// templated: a templated element is judged where a document is.
fn untemplated_text(element: &DefaultValue) -> Option<String> {
    match element {
        DefaultValue::Plain(value) => scalar_text(value),
        DefaultValue::Text(template) if !template.is_templated() => {
            Some(template.as_str().to_string())
        }
        DefaultValue::Text(_) | DefaultValue::List(_) | DefaultValue::Map(_) => None,
    }
}

/// A route against its own rule's allowed paths: weighed first, so its walk
/// stays under the ceiling, then judged under [`CaseFold::Exact`]. See the
/// [module](self).
fn check_route(rule: &Rule, route: &Route, paths: &[Pattern]) -> Result<(), RuleProblem> {
    let glob = route_glob(route);
    let weight =
        placement::weight(std::slice::from_ref(&glob)).saturating_mul(rule.placement_weight());
    if weight > PLACEMENT_CEILING {
        return Err(RuleProblem::RouteWeight { weight });
    }
    if sets_share_a_document_path(&[std::slice::from_ref(&glob), paths], CaseFold::Exact) {
        Ok(())
    } else {
        Err(RuleProblem::RouteOutsideAllowedPaths)
    }
}

/// The glob a route's folder stands for: its text with each token standing
/// as `*`, and a document's file name, `*.md`, after it.
fn route_glob(route: &Route) -> Pattern {
    let mut glob: String = route
        .template
        .parts()
        .iter()
        .map(|part| match part {
            Part::Literal(text) => text.as_str(),
            Part::Token(_) => "*",
        })
        .collect();
    glob.push_str("*.");
    glob.push_str(DOCUMENT_EXTENSION);
    Pattern::parse(&glob).expect("a route's glob is not empty")
}

/// Whether `left` and `right` may select one document together; see the
/// [module](self) for when two rules provably cannot.
fn may_co_select(schema: &VaultSchema, left: &Rule, right: &Rule) -> bool {
    !left.selector.frontmatter.iter().any(|(key, values)| {
        schema.field(key).and_then(|field| field.shape()) == Some(Shape::Single)
            && right
                .selector
                .frontmatter
                .get(key)
                .is_some_and(|other| values.keys.is_disjoint(&other.keys))
    })
}

/// The allowed-paths ceiling, judged per rule over its neighbourhood.
fn check_ceiling(schema: &VaultSchema) -> Result<(), VaultSchemaError> {
    let placed: Vec<&Rule> = schema
        .rules
        .values()
        .filter(|rule| rule.allowed_paths.is_some())
        .collect();
    for rule in &placed {
        let neighbourhood: Vec<&Rule> = placed
            .iter()
            .copied()
            .filter(|other| other.name == rule.name || may_co_select(schema, rule, other))
            .collect();
        let weight = placement::product_weight(neighbourhood.iter().copied());
        if weight > PLACEMENT_CEILING {
            return Err(VaultSchemaError::PlacementCeiling {
                rule: rule.name.clone(),
                rules: neighbourhood.iter().map(|rule| rule.name.clone()).collect(),
                weight,
            });
        }
    }
    Ok(())
}

/// The statically unavoidable conflicts, group by group.
fn check_unavoidable(schema: &VaultSchema) -> Result<(), VaultSchemaError> {
    let mut selectorless: Vec<&Rule> = Vec::new();
    let mut classes: Vec<(NormalSelector<'_>, Vec<&Rule>)> = Vec::new();
    for rule in schema.rules.values() {
        let normal = rule.selector.normal_form();
        if normal == NormalSelector::default() {
            selectorless.push(rule);
        } else if let Some((_, class)) = classes.iter_mut().find(|(held, _)| *held == normal) {
            class.push(rule);
        } else {
            classes.push((normal, vec![rule]));
        }
    }
    let groups =
        std::iter::once(selectorless.clone()).chain(classes.into_iter().map(|(_, class)| {
            class
                .into_iter()
                .chain(selectorless.iter().copied())
                .collect()
        }));
    for group in groups {
        if group.is_empty() {
            continue;
        }
        let combined: CombinedConstraint<'_> = schema.combined(&group);
        if let Some(conflict) = combined
            .conflicts(CaseFold::Ascii)
            .into_iter()
            .find(|conflict| {
                !matches!(
                    conflict,
                    RulesConflict::EmptyOneOf {
                        required: false,
                        ..
                    }
                )
            })
        {
            return Err(VaultSchemaError::RulesConflict { conflict });
        }
    }
    Ok(())
}
