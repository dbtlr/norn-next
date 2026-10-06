//! Schema rules: named constraints a vault schema places on the documents it
//! selects ([ADR 0035]).
//!
//! # The shape a rule is written in
//!
//! ```yaml
//! rules:
//!   task:
//!     description: A tracked task
//!     severity: error
//!     match:
//!       frontmatter: { type: [task, chore] }
//!       path: "projects/<project>/**"
//!     exclude:
//!       path: ["projects/*/archive/**"]
//!     required:
//!       status: { default: todo }
//!       title:
//!     forbidden:
//!       due_date: { rename_to: due }
//!       scratch: remove
//!       legacy:
//!     one_of:
//!       status: { values: [todo, doing, done], synonyms: { complete: done } }
//!     max_length: { title: 120 }
//!     allowed_paths:
//!       paths: ["projects/*/tasks/**"]
//!       route: "projects/{{path.project}}/tasks/"
//! ```
//!
//! `rules:` maps each rule's name to the rule, so two rules of one name are a
//! mapping key written twice, which the YAML reader refuses. A name is an
//! identifier, as a creation rule's is. `severity` is `error` or `warning`,
//! `warning` where absent.
//!
//! **Selectors.** `match.frontmatter` maps a key to one value or to an any-of
//! list; its keys are ANDed. Each value compares by its equality key
//! ([`VaultSchema::equality_key`]): a tag key — the tag carrier `tags`,
//! declared or not, or a key declared `tags` — by the tag it names under the
//! tag fold, a key declared `number`, `boolean` or `date` by its typed value,
//! and any other key exactly as written. A document's list value matches
//! where any element does, as a find's equality matches a list field —
//! except under a key declared [`Shape::Single`]. **A value that does not
//! read as its key's declared type or shape matches no selector**: a list or
//! map under a single-shaped key is not the key's value, as a value failing
//! its type has no typed value to compare. Find's field equality reads a value
//! the same way — the tag fold on a tag key, and a value of the wrong declared
//! shape as no value — in the store's own SQL, held to this reading by
//! `norn-host`'s suite. `match.path` is a glob whose
//! whole segments spelled `<name>` are captures, each matching one segment as
//! a whole-segment `*` does; `exclude.path` lists globs none of which may
//! match. A rule that selects by neither — or whose selectors normalize to
//! none: an empty `frontmatter`, a `match.path` of `**`, an empty `exclude`
//! list — is **selectorless** and selects every document.
//!
//! **`<` and `>` stand in no other rule glob.** A document path may hold
//! them, but in a rule glob a `<name>` segment reads as a capture, and only
//! `match.path` binds one: an `exclude.path` or `allowed_paths` glob holding
//! `<x>` would read as a capture the grammar binds nowhere there, so it is
//! refused rather than read literally, and so is a stray `<` or `>`, as
//! `match.path` refuses one that spells no whole-segment capture. A rule
//! glob matches a literal `<x>` segment through wildcards — `?x?`, or `*` —
//! which take any character. The tag patterns and the ambiguity-ignore set
//! read no capture, so `<name>` there is the literal text it spells. No rule
//! glob holds an empty segment, which no document path holds either.
//!
//! **Constraints and their fixes** ([ADR 0036]). `required` maps a field to
//! its default, or to nothing; a field is missing where it is absent or null,
//! and an empty string or list meets it. `forbidden` maps a field to
//! `remove`, `{rename_to: <field>}`, or nothing, which is no fix. `one_of`
//! maps a field to `{values: [...], synonyms: {written: member}}`, `values`
//! never empty. `max_length` maps a field to a positive limit on each
//! element's length in Unicode scalar values. `allowed_paths` lists the globs
//! a selected document may stand at and, optionally, a `route`: the folder,
//! ending in `/`, a misplaced document belongs in, its file name kept.
//!
//! **Templates.** A default and a route read the template grammar restricted
//! to `{{now}}`, `{{date}}`, `{{time}}` and `{{path.<name>}}` for a capture
//! the rule's own `match.path` defines; a route holds no `:`, as a creation
//! rule's target holds none, so the clock tokens stand in one only slugged.
//!
//! # Rules judge themselves at schema read
//!
//! Every refusal below is a schema read error, so a reload refuses it and
//! the served declaration stands. Beside the structural refusals — an unknown
//! key at any level, a node of the wrong shape, a capture misplaced — the
//! schema refuses:
//!
//! - an untemplated default that fails its own rule as a whole field value:
//!   a type or shape its field does not declare, a value outside the rule's
//!   `one_of` or past its `max_length`, a list's elements each judged;
//! - a `one_of` member or synonym target that fails as one element: not
//!   reading as its key's type, past the rule's `max_length`, or, for a
//!   target, outside the rule's own `values`. A collection's shape is judged
//!   on the composed field alone, so a `shape: list` field takes scalar
//!   members;
//! - a route no document directly inside it could stand at under the rule's
//!   own `allowed_paths`: the route's text, each token standing as a `*` and
//!   a file name `*.md` after it, is a glob sharing no document path with
//!   them under the route's own spelling, [`CaseFold::Exact`]; and a route
//!   whose glob weighs past [`PLACEMENT_CEILING`] with those paths;
//! - a template token a default or route does not admit, or a capture its
//!   own `match.path` does not define;
//! - a `rename_to` onto a field the same rule forbids, or onto a field
//!   another of its forbidden fields renames to;
//! - a neighbourhood of allowed paths past [`PLACEMENT_CEILING`];
//! - a **statically unavoidable conflict** among rules that select together
//!   on every document any of them selects — where one is selectorless, or
//!   where their selectors are identical once any-of and `exclude.path` lists
//!   are read as sets, capture names erased and a `match.path` of `**` read
//!   as absent: a field required and forbidden, a closed-set intersection
//!   with no member on a field one of them requires, or allowed paths with no
//!   document path in common, naming every contributing rule in name order.
//!
//! Two rules may select one document together unless both select on a key
//! declared `shape: single` with value sets sharing no equality key, and the
//! ceiling weighs each rule's allowed paths with those of every rule that
//! may select beside it. A conflict between rules holds on every root, so it
//! is judged under the wider fold, [`CaseFold::Ascii`]; a route is one
//! author's spelling against their own globs, judged exactly. Only a path a
//! document could stand at counts as a path two sets share.
//!
//! Every other conflict — one that depends on a value or a path — and every
//! templated default are judged where a document is.
//!
//! # Where rules are consumed
//!
//! Selection ([`VaultSchema::selecting_rules`]), the combined constraint
//! ([`VaultSchema::combined`]), rule judgment ([`VaultSchema::judge`]) and
//! the defaults fixpoint ([`VaultSchema::fill_rule_defaults`]) are pure
//! functions of a path, a frontmatter and the schema. **Derivation judges
//! every document by them**: `norn-host` files each finding the judgment
//! concludes in the document's own changeset, citing its rules and naming its
//! offending value, so a rule is a term of
//! [`VaultSchema::rederives_documents`]. **Dormant carriers beyond that:**
//! the write gate that judges a plan's documents by the same judgment lands
//! with NORN-359, and so does the fixpoint's wiring into `new` and inbox
//! capture; repair's declared fixes land at Layer 5B (NORN-351). Until the
//! gate lands, a plan's schema check leaves the rule breaches out. A rule's
//! declaration reaches `norn-host` too: it reads each rule's accessors into
//! the content model the store holds, which `describe`'s rule facet reports
//! as the schema writes it and a `validate` naming a rule is checked against.
//!
//! **A path selector makes a carried move's judgment necessary.** A rule's
//! `match.path`, `exclude.path` and `allowed_paths` conclude about a document
//! from where it stands, so the same bytes can be valid at one path and in
//! breach at another; the host applier's skip of a document a move carries
//! byte for byte (`norn-host`'s `applier::stage`) is sound only while no write
//! judges a rule.
//!
//! [ADR 0035]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0035-a-schema-rule-selects-documents-by-their-frontmatter.md
//! [ADR 0036]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0036-a-repair-fix-is-declared-on-the-constraint-it-serves.md

mod checks;
mod combined;
mod defaults;
mod judge;
mod placement;
mod read;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use norn_wire::{AuthoredValue, Captures, CaseFold, Severity, ValueMap, fold_tag};

pub use combined::{CombinedConstraint, FieldConstraint, OneOfIntersection, RulesConflict};
pub use defaults::{DefaultCandidate, DefaultsConflict, RuleDefaultsRefusal};
pub use judge::{Breach, Judgment, RuleFinding, RuleWork};
pub use placement::PLACEMENT_CEILING;

pub(super) use checks::check_rules;
pub(super) use placement::PlacementVerdicts;
pub(super) use read::read_rules;

use super::creation::DefaultValue;
use super::template::{FillError, LocalTimestamp, Part, Template, TemplateValues};
use super::{FieldType, Pattern, Shape, TypedValue, VaultSchema};

/// One named schema rule: what it selects, what it requires of what it
/// selects, and the fixes it declares.
///
/// Schema read's own checks read it, rule judgment reads it as a constraint
/// ([`VaultSchema::judge`]), and `norn-host` reads its accessors into the
/// declaration `describe`'s rule facet reports. **A dormant carrier** beyond
/// that: the write gate (NORN-359) and repair's declared fixes (NORN-351) read
/// it as a constraint, and neither is built.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rule {
    name: String,
    description: Option<String>,
    severity: Severity,
    selector: Selector,
    required: BTreeMap<String, Option<RuleDefault>>,
    forbidden: BTreeMap<String, ForbiddenFix>,
    one_of: BTreeMap<String, ClosedSet>,
    max_length: BTreeMap<String, u64>,
    allowed_paths: Option<AllowedPaths>,
}

impl Rule {
    /// The rule's name, unique in the schema.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What the schema says the rule is for.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// The severity a finding under this rule is reported at; `warning`
    /// where the schema states none.
    pub fn severity(&self) -> Severity {
        self.severity
    }

    /// What the rule selects documents by.
    pub fn selector(&self) -> &Selector {
        &self.selector
    }

    /// Each field the rule requires, in key order, with its default where the
    /// rule declares one.
    pub fn required(&self) -> impl Iterator<Item = (&str, Option<&RuleDefault>)> {
        self.required
            .iter()
            .map(|(field, default)| (field.as_str(), default.as_ref()))
    }

    /// Each field the rule forbids, in key order, with its declared fix.
    pub fn forbidden(&self) -> impl Iterator<Item = (&str, &ForbiddenFix)> {
        self.forbidden
            .iter()
            .map(|(field, fix)| (field.as_str(), fix))
    }

    /// Each field the rule closes over a set of values, in key order.
    pub fn one_of(&self) -> impl Iterator<Item = (&str, &ClosedSet)> {
        self.one_of.iter().map(|(field, set)| (field.as_str(), set))
    }

    /// Each field the rule limits in length, in key order, with its limit in
    /// Unicode scalar values per element.
    pub fn max_length(&self) -> impl Iterator<Item = (&str, u64)> {
        self.max_length
            .iter()
            .map(|(field, limit)| (field.as_str(), *limit))
    }

    /// Where a document the rule selects may stand, where the rule says.
    pub fn allowed_paths(&self) -> Option<&AllowedPaths> {
        self.allowed_paths.as_ref()
    }

    /// The weight of the rule's allowed paths: the sum over its globs of each
    /// glob's length in characters plus one, which bounds the states of the
    /// automaton the globs make together. Zero for a rule that states none.
    /// See [`PLACEMENT_CEILING`].
    ///
    /// Schema read weighs every neighbourhood by it, and rule judgment
    /// reports the weight of each placement walk it pays by it
    /// ([`RuleWork::placement_weight`]), a logical count derivation tallies.
    pub fn placement_weight(&self) -> u64 {
        self.allowed_paths
            .as_ref()
            .map_or(0, |allowed| placement::weight(&allowed.paths))
    }
}

/// What a rule selects documents by. See the [module](self) for how each
/// part compares.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Selector {
    frontmatter: BTreeMap<String, SelectorValues>,
    path: Option<Pattern>,
    exclude: Vec<Pattern>,
}

/// One `match.frontmatter` key's any-of values: in the order written, each in
/// a field row's spelling of the scalar — `1.50` reads `1.5`, and `1` and `"1"`
/// both read `1` — and as the equality keys a document's value is compared by.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SelectorValues {
    written: Vec<String>,
    keys: BTreeSet<TypedValue>,
}

impl Selector {
    /// Each `match.frontmatter` key, in key order, with the values any of
    /// which it matches, in the order written and in a field row's spelling
    /// of each scalar.
    pub fn frontmatter(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.frontmatter
            .iter()
            .map(|(key, values)| (key.as_str(), values.written.as_slice()))
    }

    /// The `match.path` glob, captures and all, where the rule states one.
    pub fn path(&self) -> Option<&Pattern> {
        self.path.as_ref()
    }

    /// The `exclude.path` globs, in the order written.
    pub fn exclude(&self) -> &[Pattern] {
        &self.exclude
    }

    /// Whether the selector selects every document.
    ///
    /// Schema read groups the rules that always select together by it, and
    /// this crate's suite reads it. `describe`'s rule facet reports a selector
    /// as the schema writes it rather than through this reading, so nothing
    /// outside this crate reads it.
    pub fn is_selectorless(&self) -> bool {
        self.normal_form() == NormalSelector::default()
    }

    /// The selector as two selectors are compared for identity: any-of and
    /// `exclude.path` lists as sets, capture names erased, and a `match.path`
    /// of `**` read as absent.
    fn normal_form(&self) -> NormalSelector<'_> {
        NormalSelector {
            frontmatter: self
                .frontmatter
                .iter()
                .map(|(key, values)| (key.as_str(), &values.keys))
                .collect(),
            path: self
                .path
                .as_ref()
                .filter(|path| path.as_str() != "**")
                .map(erased_captures),
            exclude: self.exclude.iter().map(Pattern::as_str).collect(),
        }
    }
}

/// A selector in the form two selectors are compared for identity in.
#[derive(Debug, Default, Eq, PartialEq)]
struct NormalSelector<'a> {
    frontmatter: BTreeMap<&'a str, &'a BTreeSet<TypedValue>>,
    path: Option<String>,
    exclude: BTreeSet<&'a str>,
}

/// `pattern`'s text with every capture's name erased: `<a>` and `<b>` in one
/// place are one selector.
fn erased_captures(pattern: &Pattern) -> String {
    let captures: BTreeSet<&str> = pattern.captures().collect();
    pattern
        .as_str()
        .split('/')
        .map(|segment| {
            match segment
                .strip_prefix('<')
                .and_then(|rest| rest.strip_suffix('>'))
            {
                Some(name) if captures.contains(name) => "<>",
                _ => segment,
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// A required field's default: a value, a list of values, or a template
/// that fills to one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuleDefault {
    value: DefaultValue,
}

impl RuleDefault {
    /// The default as the schema writes it: every string a template's source
    /// text.
    pub fn source(&self) -> AuthoredValue {
        self.value.source()
    }

    /// Whether some string of the default holds a token, so what it fills to
    /// is fixed only when a document is.
    pub fn is_templated(&self) -> bool {
        templates(&self.value).any(Template::is_templated)
    }

    /// The default filled at `at`, each `{{path.<name>}}` from `captures`:
    /// what the rule's match bound.
    ///
    /// Read by the defaults fixpoint here. Repair's declared fix fills one
    /// default the same way at Layer 5B (NORN-351), which is not built.
    pub fn fill(&self, at: LocalTimestamp, captures: Captures) -> Result<AuthoredValue, FillError> {
        self.value
            .fill(&TemplateValues::new(BTreeMap::new(), at).with_captures(captures))
    }

    /// Whether the default reads a path capture.
    fn reads_captures(&self) -> bool {
        templates(&self.value).any(|template| template.path_captures().next().is_some())
    }
}

/// Every template a default holds.
fn templates(value: &DefaultValue) -> Box<dyn Iterator<Item = &Template> + '_> {
    match value {
        DefaultValue::Plain(_) => Box::new(std::iter::empty()),
        DefaultValue::Text(template) => Box::new(std::iter::once(template)),
        DefaultValue::List(items) => Box::new(items.iter().flat_map(templates)),
        DefaultValue::Map(entries) => {
            Box::new(entries.iter().flat_map(|(_, value)| templates(value)))
        }
    }
}

/// The fix a rule declares for a field it forbids.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ForbiddenFix {
    /// None: the field is forbidden and repair skips it with its value.
    Unfixed,
    /// The field is removed.
    Remove,
    /// The field is renamed to this one.
    RenameTo(String),
}

/// The closed set of values a rule allows a field, and the synonyms it maps
/// onto them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosedSet {
    values: Vec<String>,
    members: BTreeMap<TypedValue, String>,
    synonyms: Vec<(String, String)>,
}

impl ClosedSet {
    /// The members, as written, in the order written.
    pub fn values(&self) -> &[String] {
        &self.values
    }

    /// Each synonym, as written, and the member it maps onto.
    pub fn synonyms(&self) -> impl Iterator<Item = (&str, &str)> {
        self.synonyms
            .iter()
            .map(|(written, member)| (written.as_str(), member.as_str()))
    }
}

/// Where a document a rule selects may stand, and where a misplaced one is
/// routed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllowedPaths {
    paths: Vec<Pattern>,
    route: Option<Route>,
}

impl AllowedPaths {
    /// The globs a document may stand at, in the order written; never empty.
    pub fn paths(&self) -> &[Pattern] {
        &self.paths
    }

    /// The folder a misplaced document is routed to, where the rule declares
    /// one.
    pub fn route(&self) -> Option<&Route> {
        self.route.as_ref()
    }

    /// Whether some glob admits `path`, tallying in `work` the characters of
    /// each glob matched up to the first that admits it.
    fn admits(&self, path: &str, case: CaseFold, work: &mut RuleWork) -> bool {
        self.paths.iter().any(|glob| {
            work.pattern_characters += glob.as_str().chars().count() as u64;
            glob.matches(path, case)
        })
    }
}

/// The folder an `allowed_paths` route names, as a template ending in `/`.
/// A document routed there keeps its file name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Route {
    template: Template,
}

impl Route {
    /// The route as it is written.
    pub fn as_str(&self) -> &str {
        self.template.as_str()
    }

    /// The template the route is.
    pub fn template(&self) -> &Template {
        &self.template
    }
}

/// What is wrong with a schema rule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuleProblem {
    /// The rule's name is not an identifier.
    Name {
        /// The name, as written.
        name: String,
    },
    /// A `match.frontmatter` value that does not read as its key's declared
    /// type, which no document's value would equal.
    SelectorValue {
        /// The value, as written.
        value: String,
        /// The key's declared type.
        declared: FieldType,
    },
    /// A default of the wrong shape for its field.
    DefaultShape {
        /// The shape the field declares.
        declared: Shape,
    },
    /// An untemplated default element its own rule refuses.
    Default {
        /// The element, as written.
        value: String,
        /// Why the rule refuses it.
        problem: ElementProblem,
    },
    /// A `one_of` member its own rule refuses as an element.
    Member {
        /// The member, as written.
        value: String,
        /// Why the rule refuses it.
        problem: ElementProblem,
    },
    /// A synonym's target its own rule refuses as an element.
    SynonymTarget {
        /// The target, as written.
        value: String,
        /// Why the rule refuses it.
        problem: ElementProblem,
    },
    /// A template breaks the template grammar.
    Template(super::TemplateError),
    /// A default or route holds a token it does not admit.
    InadmissibleToken {
        /// The token, as written between its braces.
        token: String,
    },
    /// A default or route reads a capture its rule's `match.path` does not
    /// define.
    UndefinedCapture {
        /// The capture read.
        name: String,
    },
    /// A route does not end in `/`, and a route names a folder.
    RouteNotFolder,
    /// A route names no folder path, judged on its literal text with each
    /// token standing as a plain value.
    RoutePath(norn_wire::PathProblem),
    /// A route's literal text holds `:`, `*` or `?`.
    ///
    /// `:` is not portable in a folder name, and a route is refused it as a
    /// creation rule's target is. `*` and `?` are refused because a route is
    /// judged as a glob against its rule's allowed paths, and the glob
    /// grammar has no escape: a literal `*` there would read as a wildcard,
    /// judging some other folder than the one the route names.
    RouteCharacter {
        /// The character.
        character: char,
    },
    /// A route holds `{{now}}` or `{{time}}` without `|slug`, each of which
    /// writes the clock with a `:` in it.
    RouteClockWithColon {
        /// The token's name: `now` or `time`.
        token: String,
    },
    /// No document directly inside the route's folder could stand at any of
    /// the rule's own allowed paths, under the route's own spelling.
    RouteOutsideAllowedPaths,
    /// The route's glob and the rule's allowed paths weigh more together than
    /// [`PLACEMENT_CEILING`], the bound on the walk that judges one against
    /// the other.
    RouteWeight {
        /// What they weigh: the route glob's weight times the rule's.
        weight: u64,
    },
    /// A `rename_to` onto a field the same rule forbids.
    RenameOntoForbidden {
        /// The rename's target.
        target: String,
    },
    /// Two forbidden fields of one rule renamed onto one target.
    RenameTwice {
        /// The target both rename onto.
        target: String,
        /// The other forbidden field renamed onto it.
        other: String,
    },
}

/// Why a rule refuses one element value it states.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ElementProblem {
    /// It does not read as its field's declared type.
    NotType(FieldType),
    /// It is longer, in Unicode scalar values, than the rule's limit.
    TooLong(u64),
    /// It is not a member of the rule's closed set for the field.
    OutsideOneOf,
}

impl fmt::Display for ElementProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ElementProblem::NotType(declared) => {
                write!(formatter, "does not read as the field's type, {declared}")
            }
            ElementProblem::TooLong(limit) => write!(
                formatter,
                "is longer than the rule's `max_length` of {limit}"
            ),
            ElementProblem::OutsideOneOf => {
                formatter.write_str("is not one of the rule's `one_of` values")
            }
        }
    }
}

impl fmt::Display for RuleProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        const IDENTIFIER: &str = "an ASCII letter or `_` followed by letters, digits, `_` or `-`";
        match self {
            RuleProblem::Name { name } => write!(
                formatter,
                "names the rule `{name}`, and a rule's name is {IDENTIFIER}"
            ),
            RuleProblem::SelectorValue { value, declared } => write!(
                formatter,
                "selects `{value}`, which does not read as the key's type, {declared}, so no value equals it"
            ),
            RuleProblem::DefaultShape { declared } => write!(
                formatter,
                "is a default of the wrong shape: the field is declared {declared}"
            ),
            RuleProblem::Default { value, problem } => {
                write!(formatter, "defaults to `{value}`, which {problem}")
            }
            RuleProblem::Member { value, problem } => {
                write!(formatter, "holds the member `{value}`, which {problem}")
            }
            RuleProblem::SynonymTarget { value, problem } => {
                write!(formatter, "maps a synonym onto `{value}`, which {problem}")
            }
            RuleProblem::Template(error) => write!(formatter, "{error}"),
            RuleProblem::InadmissibleToken { token } => write!(
                formatter,
                "holds `{{{{{token}}}}}`, and a rule's default or route admits only `{{{{now}}}}`, `{{{{date}}}}`, `{{{{time}}}}` and `{{{{path.NAME}}}}`"
            ),
            RuleProblem::UndefinedCapture { name } => write!(
                formatter,
                "reads the path capture `{name}`, which the rule's `match.path` does not capture"
            ),
            RuleProblem::RouteNotFolder => formatter.write_str(
                "does not end in `/`, and a route names the folder a document is moved into",
            ),
            RuleProblem::RoutePath(problem) => {
                write!(formatter, "names no folder path: {problem}")
            }
            RuleProblem::RouteCharacter { character } => write!(
                formatter,
                "holds `{character}`, which is not portable in a folder name"
            ),
            RuleProblem::RouteClockWithColon { token } => write!(
                formatter,
                "holds `{{{{{token}}}}}`, which writes the clock with `:`, and `:` is not portable in a folder name; `{{{{{token}|slug}}}}` writes it without"
            ),
            RuleProblem::RouteOutsideAllowedPaths => formatter.write_str(
                "routes to a folder no document could stand directly inside under the rule's own `allowed_paths`",
            ),
            RuleProblem::RouteWeight { weight } => write!(
                formatter,
                "routes through a folder that weighs {weight} with the rule's own `allowed_paths`, past the ceiling of {PLACEMENT_CEILING} on judging one against the other"
            ),
            RuleProblem::RenameOntoForbidden { target } => write!(
                formatter,
                "renames onto `{target}`, which the same rule forbids"
            ),
            RuleProblem::RenameTwice { target, other } => write!(
                formatter,
                "renames onto `{target}`, which the same rule's forbidden field `{other}` renames onto too"
            ),
        }
    }
}

impl VaultSchema {
    /// The schema rules, in the byte order of their names.
    pub fn rules(&self) -> impl Iterator<Item = &Rule> {
        self.rules.values()
    }

    /// The rule called `name`, if the schema declares one.
    ///
    /// Its consumer is not built: repair (Layer 5B, NORN-351), which reads
    /// back each rule a stored finding cites for the fixes it declares. Rule
    /// judgment reads the rules selecting a document rather than one by name,
    /// `describe` reports every rule, in name order, and takes no rule name,
    /// and a `validate` naming a rule is checked against the declaration the
    /// store holds rather than here.
    pub fn rule(&self, name: &str) -> Option<&Rule> {
        self.rules.get(name)
    }

    /// The value `raw` is compared by under `field`, by a selector, a closed
    /// set and the disjointness the ceiling credits alike: a tag key — the
    /// tag carrier `tags`, declared or not and whatever type it is declared
    /// with, or a key declared `tags` — by the tag `raw` names, `#` marker
    /// optional, under the tag fold; any other key declared `number`,
    /// `boolean` or `date` by its typed value; and any other key exactly as
    /// written. `None` where `raw` does not read as the declared type, which
    /// equals nothing — the tag carrier included.
    ///
    /// Find's field equality compares by the same key: the store holds a tag
    /// key's fold beside each value it reads, and a typed key's sort key.
    pub fn equality_key(&self, field: &str, raw: &str) -> Option<TypedValue> {
        equality_key(field, self.declared_type(field), raw)
    }

    /// Whether `rule` selects the document at `path` holding `frontmatter`.
    ///
    /// The defaults fixpoint and rule judgment select by the same reading
    /// ([`VaultSchema::judge`]). The write gate (NORN-359), which selects a
    /// planned document's rules through it, is not built.
    ///
    /// `case` says how the path globs' literal letters compare with the
    /// path's: the store's recorded path order names it
    /// (`norn_store::StoredPathOrder::glob_case`), as it does for every glob
    /// over a vault path.
    pub fn selects(&self, rule: &Rule, path: &str, frontmatter: &ValueMap, case: CaseFold) -> bool {
        self.selects_in(rule, path, frontmatter.entries(), case)
    }

    /// Every rule that selects the document at `path` holding `frontmatter`,
    /// in name order. See [`VaultSchema::selects`] for `case`.
    pub fn selecting_rules(
        &self,
        path: &str,
        frontmatter: &ValueMap,
        case: CaseFold,
    ) -> Vec<&Rule> {
        self.select_counted(path, frontmatter.entries(), case, &mut RuleWork::default())
    }

    /// Every rule that selects a document at `path` whose frontmatter holds
    /// `entries`, in name order, tallying the selectors evaluated in `work`.
    fn select_counted(
        &self,
        path: &str,
        entries: &[(String, AuthoredValue)],
        case: CaseFold,
        work: &mut RuleWork,
    ) -> Vec<&Rule> {
        let selected: Vec<&Rule> = self
            .rules
            .values()
            .filter(|rule| self.selects_counted(rule, path, entries, case, work))
            .collect();
        work.rules_selected += selected.len() as u64;
        selected
    }

    /// Whether `rule` selects a document at `path` whose frontmatter holds
    /// `entries`.
    fn selects_in(
        &self,
        rule: &Rule,
        path: &str,
        entries: &[(String, AuthoredValue)],
        case: CaseFold,
    ) -> bool {
        self.selects_counted(rule, path, entries, case, &mut RuleWork::default())
    }

    /// **The one matcher**: whether `rule` selects a document at `path` whose
    /// frontmatter holds `entries`, tallying in `work` the rule evaluated,
    /// each selector term evaluated up to the first that fails, and the
    /// characters of each glob matched.
    fn selects_counted(
        &self,
        rule: &Rule,
        path: &str,
        entries: &[(String, AuthoredValue)],
        case: CaseFold,
        work: &mut RuleWork,
    ) -> bool {
        let selector = &rule.selector;
        work.rules_evaluated += 1;
        let glob = |glob: &Pattern, work: &mut RuleWork| {
            work.selector_terms += 1;
            work.pattern_characters += glob.as_str().chars().count() as u64;
            glob.matches(path, case)
        };
        selector.frontmatter.iter().all(|(key, values)| {
            work.selector_terms += 1;
            self.matches_value(key, value_in(entries, key), &values.keys)
        }) && selector
            .path
            .as_ref()
            .is_none_or(|path_glob| glob(path_glob, work))
            && !selector.exclude.iter().any(|excluded| glob(excluded, work))
    }

    /// Whether `value`, held under `key`, equals one of `keys` by its
    /// equality key ([`VaultSchema::equality_key`]).
    ///
    /// A value that does not read as its key's declared type or shape
    /// matches no selector. A list matches where any element does, as find's
    /// equality matches a list field, unless the key is declared
    /// [`Shape::Single`]; a scalar matches unless the key is declared
    /// [`Shape::List`]. A value of the other shape, like a map anywhere, is
    /// not a value of the key's shape, so it has nothing to compare. The shape
    /// is read as rule judgment reads it ([`judge::read_shape`]), and find's
    /// field equality reads it the same way, by the container its key's
    /// presence row names.
    fn matches_value(
        &self,
        key: &str,
        value: Option<&AuthoredValue>,
        keys: &BTreeSet<TypedValue>,
    ) -> bool {
        let shape = self.field(key).and_then(|field| field.shape());
        match value.map(|value| judge::read_shape(shape, value)) {
            Some(judge::ShapeReading::Elements(elements)) => elements.iter().any(|element| {
                element
                    .scalar_text()
                    .and_then(|raw| self.equality_key(key, &raw))
                    .is_some_and(|value| keys.contains(&value))
            }),
            Some(judge::ShapeReading::WrongShape) | None => false,
        }
    }
}

/// The value `entries` holds under `key`, where it holds one.
fn value_in<'a>(entries: &'a [(String, AuthoredValue)], key: &str) -> Option<&'a AuthoredValue> {
    entries
        .iter()
        .find(|(held, _)| held == key)
        .map(|(_, value)| value)
}

/// The frontmatter field a document's tags are written in, the **tag
/// carrier**. `norn_text::TAGS_FIELD` names the same field; this crate
/// reaches only the vocabulary, so it spells the name itself.
const TAGS_FIELD: &str = "tags";

/// The value `raw` is compared by under `field`, declared `declared`.
///
/// A tag key — the tag carrier, declared or not, or a key declared `tags` —
/// compares by the tag `raw` names: its `#` marker optional, as a frontmatter
/// tag is read, and under the tag fold ([`fold_tag`]). Any other key compares
/// by its typed value, which a text key's raw text is. `None` where `raw`
/// does not read as the declared type.
fn equality_key(field: &str, declared: FieldType, raw: &str) -> Option<TypedValue> {
    let typed = declared.read(raw).ok()?;
    if field == TAGS_FIELD || declared == FieldType::Tags {
        let name = raw.strip_prefix('#').unwrap_or(raw);
        Some(TypedValue::Text(fold_tag(name)))
    } else {
        Some(typed)
    }
}

/// Whether `value` leaves a required field unmet: absent or null.
fn is_missing(value: Option<&AuthoredValue>) -> bool {
    matches!(value, None | Some(AuthoredValue::Null))
}

/// The higher of two severities: an error over a warning.
fn higher(left: Severity, right: Severity) -> Severity {
    if left == Severity::Error || right == Severity::Error {
        Severity::Error
    } else {
        Severity::Warning
    }
}

/// `names` as a refusal lists them: each in backticks, comma separated.
pub(super) fn named(names: &[String]) -> String {
    names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether a template's literal text holds `character`.
fn literal_holds(template: &Template, character: char) -> bool {
    template.parts().iter().any(|part| match part {
        Part::Literal(text) => text.contains(character),
        Part::Token(_) => false,
    })
}
