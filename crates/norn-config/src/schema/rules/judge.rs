//! Rule judgment ([`VaultSchema::judge`]): what one document's path and
//! frontmatter breach, under the field declarations and the combined
//! constraint of every rule selecting it ([ADR 0035]).
//!
//! **One judge, one reading.** Selection is the matcher every selection site
//! reads ([`VaultSchema::selects`]); a value's shape is read as a selector
//! reads it ([`read_shape`]); an element is read against its field's type, a
//! length limit and a closed set as schema read judges a rule's own members
//! and defaults ([`Element`]); and an empty combined constraint is the one
//! [`CombinedConstraint`] reports. Nothing here reads the schema a second way.
//!
//! # What is judged, and what each finding names
//!
//! **The field declarations judge every document**, whatever rules the schema
//! states. A declared field holding a list where it is declared
//! [`Shape::Single`], or one value where it is declared [`Shape::List`], is one
//! [`Breach::ShapeMismatch`] naming the whole value. Otherwise each distinct
//! element — the value itself where it is no list — that does not read as the
//! declared type is one [`Breach::TypeMismatch`] naming that element. A map is
//! a value of neither shape the grammar names, and no type reads one, so a map
//! — the field's value or an element of its list — does not read as the type.
//! A null is no value: it reads as nothing and breaches neither. These cite no
//! rule and are always a warning.
//!
//! **A value judged by its type or shape is judged by that alone.** The closed
//! set and the length limit judge only the elements that read as the field's
//! declared type and shape; whether the field is present still answers to
//! `required` and `forbidden`. An undeclared field reads as text, so only a
//! nested list or a map element fails it, and with no declaration to breach
//! such an element is judged against the closed set as the value it is,
//! outside every set of scalars.
//!
//! **The rules judge the documents they select**, field by field in key order,
//! each field once per constraint kind against the combined constraint:
//!
//! - a field some rule requires is unmet where it is absent or null
//!   ([`Breach::RequiredMissing`]), naming no value and citing every rule
//!   requiring it;
//! - a field some rule forbids is breached by its key, null included
//!   ([`Breach::Forbidden`]), naming its whole value and citing every rule
//!   forbidding it;
//! - each distinct element outside the intersection of the closed sets is one
//!   [`Breach::NotOneOf`], citing every rule closing the field;
//! - each distinct element longer than the smallest limit, in Unicode scalar
//!   values, is one [`Breach::TooLong`], citing every rule limiting the field.
//!
//! An element repeated in one list is one finding: elements are told apart as
//! their values are spelled, a scalar by the text its field row holds and a
//! list or a map by its structure, map entries in key order.
//!
//! **An empty combined constraint is one conflict per field, in place of the
//! findings it makes unanswerable.** A field one rule requires and another
//! forbids, or whose closed sets share no member where some rule requires it,
//! is one [`Breach::FieldRulesConflict`] whatever it holds, citing every rule
//! contributing to the conflict, instead of its required, forbidden and
//! closed-set findings; it names the field's whole value where the key is
//! present and none where it is absent. An empty intersection on a field no
//! contributing rule requires is a conflict only where the field holds a
//! value. The length limit still judges a conflicted field.
//!
//! **Placement is judged where it fails.** A document standing where some
//! contributing rule's allowed paths do not admit it is
//! [`Breach::Misplaced`], citing every rule stating allowed paths — unless
//! those rules' allowed paths share no document path at all, which makes it
//! [`Breach::DocumentRulesConflict`] instead, citing the same rules. Neither
//! names a field or a value.
//!
//! A finding citing rules is reported at the highest of their severities.
//!
//! # What judging costs
//!
//! **Judging reads one document and the schema.** Its work is tallied as
//! [`RuleWork`], whose every count is a function of the document and the
//! schema alone — never of another document — and grows with the parameters
//! the schema declares: the rule count, the terms a selector holds, the
//! constraints and closed-set members the selecting rules state, and the
//! characters of the globs a path is matched against.
//!
//! **The placement walk is paid once per set of placement rules.** Whether
//! the allowed paths of a set of rules share a document path depends on the
//! set alone, so the verdict is memoized on the schema per set and fold
//! ([`PlacementVerdicts`](super::placement::PlacementVerdicts)), and a
//! misplaced document whose set was decided before reads the verdict instead
//! of walking. A walk costs at most a constant times
//! [`PLACEMENT_CEILING`](super::PLACEMENT_CEILING): about a second on globs
//! built of hundreds of wildcards, microseconds on globs of ordinary length.
//!
//! [ADR 0035]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0035-a-schema-rule-selects-documents-by-their-frontmatter.md

use std::collections::BTreeSet;

use norn_wire::{AuthoredValue, CaseFold, FindingKind, Severity, ValueMap};

use super::super::{Shape, TypedValue, VaultSchema};
use super::combined::{CombinedConstraint, FieldConstraint, RulesConflict};
use super::{Rule, higher, is_missing, value_in};

/// What judging one document concludes: every finding it breaches, and the
/// work the judgment paid.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Judgment {
    findings: Vec<RuleFinding>,
    work: RuleWork,
}

impl Judgment {
    /// Every finding, field findings in key order — a field's type or shape
    /// first — then the document's placement.
    pub fn findings(&self) -> &[RuleFinding] {
        &self.findings
    }

    /// The findings, taken out of the judgment.
    pub fn into_findings(self) -> Vec<RuleFinding> {
        self.findings
    }

    /// What the judgment paid.
    pub fn work(&self) -> RuleWork {
        self.work
    }
}

/// One breach the judge found: one path, field, constraint kind and
/// offending value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuleFinding {
    breach: Breach,
    field: Option<String>,
    value: Option<AuthoredValue>,
    rules: Vec<String>,
    severity: Severity,
}

impl RuleFinding {
    /// The constraint kind breached.
    pub fn breach(&self) -> Breach {
        self.breach
    }

    /// The field the finding is about, or `None` for one about where the
    /// document stands.
    pub fn field(&self) -> Option<&str> {
        self.field.as_deref()
    }

    /// The offending value: one element — or the value itself where it is no
    /// list — for a closed set, a length limit and a type; the field's whole
    /// value for a forbidden field, a shape and a conflict over a field the
    /// document holds; `None` for a missing field, a placement, a conflict
    /// over an absent field and a null.
    pub fn value(&self) -> Option<&AuthoredValue> {
        self.value.as_ref()
    }

    /// Every rule contributing to the constraint breached, in name order,
    /// each once; empty for a type or shape mismatch.
    pub fn rules(&self) -> &[String] {
        &self.rules
    }

    /// The highest severity of the rules cited, or a warning where none is.
    pub fn severity(&self) -> Severity {
        self.severity
    }
}

/// A constraint kind a document breaches: the closed vocabulary of rule
/// judgment, each a document-scoped finding kind.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Breach {
    /// A required field is absent or null.
    RequiredMissing,
    /// A forbidden field's key is present.
    Forbidden,
    /// An element is outside the field's closed set.
    NotOneOf,
    /// An element is past the field's length limit.
    TooLong,
    /// An element does not read as the field's declared type.
    TypeMismatch,
    /// The value is not of the field's declared shape.
    ShapeMismatch,
    /// The document stands where its placement rules do not allow.
    Misplaced,
    /// The rules selecting the document constrain a field so nothing meets
    /// it.
    FieldRulesConflict,
    /// The placement rules selecting the document share no document path.
    DocumentRulesConflict,
}

impl Breach {
    /// Every breach, in declaration order.
    pub const ALL: [Breach; 9] = [
        Breach::RequiredMissing,
        Breach::Forbidden,
        Breach::NotOneOf,
        Breach::TooLong,
        Breach::TypeMismatch,
        Breach::ShapeMismatch,
        Breach::Misplaced,
        Breach::FieldRulesConflict,
        Breach::DocumentRulesConflict,
    ];

    /// The finding kind a breach is recorded under.
    pub const fn kind(self) -> FindingKind {
        match self {
            Breach::RequiredMissing => FindingKind::RequiredMissing,
            Breach::Forbidden => FindingKind::Forbidden,
            Breach::NotOneOf => FindingKind::NotOneOf,
            Breach::TooLong => FindingKind::TooLong,
            Breach::TypeMismatch => FindingKind::TypeMismatch,
            Breach::ShapeMismatch => FindingKind::ShapeMismatch,
            Breach::Misplaced => FindingKind::Misplaced,
            Breach::FieldRulesConflict => FindingKind::FieldRulesConflict,
            Breach::DocumentRulesConflict => FindingKind::DocumentRulesConflict,
        }
    }

    /// The breach as a finding's message states it. It names no field, value
    /// or rule: those are the finding's target, value and citations, so a
    /// message's bytes grow with none of them.
    pub const fn statement(self) -> &'static str {
        match self {
            Breach::RequiredMissing => "a field its rules require is missing",
            Breach::Forbidden => "it holds a field its rules forbid",
            Breach::NotOneOf => "a field holds a value outside the values its rules allow",
            Breach::TooLong => "a field holds a value longer than its rules allow",
            Breach::TypeMismatch => "a field holds a value that does not read as its declared type",
            Breach::ShapeMismatch => "a field holds a value that is not of its declared shape",
            Breach::Misplaced => "it stands where its rules do not allow",
            Breach::FieldRulesConflict => "its rules constrain a field so no value meets them all",
            Breach::DocumentRulesConflict => "its rules allow no path in common",
        }
    }
}

/// What judging one document paid: the logical counts of rule work, which no
/// statement counter sees.
///
/// Every count is a function of the one document judged and the schema, so a
/// judgment's work is the same however many documents the vault holds.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuleWork {
    /// Rules whose selector was evaluated: every rule, once per judgment.
    pub rules_evaluated: u64,
    /// Selector terms evaluated — a `match.frontmatter` key, a `match.path`
    /// glob, an `exclude.path` glob — stopping at a rule's first failing one.
    pub selector_terms: u64,
    /// Rules that selected the document.
    pub rules_selected: u64,
    /// Constraint entries the selecting rules' combination read: one per
    /// field required, forbidden or limited, and one per closed-set member.
    pub constraint_entries: u64,
    /// Constraints judged: a field's presence against `required` or
    /// `forbidden`, one element against its type, a closed set or a limit, a
    /// value against its declared shape, and a path against the allowed
    /// paths.
    pub constraints_judged: u64,
    /// Characters of every glob a path was matched against, selectors' and
    /// allowed paths' alike.
    pub pattern_characters: u64,
    /// Placement walks run, each over a set of rules whose verdict no
    /// earlier judgment had reached.
    pub placement_walks: u64,
    /// The weight those walks carried: the product of each walked set's
    /// allowed-path weights ([`Rule::placement_weight`]).
    pub placement_weight: u64,
    /// Placement verdicts read from the schema's memo instead of walked.
    pub placement_verdicts_reused: u64,
    /// Findings the judgment minted.
    pub findings: u64,
}

impl RuleWork {
    /// No work at all.
    pub const NONE: RuleWork = RuleWork {
        rules_evaluated: 0,
        selector_terms: 0,
        rules_selected: 0,
        constraint_entries: 0,
        constraints_judged: 0,
        pattern_characters: 0,
        placement_walks: 0,
        placement_weight: 0,
        placement_verdicts_reused: 0,
        findings: 0,
    };

    /// Every count by name, in declaration order.
    pub fn counts(self) -> [(&'static str, u64); 10] {
        [
            ("rules_evaluated", self.rules_evaluated),
            ("selector_terms", self.selector_terms),
            ("rules_selected", self.rules_selected),
            ("constraint_entries", self.constraint_entries),
            ("constraints_judged", self.constraints_judged),
            ("pattern_characters", self.pattern_characters),
            ("placement_walks", self.placement_walks),
            ("placement_weight", self.placement_weight),
            ("placement_verdicts_reused", self.placement_verdicts_reused),
            ("findings", self.findings),
        ]
    }

    /// This work and `other` together.
    #[must_use]
    pub fn plus(self, other: RuleWork) -> RuleWork {
        RuleWork {
            rules_evaluated: self.rules_evaluated + other.rules_evaluated,
            selector_terms: self.selector_terms + other.selector_terms,
            rules_selected: self.rules_selected + other.rules_selected,
            constraint_entries: self.constraint_entries + other.constraint_entries,
            constraints_judged: self.constraints_judged + other.constraints_judged,
            pattern_characters: self.pattern_characters + other.pattern_characters,
            placement_walks: self.placement_walks + other.placement_walks,
            placement_weight: self.placement_weight.saturating_add(other.placement_weight),
            placement_verdicts_reused: self.placement_verdicts_reused
                + other.placement_verdicts_reused,
            findings: self.findings + other.findings,
        }
    }
}

/// A value as its field's declared shape reads it: the elements it holds, or
/// that it is not of the declared shape.
///
/// **The one shape reading**, which selection and judgment share. A list's
/// elements are what it holds, unless its field is declared
/// [`Shape::Single`]; any other value is its one element, unless its field is
/// declared [`Shape::List`]; and a null is no value, holding no element. A
/// map is one element whatever the shape, and it reads as no type.
pub(super) enum ShapeReading<'v> {
    /// The elements to judge or compare, in the order written.
    Elements(Vec<&'v AuthoredValue>),
    /// A value of the other shape than its field declares.
    WrongShape,
}

/// `value` under the declared shape `shape`. See [`ShapeReading`].
pub(super) fn read_shape(shape: Option<Shape>, value: &AuthoredValue) -> ShapeReading<'_> {
    match value {
        AuthoredValue::Null => ShapeReading::Elements(Vec::new()),
        AuthoredValue::List(_) if shape == Some(Shape::Single) => ShapeReading::WrongShape,
        AuthoredValue::List(items) => ShapeReading::Elements(items.iter().collect()),
        AuthoredValue::Map(_) => ShapeReading::Elements(vec![value]),
        _ if shape == Some(Shape::List) => ShapeReading::WrongShape,
        _ => ShapeReading::Elements(vec![value]),
    }
}

/// One element value read against a field: its text, and the value it is
/// compared by — `None` where it does not read as the field's declared type.
///
/// **The one element reading.** Schema read judges a rule's own defaults,
/// members and synonym targets through it, and rule judgment each element a
/// document holds, so a type, a length and a closed set are read one way
/// whether the element is the schema's or a document's.
pub(super) struct Element<'a> {
    raw: &'a str,
    key: Option<TypedValue>,
}

impl<'a> Element<'a> {
    /// `raw` read as an element of `field` under `schema`.
    pub(super) fn read(schema: &VaultSchema, field: &str, raw: &'a str) -> Self {
        Element {
            raw,
            key: schema.equality_key(field, raw),
        }
    }

    /// Whether the element reads as its field's declared type.
    pub(super) fn reads_as_type(&self) -> bool {
        self.key.is_some()
    }

    /// Whether the element is longer than `limit` in Unicode scalar values.
    pub(super) fn longer_than(&self, limit: u64) -> bool {
        longer_than(self.raw, limit)
    }

    /// Whether the element is one of a closed set, by the value it is
    /// compared by: `admits` answers for a member's equality key. An element
    /// that does not read as its type is a member of none.
    pub(super) fn is_in(&self, admits: impl Fn(&TypedValue) -> bool) -> bool {
        self.key.as_ref().is_some_and(admits)
    }
}

/// Whether `raw` is longer than `limit` in Unicode scalar values: the unit a
/// length limit counts.
fn longer_than(raw: &str, limit: u64) -> bool {
    u64::try_from(raw.chars().count()).unwrap_or(u64::MAX) > limit
}

/// One element of a field as the judge holds it: the value itself, its text
/// where it is a scalar, the value a closed set compares it by where it reads
/// as its field's type, and what tells it from the field's other elements.
struct Held<'v> {
    value: &'v AuthoredValue,
    text: Option<String>,
    key: Option<TypedValue>,
    identity: Identity,
}

/// What tells one offending element from another: a scalar by the text its
/// field row holds, so `1` and `"1"` are one element, and a list or a map by
/// its structure, map entries in key order, so two spellings of one map are
/// one element. One finding stands per field, kind and identity.
#[derive(Debug, Eq, PartialEq)]
enum Identity {
    Text(String),
    Structure(AuthoredValue),
}

impl Identity {
    /// The identity of `element`, whose scalar text is `text`.
    fn of(element: &AuthoredValue, text: Option<&str>) -> Self {
        match text {
            Some(text) => Identity::Text(text.to_string()),
            None => Identity::Structure(normal_form(element)),
        }
    }
}

impl VaultSchema {
    /// Judge the document at `path` holding `frontmatter`: every finding the
    /// field declarations and the combined constraint of the rules selecting
    /// it conclude — one per field, constraint kind and offending value, a
    /// list judged element by element — and the work that cost.
    ///
    /// A pure function of the path, the frontmatter and the schema — the
    /// placement memo it reads and fills is the schema's own and changes no
    /// answer — so derivation files its findings in the document's own
    /// changeset. `case` is how the path globs' literal letters compare with
    /// the path's, as for [`VaultSchema::selects`].
    pub fn judge(&self, path: &str, frontmatter: &ValueMap, case: CaseFold) -> Judgment {
        let mut work = RuleWork::default();
        let entries = frontmatter.entries();
        let mut findings = Vec::new();

        // Every field's value read against its declaration: its type and
        // shape findings filed, and the elements the rules may judge kept.
        let mut admissible: Vec<(&str, Vec<Held<'_>>)> = Vec::new();
        for (field, value) in entries {
            let held = self.judge_declaration(field, value, &mut work, &mut findings);
            admissible.push((field, held));
        }

        let selected = self.select_counted(path, entries, case, &mut work);
        if !selected.is_empty() {
            let combined = self.combined(&selected);
            work.constraint_entries += selected.iter().map(|rule| entries_of(rule)).sum::<u64>();
            let conflicts = combined.field_conflicts();
            for (field, constraint) in combined.fields() {
                let value = value_in(entries, field);
                let elements = admissible
                    .iter()
                    .find(|(held, _)| *held == field)
                    .map_or(&[][..], |(_, elements)| elements.as_slice());
                judge_field(
                    &JudgedField {
                        name: field,
                        constraint,
                        value,
                        elements,
                        conflicts: &conflicts,
                    },
                    &mut work,
                    &mut findings,
                );
            }
            self.judge_placement(&combined, path, case, &mut work, &mut findings);
        }
        // Field findings in key order, a field's type and shape first, then
        // where the document stands; the sort is stable, so each field keeps
        // the order its findings were judged in.
        findings.sort_by(|left: &RuleFinding, right: &RuleFinding| {
            (left.field.is_none(), &left.field).cmp(&(right.field.is_none(), &right.field))
        });
        work.findings = findings.len() as u64;
        Judgment { findings, work }
    }

    /// Judge `value` against `field`'s declaration, filing its type and shape
    /// findings, and answer the elements of it the rules may judge: every
    /// element that reads as the declared type and shape, each once.
    fn judge_declaration<'v>(
        &self,
        field: &str,
        value: &'v AuthoredValue,
        work: &mut RuleWork,
        findings: &mut Vec<RuleFinding>,
    ) -> Vec<Held<'v>> {
        let declaration = self.field(field);
        let shape = declaration.and_then(|declared| declared.shape());
        if shape.is_some() {
            work.constraints_judged += 1;
        }
        let elements = match read_shape(shape, value) {
            ShapeReading::WrongShape => {
                findings.push(mismatch(Breach::ShapeMismatch, field, value.clone()));
                return Vec::new();
            }
            ShapeReading::Elements(elements) => elements,
        };
        let mut held: Vec<Held<'v>> = Vec::new();
        let mut mismatched: Vec<Identity> = Vec::new();
        // A null element is no value, as a null field is: nothing judges it.
        for element in elements
            .into_iter()
            .filter(|element| **element != AuthoredValue::Null)
        {
            let text = element.scalar_text();
            let key = text
                .as_deref()
                .and_then(|raw| Element::read(self, field, raw).key);
            let identity = Identity::of(element, text.as_deref());
            if declaration.is_some() {
                work.constraints_judged += 1;
                if key.is_none() {
                    if !mismatched.contains(&identity) {
                        mismatched.push(identity);
                        findings.push(mismatch(Breach::TypeMismatch, field, element.clone()));
                    }
                    continue;
                }
            }
            if !held.iter().any(|seen| seen.identity == identity) {
                held.push(Held {
                    value: element,
                    text,
                    key,
                    identity,
                });
            }
        }
        held
    }

    /// Judge where the document stands against the placement rules of
    /// `combined`, reading the memoized verdict on whether they share a path
    /// where one was reached.
    fn judge_placement(
        &self,
        combined: &CombinedConstraint<'_>,
        path: &str,
        case: CaseFold,
        work: &mut RuleWork,
        findings: &mut Vec<RuleFinding>,
    ) {
        let placed: Vec<&Rule> = combined.placement_rules().collect();
        if placed.is_empty() {
            return;
        }
        work.constraints_judged += 1;
        if combined.admits_path_counted(path, case, work) {
            return;
        }
        let breach = if placed.len() > 1 && !self.placement_shared(&placed, case, work) {
            Breach::DocumentRulesConflict
        } else {
            Breach::Misplaced
        };
        findings.push(cited(breach, None, None, placed));
    }
}

/// One field as [`judge_field`] judges it.
struct JudgedField<'a, 's, 'v> {
    name: &'s str,
    constraint: &'a FieldConstraint<'s>,
    /// The value the document holds under the field, null included.
    value: Option<&'v AuthoredValue>,
    /// The elements of that value that read as the field's declared type and
    /// shape, each once.
    elements: &'a [Held<'v>],
    /// Every way the combined constraint is empty on some field.
    conflicts: &'a [RulesConflict],
}

/// Judge one constrained field against the combined constraint.
fn judge_field(
    field: &JudgedField<'_, '_, '_>,
    work: &mut RuleWork,
    findings: &mut Vec<RuleFinding>,
) {
    let constraint = field.constraint;
    let missing = is_missing(field.value);
    let conflicting: BTreeSet<&str> = field
        .conflicts
        .iter()
        .filter(|conflict| match conflict {
            RulesConflict::RequiredAndForbidden { field: named, .. } => named == field.name,
            RulesConflict::EmptyOneOf {
                field: named,
                required,
                ..
            } => named == field.name && (*required || !missing),
            RulesConflict::DisjointPlacement { .. } => false,
        })
        .flat_map(|conflict| conflict.rules().iter().map(String::as_str))
        .collect();
    if !conflicting.is_empty() {
        work.constraints_judged += 1;
        let rules: Vec<&Rule> = constraint
            .required_by()
            .chain(constraint.forbidden_by())
            .chain(constraint.one_of().into_iter().flat_map(|set| set.rules()))
            .filter(|rule| conflicting.contains(rule.name()))
            .collect();
        findings.push(cited(
            Breach::FieldRulesConflict,
            Some(field.name),
            field.value.cloned(),
            rules,
        ));
    } else {
        if constraint.is_required() {
            work.constraints_judged += 1;
            if missing {
                findings.push(cited(
                    Breach::RequiredMissing,
                    Some(field.name),
                    None,
                    constraint.required_by().collect(),
                ));
            }
        }
        if constraint.is_forbidden() {
            work.constraints_judged += 1;
            if let Some(value) = field.value {
                findings.push(cited(
                    Breach::Forbidden,
                    Some(field.name),
                    Some(value.clone()),
                    constraint.forbidden_by().collect(),
                ));
            }
        }
        if let Some(set) = constraint.one_of() {
            for element in field.elements {
                work.constraints_judged += 1;
                let admitted = element.key.as_ref().is_some_and(|key| set.admits(key));
                if !admitted {
                    findings.push(cited(
                        Breach::NotOneOf,
                        Some(field.name),
                        Some(element.value.clone()),
                        set.rules().collect(),
                    ));
                }
            }
        }
    }
    if let Some(limit) = constraint.max_length() {
        for element in field.elements {
            let Some(raw) = element.text.as_deref() else {
                continue;
            };
            work.constraints_judged += 1;
            if longer_than(raw, limit) {
                findings.push(cited(
                    Breach::TooLong,
                    Some(field.name),
                    Some(element.value.clone()),
                    constraint.max_length_by().collect(),
                ));
            }
        }
    }
}

/// A finding citing `rules`, at the highest of their severities.
fn cited(
    breach: Breach,
    field: Option<&str>,
    value: Option<AuthoredValue>,
    rules: Vec<&Rule>,
) -> RuleFinding {
    let names: BTreeSet<&str> = rules.iter().map(|rule| rule.name()).collect();
    RuleFinding {
        breach,
        field: field.map(str::to_string),
        value: value.filter(|value| *value != AuthoredValue::Null),
        severity: rules.iter().fold(Severity::Warning, |severity, rule| {
            higher(severity, rule.severity())
        }),
        rules: names.into_iter().map(str::to_string).collect(),
    }
}

/// A type or shape mismatch: judged against the field declarations, which
/// state no rule and no severity.
fn mismatch(breach: Breach, field: &str, value: AuthoredValue) -> RuleFinding {
    RuleFinding {
        breach,
        field: Some(field.to_string()),
        value: Some(value),
        rules: Vec::new(),
        severity: Severity::Warning,
    }
}

/// `value` with every map's entries in key order, so two values spelling one
/// structure compare equal however their maps were written.
fn normal_form(value: &AuthoredValue) -> AuthoredValue {
    match value {
        AuthoredValue::List(items) => AuthoredValue::List(items.iter().map(normal_form).collect()),
        AuthoredValue::Map(map) => {
            let mut entries: Vec<(String, AuthoredValue)> = map
                .entries()
                .iter()
                .map(|(key, value)| (key.clone(), normal_form(value)))
                .collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            AuthoredValue::Map(ValueMap::new(entries).expect("a map's keys are each held once"))
        }
        scalar => scalar.clone(),
    }
}

/// The constraint entries `rule` states: one per field it requires, forbids
/// or limits, and one per member of each closed set.
fn entries_of(rule: &Rule) -> u64 {
    let members: usize = rule.one_of.values().map(|set| set.values.len()).sum();
    (rule.required.len() + rule.forbidden.len() + rule.max_length.len() + members) as u64
}
