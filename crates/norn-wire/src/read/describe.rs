//! `describe`: what a vault declares about itself, and what its documents
//! carry.
//!
//! **[`FieldType`], [`FieldShape`] and [`TagStance`] are copies of the config
//! crate's enums, and the copies are deliberate.** The content model is
//! `norn-config`'s, and this crate depends on nothing in the workspace, so the
//! declared type and shape a facet reports and the stance it reports on
//! undeclared tags are spelled again here rather than reached for. Neither definition is derived from the
//! other; a test in `norn-config` walks each pair of lists and holds them
//! equal, so a member added on one side without the other fails there rather
//! than crossing the seam as a spelling no reader has.
//!
//! **A facet's kind is injective.** [`Facet::kind`] maps each variant to a
//! distinct [`FacetKind`], so `--facets` selects exactly one shape and a
//! facet cursor orders one shape at a time. Two variants sharing a kind would
//! make a selection ambiguous and an ordered page interleave two row shapes
//! under one key.
//!
//! **A facet's cursor key is the facet's own key.** [`Facet::cursor_key`] is
//! the one function that turns a facet into the position a page stops at, and
//! it states that position for every shape, so the nine keys cannot drift
//! into two orders sharing a kind. What each shape is keyed by is stated
//! there rather than restated here.
//!
//! **A creation rule is reported as it is written.** Its target, its body and
//! every string in its frontmatter defaults are templates, and a facet carries
//! each as its source text — `{{var.title}}`, not a value — because what a
//! template fills to is fixed only when a document is made. The defaults
//! cross as the [`ValueMap`] a write carries, so a number stays a number and
//! a map keeps the order it is written in.
//!
//! **A schema rule is reported as it is written, too.** A rule facet mirrors
//! the rule's spelling in the vault schema: what it selects by, what it
//! excludes, and each constraint with the fix it declares, every value and
//! template as written — a required field's default is its source text, not
//! what it fills to. A part the rule does not declare is left out of the
//! facet rather than spelled empty, and its severity is always stated:
//! `warning` where the rule states none. A selector value is always a list,
//! however many values the rule wrote, each in the order written, repeats
//! included, and in the spelling the selector compares it by — a field row's
//! spelling of the scalar, so `1.50` reads `1.5`, and `1` and `"1"` both read
//! `1` — where a default and a template stay as written. The combined constraint the rules
//! selecting one document make is no facet: it is a function of these, read
//! where a document is judged.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;

use crate::address::VaultAddress;
use crate::cursor::{Cursor, CursorKey, FacetKind, Page};
use crate::finding::Severity;
use crate::plan::value::{AuthoredValue, ValueMap};

/// The type a vault's schema declares a field under.
///
/// On the wire a type is the flat string itself: `"text"`, `"date"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FieldType {
    /// Any string, which is also what an undeclared field is read as.
    Text,
    /// A number, ordered numerically.
    Number,
    /// `true` or `false`.
    Boolean,
    /// A calendar day or an instant, ordered chronologically.
    Date,
    /// A set of tag names.
    Tags,
}

impl FieldType {
    /// Every type the vocabulary holds, in declaration order.
    pub const ALL: [FieldType; 5] = [
        FieldType::Text,
        FieldType::Number,
        FieldType::Boolean,
        FieldType::Date,
        FieldType::Tags,
    ];

    /// The type as the string it is on the wire, which is the string a vault's
    /// schema declares it as.
    pub const fn as_str(&self) -> &'static str {
        match self {
            FieldType::Text => "text",
            FieldType::Number => "number",
            FieldType::Boolean => "boolean",
            FieldType::Date => "date",
            FieldType::Tags => "tags",
        }
    }
}

/// Whether a vault's schema declares a field to hold one value or a list.
///
/// On the wire a shape is the flat string itself: `"single"`, `"list"`. A
/// field declared with no shape admits either, and its facet carries none.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FieldShape {
    /// One value, never a list.
    Single,
    /// A list of values, each read as the field's declared type.
    List,
}

impl FieldShape {
    /// Every shape the vocabulary holds, in declaration order.
    pub const ALL: [FieldShape; 2] = [FieldShape::Single, FieldShape::List];

    /// The shape as the string it is on the wire, which is the string a
    /// vault's schema declares it as.
    pub const fn as_str(&self) -> &'static str {
        match self {
            FieldShape::Single => "single",
            FieldShape::List => "list",
        }
    }
}

/// What the vault says about a tag its facet does not admit.
///
/// On the wire a stance is the flat string itself: `"allow"`, `"report"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TagStance {
    /// Anything may be tagged.
    Allow,
    /// A tag outside the vocabulary is a finding.
    Report,
}

impl TagStance {
    /// Every stance the vocabulary holds, in declaration order.
    pub const ALL: [TagStance; 2] = [TagStance::Allow, TagStance::Report];

    /// The stance as the string it is on the wire, which is the string a
    /// vault's schema declares it as.
    pub const fn as_str(&self) -> &'static str {
        match self {
            TagStance::Allow => "allow",
            TagStance::Report => "report",
        }
    }
}

/// What container an observed field's value sits in.
///
/// On the wire a container is the flat string itself: `"scalar"`,
/// `"sequence"`, `"map"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContainerKind {
    /// One value.
    Scalar,
    /// A sequence of values.
    Sequence,
    /// A mapping.
    Map,
}

impl ContainerKind {
    /// Every container the vocabulary holds, in declaration order, which is
    /// the order an observed field lists the containers it is held in.
    pub const ALL: [ContainerKind; 3] = [
        ContainerKind::Scalar,
        ContainerKind::Sequence,
        ContainerKind::Map,
    ];

    /// The container as the string it is on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            ContainerKind::Scalar => "scalar",
            ContainerKind::Sequence => "sequence",
            ContainerKind::Map => "map",
        }
    }

    /// Where this container stands in [`ContainerKind::ALL`].
    const fn position(self) -> usize {
        match self {
            ContainerKind::Scalar => 0,
            ContainerKind::Sequence => 1,
            ContainerKind::Map => 2,
        }
    }
}

/// Which rule a path rule states.
///
/// On the wire a rule is the flat string itself: `"ambiguity_ignore"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PathRuleKind {
    /// Paths the resolution ladder does not count as candidates.
    AmbiguityIgnore,
}

/// What a schema rule selects documents by, as the rule writes it.
///
/// On the wire an object of the parts the rule states:
/// `{"frontmatter":{"type":["task"]},"path":"projects/<project>/**"}`. Each
/// frontmatter key's values are a list, however many the rule wrote, and a
/// part the rule does not state is left out.
#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct RuleMatch {
    /// Each frontmatter key the rule selects by, in key order, with the
    /// values any of which it matches, in the order written and in the
    /// spelling the selector compares each by: a field row's spelling of the
    /// scalar, so `1.50` reads `1.5`, and `1` and `"1"` both read `1`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub frontmatter: BTreeMap<String, Vec<String>>,
    /// The path glob the rule selects by, captures and all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl RuleMatch {
    /// A selection by `frontmatter` values and the glob `path`.
    pub fn new(
        frontmatter: impl IntoIterator<Item = (String, Vec<String>)>,
        path: Option<String>,
    ) -> Self {
        RuleMatch {
            frontmatter: frontmatter.into_iter().collect(),
            path,
        }
    }

    /// Whether the selection states nothing.
    pub fn is_empty(&self) -> bool {
        self.frontmatter.is_empty() && self.path.is_none()
    }
}

/// What a schema rule keeps out of what it selects, as the rule writes it.
///
/// On the wire: `{"path":["archive/**"]}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct RuleExclude {
    /// The globs whose paths the rule does not select, in the order written.
    pub path: Vec<String>,
}

impl RuleExclude {
    /// An exclusion of the paths `globs` match.
    pub fn new(globs: impl IntoIterator<Item = String>) -> Self {
        RuleExclude {
            path: globs.into_iter().collect(),
        }
    }
}

/// The fix a schema rule declares for a field it forbids.
///
/// On the wire: `"remove"`, or `{"rename_to":"owner"}`. A forbidden field
/// with no fix is `null` in its place.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RuleForbiddenFix {
    /// The field is removed.
    Remove,
    /// The field is renamed to this one.
    RenameTo(String),
}

/// The closed set of values a schema rule allows a field, as the rule writes
/// it.
///
/// On the wire: `{"values":["todo","done"],"synonyms":{"complete":"done"}}`,
/// with `synonyms` left out where the rule declares none.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct RuleClosedSet {
    /// The members, as written, in the order written.
    pub values: Vec<String>,
    /// Each synonym as written, in byte order, with the member it maps onto.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub synonyms: BTreeMap<String, String>,
}

impl RuleClosedSet {
    /// The set of `values`, with each synonym mapping onto a member.
    pub fn new(
        values: impl IntoIterator<Item = String>,
        synonyms: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        RuleClosedSet {
            values: values.into_iter().collect(),
            synonyms: synonyms.into_iter().collect(),
        }
    }
}

/// Where a document a schema rule selects may stand, and where a misplaced
/// one is routed, as the rule writes it.
///
/// On the wire: `{"paths":["tasks/**"],"route":"tasks/"}`, with `route` left
/// out where the rule declares none.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct RuleAllowedPaths {
    /// The globs a document may stand at, in the order written.
    pub paths: Vec<String>,
    /// The folder a misplaced document is routed to, as a template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
}

impl RuleAllowedPaths {
    /// The globs `paths`, routing a misplaced document to `route`.
    pub fn new(paths: impl IntoIterator<Item = String>, route: Option<String>) -> Self {
        RuleAllowedPaths {
            paths: paths.into_iter().collect(),
            route,
        }
    }
}

/// One named schema rule, as the vault's schema writes it.
///
/// On the wire the facet's own fields:
/// `{"facet":"rule","name":"tasks","severity":"warning","match":{…},"required":{"status":"todo"}}`.
/// The severity is always stated; every other part is left out where the
/// rule does not declare it. Each constraint is keyed by field, in key order:
/// `required` with the field's default as written, or `null` where it
/// declares none; `forbidden` with the fix it declares, or `null`; `one_of`
/// with the closed set; and `max_length` with the limit, in characters per
/// value.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct SchemaRule {
    /// The rule's name, unique in the schema.
    pub name: String,
    /// What the schema says the rule is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The severity a finding under the rule is reported at: `warning` where
    /// the rule states none.
    pub severity: Severity,
    /// What the rule selects documents by.
    #[serde(rename = "match", default, skip_serializing_if = "Option::is_none")]
    pub selects: Option<RuleMatch>,
    /// What the rule keeps out of what it selects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<RuleExclude>,
    /// Each field the rule requires, with its default as written, or `null`
    /// where it declares none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub required: BTreeMap<String, Option<AuthoredValue>>,
    /// Each field the rule forbids, with its fix, or `null` where it declares
    /// none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub forbidden: BTreeMap<String, Option<RuleForbiddenFix>>,
    /// Each field the rule closes over a set of values.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub one_of: BTreeMap<String, RuleClosedSet>,
    /// Each field the rule limits in length, in characters per value.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub max_length: BTreeMap<String, u64>,
    /// Where a document the rule selects may stand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_paths: Option<RuleAllowedPaths>,
}

impl SchemaRule {
    /// The rule `name`, reported at `severity`, declaring nothing else yet.
    pub fn new(name: impl Into<String>, severity: Severity) -> Self {
        SchemaRule {
            name: name.into(),
            description: None,
            severity,
            selects: None,
            exclude: None,
            required: BTreeMap::new(),
            forbidden: BTreeMap::new(),
            one_of: BTreeMap::new(),
            max_length: BTreeMap::new(),
            allowed_paths: None,
        }
    }

    /// The same rule, described as `description`.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// The same rule, selecting by `selects`; a selection stating nothing is
    /// left out.
    #[must_use]
    pub fn with_match(mut self, selects: RuleMatch) -> Self {
        self.selects = (!selects.is_empty()).then_some(selects);
        self
    }

    /// The same rule, excluding the paths `exclude` names; an exclusion naming
    /// none is left out.
    #[must_use]
    pub fn with_exclude(mut self, exclude: RuleExclude) -> Self {
        self.exclude = (!exclude.path.is_empty()).then_some(exclude);
        self
    }

    /// The same rule, requiring `field`, filled by `default` where it declares
    /// one.
    #[must_use]
    pub fn with_required(
        mut self,
        field: impl Into<String>,
        default: Option<AuthoredValue>,
    ) -> Self {
        self.required.insert(field.into(), default);
        self
    }

    /// The same rule, forbidding `field`, fixed by `fix` where it declares one.
    #[must_use]
    pub fn with_forbidden(
        mut self,
        field: impl Into<String>,
        fix: Option<RuleForbiddenFix>,
    ) -> Self {
        self.forbidden.insert(field.into(), fix);
        self
    }

    /// The same rule, closing `field` over `set`.
    #[must_use]
    pub fn with_one_of(mut self, field: impl Into<String>, set: RuleClosedSet) -> Self {
        self.one_of.insert(field.into(), set);
        self
    }

    /// The same rule, limiting `field` to `limit` characters per value.
    #[must_use]
    pub fn with_max_length(mut self, field: impl Into<String>, limit: u64) -> Self {
        self.max_length.insert(field.into(), limit);
        self
    }

    /// The same rule, allowing a document it selects to stand at `allowed`.
    #[must_use]
    pub fn with_allowed_paths(mut self, allowed: RuleAllowedPaths) -> Self {
        self.allowed_paths = Some(allowed);
        self
    }
}

/// One thing a vault says about itself, or one thing its documents say.
///
/// On the wire a facet is an object tagged `facet`:
/// `{"facet":"declared_tag","name":"area"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "facet", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Facet {
    /// A field the vault's schema declares, with the declaration.
    #[non_exhaustive]
    DeclaredField {
        /// The frontmatter key.
        key: String,
        /// The type the declaration gives it.
        field_type: FieldType,
        /// The shape the declaration gives it, left out where it declares
        /// none and either is admitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shape: Option<FieldShape>,
    },
    /// A field the vault's documents carry, declared or not.
    ///
    /// One facet per key: a key one document holds as a scalar and another as
    /// a sequence is one observed field held in both.
    #[non_exhaustive]
    ObservedField {
        /// The frontmatter key.
        key: String,
        /// Every container some document holds the key's value in, each once,
        /// in the order the container vocabulary declares: scalar, sequence,
        /// map.
        containers: Vec<ContainerKind>,
    },
    /// A tag name the vault's schema declares.
    #[non_exhaustive]
    DeclaredTag {
        /// The tag, without its `#`.
        name: String,
    },
    /// A pattern the vault's tag facet admits beyond its literal names.
    #[non_exhaustive]
    TagPattern {
        /// The pattern, as the schema writes it.
        pattern: String,
    },
    /// A path rule the vault's schema states.
    #[non_exhaustive]
    PathRule {
        /// Which rule it states.
        rule: PathRuleKind,
        /// The pattern it states it over.
        pattern: String,
    },
    /// What the vault's schema says about a tag its facet does not admit.
    #[non_exhaustive]
    UndeclaredTags {
        /// The stance the schema declares.
        stance: TagStance,
    },
    /// A named rule the vault's schema declares for making a document.
    #[non_exhaustive]
    CreationRule {
        /// The name a document is made under: `new --as task`.
        name: String,
        /// Where a document the rule makes is written, as a template.
        target: String,
        /// The variables a caller must supply, in the order declared.
        variables: Vec<String>,
        /// The frontmatter a document the rule makes starts with; every
        /// string in it is a template.
        frontmatter_defaults: ValueMap,
        /// The body a document the rule makes starts with, as a template, and
        /// `null` where the rule writes none.
        body: Option<String>,
    },
    /// Where the vault's schema says untyped capture lands.
    #[non_exhaustive]
    Inbox {
        /// Where a captured document is written, as a template.
        target: String,
    },
    /// A named rule the vault's schema states about the documents it
    /// selects, as the schema writes it.
    Rule(SchemaRule),
}

impl Facet {
    /// The field `key`, declared as `field_type` and, where it declares one,
    /// as `shape`.
    pub fn declared_field(
        key: impl Into<String>,
        field_type: FieldType,
        shape: Option<FieldShape>,
    ) -> Self {
        Facet::DeclaredField {
            key: key.into(),
            field_type,
            shape,
        }
    }

    /// The field `key`, observed held in each of `containers`: listed each
    /// once, in [`ContainerKind::ALL`]'s order, whatever order they are named
    /// in.
    pub fn observed_field(
        key: impl Into<String>,
        containers: impl IntoIterator<Item = ContainerKind>,
    ) -> Self {
        let mut held = [false; ContainerKind::ALL.len()];
        for container in containers {
            held[container.position()] = true;
        }
        Facet::ObservedField {
            key: key.into(),
            containers: ContainerKind::ALL
                .into_iter()
                .filter(|container| held[container.position()])
                .collect(),
        }
    }

    /// The declared tag `name`.
    pub fn declared_tag(name: impl Into<String>) -> Self {
        Facet::DeclaredTag { name: name.into() }
    }

    /// The tag pattern `pattern`.
    pub fn tag_pattern(pattern: impl Into<String>) -> Self {
        Facet::TagPattern {
            pattern: pattern.into(),
        }
    }

    /// The `rule` stated over `pattern`.
    pub fn path_rule(rule: PathRuleKind, pattern: impl Into<String>) -> Self {
        Facet::PathRule {
            rule,
            pattern: pattern.into(),
        }
    }

    /// The schema's `stance` on a tag its facet does not admit.
    pub const fn undeclared_tags(stance: TagStance) -> Self {
        Facet::UndeclaredTags { stance }
    }

    /// The creation rule `name`, writing to `target`.
    pub fn creation_rule(
        name: impl Into<String>,
        target: impl Into<String>,
        variables: Vec<String>,
        frontmatter_defaults: ValueMap,
        body: Option<String>,
    ) -> Self {
        Facet::CreationRule {
            name: name.into(),
            target: target.into(),
            variables,
            frontmatter_defaults,
            body,
        }
    }

    /// The inbox, writing to `target`.
    pub fn inbox(target: impl Into<String>) -> Self {
        Facet::Inbox {
            target: target.into(),
        }
    }

    /// The schema rule `rule`.
    pub const fn rule(rule: SchemaRule) -> Self {
        Facet::Rule(rule)
    }

    /// What this facet is a facet of.
    ///
    /// The match carries no wildcard, so a facet minted without a kind does not
    /// compile: [`FacetKind`] is what a request selects facets by and what a
    /// facet cursor orders by, and this is the one place the two lists are held
    /// together. The map is injective — one kind per shape — so a request
    /// naming a kind names one shape and a page ordered by kind holds one.
    pub const fn kind(&self) -> FacetKind {
        match self {
            Facet::DeclaredField { .. } => FacetKind::DeclaredField,
            Facet::ObservedField { .. } => FacetKind::ObservedField,
            Facet::DeclaredTag { .. } => FacetKind::DeclaredTag,
            Facet::TagPattern { .. } => FacetKind::TagPattern,
            Facet::PathRule { .. } => FacetKind::PathRule,
            Facet::UndeclaredTags { .. } => FacetKind::UndeclaredTags,
            Facet::CreationRule { .. } => FacetKind::CreationRule,
            Facet::Inbox { .. } => FacetKind::Inbox,
            Facet::Rule(_) => FacetKind::Rule,
        }
    }

    /// Where a page of facets stops at this facet.
    ///
    /// The key is the one text the facet itself spells: a declared field's
    /// and an observed field's frontmatter key, a declared tag's name, a tag
    /// pattern's and a path rule's pattern, a creation rule's and a schema
    /// rule's name, the inbox's target, and, for the undeclared-tags facet, the
    /// stance spelling — `allow` or `report`.
    /// Beside the kind, that names one facet within its shape, which is what a
    /// continuation resumes after.
    ///
    /// The destructuring carries no wildcard, so neither a facet shape minted
    /// without a key nor a field added to one compiles until this says what
    /// the order stops at.
    pub fn cursor_key(&self) -> CursorKey {
        let key = match self {
            Facet::DeclaredField {
                key,
                field_type: _,
                shape: _,
            } => key.clone(),
            Facet::ObservedField { key, containers: _ } => key.clone(),
            Facet::DeclaredTag { name } => name.clone(),
            Facet::TagPattern { pattern } => pattern.clone(),
            Facet::PathRule { rule: _, pattern } => pattern.clone(),
            Facet::UndeclaredTags { stance } => stance.as_str().to_string(),
            Facet::CreationRule {
                name,
                target: _,
                variables: _,
                frontmatter_defaults: _,
                body: _,
            } => name.clone(),
            Facet::Inbox { target } => target.clone(),
            Facet::Rule(SchemaRule {
                name,
                description: _,
                severity: _,
                selects: _,
                exclude: _,
                required: _,
                forbidden: _,
                one_of: _,
                max_length: _,
                allowed_paths: _,
            }) => name.clone(),
        };
        CursorKey::facet(self.kind(), key)
    }
}

/// What `describe` answers with: one page of facets, in `(kind, key)` order —
/// the kinds in the byte order of their codes ([`FacetKind::in_code_order`]),
/// and within a kind the facets in the byte order of their keys.
pub type DescribeReport = Page<Facet>;

/// What a `describe` request carries.
///
/// The facets it answers stand in `(kind, key)` order: the kind, in the byte
/// order of its code, then the key in byte order, which is the order a facet
/// cursor names a position in.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct DescribeParams {
    /// The vault to answer about.
    pub vault: VaultAddress,
    /// The kinds of facet to report. Empty reports every kind. Whatever order
    /// it names them in, they are answered in the byte order of their codes.
    pub facets: Vec<FacetKind>,
    /// How many facets at most. `null` leaves the ceiling to the host.
    pub limit: Option<u32>,
    /// Where to continue from. `null` starts at the first facet.
    pub after: Option<Cursor>,
}

impl DescribeParams {
    /// A `describe` of `vault`: every facet of every kind.
    pub const fn new(vault: VaultAddress) -> Self {
        DescribeParams {
            vault,
            facets: Vec::new(),
            limit: None,
            after: None,
        }
    }

    /// The request reporting `facets` alone.
    #[must_use]
    pub fn with_facets(mut self, facets: impl IntoIterator<Item = FacetKind>) -> Self {
        self.facets = facets.into_iter().collect();
        self
    }

    /// The request bounded at `limit` facets.
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
